//! Quitting the app (§9.8, §3).
//!
//! Closing the window (or Cmd-Q) does not end the process: it starts *this*.
//! Every tab's swarm is stopped the ladder's way — `POST /shutdown`, then
//! `SIGTERM`, then `SIGKILL` — on a thread of its own, so the UI never waits;
//! `app.json` is written while that runs; and the process exits when the last
//! tab is gone or the deadline passes.
//!
//! If the platform terminates us some other way (a logout, the dock's Quit
//! while our key binding never saw the keystroke), there is no time for any of
//! this: GPUI's own shutdown gives observers
//! [`SHUTDOWN_TIMEOUT`](gpui_kit::SHUTDOWN_TIMEOUT) — 200 ms — so the ladder is
//! started and left behind, and the swarms stop because this process
//! (`EVO_SERVE_WATCH_PID`) does.

use std::time::Duration;

use std::path::PathBuf;


use gpui_kit::{App, WeakEntity};

use store::app_state::{AppState, Recent};
use store::paths::TabId as StoredTabId;
use store::time;
use tab_engine::{EngineHandle, shutdown_all};
use workspace::WorkspaceView;

use crate::Shell;

/// How long §3's ladder is given before a tab is left to finish stopping on its
/// own: `POST /shutdown` (≤10 s), then `SIGTERM` (5 s), then `SIGKILL`.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);

/// Whether the quit sequence has begun.
pub fn is_quitting(cx: &App) -> bool {
    cx.global::<Shell>().quitting
}

/// Start the quit sequence, taking the tabs' engines out of the workspace.
/// Idempotent: the second caller (a close, then Cmd-Q) returns at once.
pub fn begin(cx: &mut App) {
    let engines = take_engines(cx);
    begin_with(cx, engines);
}

/// Start the quit sequence with engines the caller already took — the window's
/// quit hook is handed them when the user closes the window (§9.8). Idempotent.
pub fn begin_with(cx: &mut App, engines: Vec<EngineHandle>) {
    if is_quitting(cx) {
        return;
    }
    cx.global_mut::<Shell>().quitting = true;

    let log = cx.global::<Shell>().log.clone();
    log.info("quitting: stopping every tab");

    // The bounds and the recents are what the next launch needs, and they do not
    // depend on the swarms stopping, so they are written first.
    let tabs = engines.len();
    save_state(cx);

    let (done_tx, done_rx) = async_channel::bounded::<()>(1);
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-shutdown".to_owned())
        .spawn(move || {
            let report = shutdown_all(engines, SHUTDOWN_DEADLINE);
            if report.all_exited() {
                thread_log.info(format!("shutdown: all {tabs} tab(s) exited"));
            } else {
                thread_log.warn(format!(
                    "shutdown: {} of {tabs} tab(s) exited; still stopping: {:?}",
                    report.exited.len(),
                    report.pending
                ));
            }
            let _ = done_tx.send_blocking(());
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the shutdown thread: {error}"));
    }

    // Quit once the ladder is done (or the deadline passed, which is when the
    // thread returns anyway).
    cx.spawn(async move |cx| {
        let _ = done_rx.recv().await;
        cx.update(|cx| {
            log.info("stopped; exiting");
            cx.quit();
        });
    })
    .detach();
}

/// The snapshot the quit sequence persists: the window's last bounds and the
/// recents `app.json` already knew.
/// One open tab, as `app.json` records it (§6).
#[derive(Clone, Debug, PartialEq)]
pub struct TabRecord {
    /// The window's handle for this tab (`workspace::TabId`), as a number: the
    /// app never mints one, it only records what the window handed out.
    pub id: u64,
    /// Where it runs, once a folder was chosen.
    pub folder: Option<PathBuf>,
    /// The session it resumed, when it has one — what makes a tab resumable.
    pub session: Option<PathBuf>,
}

impl TabRecord {
    /// The id `app.json` stores for this tab.
    ///
    /// This should be the tab's own `tabs/<id>/` id — the one its `tab.json` and
    /// `swarm.log` live under — so that the stored set and the directories on
    /// disk name the same tabs. It is not reachable today: `TabContent` creates
    /// that directory in `launch::start` from a fresh `TabId` and keeps only the
    /// *path* privately, and `WorkspaceView::tab_records` hands back the window's
    /// own per-run handle. Asked of lane 1; when it lands this is one line.
    ///
    /// Until then the id is derived from that handle, which keeps the set ordered
    /// and unique — nothing restores from it yet — and the tab-directory prune
    /// (which matches ids to directories) simply never recognises a live one. A
    /// live tab's directory is minutes old, so the week-long prune does not reach
    /// it either way.
    pub fn stored_id(&self) -> StoredTabId {
        StoredTabId::parse(&format!("tab-{}", self.id)).expect("`tab-<n>` is safe as a path segment")
    }
}

/// The window's open tabs in strip order, and which one is shown (§6, §7.1).
pub fn open_tabs(cx: &App) -> (Vec<TabRecord>, Option<usize>) {
    let Some(view) = view(cx) else {
        return (Vec::new(), None);
    };
    let view = view.read(cx);
    let records = view
        .tab_records(cx)
        .into_iter()
        .map(|(id, folder, session)| TabRecord {
            id: id.get(),
            folder,
            session,
        })
        .collect();
    (records, Some(view.selected_index()))
}

/// Record the tab set (§6) and touch a recent for every tab that had a session
/// (§9.5), so the next launch's history puts what was open first.
///
/// Pure over `AppState`, so the quit path's only interesting decision is
/// testable without a window.
pub fn remember_tab_set(state: &mut AppState, records: &[TabRecord], selected: Option<usize>) {
    state.tabs.clear();
    state.selected = None;
    for record in records {
        // `add_tab` also selects; the strip's own selection is set below.
        state.add_tab(record.stored_id());
    }
    state.selected = None;
    if let Some(record) = selected.and_then(|index| records.get(index)) {
        state.select(&record.stored_id());
    }

    // The flag means "open at the *last* quit": clear it everywhere first, so a
    // tab closed since then stops claiming to be.
    for recent in &mut state.recents {
        recent.open_at_quit = false;
    }

    let when = time::now_rfc3339();
    for record in records {
        let Some(session) = record.session.clone() else {
            continue;
        };
        // What the app already knew about this session (its lanes model, its lane
        // count) outlives this quit: the entry is refreshed, not replaced.
        let known = state.recent_for(&session).cloned();
        let mut recent = Recent::new(
            session,
            record.folder.clone().unwrap_or_default(),
            known.as_ref().map_or(0, |recent| recent.lanes),
        )
        .open_at_quit();
        recent.when = when.clone();
        if let Some(known) = known {
            recent.models = known.models;
        }
        state.touch_recent(recent);
    }
}

fn save_state(cx: &mut App) {
    let (root, log, bounds) = {
        let shell = cx.global::<Shell>();
        let bounds = shell.tracker.as_ref().map(|tracker| tracker.read(cx).bounds());
        (shell.root.clone(), shell.log.clone(), bounds)
    };
    let mut state = AppState::load(&root);
    if let Some(bounds) = bounds {
        state.window = bounds;
    }
    // The theme the run used. Nothing changes it yet (there is no settings UI),
    // so this is a write-through — but a `Light` in the file has to survive a
    // quit, or the next launch would be the system's again.
    state.theme = cx.global::<Shell>().theme;
    let (records, selected) = open_tabs(cx);
    let open_with_session = records.iter().filter(|record| record.session.is_some()).count();
    remember_tab_set(&mut state, &records, selected);
    match state.save(&root) {
        Ok(()) => log.info(format!(
            "saved {}: {} tab(s), {open_with_session} of them resumable",
            root.app_json().display(),
            state.tabs.len()
        )),
        Err(error) => log.error(format!("could not save {}: {error}", root.app_json().display())),
    }
}

/// Every live tab's engine, taken out of the workspace.
///
/// The tab list owns its engines; this is the one place the app asks for them.
/// A tab that has already been handed over (the window's quit hook) has none
/// left, so a later call returns nothing rather than a second copy.
pub fn take_engines(cx: &mut App) -> Vec<EngineHandle> {
    let Some(view) = view(cx) else {
        return Vec::new();
    };
    view.update(cx, |view, cx| view.take_engines(cx))
}

/// The window's root view, while it exists.
pub fn view(cx: &App) -> Option<gpui_kit::Entity<WorkspaceView>> {
    view_handle(cx)?.upgrade()
}

fn view_handle(cx: &App) -> Option<&WeakEntity<WorkspaceView>> {
    cx.global::<Shell>().view.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn record(id: u64, folder: Option<&str>, session: Option<&str>) -> TabRecord {
        TabRecord {
            id,
            folder: folder.map(PathBuf::from),
            session: session.map(PathBuf::from),
        }
    }

    fn sessions(state: &AppState) -> Vec<(String, bool)> {
        state
            .recents
            .iter()
            .map(|recent| (recent.session.display().to_string(), recent.open_at_quit))
            .collect()
    }

    #[test]
    fn the_tab_set_is_written_in_strip_order_with_the_selection() {
        let mut state = AppState::default();
        state.tabs.push(StoredTabId::parse("stale").unwrap());
        state.selected = Some(StoredTabId::parse("stale").unwrap());

        let records = [
            record(7, Some("/coding/a"), None),
            record(9, None, None),
            record(12, Some("/coding/c"), None),
        ];
        remember_tab_set(&mut state, &records, Some(2));

        let ids: Vec<&str> = state.tabs.iter().map(StoredTabId::as_str).collect();
        assert_eq!(ids, ["tab-7", "tab-9", "tab-12"], "in strip order, and the old set gone");
        assert_eq!(state.selected.as_ref().map(StoredTabId::as_str), Some("tab-12"));
    }

    #[test]
    fn a_selection_the_strip_no_longer_has_selects_nothing() {
        let mut state = AppState::default();
        remember_tab_set(&mut state, &[record(1, None, None)], None);
        assert_eq!(state.tabs.len(), 1);
        assert!(state.selected.is_none());

        remember_tab_set(&mut state, &[], Some(3));
        assert!(state.tabs.is_empty());
        assert!(state.selected.is_none(), "an index past the strip is not a selection");
    }

    #[test]
    fn only_a_tab_with_a_session_becomes_a_recent() {
        let mut state = AppState::default();
        let records = [
            record(1, Some("/coding/a"), Some("/sessions/one.sexp")),
            record(2, Some("/coding/b"), None),
            record(3, Some("/coding/c"), Some("/sessions/three.sexp")),
        ];
        remember_tab_set(&mut state, &records, Some(0));

        assert_eq!(
            sessions(&state),
            [
                ("/sessions/three.sexp".to_owned(), true),
                ("/sessions/one.sexp".to_owned(), true),
            ],
            "newest first, and only the tabs that had a session"
        );
        let one = state.recent_for(Path::new("/sessions/one.sexp")).expect("a recent");
        assert_eq!(one.folder, PathBuf::from("/coding/a"));
        assert!(one.open_at_quit, "it was open when the app quit");
        assert!(!one.when.is_empty(), "the tab's use is stamped");
    }

    #[test]
    fn the_flag_means_open_at_the_last_quit() {
        let mut state = AppState::default();
        state.touch_recent(Recent::new("/sessions/old.sexp", "/coding/old", 4).open_at_quit());
        state.touch_recent(Recent::new("/sessions/other.sexp", "/coding/other", 2));

        // This quit: only `other` is open and it has no session, so nothing is
        // open any more and the stale flag has to go.
        remember_tab_set(&mut state, &[record(1, Some("/coding/other"), None)], Some(0));
        assert!(
            state.recents.iter().all(|recent| !recent.open_at_quit),
            "a flag from an earlier quit must not survive: {:?}",
            sessions(&state)
        );
        assert_eq!(state.recents.len(), 2, "the recents themselves are kept");
    }

    #[test]
    fn an_existing_recent_keeps_what_the_app_knew_and_moves_to_the_top() {
        let mut state = AppState::default();
        let mut known = Recent::new("/sessions/a.sexp", "/coding/a", 4);
        known.models.coordinator = Some("coord-model".to_owned());
        known.models.lanes = Some("lanes-model".to_owned());
        known.when = "2026-01-01T00:00:00Z".to_owned();
        state.touch_recent(known);
        state.touch_recent(Recent::new("/sessions/b.sexp", "/coding/b", 2));

        remember_tab_set(&mut state, &[record(1, Some("/coding/a"), Some("/sessions/a.sexp"))], Some(0));

        let refreshed = state.recent_for(Path::new("/sessions/a.sexp")).expect("still there");
        assert!(refreshed.open_at_quit);
        assert_eq!(refreshed.models.lanes.as_deref(), Some("lanes-model"));
        assert_eq!(refreshed.lanes, 4, "the lane count it was started with");
        assert_eq!(refreshed.folder, PathBuf::from("/coding/a"));
        assert_ne!(refreshed.when, "2026-01-01T00:00:00Z", "stamped with this quit");
        assert_eq!(
            sessions(&state).first().map(|(path, _)| path.as_str()),
            Some("/sessions/a.sexp"),
            "and it is the most recent now"
        );
        assert_eq!(state.recents.len(), 2, "no duplicate row for the same session");
    }

    #[test]
    fn a_tab_that_never_chose_a_folder_is_still_in_the_set() {
        let mut state = AppState::default();
        remember_tab_set(&mut state, &[record(4, None, None)], Some(0));
        let ids: Vec<&str> = state.tabs.iter().map(StoredTabId::as_str).collect();
        assert_eq!(ids, ["tab-4"], "an empty tab is an open tab");
    }
}
