//! What the empty tab needs before the user can choose anything (§9.4, §9.5),
//! loaded without ever blocking the UI thread (§2 rule 6).
//!
//! Three things happen here, in this order:
//!
//! 1. the cached catalog (`model-cache.json`) is pushed straight away — it is
//!    already on disk, so the choosers fill in the first frame;
//! 2. the session scan (`store::history`) runs on its own thread and its rows
//!    arrive later;
//! 3. if the cache is missing or older than [`CACHE_MAX_AGE`], a catalog probe
//!    (`tab_engine::catalog`, two throwaway servers under
//!    `~/.evo/desktop/probe`) refreshes it, is written back through `store`, and
//!    pushed when it lands.
//!
//! Nothing here touches `~/.evo/desktop` itself: persistence is the store
//! crate's, and the probe's scratch files live under `probe/`.

use std::time::Duration;

use gpui_kit::App;

use store::app_state::{Binaries, Recent};
use store::history::{self, HistoryEntry, ScanBudget};
use store::model_cache::ModelCache;
use store::paths::Root;

use workspace::{TabContent, TabState, WorkspaceView};

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
    let (root, log, cache, recents, binaries, view) = snapshot(cx);
    log.info(format!(
        "startup: cached catalog has {} model(s), fetched {}",
        cache.models().len(),
        if cache.fetched_at.is_empty() { "never" } else { &cache.fetched_at }
    ));

    // 1. The cache we already have goes out first.
    push(cx, &view, &log, TabData::Catalog(Box::new(cache.clone())));

    // 2. The session scan, on the store's own thread, bridged onto an
    //    `async_channel` so the rows land on a gpui task.
    push(cx, &view, &log, TabData::Scanning(true));
    let scan = history::scan_in_background(history::sessions_dir(), ScanBudget::default());
    let (scan_tx, scan_rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("evo-desktop-history".to_owned())
        .spawn(move || {
            if let Ok(outcome) = scan.recv() {
                let _ = scan_tx.send_blocking(outcome);
            }
        })
        .ok();
    let scan_log = log.clone();
    let scan_view = view.clone();
    cx.spawn(async move |cx| {
        let Ok(outcome) = scan_rx.recv().await else {
            return;
        };
        cx.update(|cx| {
            // The app's own recents join the scan (§9.5, §14.3).
            let entries = history::merge(outcome.entries, &recents);
            scan_log.info(format!(
                "history: {} entries ({} files read of {} seen{})",
                entries.len(),
                outcome.files_read,
                outcome.files_seen,
                if outcome.stopped_early { ", stopped early" } else { "" }
            ));
            push(cx, &scan_view, &scan_log, TabData::History(entries));
            push(cx, &scan_view, &scan_log, TabData::Scanning(false));
        });
    })
    .detach();

    // 3. The catalog probe, when the cache cannot be trusted.
    if cache_is_stale(&cache, store::time::now_epoch()) {
        log.info(format!(
            "catalog: probing with {} (cache older than {:?} or missing)",
            binaries.evo_agent.display(),
            CACHE_MAX_AGE
        ));
        let probe = tab_engine::catalog::learn(binaries.evo_agent.clone(), root.probe_dir());
        let probe_log = log.clone();
        let probe_root = root.clone();
        let probe_view = view;
        cx.spawn(async move |cx| {
            let Ok(update) = probe.recv().await else {
                return;
            };
            cx.update(|cx| {
                match update {
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
                        push(cx, &probe_view, &probe_log, TabData::Catalog(Box::new(cache)));
                    }
                    tab_engine::catalog::CatalogUpdate::Failed { message, log_tail } => {
                        // The cache we have stays in use; the tab simply keeps
                        // showing what it had (§9.4).
                        probe_log.error(format!("catalog probe failed: {message}"));
                        if !log_tail.is_empty() {
                            probe_log.error(format!("catalog probe log tail:\n{log_tail}"));
                        }
                    }
                }
            });
        })
        .detach();
    }
}

/// One of the three things the empty tab is fed.
pub enum TabData {
    Catalog(Box<ModelCache>),
    History(Vec<HistoryEntry>),
    /// The scan started (`true`) or finished (`false`) — the empty tab's
    /// "scanning…" state.
    Scanning(bool),
}

/// Push one piece of launch data into every empty tab.
///
/// **Seam.** The empty tab's setters are lane 6's — `set_registry` /
/// `set_kernel_apis` / `set_history_entries` / `set_scanning`. Until they exist
/// this records what would have gone where; the durable half (the model cache in
/// `~/.evo/desktop/model-cache.json`) is already written, so a later launch
/// still benefits. Wiring them is this one function.
pub fn push(cx: &mut App, view: &Option<gpui_kit::WeakEntity<WorkspaceView>>, log: &AppLog, data: TabData) {
    let empty = empty_tabs(cx, view);
    match data {
        TabData::Catalog(cache) => log.info(format!(
            "catalog: {} model(s), {} kernel api(s) ready for {empty} empty tab(s)",
            cache.models().len(),
            cache.kernel_apis.len()
        )),
        TabData::History(entries) => {
            log.info(format!("history: {} row(s) ready for {empty} empty tab(s)", entries.len()))
        }
        TabData::Scanning(true) => log.info(format!("history: scanning ({empty} empty tab(s))")),
        TabData::Scanning(false) => log.info("history: scan finished"),
    }
}

/// How many tabs are still empty (no folder chosen, no swarm).
fn empty_tabs(cx: &mut App, view: &Option<gpui_kit::WeakEntity<WorkspaceView>>) -> usize {
    let Some(view) = view.as_ref().and_then(|view| view.upgrade()) else {
        return 0;
    };
    view.read(cx)
        .tabs()
        .iter()
        .filter(|tab: &&gpui_kit::Entity<TabContent>| matches!(tab.read(cx).state(), TabState::Empty))
        .count()
}

type Snapshot = (Root, AppLog, ModelCache, Vec<Recent>, Binaries, Option<gpui_kit::WeakEntity<WorkspaceView>>);

fn snapshot(cx: &App) -> Snapshot {
    let shell = cx.global::<Shell>();
    (
        shell.root.clone(),
        shell.log.clone(),
        shell.cache.clone(),
        shell.recents.clone(),
        shell.binaries.clone(),
        shell.view.clone(),
    )
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
        let empty = ModelCache::default();
        assert!(cache_is_stale(&empty, 1_000));
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
