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
    /// The clock the rows' relative times are read against.
    pub now: i64,
    /// The system's UTC offset in seconds, so the rows show local times.
    pub offset_seconds: i32,
    /// The `~` the history rows are shortened around.
    pub home: Option<String>,
    /// The session read is still running.
    pub history_loading: bool,
    /// Why there is no catalog, when there is none.
    pub catalog_error: Option<String>,
    /// Why the history is empty, when the read could not run at all.
    pub history_error: Option<String>,
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
            history_loading: false,
            catalog_error: None,
            history_error: None,
        }
    }

    /// This, as the window takes it.
    ///
    /// The shape the empty tabs are fed in — the app's whole contract with the
    /// window, and what a test (or a capture) reads instead of the widgets. An
    /// empty catalog is handed on as `None`: the choosers stay on their loading
    /// hint rather than claiming a catalog of nothing.
    pub fn data(&self) -> workspace::LauncherData {
        workspace::LauncherData {
            catalog: (!self.cache.is_empty()).then(|| self.cache.raw().clone()),
            catalog_error: self.catalog_error.clone(),
            history_loading: self.history_loading,
            history: self.history.clone(),
            now: self.now,
            offset_seconds: self.offset_seconds,
            history_error: self.history_error.clone(),
            home: self.home.clone(),
        }
    }
}

/// The `~` the history rows are shortened around, and where the folder dialog
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
        launch.history_loading = false;
        launch.history_error = error;
    }
    push_launcher_data(cx);
}

/// The session read started (`true`) or finished (`false`).
pub fn set_history_loading(cx: &mut App, loading: bool) {
    cx.global_mut::<Shell>().launcher.history_loading = loading;
    push_launcher_data(cx);
}

/// The catalog could not be learned: the tabs say so where the loading hint was.
pub fn set_catalog_error(cx: &mut App, message: Option<String>) {
    cx.global_mut::<Shell>().launcher.catalog_error = message;
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
    fn an_empty_catalog_is_handed_over_as_nothing_rather_than_an_empty_body() {
        let data = Launcher::new(ModelCache::default()).data();
        assert!(
            data.catalog.is_none(),
            "the choosers stay on their loading hint"
        );
        assert!(!data.history_loading);
    }

    #[test]
    fn a_catalog_is_handed_over_as_the_body_the_window_reads() {
        let cache = ModelCache::from_catalog(
            "evo-swarm",
            serde_json::json!({ "models": [{ "id": "m" }], "lanes": {"models": []} }),
        );
        let data = Launcher::new(cache.clone()).data();
        assert_eq!(data.catalog, Some(cache.raw().clone()));
    }

    #[test]
    fn the_history_and_its_errors_are_carried_through() {
        let mut launch = Launcher::new(ModelCache::default());
        launch.history = history_entries(&[store_entry(StoreSource::Index, Some(5), 3)]);
        launch.history_loading = false;
        launch.history_error = Some("no such binary".to_owned());
        launch.home = Some("/Users/x".to_owned());
        let data = launch.data();
        assert_eq!(data.history.len(), 1);
        assert_eq!(data.history_error.as_deref(), Some("no such binary"));
        assert_eq!(data.home.as_deref(), Some("/Users/x"));
    }

    /// The whole chain for one row, from an index body to the line it shows: the index's
    /// `title` is what a session's first user-role entry says, and for a session evo
    /// started itself that is evo's scaffolding — a goal's continuation prompt, a lane's
    /// brief — so it does not name the row. The folder does.
    #[test]
    fn an_index_title_from_evos_own_scaffolding_does_not_name_a_row() {
        let body = serde_json::json!({
            "sessions": [{
                "id": "e82d8f3835d28a23",
                "path": "/Users/you/.evo/sessions/x/1.sexp",
                "cwd": "/Users/you/coding/evo-gui/",
                "program": "evo-swarm",
                "swarm_id": "20260930T132823-4fa5",
                "title": "You are idle but your goal is still active. Continue working toward \
                          it now.",
                "created_at": 1_756_000_000_000u64,
                "updated_at": 1_756_000_300_000u64,
                "entries": 926
            }]
        });
        let stored: Vec<store::history::HistoryEntry> = store::history::parse(&body)
            .iter()
            .map(store::history::from_session)
            .collect();
        let rows = session::history_rows(
            &history_entries(&stored),
            1_756_000_300,
            0,
            Some("/Users/you"),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "evo-gui", "the folder names the row");
        assert_eq!(rows[0].folder_short, "~/coding/evo-gui");
        assert!(
            !rows[0].tooltip.contains("You are idle"),
            "and evo's scaffolding is not smuggled into the tooltip either: {}",
            rows[0].tooltip
        );
    }
}
