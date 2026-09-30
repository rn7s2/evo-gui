//! What every empty tab is shown (§9.4, §9.5) — the catalog and the sessions the
//! launch found — gathered in one place and handed to the window.
//!
//! The app owns reading them; the window owns showing them. So this module keeps
//! the latest of each ([`Launcher`] on the [`Shell`]) and
//! [`push_launcher_data`]es it through the window, which gives it to every tab it
//! has *and* to every one it opens later — the `+`, or ⌘T, while the session read
//! is still running.
//!
//! Two reads feed it, and both are processes rather than servers (§9):
//! `evo-swarm catalog --json` and `evo-agent sessions --json`. This module is
//! where what they printed becomes the shapes the window's choosers and rows take.
//!
//! The window is not on the update stack while a launch task runs, but it may be
//! when a menu action does, so the hand-off is deferred to the end of the effect
//! cycle: by then the window is free to be borrowed (the same trick
//! `Context::defer_in` uses).

use gpui_kit::App;

use session::{HistoryEntry, HistorySource};
use store::history::HistorySource as StoreSource;
use store::model_cache::ModelCache;
use store::time;

use crate::Shell;

/// Everything the empty tabs show, in the shapes the window wants (§9.4, §9.5).
#[derive(Clone, Debug, Default)]
pub struct Launcher {
    /// The catalog: the last `catalog --json` body, kept in `model-cache.json`.
    pub cache: ModelCache,
    /// The resumable swarms the session index lists, plus the app's own recents.
    pub history: Vec<HistoryEntry>,
    /// The clock the rows' relative times are measured against.
    pub now: i64,
    /// The system's UTC offset in seconds, so the rows show local times.
    pub offset_seconds: i32,
    /// The `~` the history paths are shortened around.
    pub home: Option<String>,
    /// The session read is still running.
    pub scanning: bool,
    /// Why there is no catalog, when there is none.
    pub catalog_error: Option<String>,
    /// Why the history is empty, when the read could not run at all.
    pub history_error: Option<String>,
    /// The swarm binary `app.json` names cannot be run at all (§9.7, said before a
    /// launch instead of after one). See [`crate::about::missing_swarm`].
    pub swarm_problem: Option<String>,
}

impl Launcher {
    /// The state a launch starts in: the cache from disk, nothing read yet.
    pub fn new(cache: ModelCache) -> Launcher {
        Launcher {
            cache,
            history: Vec::new(),
            now: time::now_epoch() as i64,
            offset_seconds: utc_offset_seconds(),
            home: home_string(),
            scanning: false,
            catalog_error: None,
            history_error: None,
            swarm_problem: None,
        }
    }

    /// This, as the window takes it.
    ///
    /// The shape the empty tabs are fed in — public because it is the app's whole
    /// contract with the window, and what a test (or a capture) reads instead of
    /// the widgets: `registry`/`model_cache` are `None` until a catalog is known,
    /// which is exactly the state the choosers call "still loading".
    pub fn data(&self) -> workspace::LauncherData {
        // An empty catalog is the same as no catalog: leave the choosers saying
        // they are still loading rather than claiming a registry of nothing.
        let known = !self.cache.is_empty();
        workspace::LauncherData {
            registry: known.then(|| self.cache.raw().clone()),
            model_cache: known.then(|| self.cache.clone()),
            catalog_error: self.catalog_error.clone(),
            scanning: self.scanning,
            history: self.history.clone(),
            now: self.now,
            offset_seconds: self.offset_seconds,
            history_error: self.history_error.clone(),
            swarm_problem: self.swarm_problem.clone(),
            home: self.home.clone(),
        }
    }
}

/// The `~` the history paths are shortened around, and where the folder dialog
/// starts.
pub fn home_string() -> Option<String> {
    let home = store::paths::home_dir();
    (!home.as_os_str().is_empty()).then(|| home.to_string_lossy().into_owned())
}

/// The system's current UTC offset, in seconds east of UTC (`UTC+08:00` → 28800).
///
/// Only for display: every timestamp the app *writes* stays UTC (`store::time`).
/// The offset comes from `localtime_r`, so it follows the zone the machine is
/// actually in, including a daylight-saving change since launch.
pub fn utc_offset_seconds() -> i32 {
    #[cfg(unix)]
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut broken_down: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut broken_down).is_null() {
            return 0;
        }
        broken_down.tm_gmtoff as i32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The store's history entries as `session` wants them (§7.2). The two crates
/// deliberately do not know about each other, so the app is where they meet.
pub fn history_entries(entries: &[store::history::HistoryEntry]) -> Vec<HistoryEntry> {
    entries
        .iter()
        .map(|entry| HistoryEntry {
            session_path: entry.session.to_string_lossy().into_owned(),
            folder: entry.folder.to_string_lossy().into_owned(),
            title: entry.title.clone(),
            when: entry.when_epoch.map(|epoch| epoch as i64),
            // The app records 0 for a session it did not start; the list's
            // `Option` means "unknown", and a row with no count is better than a
            // row claiming zero lanes.
            lanes: (entry.lanes > 0).then_some(entry.lanes),
            coordinator_model: entry.models.coordinator.clone(),
            lanes_model: entry.models.lanes.clone(),
            source: match entry.source {
                StoreSource::Index => HistorySource::Index,
                StoreSource::Recent => HistorySource::Recent,
            },
            // Only the app's own recents know this: the tab was open when it last quit.
            open_at_quit: entry.open_at_quit,
        })
        .collect()
}

// --- handing it to the window ----------------------------------------------

/// Give the window everything the launch has learned. This is the only way the
/// empty tabs are fed, so every arrival ends here.
pub fn push_launcher_data(cx: &mut App) {
    let (Some(view), Some(window)) = (crate::quit::view(cx), window_of(cx)) else {
        log_warn(cx, "launcher: no window to show the catalog and history in");
        return;
    };
    let data = cx.global::<Shell>().launcher.data();
    // Deferred: the caller may be running *inside* a window update (a menu action
    // is), and a window on the stack cannot be borrowed again. The end of the
    // effect cycle gives it back.
    cx.defer(move |cx| {
        let shown = window.update(cx, |_root, window, cx| {
            view.update(cx, |view, cx| view.set_launcher_data(data, window, cx));
        });
        if let Err(error) = shown {
            log_warn(
                cx,
                format!("launcher: the window could not be updated: {error}"),
            );
        }
    });
}

/// The window the tabs live in. The app has exactly one (§7.1).
pub fn window_of(cx: &App) -> Option<gpui_kit::AnyWindowHandle> {
    cx.windows().into_iter().next()
}

/// How many tabs the window has, for a log line or a diagnostic.
pub fn tab_count(cx: &App) -> usize {
    match crate::quit::view(cx) {
        Some(view) => view.read(cx).open_tab_count(),
        None => 0,
    }
}

// --- what the background work reports --------------------------------------

/// The catalog is known: the disk cache at launch, or `catalog --json`'s answer.
pub fn set_catalog(cx: &mut App, cache: ModelCache, error: Option<String>) {
    {
        let launch = &mut cx.global_mut::<Shell>().launcher;
        launch.cache = cache;
        launch.catalog_error = error;
    }
    push_launcher_data(cx);
}

/// The session index said what it said. `error` is set when the read could not
/// run at all.
pub fn set_history(
    cx: &mut App,
    entries: Vec<store::history::HistoryEntry>,
    error: Option<String>,
) {
    {
        let launch = &mut cx.global_mut::<Shell>().launcher;
        launch.history = history_entries(&entries);
        launch.now = time::now_epoch() as i64;
        launch.offset_seconds = utc_offset_seconds();
        launch.home = home_string();
        launch.scanning = false;
        launch.history_error = error;
    }
    push_launcher_data(cx);
}

/// The session read started (`true`) or finished (`false`).
pub fn set_scanning(cx: &mut App, scanning: bool) {
    cx.global_mut::<Shell>().launcher.scanning = scanning;
    push_launcher_data(cx);
}

/// The catalog could not be learned: the tabs say so where the loading hint was.
pub fn set_catalog_error(cx: &mut App, message: Option<String>) {
    cx.global_mut::<Shell>().launcher.catalog_error = message;
    push_launcher_data(cx);
}

/// The swarm binary `app.json` names cannot be run: the empty tabs say so next to
/// the folder button, rather than only failing after a launch (§9.7). `None`
/// clears the line — the path was fixed in Settings.
pub fn set_swarm_problem(cx: &mut App, problem: Option<String>) {
    if let Some(problem) = &problem {
        log_warn(cx, format!("swarm binary: {problem}"));
    }
    cx.global_mut::<Shell>().launcher.swarm_problem = problem;
    push_launcher_data(cx);
}

fn log_warn(cx: &App, message: impl AsRef<str>) {
    cx.global::<Shell>().log.warn(message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use store::history::HistoryEntry as StoreEntry;
    use store::tab::TabModels;

    fn store_entry(source: StoreSource, when_epoch: Option<u64>, lanes: u32) -> StoreEntry {
        StoreEntry {
            session: PathBuf::from("/sessions/a.sexp"),
            folder: PathBuf::from("/coding/foo"),
            when: "2026-09-29T09:25:44Z".to_owned(),
            when_epoch,
            session_id: "a".to_owned(),
            swarm_id: "s".to_owned(),
            title: "wire the lanes chooser to the catalog".to_owned(),
            workers: lanes,
            lanes,
            models: TabModels {
                coordinator: Some("coord-1".to_owned()),
                lanes: Some("lane-1".to_owned()),
            },
            source,
            open_at_quit: false,
        }
    }

    #[test]
    fn the_store_entry_becomes_a_session_entry() {
        let entries = history_entries(&[store_entry(StoreSource::Index, Some(1_700_000_000), 4)]);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.session_path, "/sessions/a.sexp");
        assert_eq!(entry.folder, "/coding/foo");
        assert_eq!(entry.title, "wire the lanes chooser to the catalog");
        assert_eq!(entry.when, Some(1_700_000_000));
        assert_eq!(entry.lanes, Some(4));
        assert_eq!(entry.coordinator_model.as_deref(), Some("coord-1"));
        assert_eq!(entry.lanes_model.as_deref(), Some("lane-1"));
        assert_eq!(entry.source, HistorySource::Index);
    }

    #[test]
    fn a_recent_entry_keeps_its_source() {
        let entries = history_entries(&[store_entry(StoreSource::Recent, None, 2)]);
        assert_eq!(entries[0].source, HistorySource::Recent);
        assert_eq!(
            entries[0].when, None,
            "no epoch means the row sorts last, not that it is now"
        );
    }

    #[test]
    fn a_session_with_no_lane_count_says_nothing() {
        let entries = history_entries(&[store_entry(StoreSource::Index, Some(1), 0)]);
        assert_eq!(
            entries[0].lanes, None,
            "0 means the app did not record a count"
        );
    }

    #[test]
    fn the_local_offset_is_a_plausible_zone_offset() {
        let offset = utc_offset_seconds();
        // Whole minutes, within ±14 h — and the same answer twice.
        assert_eq!(offset % 60, 0, "{offset} is not a whole minute");
        assert!(
            offset.abs() <= 14 * 3600,
            "{offset} is not a real zone offset"
        );
        assert_eq!(offset, utc_offset_seconds());
    }

    #[test]
    fn an_empty_catalog_is_handed_over_as_nothing_rather_than_an_empty_registry() {
        let launch = Launcher::new(ModelCache::default());
        let data = launch.data();
        assert!(
            data.registry.is_none(),
            "the choosers stay on their loading hint"
        );
        assert!(data.model_cache.is_none());
        assert!(!data.scanning);
    }

    #[test]
    fn a_catalog_is_handed_over_as_both_shapes_the_tabs_take() {
        let cache = ModelCache::from_catalog(
            "evo-swarm",
            serde_json::json!({ "models": [{ "id": "m" }], "lanes": {"models": []} }),
        );
        let data = Launcher::new(cache.clone()).data();
        assert_eq!(data.registry, Some(cache.raw().clone()));
        assert!(data.model_cache.is_some());
    }

    #[test]
    fn the_history_and_its_errors_are_carried_through() {
        let mut launch = Launcher::new(ModelCache::default());
        launch.history = history_entries(&[store_entry(StoreSource::Index, Some(5), 3)]);
        launch.scanning = false;
        launch.history_error = Some("no such binary".to_owned());
        launch.home = Some("/Users/x".to_owned());
        launch.offset_seconds = 8 * 3600;
        launch.now = 42;
        let data = launch.data();
        assert_eq!(data.history.len(), 1);
        assert_eq!(data.history_error.as_deref(), Some("no such binary"));
        assert_eq!(data.home.as_deref(), Some("/Users/x"));
        assert_eq!(data.offset_seconds, 8 * 3600);
        assert_eq!(data.now, 42);
    }
}
