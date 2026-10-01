//! Quitting the app (§9.8, §3).
//!
//! Closing the window (or holding ⌘Q, or the menu's Quit) does not end the
//! process: it starts *this*. `app.json` is written, every swarm is told to stop,
//! and the window says so while they go — the process ends when the last engine's
//! thread is over, or when the wait has gone on long enough to leave anyway.
//!
//! A tab's server stops when its engine does: the engine holds the child's stdin
//! (`--watch-stdin`), EOF on that pipe is what tells the child its tab is gone
//! (§8), and the ladder that follows — EOF, a 10 s wait, SIGTERM, 5 s, SIGKILL —
//! runs on the engine's own thread. Nothing here blocks on it: the engines are
//! asked to stop, and then looked at on a timer.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{App, Entity, Subscription, WeakEntity};

use store::app_state::{AppState, Recent};
use store::paths::TabId as StoredTabId;
use store::time;
use tab_engine::EngineHandle;
use workspace::WorkspaceView;

use crate::logging::AppLog;
use crate::Shell;

/// How often the quit looks whether the swarms it stopped have gone (§9.8).
const GOODBYE_POLL: Duration = Duration::from_millis(50);

/// How long the quit waits for them before it leaves anyway: the ladder's own
/// worst case is 10 s of patience plus 5 s for a server that has to be signalled,
/// and this leaves room over it (§9.8).
const GOODBYE_CAP: Duration = Duration::from_secs(20);

/// Whether the quit sequence has begun.
pub fn is_quitting(cx: &App) -> bool {
    cx.global::<Shell>().quitting
}

/// Wire a window's half of the hold: a ⌘Q held to its end ends in the same quit
/// the menu item runs (§9.8).
///
/// The key cannot be the menu item's: a menu item's key equivalent is fired by
/// AppKit before any window sees the key, and a hold is a key-down *and* a
/// key-up. So ⌘Q is bound to the window's own action (`workspace::HoldToQuit`),
/// the window works out whether it was held, and this is what it says when it
/// was. Returns the subscription, which lives as long as the caller keeps it.
pub fn watch_held_quit(cx: &mut App, view: &Entity<WorkspaceView>) -> Subscription {
    cx.subscribe(view, |_, _: &workspace::QuitHeld, cx| begin(cx))
}

/// Start the quit sequence, with every swarm the window still has running.
/// Idempotent: the second caller (a close, then ⌘Q) returns at once.
pub fn begin(cx: &mut App) {
    let swarms = swarms(cx);
    begin_with(cx, swarms);
}

/// Start the quit sequence with swarms the caller already collected — the
/// window's quit hook is handed them when the user closes the window (§9.8).
/// Idempotent.
pub fn begin_with(cx: &mut App, swarms: Vec<Rc<EngineHandle>>) {
    if is_quitting(cx) {
        return;
    }
    cx.global_mut::<Shell>().quitting = true;

    let log = cx.global::<Shell>().log.clone();
    log.info("quitting: stopping every tab");

    // The bounds and the recents are what the next launch needs, and they do not
    // depend on the servers stopping, so they are written first.
    let stopping = swarms.len();
    save_state(cx);

    // Every swarm is told to stop now: closing the child's stdin is the server's
    // own signal (§8), and the ladder that follows runs on the engine's thread.
    for swarm in &swarms {
        swarm.shutdown();
    }

    // Nothing running: there is nothing to show and nothing to wait for.
    if swarms.is_empty() {
        log.info("stopped; exiting (0 swarms)");
        cx.defer(|cx| cx.quit());
        return;
    }

    log.info(format!("waiting for {stopping} swarm(s) to stop"));
    // The window covers itself and says what is happening: the app is what ends
    // the process, and it does not end under swarms that are still running.
    if let Some(view) = view(cx) {
        view.update(cx, |view, cx| view.show_quitting(stopping, cx));
    }
    watch(cx, swarms, stopping, log);
}

/// Wait for the swarms this quit stopped, and end the app when the last has gone
/// (§9.8).
///
/// The wait is a timer, not a join: an engine's thread is what `is_running`
/// reports on, so nothing here holds the UI thread and nothing here needs a
/// handle to it. The cap is what keeps a swarm that will not die from keeping the
/// app alive: the ladder has already been given its chance by then.
fn watch(cx: &mut App, swarms: Vec<Rc<EngineHandle>>, stopping: usize, log: AppLog) {
    cx.spawn(async move |cx| {
        let deadline = cx.background_executor().now() + GOODBYE_CAP;
        while running(&swarms) > 0 && cx.background_executor().now() < deadline {
            cx.background_executor().timer(GOODBYE_POLL).await;
        }
        let left = running(&swarms);
        if left > 0 {
            log.warn(format!(
                "{left} swarm(s) still running after {GOODBYE_CAP:?}; exiting anyway"
            ));
        }
        drop(swarms);
        log.info(format!("stopped; exiting ({stopping} swarm(s))"));
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// How many of these swarms are still going: one whose engine's thread — its
/// stop ladder included — has not ended yet (§9.8).
fn running(swarms: &[Rc<EngineHandle>]) -> usize {
    swarms.iter().filter(|swarm| swarm.is_running()).count()
}

/// One open tab, as `app.json` records it (§6).
#[derive(Clone, Debug, PartialEq)]
pub struct TabRecord {
    /// The window's handle for this tab (`workspace::TabId`), as a number: the
    /// app never mints one, it only records what the window handed out.
    pub id: u64,
    /// The `tabs/<id>/` directory this tab's swarm writes to. `None` until it
    /// starts one.
    pub store_id: Option<StoredTabId>,
    /// Where it runs, once a folder was chosen.
    pub folder: Option<PathBuf>,
    /// The session it resumed, when it has one — what makes a tab resumable.
    pub session: Option<PathBuf>,
}

impl TabRecord {
    /// The id `app.json` stores for this tab.
    ///
    /// Once the tab has started a swarm that is the id its `tabs/<id>/` directory
    /// is named by, so the stored set and the directories on disk name the same
    /// tabs — which is what the tab-directory prune matches against. A tab that
    /// has not started one names no directory; it is stored under its window
    /// handle, which keeps the set ordered and unique.
    pub fn stored_id(&self) -> StoredTabId {
        self.store_id.clone().unwrap_or_else(|| {
            StoredTabId::parse(&format!("tab-{}", self.id))
                .expect("`tab-<n>` is safe as a path segment")
        })
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
        .map(|record| TabRecord {
            id: record.window_id.get(),
            store_id: record.store_id,
            folder: record.folder,
            session: record.session,
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
        // A swarm writes its journal with its first reply, so a tab that was opened
        // and quit again names a session with no file behind it: nothing to resume,
        // and therefore nothing to offer as "open at the last quit".
        if !session.is_file() {
            continue;
        }
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
        let bounds = shell
            .tracker
            .as_ref()
            .map(|tracker| tracker.read(cx).bounds());
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
    // "Resumable" is the same thing everywhere else here: a session whose journal is
    // a file. A swarm that never answered names a session and writes nothing, so
    // counting the *paths* would promise a resume that cannot come back.
    let resumable = records
        .iter()
        .filter(|record| {
            record
                .session
                .as_ref()
                .is_some_and(|session| session.is_file())
        })
        .count();
    remember_tab_set(&mut state, &records, selected);
    match state.save(&root) {
        Ok(()) => log.info(format!(
            "saved {}: {} tab(s), {resumable} of them resumable",
            root.app_json().display(),
            state.tabs.len()
        )),
        Err(error) => log.error(format!(
            "could not save {}: {error}",
            root.app_json().display()
        )),
    }
}

/// Every swarm the window still has running (§9.8): the live tabs' engines and
/// the ones a closed tab is still waiting for.
///
/// The tab list owns its engines; this is the one place the app asks for them.
/// The handles are shared — the window keeps its own — so asking twice is asking
/// the same question, not taking a second copy.
pub fn swarms(cx: &App) -> Vec<Rc<EngineHandle>> {
    let Some(view) = view(cx) else {
        return Vec::new();
    };
    view.read(cx).swarms(cx)
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

    fn record(id: u64, folder: Option<&str>, session: Option<&str>) -> TabRecord {
        TabRecord {
            id,
            store_id: None,
            folder: folder.map(PathBuf::from),
            session: session.map(PathBuf::from),
        }
    }

    #[test]
    fn a_tab_with_a_swarm_is_stored_under_its_own_directory_id() {
        let mut state = AppState::default();
        let started = TabRecord {
            id: 4,
            store_id: Some(StoredTabId::parse("9f2c1a").unwrap()),
            folder: Some(PathBuf::from("/coding/a")),
            session: Some(PathBuf::from("/sessions/one.sexp")),
        };
        remember_tab_set(&mut state, &[started, record(5, None, None)], Some(1));

        let ids: Vec<&str> = state.tabs.iter().map(StoredTabId::as_str).collect();
        assert_eq!(
            ids,
            ["9f2c1a", "tab-5"],
            "the directory id when the tab has one, the window handle otherwise"
        );
        assert_eq!(
            state.selected.as_ref().map(StoredTabId::as_str),
            Some("tab-5")
        );
    }

    /// A temp directory holding `count` journals that really exist.
    ///
    /// A session is only a recent once its file does: a swarm writes its journal with
    /// its first reply, so a tab that was opened and quit again has a session path and
    /// nothing behind it.
    fn journals(name: &str, count: usize) -> (PathBuf, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!("evo-quit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory for the journals");
        let paths = (0..count)
            .map(|n| {
                let path = dir.join(format!("{n}.sexp"));
                std::fs::write(&path, "(:type :session)\n").expect("a journal");
                path
            })
            .collect();
        (dir, paths)
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
        assert_eq!(
            ids,
            ["tab-7", "tab-9", "tab-12"],
            "in strip order, and the old set gone"
        );
        assert_eq!(
            state.selected.as_ref().map(StoredTabId::as_str),
            Some("tab-12")
        );
    }

    #[test]
    fn a_selection_the_strip_no_longer_has_selects_nothing() {
        let mut state = AppState::default();
        remember_tab_set(&mut state, &[record(1, None, None)], None);
        assert_eq!(state.tabs.len(), 1);
        assert!(state.selected.is_none());

        remember_tab_set(&mut state, &[], Some(3));
        assert!(state.tabs.is_empty());
        assert!(
            state.selected.is_none(),
            "an index past the strip is not a selection"
        );
    }

    #[test]
    fn only_a_tab_with_a_session_becomes_a_recent() {
        let (dir, journals) = journals("with-sessions", 2);
        let mut state = AppState::default();
        let records = [
            record(1, Some("/coding/a"), journals[0].to_str()),
            record(2, Some("/coding/b"), None),
            record(3, Some("/coding/c"), journals[1].to_str()),
        ];
        remember_tab_set(&mut state, &records, Some(0));

        assert_eq!(
            sessions(&state),
            [
                (journals[1].display().to_string(), true),
                (journals[0].display().to_string(), true),
            ],
            "newest first, and only the tabs that had a session"
        );
        let one = state.recent_for(&journals[0]).expect("a recent");
        assert_eq!(one.folder, PathBuf::from("/coding/a"));
        assert!(one.open_at_quit, "it was open when the app quit");
        assert!(!one.when.is_empty(), "the tab's use is stamped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_whose_journal_was_never_written_is_not_a_recent() {
        // The tab was opened and quit again without a turn: the swarm named a session
        // path and never wrote the file, so there is nothing to resume — and nothing
        // to offer as "open at the last quit".
        let (dir, journals) = journals("never-written", 1);
        let never = dir.join("never.sexp");

        // Nothing knew about it, and nothing records it.
        let mut state = AppState::default();
        remember_tab_set(
            &mut state,
            &[record(1, Some("/coding/never"), never.to_str())],
            Some(0),
        );
        assert!(
            state.recents.is_empty(),
            "a session with no journal is not recorded: {:?}",
            sessions(&state)
        );

        // An older `app.json` did record one (before the tab's own recording was
        // gated): it is not what "open at the last quit" means any more.
        state.touch_recent(Recent::new(&never, "/coding/never", 1).open_at_quit());
        remember_tab_set(
            &mut state,
            &[
                record(1, Some("/coding/a"), journals[0].to_str()),
                record(2, Some("/coding/never"), never.to_str()),
            ],
            Some(0),
        );
        assert_eq!(
            sessions(&state),
            [
                (journals[0].display().to_string(), true),
                (never.display().to_string(), false),
            ],
            "the tab with a journal is a recent; the one without is not flagged"
        );
        assert_eq!(state.tabs.len(), 2, "both tabs are still part of the strip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_flag_means_open_at_the_last_quit() {
        let mut state = AppState::default();
        state.touch_recent(Recent::new("/sessions/old.sexp", "/coding/old", 4).open_at_quit());
        state.touch_recent(Recent::new("/sessions/other.sexp", "/coding/other", 2));

        // This quit: only `other` is open and it has no session, so nothing is
        // open any more and the stale flag has to go.
        remember_tab_set(
            &mut state,
            &[record(1, Some("/coding/other"), None)],
            Some(0),
        );
        assert!(
            state.recents.iter().all(|recent| !recent.open_at_quit),
            "a flag from an earlier quit must not survive: {:?}",
            sessions(&state)
        );
        assert_eq!(state.recents.len(), 2, "the recents themselves are kept");
    }

    #[test]
    fn an_existing_recent_keeps_what_the_app_knew_and_moves_to_the_top() {
        let (dir, journals) = journals("known-session", 1);
        let mut state = AppState::default();
        let mut known = Recent::new(&journals[0], "/coding/a", 4);
        known.models.coordinator = Some("coord-model".to_owned());
        known.models.lanes = Some("lanes-model".to_owned());
        known.when = "2026-01-01T00:00:00Z".to_owned();
        state.touch_recent(known);
        state.touch_recent(Recent::new(dir.join("b.sexp"), "/coding/b", 2));

        remember_tab_set(
            &mut state,
            &[record(1, Some("/coding/a"), journals[0].to_str())],
            Some(0),
        );

        let refreshed = state.recent_for(&journals[0]).expect("still there");
        assert!(refreshed.open_at_quit);
        assert_eq!(refreshed.models.lanes.as_deref(), Some("lanes-model"));
        assert_eq!(refreshed.lanes, 4, "the lane count it was started with");
        assert_eq!(refreshed.folder, PathBuf::from("/coding/a"));
        assert_ne!(
            refreshed.when, "2026-01-01T00:00:00Z",
            "stamped with this quit"
        );
        assert_eq!(
            sessions(&state).first().map(|(path, _)| path.as_str()),
            Some(journals[0].to_str().unwrap()),
            "and it is the most recent now"
        );
        assert_eq!(
            state.recents.len(),
            2,
            "no duplicate row for the same session"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tab_that_never_chose_a_folder_is_still_in_the_set() {
        let mut state = AppState::default();
        remember_tab_set(&mut state, &[record(4, None, None)], Some(0));
        let ids: Vec<&str> = state.tabs.iter().map(StoredTabId::as_str).collect();
        assert_eq!(ids, ["tab-4"], "an empty tab is an open tab");
    }
}
