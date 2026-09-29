//! History: the resumable swarms on disk (§9.5).
//!
//! Sessions live in `~/.evo/sessions/<encoded-cwd>/<timestamp>_<id>.sexp`, one
//! append-only journal per session. A session is *resumable as a swarm* exactly
//! when it contains a `:custom` entry under key `swarm` — that record is what
//! `evo-swarm --resume` reads back to rebuild the lanes. So the scan walks the
//! files newest first, reads the header (folder, id, timestamp) and the **last**
//! swarm record, and hands the UI a row.
//!
//! It is I/O over data that is not ours: read-only, budgeted, and safe to run
//! on a background thread ([`scan_in_background`]).

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::app_state::Recent;
use crate::paths::{self, Root};
use crate::sexp::Sexp;
use crate::tab::TabModels;
use crate::time;

/// The session vocabulary the scan needs. Anything else in a journal is
/// somebody else's business.
const SWARM_KEY: &str = "swarm";

/// How much of `~/.evo/sessions` one scan may touch (§9.5: ~500 files / 5 s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanBudget {
    /// Newest-first file cap.
    pub max_files: usize,
    /// Wall-clock cap for the whole scan.
    pub max_duration: Duration,
    /// Per-file read cap. A session larger than this is skipped rather than
    /// read to the end.
    pub max_file_bytes: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        ScanBudget { max_files: 500, max_duration: Duration::from_secs(5), max_file_bytes: 8 << 20 }
    }
}

/// Where a row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// Found by walking `~/.evo/sessions`.
    Scanned,
    /// Remembered by the app in `app.json` (it may be old enough that the file
    /// is gone, or newer than the scan's budget reached).
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
    /// The journal file's mtime, the scan's recency key.
    pub mtime: u64,
    /// The session id from the journal header.
    pub session_id: String,
    /// The swarm id evo assigned (`20260929T051622-d540`).
    pub swarm_id: String,
    /// Worker count as the swarm record has it.
    pub workers: u32,
    /// Lane count: the record's lanes, else its worker count.
    pub lanes: u32,
    /// Each lane's cwd, when the record was read (worktrees included).
    pub lane_cwds: Vec<PathBuf>,
    /// Models, when known. The scan knows the coordinator's from the last
    /// `:model-change`; the lanes model only the app remembers.
    pub models: TabModels,
    pub source: HistorySource,
}

impl HistoryEntry {
    /// The folder's last path component, for a row label.
    pub fn folder_name(&self) -> String {
        crate::tab::folder_label(&self.folder)
    }

    /// How long ago this swarm was last written, in seconds.
    pub fn age_secs(&self, now_epoch: u64) -> Option<u64> {
        now_epoch.checked_sub(self.mtime)
    }

    /// `--resume` arguments for opening this row: folder, session path, lane
    /// count (a resumed swarm keeps the count its record has).
    pub fn resume_args(&self) -> (PathBuf, PathBuf, u32) {
        (self.folder.clone(), self.session.clone(), self.lanes)
    }
}

/// What one scan of the sessions directory found.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanOutcome {
    /// Resumable swarms, newest first.
    pub entries: Vec<HistoryEntry>,
    /// How many `.sexp` files the directory holds (before the file cap).
    pub files_seen: usize,
    /// How many were actually read.
    pub files_read: usize,
    /// True when a budget (files, bytes, or time) cut the scan short.
    pub stopped_early: bool,
}

/// `~/.evo/sessions`, or `$EVO_SESSIONS_DIR` when a process overrides it.
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("EVO_SESSIONS_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => paths::evo_dir().join("sessions"),
    }
}

/// Walk `dir` newest first and return every resumable swarm within `budget`.
pub fn scan(dir: &Path, budget: &ScanBudget) -> ScanOutcome {
    let started = Instant::now();
    let mut candidates = candidates(dir);
    let files_seen = candidates.len();
    candidates.truncate(budget.max_files);
    let capped = files_seen > candidates.len();

    let mut outcome = ScanOutcome {
        entries: Vec::new(),
        files_seen,
        files_read: 0,
        stopped_early: capped,
    };
    for (path, mtime) in candidates {
        if started.elapsed() > budget.max_duration {
            outcome.stopped_early = true;
            break;
        }
        outcome.files_read += 1;
        // A file we could not read, or one the byte budget cut short, yields no
        // entry; it is the read that counts, not the result.
        if let Ok((entry, truncated)) = read_history(&path, mtime, budget.max_file_bytes) {
            outcome.stopped_early |= truncated;
            if let Some(entry) = entry {
                outcome.entries.push(entry);
            }
        }
    }
    // Newest first, then by path so equal mtimes still order deterministically.
    outcome.entries.sort_by(|a, b| {
        b.mtime.cmp(&a.mtime).then_with(|| a.session.cmp(&b.session))
    });
    outcome
}

/// Scan the default sessions directory.
pub fn scan_default(budget: &ScanBudget) -> ScanOutcome {
    scan(&sessions_dir(), budget)
}

/// Scan on a thread of its own and deliver the result on a channel — the UI
/// thread never waits for this (§2 rule 6).
pub fn scan_in_background(dir: PathBuf, budget: ScanBudget) -> Receiver<ScanOutcome> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("store-history-scan".into())
        .spawn(move || {
            let outcome = scan(&dir, &budget);
            let _ = tx.send(outcome);
        })
        .expect("spawn history scan");
    rx
}

/// Merge the on-disk scan with the app's own recents: deduped by session path,
/// newest first (§9.5, §14.3 — history lists every resumable swarm, not only
/// the ones this app created).
pub fn merge(scanned: Vec<HistoryEntry>, recents: &[Recent]) -> Vec<HistoryEntry> {
    let mut entries: Vec<HistoryEntry> = Vec::with_capacity(scanned.len() + recents.len());
    for scanned in scanned {
        entries.push(scanned);
    }
    for recent in recents {
        if recent.session.as_os_str().is_empty() {
            continue;
        }
        if let Some(existing) = entries.iter_mut().find(|e| e.session == recent.session) {
            // The scan knows the swarm; the app knows the lanes model and when
            // the tab was last opened. Keep both.
            fill_from_recent(existing, recent);
            continue;
        }
        entries.push(from_recent(recent));
    }
    entries.sort_by(|a, b| {
        b.mtime.cmp(&a.mtime).then_with(|| a.session.cmp(&b.session))
    });
    entries
}

/// The whole history for a root: a scan of `dir` plus the app's own recents
/// from `app.json`.
pub fn load_history(root: &Root, dir: &Path, budget: &ScanBudget) -> Vec<HistoryEntry> {
    let recents = crate::app_state::AppState::load(root).recents;
    merge(scan(dir, budget).entries, &recents)
}

fn from_recent(recent: &Recent) -> HistoryEntry {
    let when_epoch = time::parse_rfc3339(&recent.when);
    HistoryEntry {
        session: recent.session.clone(),
        folder: recent.folder.clone(),
        when: recent.when.clone(),
        when_epoch,
        mtime: when_epoch.unwrap_or(0),
        session_id: session_id_of(&recent.session),
        swarm_id: String::new(),
        workers: 0,
        lanes: recent.lanes,
        lane_cwds: Vec::new(),
        models: recent.models.clone(),
        source: HistorySource::Recent,
    }
}

fn fill_from_recent(entry: &mut HistoryEntry, recent: &Recent) {
    if entry.models.coordinator.is_none() {
        entry.models.coordinator = recent.models.coordinator.clone();
    }
    // The scan cannot see the lanes model: it lives in the project's swarm.lisp.
    if entry.models.lanes.is_none() {
        entry.models.lanes = recent.models.lanes.clone();
    }
    if entry.lanes == 0 {
        entry.lanes = recent.lanes;
    }
    if entry.folder.as_os_str().is_empty() {
        entry.folder = recent.folder.clone();
    }
    // Whichever of the two is later is the row's recency.
    if let Some(recent_epoch) = time::parse_rfc3339(&recent.when) {
        if recent_epoch > entry.mtime {
            entry.mtime = recent_epoch;
            entry.when = recent.when.clone();
            entry.when_epoch = Some(recent_epoch);
        }
    }
}

/// `20260929T090956Z_ed99c60d1dee3c3f.sexp` → `ed99c60d1dee3c3f`.
fn session_id_of(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match stem.split_once('_') {
        Some((_, id)) => id.to_owned(),
        None => stem,
    }
}

/// Every `.sexp` under `dir/<encoded-cwd>/`, with its mtime, newest first.
fn candidates(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(projects) = fs::read_dir(dir) else {
        return out;
    };
    for project in projects.flatten() {
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sexp") {
                continue;
            }
            let mtime = file
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(time::epoch_of)
                .unwrap_or(0);
            out.push((path, mtime));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Read one journal far enough to describe it.
///
/// `Ok((Some(entry), …))` when the file is a resumable swarm; `Ok((None, …))`
/// when it is not. The flag says whether the byte budget stopped the read
/// before the end of the file, so the caller can report a truncated scan.
///
/// The read is line-oriented and only looks at lines that begin an entry
/// (`(:type …` in column 0); entries we care about are buffered until they
/// balance, everything else is skipped. A journal entry whose *text* happens to
/// contain a column-0 line that looks like one of those headers would be read as
/// one — the price of not walking every byte of every journal.
fn read_history(path: &Path, mtime: u64, max_bytes: u64) -> io::Result<(Option<HistoryEntry>, bool)> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut read: u64 = 0;
    let mut header: Option<Sexp> = None;
    let mut swarm: Option<Sexp> = None;
    // A recorded model change is authoritative and later than the messages
    // around it, so the *last* one wins; a journal that never recorded one
    // falls back to the model of its first assistant message.
    let mut model_change: Option<String> = None;
    let mut first_message_model: Option<String> = None;
    let mut pending: Option<String> = None;
    let mut truncated = false;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        read += line.len() as u64 + 1;
        if read > max_bytes {
            truncated = true;
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);

        match pending.as_mut() {
            // Inside a form we decided to read: keep taking lines until it is
            // whole. A string in a task description may contain anything,
            // including something that looks like the start of an entry.
            Some(_) => {
                let complete = {
                    let buffer = pending.as_mut().unwrap();
                    buffer.push('\n');
                    buffer.push_str(trimmed);
                    balanced(buffer)
                };
                if complete {
                    let text = pending.take().unwrap();
                    record(&text, &mut swarm, &mut model_change);
                }
            }
            None => {
                if header.is_none() {
                    if trimmed.trim_start().starts_with("(:type :session") {
                        if balanced(trimmed) {
                            header = Sexp::parse(trimmed).ok();
                        } else {
                            pending = Some(trimmed.to_owned());
                        }
                    }
                    continue;
                }
                match entry_type_of(trimmed) {
                    Some("custom") | Some("model-change") => {
                        if balanced(trimmed) {
                            record(trimmed, &mut swarm, &mut model_change);
                        } else {
                            pending = Some(trimmed.to_owned());
                        }
                    }
                    // A message entry is never buffered: it carries the whole
                    // conversation turn. Only its header is read, only for the
                    // model it names, and only until one is found — this is the
                    // fallback for journals that recorded no model change.
                    Some("message")
                        if model_change.is_none() && first_message_model.is_none() =>
                    {
                        first_message_model = message_model(trimmed);
                    }
                    _ => {}
                }
            }
        }
    }

    // A form we were still reading when the file (or the budget) ended.
    if let Some(buffer) = pending {
        if balanced(&buffer) {
            if header.is_none() && buffer.trim_start().starts_with("(:type :session") {
                header = Sexp::parse(&buffer).ok();
            } else if header.is_some() {
                record(&buffer, &mut swarm, &mut model_change);
            }
        }
    }

    if truncated && swarm.is_none() {
        return Ok((None, truncated));
    }
    let Some(header) = header else {
        return Ok((None, truncated));
    };
    if header.entry_type() != Some("session") {
        return Ok((None, truncated));
    }
    let Some(swarm) = swarm else {
        return Ok((None, truncated)); // not a swarm session: not resumable
    };

    let lane_cwds: Vec<PathBuf> = swarm
        .get("lanes")
        .map(|lanes| {
            lanes
                .items()
                .iter()
                .filter_map(|lane| lane.get_str("cwd"))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    let workers = swarm.get_i64("workers").unwrap_or(0).clamp(0, u32::MAX as i64) as u32;
    let lanes = if lane_cwds.is_empty() { workers } else { lane_cwds.len() as u32 };

    let folder = header
        .get_str("cwd")
        .map(PathBuf::from)
        .or_else(|| swarm.get_str("dir").map(PathBuf::from))
        .unwrap_or_default();
    let when = header.get_str("timestamp").unwrap_or_default().to_string();

    Ok((
        Some(HistoryEntry {
            session: path.to_path_buf(),
            folder,
            when_epoch: time::parse_rfc3339(&when),
            when: if when.is_empty() { time::format_rfc3339(mtime) } else { when },
            mtime,
            session_id: header.get_str("id").unwrap_or_default().to_string(),
            swarm_id: swarm.get_str("id").unwrap_or_default().to_string(),
            workers,
            lanes,
            lane_cwds,
            models: TabModels { coordinator: model_change.or(first_message_model), lanes: None },
            source: HistorySource::Scanned,
        }),
        truncated,
    ))
}

/// Remember the interesting parts of one parsed form.
fn record(text: &str, swarm: &mut Option<Sexp>, model_change: &mut Option<String>) {
    let Ok(form) = Sexp::parse(text) else { return };
    match form.entry_type() {
        // Only the newest record counts: a swarm that grew or shrank journals
        // the shape again, and the last one is the live one.
        Some("custom") if form.get_str("key") == Some(SWARM_KEY) => {
            if let Some(data) = form.get("data") {
                *swarm = Some(data.clone());
            }
        }
        Some("model-change") => {
            if let Some(model) = form.get_str("model") {
                *model_change = Some(model.to_owned());
            }
        }
        _ => {}
    }
}

/// The model an assistant message names, read out of its header.
///
/// evo journals the model on every assistant message
/// (`:message (:role :assistant :api … :provider … :model "…" :stop-reason …)`),
/// which is the only place a real session states it — most journals contain no
/// `:model-change` at all. A message entry spans lines (its text has newlines),
/// so only the header is read: everything before the message's own `:content`,
/// walked as key/value pairs with the shared reader. A header cut short by the
/// line end simply ends the walk.
fn message_model(line: &str) -> Option<String> {
    /// The header is a few hundred bytes; never look past this much of a line.
    const HEADER_CAP: usize = 4096;

    let rest = line.strip_prefix("(:type :message ")?;
    // The cap is in bytes and journals are UTF-8: step back to a character
    // boundary rather than panicking on a multi-byte character at the edge.
    let mut cap = rest.len().min(HEADER_CAP);
    while !rest.is_char_boundary(cap) {
        cap -= 1;
    }
    let window = &rest[..cap];
    let after_body = &window[window.find(":message (")? + ":message (".len()..];
    let header = match after_body.find(" :content ") {
        Some(content) => &after_body[..content],
        None => after_body,
    };

    let mut role = None;
    let mut model = None;
    let mut pos = 0;
    while let Ok((key, read)) = Sexp::parse_prefix(&header[pos..]) {
        pos += read;
        let Some(key) = key.as_symbol() else { break };
        let Ok((value, read)) = Sexp::parse_prefix(&header[pos..]) else { break };
        pos += read;
        match key {
            "role" => role = value.as_symbol().map(str::to_owned),
            "model" => model = value.as_str().map(str::to_owned),
            _ => {}
        }
    }
    match role.as_deref() {
        Some("assistant") => model,
        _ => None,
    }
}

/// The `:type` of a line that starts an entry, cheaply — without parsing.
/// The leading colon is dropped, the same way [`Sexp::entry_type`] reports it.
fn entry_type_of(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("(:type ")?;
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    Some(rest[..end].trim_start_matches(':'))
}

/// True when the text holds a whole form: every paren closed, no open string.
fn balanced(text: &str) -> bool {
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    !in_string && depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-hist-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    /// Write a journal with a set mtime: the scan orders by mtime, and a test
    /// that writes four files in one tick would otherwise be ordered by path.
    fn write_journal(path: &Path, contents: &str, mtime: &str) {
        fs::write(path, contents).unwrap();
        let at = std::time::UNIX_EPOCH + Duration::from_secs(time::parse_rfc3339(mtime).unwrap());
        fs::OpenOptions::new().write(true).open(path).unwrap().set_modified(at).unwrap();
    }

    fn header(cwd: &str, id: &str, ts: &str) -> String {
        format!("(:type :session :version 1 :id \"{id}\" :cwd \"{cwd}\" :timestamp \"{ts}\")")
    }

    fn swarm_line(swarm_id: &str, workers: u32, cwds: &[&str]) -> String {
        let lanes: Vec<String> = cwds
            .iter()
            .enumerate()
            .map(|(i, cwd)| {
                format!("(:n {} :cwd \"{cwd}\" :worktree nil :branch nil :task nil :extra-forms #())", i + 1)
            })
            .collect();
        format!(
            "(:type :custom :id \"deadbeef\" :parent-id nil :timestamp \"2026-09-29T05:16:22Z\" \
             :key \"swarm\" :data (:id \"{swarm_id}\" :dir \"/Users/x/.evo/swarm/{swarm_id}/\" \
             :workers {workers} :lanes #({})))",
            lanes.join(" ")
        )
    }

    /// An assistant message in the shape evo writes it: the header carries the
    /// model, and the text — which may span lines — follows `:content`.
    fn assistant_message(model: &str, text: &str) -> String {
        format!(
            "(:type :message :id \"ma\" :parent-id nil :timestamp \"2026-09-29T01:00:03Z\" \
             :message (:role :assistant :api :anthropic-messages :provider :aiden \
             :model \"{model}\" :stop-reason :end-turn \
             :usage (:input 4 :output 118 :cache-read 0 :cache-write 168541) \
             :content ((:type :text :text \"{text}\"))))"
        )
    }

    /// A sessions tree with one journal per case; returns (root, sessions dir).
    fn fixture(name: &str) -> (Root, PathBuf) {
        let root = temp_root(name);
        let dir = root.path().join("sessions");
        fs::create_dir_all(dir.join("-Users-x-foo")).unwrap();
        fs::create_dir_all(dir.join("-Users-x-bar")).unwrap();

        // A live swarm: header, chatter, a swarm record, then an assistant
        // message that names the model. Most real journals have no
        // `:model-change` at all, so this is where the model comes from.
        write_journal(
            &dir.join("-Users-x-foo").join("20260929T010000Z_aaaa1111.sexp"),
            &format!(
                "{}\n{}\n{}\n{}\n",
                header("/Users/x/foo/", "aaaa1111", "2026-09-29T01:00:00Z"),
                "(:type :message :id \"m1\" :parent-id nil :timestamp \"2026-09-29T01:00:01Z\" :message (:role :user :content ((:type :text :text \"hi\"))))",
                swarm_line("20260929T010000-aaaa", 3, &["/Users/x/foo/", "/Users/x/foo/", "/Users/x/foo/"]),
                // The entry spans lines: only its header is read.
                assistant_message("ark-deepseek-v4.1-flash", "first line\nthe model is :model \\\"not this\\\""),
            ),
            "2026-09-29T01:00:00Z",
        );

        // A swarm whose shape changed: the last record wins, and it carries the
        // models journaled elsewhere in the file.
        write_journal(
            &dir.join("-Users-x-foo").join("20260929T020000Z_bbbb2222.sexp"),
            &format!(
                "{}\n{}\n{}\n{}\n{}\n{}\n",
                header("/Users/x/foo/", "bbbb2222", "2026-09-29T02:00:00Z"),
                assistant_message("claude-opus-5", "a first turn on the old model"),
                swarm_line("20260929T020000-bbbb", 2, &["/Users/x/foo/", "/Users/x/foo/"]),
                "(:type :custom :id \"cs\" :parent-id nil :timestamp \"2026-09-29T02:00:05Z\" :key \"cache-stats\" :data (:input 1 :cache-read 0 :cache-write 0))",
                swarm_line("20260929T020000-bbbb", 1, &["/Users/x/foo/", "/Users/x/.evo/swarm/bbbb/lane-2/"]),
                // A recorded change is authoritative: it wins over the message
                // above, whatever order they sit in.
                "(:type :model-change :id \"mc1\" :parent-id nil :timestamp \"2026-09-29T02:00:09Z\" :model \"claude-sonnet-5\" :provider :anthropic)",
            ),
            "2026-09-29T02:00:00Z",
        );

        // A plain session: no swarm record → not resumable.
        write_journal(
            &dir.join("-Users-x-bar").join("20260929T030000Z_cccc3333.sexp"),
            &format!("{}\n{}\n", header("/Users/x/bar/", "cccc3333", "2026-09-29T03:00:00Z"), "(:type :custom :id \"x\" :parent-id nil :timestamp \"2026-09-29T03:00:01Z\" :key \"todo\" :data #())"),
            "2026-09-29T03:00:00Z",
        );

        // A file with no header at all, and a non-journal file.
        write_journal(&dir.join("-Users-x-bar").join("broken.sexp"), "garbage\n", "2026-09-29T04:00:00Z");
        fs::write(dir.join("-Users-x-bar").join("notes.txt"), "ignore me").unwrap();

        (root, dir)
    }

    #[test]
    fn finds_only_resumable_swarms_newest_first() {
        let (root, dir) = fixture("basic");
        let outcome = scan(&dir, &ScanBudget::default());
        assert_eq!(outcome.files_seen, 4, "three journals plus the broken one");
        assert_eq!(outcome.entries.len(), 2);
        assert!(!outcome.stopped_early);

        // Newest file first: bbbb2222 was written last.
        let newest = &outcome.entries[0];
        assert_eq!(newest.session_id, "bbbb2222");
        assert_eq!(newest.swarm_id, "20260929T020000-bbbb");
        assert_eq!(newest.folder, PathBuf::from("/Users/x/foo/"));
        assert_eq!(newest.folder_name(), "foo");
        assert_eq!(newest.when, "2026-09-29T02:00:00Z");
        assert_eq!(newest.when_epoch, time::parse_rfc3339("2026-09-29T02:00:00Z"));
        // A recorded `:model-change` is authoritative and wins over the
        // assistant message in the same file; the message is only the fallback
        // for journals that never recorded a change (see the older entry).
        assert_eq!(newest.models.coordinator.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(newest.models.lanes, None);
        assert_eq!(newest.source, HistorySource::Scanned);
        // The last record wins: one lane, whose cwd is a worktree.
        assert_eq!(newest.lanes, 2);
        assert_eq!(newest.workers, 1);
        assert_eq!(newest.lane_cwds[1], PathBuf::from("/Users/x/.evo/swarm/bbbb/lane-2/"));

        let older = &outcome.entries[1];
        assert_eq!(older.session_id, "aaaa1111");
        assert_eq!(older.lanes, 3);
        assert_eq!(older.workers, 3);
        // No `:model-change` in this file at all: the model comes from the
        // assistant message's header, and the message's own text (which spans
        // lines and even quotes a `:model`) never leaks into it.
        assert_eq!(older.models.coordinator.as_deref(), Some("ark-deepseek-v4.1-flash"));
        assert_eq!(older.age_secs(older.mtime + 90), Some(90));
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn the_model_comes_from_an_assistant_messages_header() {
        // The shape a real journal writes (verified against
        // ~/.evo/sessions/*/*.sexp): the header before `:content` is all we read.
        let line = assistant_message("claude-opus-5-5", "hello");
        assert_eq!(message_model(&line).as_deref(), Some("claude-opus-5-5"));
        // Any order of the header's keys works.
        let shuffled = "(:type :message :id \"m\" :message (:provider :aiden :role :assistant :model \"m-2\" \
                        :usage (:input 1) :content ((:type :text :text \"x\"))))";
        assert_eq!(message_model(shuffled).as_deref(), Some("m-2"));
        // The message's text is not searched: only the header is.
        let quoted = assistant_message("honest", "the header of a message is (:role :assistant :model \"liar\")");
        assert_eq!(message_model(&quoted).as_deref(), Some("honest"));

        // Not a message, not an assistant, or a header that never says :model.
        assert_eq!(message_model("(:type :custom :role :assistant :model \"no\")"), None);
        assert_eq!(message_model("(:type :message :id \"u\" :message (:role :user :content ()))"), None);
        assert_eq!(message_model("(:type :message :id \"a\" :message (:role :assistant :content ()))"), None);
        // A header cut off mid-value still yields what came before the cut.
        let cut = "(:type :message :id \"a\" :message (:role :assistant :model \"cut\" :content ((:type :text :text \"oops";
        assert_eq!(message_model(cut).as_deref(), Some("cut"));
        let cut_early = "(:type :message :id \"a\" :message (:role :assistant :model \"cu";
        assert_eq!(message_model(cut_early), None);
    }

    #[test]
    fn budgets_stop_the_scan() {
        let (root, dir) = fixture("budget");
        let one = ScanBudget { max_files: 1, ..ScanBudget::default() };
        let outcome = scan(&dir, &one);
        assert_eq!(outcome.files_read, 1);
        assert!(outcome.stopped_early);

        let tiny = ScanBudget { max_file_bytes: 40, ..ScanBudget::default() };
        let outcome = scan(&dir, &tiny);
        // Nothing past 40 bytes can be recognized as a swarm.
        assert!(outcome.entries.is_empty());
        assert!(outcome.stopped_early);

        let no_time = ScanBudget { max_duration: Duration::ZERO, ..ScanBudget::default() };
        let outcome = scan(&dir, &no_time);
        assert!(outcome.entries.is_empty());
        assert!(outcome.stopped_early);
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn a_missing_directory_is_an_empty_scan() {
        let outcome = scan(Path::new("/nonexistent/store/history"), &ScanBudget::default());
        assert_eq!(outcome.entries.len(), 0);
        assert_eq!(outcome.files_seen, 0);
        assert!(!outcome.stopped_early);
    }

    #[test]
    fn multi_line_and_hostile_entries_are_survived() {
        let root = temp_root("hostile");
        let dir = root.path().join("sessions/-Users-x-q");
        fs::create_dir_all(&dir).unwrap();
        // A swarm record whose task string spans lines and contains text that
        // looks like the start of another entry.
        let file = dir.join("20260929T040000Z_dddd4444.sexp");
        write_journal(
            &file,
            &format!(
                "{}\n{}\n{}\n(:type :custom :id \"z\" :parent-id nil :timestamp \"t\" :key \"swarm\" :data (:id \"s1\" :workers 1 :lanes #()))\n",
                header("/Users/x/q/", "dddd4444", "2026-09-29T04:00:00Z"),
                "(:type :custom :id \"a\" :parent-id nil :timestamp \"t\" :key \"swarm\" :data (:id \"s0\" :workers 9 :lanes #((:n 1 :cwd \"/a/\" :task \"line one\n(:type :custom :key \"swarm\" :data (:id \"fake\"))\nline three\" :extra-forms #()))))",
                "(:type :message :id \"m\" :parent-id nil :timestamp \"t\" :message (:role :user :content ((:type :text :text \"x\"))))",
            ),
            "2026-09-29T04:00:00Z",
        );
        let outcome = scan(&root.path().join("sessions"), &ScanBudget::default());
        assert_eq!(outcome.entries.len(), 1);
        // The embedded fake never becomes the record; the real last one wins.
        assert_eq!(outcome.entries[0].swarm_id, "s1");
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn merge_dedupes_by_session_and_keeps_the_newer_fact() {
        let (root, dir) = fixture("merge");
        let scanned = scan(&dir, &ScanBudget::default()).entries;
        let older = scanned[1].session.clone();

        let mut recent_same = Recent::new(&older, "/Users/x/foo", 3);
        recent_same.when = "2026-09-29T09:00:00Z".into(); // app used it later than the journal
        recent_same.models.lanes = Some("lanes-model".into());
        let mut recent_orphan = Recent::new("/Users/x/gone/9.sexp", "/Users/x/gone", 2);
        // Pinned so the row order does not depend on when the test runs.
        recent_orphan.when = "2026-09-29T00:30:00Z".into();

        let merged = merge(scanned.clone(), &[recent_same.clone(), recent_orphan.clone()]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].session, older, "the app's later use moves it to the top");
        // Scanned facts survive the merge; the app's lanes model is added.
        assert_eq!(merged[0].source, HistorySource::Scanned);
        assert_eq!(merged[0].swarm_id, "20260929T010000-aaaa");
        assert_eq!(merged[0].models.lanes.as_deref(), Some("lanes-model"));
        assert_eq!(merged[0].when, "2026-09-29T09:00:00Z");
        assert_eq!(merged[0].lanes, 3);

        let orphan = merged.iter().find(|e| e.session == recent_orphan.session).unwrap();
        assert_eq!(orphan.source, HistorySource::Recent);
        assert_eq!(orphan.session_id, "9");
        assert_eq!(orphan.folder, PathBuf::from("/Users/x/gone"));
        assert_eq!(orphan.resume_args().1, recent_orphan.session);
        assert_eq!(orphan.age_secs(orphan.mtime + 1), Some(1));

        // A recent with no session path is not a row.
        let empty = Recent { session: PathBuf::new(), ..Recent::default() };
        assert_eq!(merge(Vec::new(), &[empty]).len(), 0);
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn load_history_reads_recents_from_app_json() {
        let (root, dir) = fixture("load");
        let mut app = crate::app_state::AppState::default();
        app.touch_recent(Recent::new("/Users/x/elsewhere/1.sexp", "/Users/x/elsewhere", 4));
        app.save(&root).unwrap();
        let history = load_history(&root, &dir, &ScanBudget::default());
        assert_eq!(history.len(), 3);
        assert!(history.iter().any(|e| e.folder == *"/Users/x/elsewhere"));
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn balance_detection() {
        assert!(balanced("(:a 1)"));
        assert!(!balanced("(:a 1"));
        assert!(!balanced("(:a \"unterminated"));
        assert!(balanced("(:a \"a ) b\")"));
        assert!(balanced("(:a \"escaped \\\" quote\")"));
        assert!(!balanced(")"));
        assert!(balanced("#((:n 1))"));
    }

    #[test]
    fn entry_type_is_read_cheaply() {
        assert_eq!(entry_type_of("(:type :custom :id \"x\")"), Some("custom"));
        assert_eq!(entry_type_of("(:type :swarm-state :x 1)"), Some("swarm-state"));
        assert_eq!(entry_type_of("  (:type :custom)"), None);
        assert_eq!(entry_type_of("(:type)"), None);
    }

    #[test]
    fn session_ids_come_from_the_filename() {
        assert_eq!(session_id_of(Path::new("/a/20260929T090956Z_ed99c60d1dee3c3f.sexp")), "ed99c60d1dee3c3f");
        assert_eq!(session_id_of(Path::new("/a/odd.sexp")), "odd");
    }
}
