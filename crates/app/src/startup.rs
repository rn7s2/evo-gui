//! What the empty tab needs before the user can choose anything (§9.4, §9.5),
//! loaded without ever blocking the UI thread (§2 rule 6).
//!
//! Three things happen here, in this order:
//!
//! 1. the cached catalog (`model-cache.json`) is handed to the empty tabs
//!    straight away — it is already on disk, so the choosers fill in the first
//!    frame;
//! 2. the session scan (`store::history`) runs on its own thread and its rows
//!    arrive later;
//! 3. if that cache is missing or older than [`CACHE_MAX_AGE`], a catalog probe
//!    (`tab_engine::catalog`, two throwaway servers under
//!    `~/.evo/desktop/probe`) refreshes it, is written back through `store`, and
//!    handed over when it lands.
//!
//! Each arrival goes through [`crate::launcher`], which both pushes it into the
//! tabs that exist and remembers it for the ones created later.

use std::time::{Duration, SystemTime};

use gpui_kit::App;

use store::app_state::{AppState, Binaries};
use store::history::{self, ScanBudget};
use store::model_cache::ModelCache;
use store::paths::Root;
use store::time;

use crate::housekeeping;
use crate::launcher;
use crate::logging::AppLog;
use crate::Shell;

/// How old the cached catalog may be before it is probed again.
pub const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether the cache has to be refreshed: empty, undated, or older than
/// [`CACHE_MAX_AGE`].
pub fn cache_is_stale(cache: &ModelCache, now_epoch: u64) -> bool {
    if cache.is_empty() {
        return true;
    }
    match cache.fetched_epoch() {
        Some(fetched) => now_epoch.saturating_sub(fetched) > CACHE_MAX_AGE.as_secs(),
        // A cache with no readable timestamp cannot be judged; refresh it.
        None => true,
    }
}

/// Start the launch-time loads. Called once, after the window exists.
pub fn start(cx: &mut App) {
    let (root, log, cache, binaries) = snapshot(cx);
    log.info(format!(
        "startup: cached catalog has {} model(s), fetched {}",
        cache.models().len(),
        if cache.fetched_at.is_empty() { "never" } else { &cache.fetched_at }
    ));

    // 1. The cache we already have goes out first.
    launcher::set_catalog(cx, cache.clone(), None);
    log.info(format!(
        "startup: {} tab(s) will show it",
        launcher::tab_count(cx)
    ));

    // 2. The session scan, on the store's own thread, bridged onto an
    //    `async_channel` so the rows land on a gpui task.
    launcher::set_scanning(cx, true);
    let sessions = history::sessions_dir();
    let scan = history::scan_in_background(sessions.clone(), ScanBudget::default());
    let (scan_tx, scan_rx) = async_channel::bounded(1);
    let bridged = std::thread::Builder::new()
        .name("evo-desktop-history".to_owned())
        .spawn(move || {
            if let Ok(outcome) = scan.recv() {
                let _ = scan_tx.send_blocking(outcome);
            }
        });
    if let Err(error) = bridged {
        log.error(format!("could not start the history bridge: {error}"));
    }
    let scan_log = log.clone();
    let scan_sessions = sessions.clone();
    cx.spawn(async move |cx| {
        let Ok(outcome) = scan_rx.recv().await else {
            return;
        };
        cx.update(|cx| {
            // The app's own recents join the scan (§9.5, §14.3).
            let recents = cx.global::<Shell>().recents.clone();
            let entries = history::merge(outcome.entries, &recents);
            scan_log.info(format!(
                "history: {} row(s) ({} files read of {} seen{})",
                entries.len(),
                outcome.files_read,
                outcome.files_seen,
                if outcome.stopped_early { ", stopped early" } else { "" }
            ));
            let error = (!scan_sessions.is_dir()).then(|| {
                format!("{} could not be read", scan_sessions.display())
            });
            if let Some(error) = &error {
                scan_log.warn(format!("history: {error}"));
            }
            launcher::set_history(cx, entries, error);
        });
    })
    .detach();

    // 3. Tab directories nobody has touched for a week, on a thread of its own:
    //    the walk is I/O and §9.7's evidence is what is at stake, not speed.
    prune_tab_dirs(root.clone(), log.clone());

    // 4. The catalog probe, when the cache cannot be trusted.
    if cache_is_stale(&cache, time::now_epoch()) {
        log.info(format!(
            "catalog: probing with {} (cache older than {:?} or missing)",
            binaries.evo_agent.display(),
            CACHE_MAX_AGE
        ));
        let probe = tab_engine::catalog::learn(binaries.evo_agent.clone(), root.probe_dir());
        let probe_log = log.clone();
        let probe_root = root.clone();
        cx.spawn(async move |cx| {
            let Ok(update) = probe.recv().await else {
                return;
            };
            cx.update(|cx| match update {
                tab_engine::catalog::CatalogUpdate::Done { registry, kernel_apis } => {
                    let mut cache = ModelCache::from_probe(registry);
                    if let Some(apis) = kernel_apis {
                        cache = cache.with_kernel_apis(apis);
                    }
                    probe_log.info(format!(
                        "catalog: probed {} model(s), {} kernel api(s)",
                        cache.models().len(),
                        cache.kernel_apis.len()
                    ));
                    if let Err(error) = cache.save(&probe_root) {
                        probe_log.error(format!(
                            "could not save {}: {error}",
                            probe_root.model_cache().display()
                        ));
                    }
                    launcher::set_catalog(cx, cache, None);
                }
                tab_engine::catalog::CatalogUpdate::Failed { message, log_tail } => {
                    // The cache we have stays in use; the tab says the catalog
                    // could not be refreshed (§9.4).
                    probe_log.error(format!("catalog probe failed: {message}"));
                    if !log_tail.is_empty() {
                        probe_log.error(format!("catalog probe log tail:\n{log_tail}"));
                    }
                    launcher::set_catalog_error(cx, Some(message));
                }
            });
        })
        .detach();
    }
}

/// Remove the `tabs/<id>/` directories the app is done with (§6).
///
/// A directory that `app.json` names as an open tab is kept whatever its age, and
/// so is anything touched inside [`housekeeping::TAB_DIR_TTL`] — a swarm's
/// `swarm.log` is the evidence §9.7 shows when a boot fails, so it is never
/// thrown away while it might still be wanted.
fn prune_tab_dirs(root: Root, log: AppLog) {
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-prune".to_owned())
        .spawn(move || {
            let keep: Vec<String> = AppState::load(&root)
                .tabs
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect();
            let pruned =
                housekeeping::prune_tab_dirs(&root, &keep, housekeeping::TAB_DIR_TTL, SystemTime::now());
            match pruned {
                Ok(pruned) if pruned.removed.is_empty() && pruned.failed == 0 => {
                    thread_log
                        .info(format!("tab dirs: {} kept, none old enough to prune", pruned.kept));
                }
                Ok(pruned) => thread_log.info(format!(
                    "tab dirs: pruned {:?}, kept {}{}",
                    pruned.removed,
                    pruned.kept,
                    if pruned.failed > 0 {
                        format!(", {} could not be removed", pruned.failed)
                    } else {
                        String::new()
                    }
                )),
                Err(error) => thread_log.warn(format!("tab dirs: could not be pruned: {error}")),
            }
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the tab-directory prune: {error}"));
    }
}

fn snapshot(cx: &App) -> (Root, AppLog, ModelCache, Binaries) {
    let shell = cx.global::<Shell>();
    (shell.root.clone(), shell.log.clone(), shell.launcher.cache.clone(), shell.binaries.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache(registry: serde_json::Value, fetched_at: &str) -> ModelCache {
        ModelCache { version: 1, fetched_at: fetched_at.to_owned(), kernel_apis: Vec::new(), registry }
    }

    #[test]
    fn a_missing_cache_is_stale() {
        assert!(cache_is_stale(&ModelCache::default(), 1_000));
    }

    #[test]
    fn a_cache_with_no_models_is_stale() {
        let empty_models = cache(json!({ "models": [] }), "2026-09-29T00:00:00Z");
        assert!(cache_is_stale(&empty_models, 1_000));
    }

    #[test]
    fn a_cache_younger_than_a_day_is_fresh() {
        let fresh = cache(json!({ "models": [{ "id": "m" }] }), "2026-09-29T00:00:00Z");
        let fetched = fresh.fetched_epoch().expect("a timestamp");
        assert!(!cache_is_stale(&fresh, fetched + 60));
        assert!(!cache_is_stale(&fresh, fetched + CACHE_MAX_AGE.as_secs()));
    }

    #[test]
    fn a_cache_older_than_a_day_is_stale() {
        let old = cache(json!({ "models": [{ "id": "m" }] }), "2026-09-27T00:00:00Z");
        let fetched = old.fetched_epoch().expect("a timestamp");
        assert!(cache_is_stale(&old, fetched + CACHE_MAX_AGE.as_secs() + 1));
    }

    #[test]
    fn an_undated_cache_is_stale() {
        let undated = cache(json!({ "models": [{ "id": "m" }] }), "not a date");
        assert!(cache_is_stale(&undated, 1_000));
    }
}
