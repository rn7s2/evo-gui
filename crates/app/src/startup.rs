//! What the empty tab needs before the user can choose anything (§9.4, §9.5),
//! loaded without ever blocking the UI thread (§2 rule 6).
//!
//! Three things happen here, in this order:
//!
//! 1. the cached catalog (`model-cache.json`) is handed to the empty tabs
//!    straight away — it is already on disk, so the choosers fill in the first
//!    frame;
//! 2. `evo-agent sessions --json` runs on a thread of its own and its rows
//!    arrive later: one process, one JSON document, no journal is read here
//!    (§9);
//! 3. `evo-swarm catalog --json` refreshes the cache — the same shape of read,
//!    and it replaces the two throwaway servers this used to need. What it found
//!    is written back through `store` and handed over when it lands.
//!
//! Two more run beside them: the tab directories nobody has touched are pruned
//! (§6), and the two binaries are asked what they take (`<bin> --help`) so that the
//! question a window asks while it is being built — whether this client's own note
//! can be passed to them ([`crate::prompt_note`], via
//! [`store::cli::supports_prompt_note`]) — is a cache read rather than a process on
//! the UI thread.
//!
//! Each arrival goes through [`crate::launcher`], which both pushes it into the
//! tabs that exist and remembers it for the ones created later.

use std::path::PathBuf;
use std::time::SystemTime;

use gpui_kit::App;

use store::app_state::Binaries;
use store::cli;
use store::history::{self, SessionsQuery};
use store::model_cache::ModelCache;
use store::paths::Root;

use crate::housekeeping;
use crate::launcher;
use crate::logging::AppLog;
use crate::Shell;

/// The catalog is read with `evo-swarm`: its body is the one that carries
/// `lanes` — the models a lane can register, which the lanes chooser greys out
/// against (§5.6, §9). `evo-agent catalog --json` prints the same body without
/// them.
const CATALOG: &[&str] = &["catalog", "--json"];

/// Start the launch-time loads. Called once, after the window exists.
pub fn start(cx: &mut App) {
    let (root, log, cache, binaries) = snapshot(cx);
    log.info(format!(
        "startup: cached catalog has {} model(s), fetched {}",
        cache.models().len(),
        if cache.fetched_at.is_empty() {
            "never"
        } else {
            &cache.fetched_at
        }
    ));

    // 1. The cache we already have goes out first: the window is up, and this is
    //    what it shows while a real catalog is being read.
    launcher::set_catalog(cx, cache, None);
    log.info(format!(
        "startup: {} tab(s) will show it",
        launcher::tab_count(cx)
    ));

    // 2. The session index, read by evo itself, on a thread of its own.
    load_history(cx, binaries.evo_agent.clone());

    // 3. Tab directories nobody has touched for a week, on a thread of its own:
    //    the walk is I/O and §9.7's evidence is what is at stake, not speed.
    prune_tab_dirs(root.clone(), log.clone());

    // 4. The catalog, refreshed the same way.
    load_catalog(cx, root, binaries.evo_swarm.clone());

    // What the binaries take, asked off the UI thread: the usage read behind
    // `--prompt-note` (see the module docs). The window asks the same question of
    // the same two binaries while it is being built (§7.2), and this is what makes
    // that a cache read.
    probe_prompt_notes(binaries, log);
}

/// Ask both binaries what they take (`--prompt-note`), on a thread of its own.
///
/// Nothing is pushed to a tab and nothing here can fail a launch: the answer is
/// kept in `store::cli`'s own cache, and whoever asks next — the window, building
/// a [`LaunchEnv`](workspace::LaunchEnv) — gets it without a process. The line in
/// the log is what says the ask happened, for the sessions that are then told
/// nothing.
fn probe_prompt_notes(binaries: Binaries, log: AppLog) {
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-prompt-note".to_owned())
        .spawn(move || {
            let takes = |bin: &std::path::Path| {
                if cli::supports_prompt_note(bin) {
                    "takes"
                } else {
                    "does not take"
                }
            };
            thread_log.info(format!(
                "prompt note: {} {} --prompt-note, {} {} it",
                binaries.evo_agent.display(),
                takes(&binaries.evo_agent),
                binaries.evo_swarm.display(),
                takes(&binaries.evo_swarm)
            ));
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the prompt-note probe: {error}"));
    }
}

/// Read the model catalog again, in the background, and hand it over when it
/// lands. Called at startup and whenever Settings changes the binaries (§9.4):
/// a new `evo-swarm` is a new catalog.
pub fn refresh_catalog(cx: &mut App) {
    let (root, bin) = {
        let shell = cx.global::<Shell>();
        (shell.root.clone(), shell.binaries.evo_swarm.clone())
    };
    load_catalog(cx, root, bin);
}

/// `evo-agent sessions --json` on a thread of its own, bridged onto a gpui task.
///
/// A failure is not an empty history: a read that could not run says why, and
/// the empty tabs show that line instead of claiming there is nothing to resume
/// (§9.5).
///
/// Called at startup, and again whenever a tab that held a session is removed
/// ([`workspace::HistoryStale`]): the newly freed session should appear in the
/// history of every empty tab that is still open.
pub fn load_history(cx: &mut App, bin: PathBuf) {
    let log = cx.global::<Shell>().log.clone();
    launcher::set_history_loading(cx, true);

    let (tx, rx) = async_channel::bounded(1);
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-history".to_owned())
        .spawn(move || {
            let read =
                history::fetch(&bin, &SessionsQuery::resumable()).map_err(|error| error.summary());
            if let Err(message) = &read {
                thread_log.warn(format!("history: {message}"));
            }
            let _ = tx.send_blocking(read);
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the history read: {error}"));
        launcher::set_history_loading(cx, false);
        return;
    }

    cx.spawn(async move |cx| {
        let Ok(read) = rx.recv().await else {
            return;
        };
        cx.update(|cx| match read {
            Ok(sessions) => {
                // The app's own recents join the index (§9.5, §14.3).
                let recents = cx.global::<Shell>().recents.clone();
                let entries = history::merge(sessions, &recents);
                cx.global::<Shell>().log.info(format!(
                    "history: {} row(s) from the session index",
                    entries.len()
                ));
                launcher::set_history(cx, entries, None);
            }
            Err(message) => launcher::set_history(cx, Vec::new(), Some(message)),
        });
    })
    .detach();
}

/// `evo-swarm catalog --json` on a thread of its own: one process, one document,
/// and the cache is rewritten with what it printed.
fn load_catalog(cx: &mut App, root: Root, bin: PathBuf) {
    let log = cx.global::<Shell>().log.clone();
    log.info(format!("catalog: reading with {}", bin.display()));

    let (tx, rx) = async_channel::bounded(1);
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-catalog".to_owned())
        .spawn(move || {
            let read = cli::run_json(&bin, &cli::args(CATALOG))
                .map(|body| ModelCache::from_catalog("evo-swarm", body))
                .map_err(|error| error.summary());
            if let Err(message) = &read {
                thread_log.warn(format!("catalog: {message}"));
            }
            let _ = tx.send_blocking(read);
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the catalog read: {error}"));
        launcher::set_catalog_error(cx, Some(format!("{error}")));
        return;
    }

    cx.spawn(async move |cx| {
        let Ok(read) = rx.recv().await else {
            return;
        };
        cx.update(|cx| match read {
            Ok(cache) => {
                let log = cx.global::<Shell>().log.clone();
                log.info(format!(
                    "catalog: {} model(s), {} lane model(s)",
                    cache.models().len(),
                    cache.catalog().lane_models().map_or(0, |lanes| lanes.len())
                ));
                if let Err(error) = cache.save(&root) {
                    log.error(format!(
                        "could not save {}: {error}",
                        root.model_cache().display()
                    ));
                }
                launcher::set_catalog(cx, cache, None);
            }
            // The cache we have stays in use; the tab says the catalog could not
            // be refreshed (§9.4).
            Err(message) => launcher::set_catalog_error(cx, Some(message)),
        });
    })
    .detach();
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
            let keep: Vec<String> = store::app_state::AppState::load(&root)
                .tabs
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect();
            let pruned = housekeeping::prune_tab_dirs(
                &root,
                &keep,
                housekeeping::TAB_DIR_TTL,
                SystemTime::now(),
            );
            match pruned {
                Ok(pruned) if pruned.removed.is_empty() && pruned.failed == 0 => {
                    thread_log.info(format!(
                        "tab dirs: {} kept, none old enough to prune",
                        pruned.kept
                    ));
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
    (
        shell.root.clone(),
        shell.log.clone(),
        shell.launcher.cache.clone(),
        shell.binaries.clone(),
    )
}

/// A `model-cache.json` holding `body`, for the tests below.
#[cfg(test)]
fn cache_from(body: serde_json::Value) -> ModelCache {
    ModelCache::from_catalog("evo-swarm", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cache_holds_no_models() {
        assert!(ModelCache::default().is_empty());
    }

    #[test]
    fn a_catalog_body_becomes_the_cache_the_choosers_read() {
        let cache = cache_from(serde_json::json!({
            "models": [{"id": "m", "provider": "p", "ready": true}],
            "lanes": {"models": [{"id": "m", "provider": "p", "ok": true}]},
        }));
        assert_eq!(cache.models().len(), 1);
        assert!(!cache.is_empty());
        assert!(cache.catalog().lane_model_ok("m", Some("p")));
        assert!(cache.fetched_epoch().is_some(), "a read is stamped");
    }

    #[test]
    fn a_body_with_no_models_is_the_same_as_no_catalog() {
        let cache = cache_from(serde_json::json!({"models": []}));
        assert!(cache.is_empty(), "the choosers stay on Default");
    }
}
