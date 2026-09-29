//! real_history — the empty tab's resumable-swarm list, from the **real**
//! `~/.evo/sessions` (docs/PROMPT.md §9.5): `store::history::scan` walks the
//! journals under the scan's own budget, and `session::history_rows` turns them
//! into the rows the list shows.
//!
//! ```text
//! CARGO_TARGET_DIR=target/real cargo run -p swarm_client --example real_history
//! CARGO_TARGET_DIR=target/real cargo run -p swarm_client --example real_history -- --limit 3
//! ```
//!
//! `--dir <dir>` scans somewhere else (a copy, a fixture), `--limit N` prints
//! that many rows (5 by default). Paths and names only: a journal carries no
//! secret, and nothing here reads a body beyond the header and the swarm record.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use session::{history_rows, HistoryEntry, HistorySource, When};
use store::history::{HistorySource as StoreSource, ScanBudget};

fn main() {
    let mut limit = 5usize;
    let mut dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--limit" => limit = args.next().and_then(|n| n.parse().ok()).unwrap_or(5),
            "--dir" => dir = args.next().map(PathBuf::from),
            other => {
                eprintln!("usage: real_history [--limit N] [--dir <sessions_dir>] (got {other})");
                std::process::exit(2);
            }
        }
    }

    let dir = dir.unwrap_or_else(store::history::sessions_dir);
    let started = std::time::Instant::now();
    let outcome = store::history::scan(&dir, &ScanBudget::default());
    let took = started.elapsed();

    println!("=== real_history ===");
    println!("sessions_dir:       {}", dir.display());
    println!("files_seen:         {}", outcome.files_seen);
    println!("files_read:         {}", outcome.files_read);
    println!("stopped_early:      {}", outcome.stopped_early);
    println!("resumable swarms:   {} in {:?}", outcome.entries.len(), took);

    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let offset = utc_offset_seconds();
    let home = std::env::var("HOME").ok();
    let entries: Vec<HistoryEntry> = outcome.entries.iter().map(to_session_entry).collect();
    let rows = history_rows(&entries, now, offset, home.as_deref());

    println!(
        "scan:               now={now} offset={offset:+}s home={}",
        home.as_deref().unwrap_or("(unset)")
    );
    println!("rows:               {}", rows.len());
    // `history_rows` orders by the journal's own time and dedups by session path;
    // the scan ordered by file mtime. Pair each row with the entry it came from.
    for (n, row) in rows.iter().take(limit).enumerate() {
        let entry = entries.iter().find(|entry| entry.session_path == row.session_path);
        println!("--- row {} (newest first) ---", n + 1);
        match entry {
            Some(entry) => println!(
                "scan:               folder={} lanes={:?} when={:?} coordinator_model={:?} lanes_model={:?} source={:?}",
                entry.folder, entry.lanes, fmt_when(&entry.when), entry.coordinator_model,
                entry.lanes_model, entry.source
            ),
            None => println!("scan:               (no entry for this row)"),
        }
        println!("  title:            {}", row.title);
        println!("  subtitle:         {}", row.subtitle);
        println!("  meta:             {}", row.meta);
        println!("  tooltip:          {}", row.tooltip);
        println!("  folder:           {}", row.folder);
        println!("  session_path:     {}", row.session_path);
    }
}

/// The store's entry as `session` wants it — the join the app does in
/// `app/src/launcher.rs::history_entries`, repeated here because the two crates
/// deliberately do not know about each other.
fn to_session_entry(entry: &store::history::HistoryEntry) -> HistoryEntry {
    HistoryEntry {
        session_path: entry.session.to_string_lossy().into_owned(),
        folder: entry.folder.to_string_lossy().into_owned(),
        when: match entry.when_epoch {
            Some(epoch) => When::Epoch(epoch as i64),
            None => When::Text(entry.when.clone()),
        },
        lanes: (entry.lanes > 0).then_some(entry.lanes),
        coordinator_model: entry.models.coordinator.clone(),
        lanes_model: entry.models.lanes.clone(),
        source: match entry.source {
            StoreSource::Scanned => HistorySource::Scan,
            StoreSource::Recent => HistorySource::Recent,
        },
        open_at_quit: entry.open_at_quit,
    }
}

fn fmt_when(when: &When) -> String {
    match when {
        When::Epoch(seconds) => format!("epoch {seconds}"),
        When::Text(text) => text.clone(),
    }
}

/// The system's current UTC offset in seconds east of UTC, from `localtime_r` —
/// the same source the app's rows use (`app/src/launcher.rs::utc_offset_seconds`).
fn utc_offset_seconds() -> i32 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut broken_down: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut broken_down).is_null() {
            return 0;
        }
        broken_down.tm_gmtoff as i32
    }
}
