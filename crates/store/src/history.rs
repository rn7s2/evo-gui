//! History: the resumable swarms on disk, as `evo-agent sessions --json` reports
//! them (§2, §9).
//!
//! The app no longer reads journals. evo keeps an index
//! (`~/.evo/sessions/index.jsonl`) and prints it; the CLI walks the journals only
//! when the index is missing or `--rescan` was asked for, so the app's cost is one
//! process and one document — no sexp reader, no per-file budget, no half-read
//! journal.
//!
//! ```text
//! Session = {id, path, cwd, program, swarm_id|null, title, created_at, updated_at, entries}
//! ```
//!
//! Two things the index cannot say, and this module keeps from the app's own
//! record (`app.json`'s recents): the models a session ran with, its lane count,
//! and whether the tab was open at the last quit. [`merge`] puts the two sides
//! together, deduped by session path.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::app_state::Recent;
use crate::cli::{self, CliError};
use crate::paths::Root;
use crate::tab::TabModels;
use crate::time;

/// One session, as `evo-agent sessions --json` writes it (§2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    /// The journal (`--resume` argument).
    pub path: PathBuf,
    /// The folder it ran in.
    pub cwd: PathBuf,
    /// `evo-agent` | `evo-swarm` | `lane`.
    pub program: String,
    /// The swarm id, for a coordinator.
    pub swarm_id: Option<String>,
    /// The first user text, ≤ 80 chars, one line.
    pub title: String,
    /// Epoch milliseconds.
    pub created_at: u64,
    /// Epoch milliseconds.
    pub updated_at: u64,
    /// How many entries the journal holds.
    pub entries: u64,
}

impl Session {
    /// Read one object of the `sessions` array. An entry without an id or a path
    /// is not a session.
    pub fn from_json(value: &Value) -> Option<Session> {
        let id = string(value, "id")?;
        let path = string(value, "path")?;
        Some(Session {
            id,
            path: PathBuf::from(path),
            cwd: PathBuf::from(string(value, "cwd").unwrap_or_default()),
            program: string(value, "program").unwrap_or_default(),
            swarm_id: string(value, "swarm_id"),
            title: string(value, "title").unwrap_or_default(),
            created_at: millis(value, "created_at"),
            updated_at: millis(value, "updated_at"),
            entries: value.get("entries").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// `updated_at` as epoch seconds — the recency key the app sorts and phrases
    /// by.
    pub fn updated_epoch(&self) -> u64 {
        self.updated_at / 1000
    }

    /// `updated_at` as RFC 3339 UTC, for a row's text.
    pub fn updated_text(&self) -> String {
        time::format_rfc3339(self.updated_epoch())
    }

    /// The journal is really there: a resumed path that is not a file is a
    /// launch that cannot come back.
    pub fn is_resumable(&self) -> bool {
        self.path.is_file()
    }

    /// Whether this row is a swarm's coordinator.
    pub fn is_swarm(&self) -> bool {
        self.swarm_id.is_some() || self.program == "evo-swarm"
    }
}

/// What to ask `evo-agent sessions --json` for (§2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsQuery {
    /// `--all`: every folder, not only this process's cwd.
    pub all: bool,
    /// `--cwd DIR`: that folder's sessions.
    pub cwd: Option<PathBuf>,
    /// `--program P`: only sessions of that program (`evo-swarm`, `evo-agent`).
    pub program: Option<String>,
}

impl SessionsQuery {
    /// The resumable swarms, wherever they ran: what the empty tab lists.
    pub fn swarms() -> SessionsQuery {
        SessionsQuery {
            all: true,
            cwd: None,
            program: Some("evo-swarm".to_owned()),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec!["sessions".to_owned(), "--json".to_owned()];
        if self.all {
            argv.push("--all".to_owned());
        }
        if let Some(cwd) = &self.cwd {
            argv.push("--cwd".to_owned());
            argv.push(cwd.display().to_string());
        }
        if let Some(program) = &self.program {
            argv.push("--program".to_owned());
            argv.push(program.clone());
        }
        argv
    }
}

/// Read a `sessions --json` body: `{"sessions":[…]}` in whatever order evo
/// printed it.
pub fn parse(body: &Value) -> Vec<Session> {
    let Some(list) = body.get("sessions").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter().filter_map(Session::from_json).collect()
}

/// Run `bin sessions --json …` and read the index.
pub fn fetch(bin: &Path, query: &SessionsQuery) -> Result<Vec<Session>, CliError> {
    let body = cli::run_json(bin, &query.argv())?;
    Ok(parse(&body))
}

/// The default binary's index.
pub fn fetch_default(query: &SessionsQuery) -> Result<Vec<Session>, CliError> {
    fetch(&cli::agent_bin(), query)
}

/// Where a row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// Listed by `evo-agent sessions --json`.
    Index,
    /// Remembered by the app in `app.json` (the file may be gone, or older than
    /// anything the index knows).
    Recent,
}

/// One resumable swarm, as the empty tab's history list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Coordinator session path — the `--resume` argument.
    pub session: PathBuf,
    /// The folder the swarm ran in (cwd).
    pub folder: PathBuf,
    /// RFC 3339 UTC, for display.
    pub when: String,
    /// `when` in epoch seconds, for sorting and "2h ago".
    pub when_epoch: Option<u64>,
    /// The session id from the index.
    pub session_id: String,
    /// The swarm id evo assigned (`20260929T051622-d540`), when it named one.
    pub swarm_id: String,
    /// The first user text, ≤ 80 chars — the row's own label when it has one.
    pub title: String,
    /// Worker count, when the app remembers starting one.
    pub workers: u32,
    /// Lane count, when the app remembers it.
    pub lanes: u32,
    /// The models the session ran with, as far as the app remembers.
    pub models: TabModels,
    pub source: HistorySource,
    /// The session was still open as a tab when the app last quit (§9.5).
    ///
    /// Only the app's own recents can say this: a swarm that is still up has not
    /// written its journal again.
    pub open_at_quit: bool,
}

impl HistoryEntry {
    /// The folder's last path component, for a row label.
    pub fn folder_name(&self) -> String {
        crate::tab::folder_label(&self.folder)
    }

    /// What the row is called: the session's own title when the index has one,
    /// else the folder's name.
    pub fn label(&self) -> String {
        if self.title.trim().is_empty() {
            self.folder_name()
        } else {
            self.title.clone()
        }
    }

    /// How long ago this swarm was last written, in seconds.
    pub fn age_secs(&self, now_epoch: u64) -> Option<u64> {
        now_epoch.checked_sub(self.mtime())
    }

    /// The recency key, in epoch seconds.
    pub fn mtime(&self) -> u64 {
        self.when_epoch.unwrap_or(0)
    }

    /// `--resume` arguments for opening this row: folder, session path, lane
    /// count (a resumed swarm keeps the count its record has).
    pub fn resume_args(&self) -> (PathBuf, PathBuf, u32) {
        (self.folder.clone(), self.session.clone(), self.lanes)
    }
}

/// One index row as the empty tab's list takes it.
pub fn from_session(session: &Session) -> HistoryEntry {
    HistoryEntry {
        session: session.path.clone(),
        folder: session.cwd.clone(),
        when: session.updated_text(),
        when_epoch: Some(session.updated_epoch()),
        session_id: session.id.clone(),
        swarm_id: session.swarm_id.clone().unwrap_or_default(),
        title: session.title.clone(),
        workers: 0,
        lanes: 0,
        models: TabModels::default(),
        source: HistorySource::Index,
        open_at_quit: false,
    }
}

/// Merge the index with the app's own recents: deduped by session path, newest
/// first (§9.5, §14.3 — history lists every resumable swarm, not only the ones
/// this app created).
pub fn merge(indexed: Vec<Session>, recents: &[Recent]) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::with_capacity(indexed.len() + recents.len());
    for session in indexed {
        // A journal the index remembers but the disk no longer has is not a row:
        // `--resume` on a file that is not there is a launch that cannot come
        // back.
        if !session.is_resumable() {
            continue;
        }
        entries.push(from_session(&session));
    }
    for recent in recents {
        if recent.session.as_os_str().is_empty() || !recent.session.is_file() {
            continue;
        }
        if let Some(existing) = entries.iter_mut().find(|e| e.session == recent.session) {
            // The index knows the session; the app knows the models, the lane
            // count and when the tab was last opened. Keep both.
            fill_from_recent(existing, recent);
            continue;
        }
        entries.push(entry_from_recent(recent));
    }
    // Sessions that were still open at the last quit come first: they are what
    // the user was working on, and recency cannot say so — a swarm that never
    // came down had no reason to write its journal again.
    entries.sort_by(|a, b| {
        b.open_at_quit
            .cmp(&a.open_at_quit)
            .then_with(|| b.mtime().cmp(&a.mtime()))
            .then_with(|| a.session.cmp(&b.session))
    });
    entries
}

/// The whole history for a root: `evo-agent sessions --json` plus the app's own
/// recents from `app.json`.
pub fn load(root: &Root, bin: &Path, query: &SessionsQuery) -> Result<Vec<HistoryEntry>, CliError> {
    let recents = crate::app_state::AppState::load(root).recents;
    Ok(merge(fetch(bin, query)?, &recents))
}

fn entry_from_recent(recent: &Recent) -> HistoryEntry {
    let when_epoch = time::parse_rfc3339(&recent.when);
    HistoryEntry {
        session: recent.session.clone(),
        folder: recent.folder.clone(),
        when: recent.when.clone(),
        when_epoch,
        session_id: session_id_of(&recent.session),
        swarm_id: String::new(),
        title: String::new(),
        workers: recent.lanes,
        lanes: recent.lanes,
        models: recent.models.clone(),
        source: HistorySource::Recent,
        open_at_quit: recent.open_at_quit,
    }
}

fn fill_from_recent(entry: &mut HistoryEntry, recent: &Recent) {
    if entry.models.coordinator.is_none() {
        entry.models.coordinator = recent.models.coordinator.clone();
    }
    // The index does not carry the lanes' model: the app chose it at launch.
    if entry.models.lanes.is_none() {
        entry.models.lanes = recent.models.lanes.clone();
    }
    if entry.lanes == 0 {
        entry.lanes = recent.lanes;
    }
    if entry.workers == 0 {
        entry.workers = recent.lanes;
    }
    // The app is the only one that knows this.
    entry.open_at_quit |= recent.open_at_quit;
    if entry.folder.as_os_str().is_empty() {
        entry.folder = recent.folder.clone();
    }
    // Whichever of the two is later is the row's recency.
    if let Some(recent_epoch) = time::parse_rfc3339(&recent.when) {
        if recent_epoch > entry.mtime() {
            entry.when = recent.when.clone();
            entry.when_epoch = Some(recent_epoch);
        }
    }
}

/// `20260929T090956Z_ed99c60d1dee3c3f.sexp` → `ed99c60d1dee3c3f`.
fn session_id_of(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split_once('_') {
        Some((_, id)) => id.to_owned(),
        None => stem,
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A millisecond timestamp; a body that wrote seconds (or nothing) reads as 0
/// rather than as a time in 1970-and-something.
fn millis(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-hist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    /// A journal that exists, so the row survives the resumability check.
    fn journal(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "store-hist-journal-{}-{name}.sexp",
            std::process::id()
        ));
        std::fs::write(&path, "(:type :session :version 1)").unwrap();
        path
    }

    /// An index body in the contract's shape (§2), newest first.
    fn index() -> Value {
        serde_json::json!({
            "sessions": [
                {
                    "id": "bbbb2222", "path": journal("b").display().to_string(),
                    "cwd": "/Users/x/foo", "program": "evo-swarm",
                    "swarm_id": "20260929T020000-bbbb",
                    "title": "make the history list read the index",
                    "created_at": 1_756_000_000_000u64,
                    "updated_at": 1_756_000_300_000u64,
                    "entries": 42
                },
                {
                    "id": "aaaa1111", "path": journal("a").display().to_string(),
                    "cwd": "/Users/x/bar", "program": "evo-swarm",
                    "swarm_id": null, "title": "", "created_at": 1_755_000_000_000u64,
                    "updated_at": 1_755_000_060_000u64, "entries": 7
                }
            ]
        })
    }

    #[test]
    fn the_index_parses_into_sessions() {
        let sessions = parse(&index());
        assert_eq!(sessions.len(), 2);
        let first = &sessions[0];
        assert_eq!(first.id, "bbbb2222");
        assert_eq!(first.cwd, PathBuf::from("/Users/x/foo"));
        assert_eq!(first.program, "evo-swarm");
        assert_eq!(first.swarm_id.as_deref(), Some("20260929T020000-bbbb"));
        assert_eq!(first.title, "make the history list read the index");
        assert_eq!(first.updated_epoch(), 1_756_000_300);
        assert_eq!(first.updated_text(), "2025-08-24T01:51:40Z");
        assert!(first.is_swarm() && first.is_resumable());
        // A session with no swarm id is still a session, just not a named swarm.
        assert_eq!(sessions[1].swarm_id, None);
        assert!(sessions[1].is_swarm(), "its program says so");
    }

    #[test]
    fn a_body_without_sessions_is_no_rows_and_entries_without_ids_are_dropped() {
        assert!(parse(&serde_json::json!({})).is_empty());
        assert!(parse(&serde_json::json!({"sessions": []})).is_empty());
        let partial = parse(&serde_json::json!({"sessions": [
            {"path": "/x.sexp"},
            {"id": "kept", "path": "/y.sexp"}
        ]}));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].id, "kept");
    }

    #[test]
    fn the_query_becomes_the_cli_arguments() {
        assert_eq!(SessionsQuery::swarms().argv(), ["sessions", "--json", "--all", "--program", "evo-swarm"]);
        assert_eq!(
            SessionsQuery {
                all: false,
                cwd: Some(PathBuf::from("/coding/foo")),
                program: None,
            }
            .argv(),
            ["sessions", "--json", "--cwd", "/coding/foo"]
        );
    }

    #[test]
    fn merging_keeps_the_apps_own_knowledge_about_a_session() {
        let body = index();
        let sessions = parse(&body);
        let recents = vec![Recent {
            session: sessions[0].path.clone(),
            folder: PathBuf::from("/Users/x/foo"),
            when: "2026-08-24T09:00:00Z".to_owned(),
            models: TabModels {
                coordinator: Some("claude-opus-5".to_owned()),
                lanes: Some("ark-deepseek-v4.1-flash".to_owned()),
            },
            lanes: 4,
            open_at_quit: true,
        }];
        let entries = merge(sessions, &recents);
        assert_eq!(entries.len(), 2);
        let merged = &entries[0];
        assert_eq!(merged.session_id, "bbbb2222");
        assert_eq!(merged.lanes, 4);
        assert_eq!(merged.workers, 4);
        assert_eq!(
            merged.models.coordinator.as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(merged.models.lanes.as_deref(), Some("ark-deepseek-v4.1-flash"));
        assert_eq!(merged.source, HistorySource::Index);
        assert!(merged.open_at_quit, "the app is the only side that knows");
        // The app's newer timestamp wins as the row's recency.
        assert_eq!(
            merged.when_epoch,
            time::parse_rfc3339("2026-08-24T09:00:00Z")
        );
        // …and it sorts first, because it was open at the last quit.
        assert_eq!(entries[1].session_id, "aaaa1111");
        assert_eq!(entries[1].label(), "bar", "no title → the folder's name");
    }

    #[test]
    fn a_recent_whose_journal_is_gone_is_not_a_row() {
        let entries = merge(
            Vec::new(),
            &[Recent::new(
                "/nonexistent/gone.sexp",
                "/Users/x/gone",
                2,
            )],
        );
        assert!(entries.is_empty());
        let entries = merge(
            Vec::new(),
            &[Recent::new(
                journal("recent"),
                "/Users/x/gone",
                2,
            )],
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, HistorySource::Recent);
        assert_eq!(entries[0].lanes, 2);
        assert_eq!(entries[0].label(), "gone");
    }

    #[test]
    fn an_index_row_whose_journal_vanished_is_dropped() {
        let mut body = index();
        body["sessions"][0]["path"] = Value::String("/nonexistent/vanished.sexp".to_owned());
        let entries = merge(parse(&body), &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "aaaa1111");
    }

    #[test]
    fn rows_come_back_newest_first_with_no_duplicates() {
        let sessions = parse(&index());
        let recents = vec![Recent {
            when: "2025-08-24T00:00:00Z".to_owned(),
            ..Recent::new(sessions[1].path.clone(), PathBuf::from("/Users/x/bar"), 3)
        }];
        let entries = merge(sessions, &recents);
        assert_eq!(entries.len(), 2, "the recent is the same session, not a third");
        assert!(entries[0].mtime() >= entries[1].mtime());
        assert_eq!(entries[1].session_id, "aaaa1111");
        assert_eq!(entries[1].lanes, 3, "the app remembered the lane count");
        assert_eq!(entries[1].resume_args().1, entries[1].session);
        assert!(entries[1].age_secs(entries[1].mtime() + 90) == Some(90));
    }

    #[test]
    fn the_session_id_comes_out_of_the_journal_name() {
        assert_eq!(
            session_id_of(Path::new("/x/20260929T090956Z_ed99c60d1dee3c3f.sexp")),
            "ed99c60d1dee3c3f"
        );
        assert_eq!(session_id_of(Path::new("/x/plain.sexp")), "plain");
    }

    #[test]
    fn the_root_is_not_consulted_for_the_history_itself() {
        // `load` merges the index with this root's recents; an empty root means
        // an empty recent list, not a missing file.
        let root = root("load");
        let state = crate::app_state::AppState::default();
        assert!(state.recents.is_empty());
        assert!(crate::app_state::AppState::load(&root).recents.is_empty());
        let _ = std::fs::remove_dir_all(root.path());
    }
}
