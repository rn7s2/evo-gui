//! What a shell has to fetch, and how a body becomes the window's launcher data.
//!
//! The desktop app reads the catalog and the session index by **running the two
//! binaries** (`evo-swarm catalog --json`, `evo-agent sessions --json`) and hands
//! the answers to the window as [`LauncherData`]. A browser cannot run anything:
//! the same two documents come from the `evo-web` server that served the page —
//! `GET /api/catalog` and `GET /api/sessions`, which are those very commands run
//! on the serving machine.
//!
//! So this module is the *shape* of that hand-off and nothing else. It does not
//! fetch: fetching is the shell's, and the two shells fetch differently (a thread
//! and a `Command` on the desktop, `fetch` in a browser), which is exactly why the
//! seam is a data type rather than a function. What both sides must agree on is
//! how a JSON body turns into the rows and choosers the empty tab draws, and that
//! is here — once, for both.
//!
//! ```text
//! desktop  evo-agent sessions --json  ─┐
//!                                     ├─▶ session index body ─┐
//! browser  GET /api/sessions          ─┘                      │
//!                                                             ├─▶ LauncherData ─▶ empty tabs
//! desktop  evo-swarm catalog --json   ─┐                      │
//!                                     ├─▶ catalog body ───────┘
//! browser  GET /api/catalog           ─┘
//! ```

use serde_json::Value;

use store::history::{self, HistoryEntry as StoreEntry};

use crate::chrome::LauncherData;

/// The launcher data a browser's two reads make, in the shape the window takes.
///
/// `sessions` is `GET /api/sessions`'s body and `catalog` is `GET /api/catalog`'s
/// — both as they arrived, because both are the *same documents* the desktop's
/// two commands print, and the app hands its own over the same way.
///
/// `now` is the clock the rows' relative times read against ("2h ago"), in epoch
/// seconds, and it is the caller's for a reason: this crate is compiled for both
/// targets, and a clock is the one thing the two do not share. `std::time` is
/// unimplemented on `wasm32-unknown-unknown` — `SystemTime::now()` **panics**
/// there, it does not return an error — so a browser's clock comes from the page
/// (`Date.now`), which is a fact about the page and belongs to the shell that has
/// one. Passing it also keeps this function pure, which is what lets the tests
/// below read the rows without a clock at all.
///
/// The index is parsed exactly as the desktop parses it
/// ([`store::history::parse`] then [`store::history::from_session`]), so a row in
/// a browser is the row a desktop would show for the same journal. There are no
/// recents to merge: `app.json` is the desktop's, and a browser has none — which
/// costs only the two things only the app can know (a session's lane count, and
/// whether it was open at the last quit).
///
/// `error` is what to say when a read could not be made at all: the rows are then
/// empty and the list shows the reason rather than claiming there is nothing to
/// resume (§9.5).
pub fn from_server(
    sessions: Option<&Value>,
    catalog: Option<&Value>,
    error: Option<String>,
    now: i64,
) -> LauncherData {
    let history = sessions.map(session_entries).unwrap_or_default();
    LauncherData {
        catalog: catalog.cloned(),
        catalog_error: None,
        history_loading: false,
        history,
        now,
        offset_seconds: 0,
        history_error: error,
        home: None,
    }
}

/// The rows one `GET /api/sessions` body makes.
///
/// The server's query is `SessionsQuery::resumable()`, the same one the desktop
/// asks with, so what comes back is already the resumable set: no filtering here,
/// and none there.
pub fn session_entries(body: &Value) -> Vec<session::HistoryEntry> {
    let indexed: Vec<StoreEntry> = history::parse(body)
        .iter()
        .map(history::from_session)
        .collect();
    to_session(&indexed)
}

/// The store's history entries as `session` wants them (§7.2).
///
/// The two crates deliberately do not know about each other, so whoever holds both
/// is where they meet — the desktop app has this same conversion
/// (`evo_desktop::history_entries`), and a browser has no app to do it. It is
/// written out here rather than shared because the two differ in one field the
/// desktop's side can answer and this one cannot: `open_at_quit` is the app's own
/// record of what it had open, and a browser's index rows are always `false`.
pub fn to_session(entries: &[StoreEntry]) -> Vec<session::HistoryEntry> {
    entries
        .iter()
        .map(|entry| session::HistoryEntry {
            session_path: entry.session.to_string_lossy().into_owned(),
            folder: entry.folder.to_string_lossy().into_owned(),
            when: entry.when_epoch.map(|epoch| epoch as i64),
            // The app records 0 for a session it did not start; the list's `Option`
            // means "unknown", and a row with no count is better than a row claiming
            // zero lanes.
            lanes: (entry.lanes > 0).then_some(entry.lanes),
            coordinator_model: entry.models.coordinator.clone(),
            lanes_model: entry.models.lanes.clone(),
            swarm: entry.swarm,
            source: match entry.source {
                history::HistorySource::Index => session::HistorySource::Index,
                history::HistorySource::Recent => session::HistorySource::Recent,
            },
            // A browser has no `app.json`, so it cannot know this; the index never
            // says so either (§9.5).
            open_at_quit: entry.open_at_quit,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One index row, as `GET /api/sessions` prints it — the same shape
    /// `store::history::parse` reads.
    fn index_body() -> Value {
        serde_json::json!({
            "sessions": [{
                "id": "e82d8f3835d28a23",
                "path": "/Users/you/.evo/sessions/x/1.sexp",
                "cwd": "/Users/you/coding/foo",
                "program": "evo-swarm",
                "swarm_id": "20260930T132823-4fa5",
                "title": "a goal",
                "created_at": 1_756_000_000_000u64,
                "updated_at": 1_756_000_300_000u64,
                "entries": 926
            }]
        })
    }

    /// A body from the server becomes the row the window draws: the folder names it,
    /// the journal is what a resume opens, and it is a swarm's.
    #[test]
    fn a_server_body_becomes_a_history_row() {
        let entries = session_entries(&index_body());
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.session_path, "/Users/you/.evo/sessions/x/1.sexp");
        assert_eq!(entry.folder, "/Users/you/coding/foo");
        assert!(entry.swarm, "an `evo-swarm` session resumes as a swarm");
        assert_eq!(entry.when, Some(1_756_000_300));
        assert!(
            !entry.open_at_quit,
            "a browser has no recents to know this from"
        );
    }

    /// The whole hand-off: both bodies and no error is a filled launcher, and the rows
    /// are the ones the list will draw.
    #[test]
    fn both_reads_make_the_launcher_data_the_window_takes() {
        let catalog = serde_json::json!({"models": [{"id": "m", "provider": "p", "ready": true}]});
        let data = from_server(Some(&index_body()), Some(&catalog), None, 1_756_000_300);

        assert_eq!(data.catalog, Some(catalog));
        assert_eq!(data.catalog_error, None);
        assert_eq!(data.history.len(), 1);
        assert_eq!(data.history_error, None);
        assert!(!data.history_loading);
        assert_eq!(data.now, 1_756_000_300, "the caller's clock, untouched");
    }

    /// A read that could not be made says why instead of claiming there is nothing to
    /// resume (§9.5), and one that has not answered yet is simply empty.
    #[test]
    fn a_failed_read_is_a_reason_and_a_missing_one_is_nothing() {
        let failed = from_server(None, None, Some("no such binary".to_owned()), 0);
        assert!(failed.history.is_empty());
        assert_eq!(failed.history_error.as_deref(), Some("no such binary"));
        assert_eq!(failed.catalog, None);

        let nothing = from_server(None, None, None, 0);
        assert!(nothing.history.is_empty());
        assert_eq!(nothing.history_error, None);
        assert_eq!(nothing.catalog, None);
    }

    /// A body that is not an index is no rows, not a panic: a 404 body or a proxy's
    /// error JSON must not take the page down.
    #[test]
    fn a_body_that_is_not_an_index_is_no_rows() {
        assert!(session_entries(&serde_json::json!({"error": "not found"})).is_empty());
        assert!(session_entries(&Value::Null).is_empty());
    }
}
