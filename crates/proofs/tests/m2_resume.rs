//! m2 — resume: a swarm that ran once is found by the history scan and comes back
//! with its conversation and its lanes.
//!
//! The chain proven here is the empty tab's: `store::history::scan` over the
//! sessions directory finds the journal, describes it (folder, lane count,
//! coordinator model) and hands back the `--resume` argument, which a new tab
//! starts with. What comes back is a real swarm reading its own journal.
//!
//! The last step is the app's own list: `app.json`'s recents are merged over the
//! scan, and a session that was still open when the app last quit comes first even
//! though a newer swarm's journal sits above it in the scan (§9.5).
//!
//! Run with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m2_resume`.

mod common;

use std::time::{Duration, Instant};

use common::{fixture, one_swarm, row_summary, spec, spec_at, Drive};
use session::RowKind;
use store::history::{self, HistorySource, ScanBudget};
use store::{AppState, Recent, Root};
use swarm_client::harness::{Fixture, STUB_MODEL};
use tab_engine::{Agent, Update};

#[test]
fn m2_resume() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let deadline = Instant::now() + Duration::from_secs(240);

    // --- a swarm in a temp folder, one prompt, then down ---------------------
    let first = {
        let mut drive = Drive::start(spec(&fixture, 2));
        drive.wait_connected(Agent::Coordinator, deadline);
        drive.wait_lanes_idle(deadline, 2);
        drive.prompt(1, "m2 resume works");
        drive.next(deadline, "the run settling", |update| {
            matches!(update, Update::Event { agent: Agent::Coordinator, kind, .. } if kind == "settled")
        });
        drive.wait_model(deadline, "the coordinator idle", |model| {
            model.activity() == session::Activity::Idle
        });

        drive.shutdown();
        drive.next(deadline, "Exited", |update| matches!(update, Update::Exited { .. }));
        let session = drive.session.clone().expect("/state named the session");
        let pids = drive.pids().to_vec();
        drive.join_and_assert_gone(deadline);
        (session, pids)
    };
    let (session_path, first_pids) = first;
    assert!(
        first_pids.iter().all(|pid| !common::process_alive(*pid)),
        "the first swarm is gone: {first_pids:?}"
    );
    eprintln!("m2: the swarm wrote {session_path}");

    // --- the history scan finds it ------------------------------------------
    // The sessions directory the scan walks is the temp HOME's — the same one the
    // swarm just wrote to.
    let sessions_dir = fixture.home.join("sessions");
    assert!(sessions_dir.is_dir(), "{} should exist", sessions_dir.display());
    let outcome = history::scan(&sessions_dir, &ScanBudget::default());
    assert!(outcome.files_read >= 1, "the scan read nothing: {outcome:?}");

    let entry = outcome
        .entries
        .iter()
        .find(|entry| entry.session.to_string_lossy() == session_path)
        .unwrap_or_else(|| {
            panic!(
                "the session was not listed as resumable: {:?}",
                outcome.entries.iter().map(|e| e.session.clone()).collect::<Vec<_>>()
            )
        });
    assert_eq!(entry.source, HistorySource::Scanned, "found by the walk, not remembered");
    assert_eq!(entry.lanes, 2, "the record's lane count: {entry:?}");
    assert_eq!(entry.workers, 2, "the record's workers: {entry:?}");
    assert_eq!(
        entry.models.coordinator.as_deref(),
        Some(STUB_MODEL),
        "the coordinator's model, from its last model-change: {entry:?}"
    );
    // The folder the swarm ran in — as the swarm recorded it, which on macOS is the
    // resolved `/private/var/...` of the temp project it was started in.
    assert_eq!(
        std::fs::canonicalize(&entry.folder).expect("the folder exists"),
        std::fs::canonicalize(&fixture.project).expect("the project exists"),
        "the folder the swarm ran in: {entry:?}"
    );
    assert!(!entry.swarm_id.is_empty(), "the swarm id: {entry:?}");
    let (folder, resume_session, lanes) = entry.resume_args();
    assert_eq!((resume_session, lanes), (session_path.clone().into(), 2));
    // A resume starts in the folder the record names; a tab that had only the
    // history row would use exactly this.
    assert_eq!(
        std::fs::canonicalize(&folder).expect("the folder exists"),
        std::fs::canonicalize(&fixture.project).expect("the project exists")
    );
    eprintln!("m2: history row {:?} {}", entry.folder_name(), entry.when);

    // --- a second, newer swarm ----------------------------------------------
    // So the flag has something to win against: the scan's rule is newest first,
    // and this journal is written after the one we mean to flag.
    run_once(&fixture, "second-swarm", 1, "m2 the newer swarm", deadline);
    let scanned = history::scan(&sessions_dir, &ScanBudget::default());
    assert_eq!(scanned.entries.len(), 2, "both journals: {:?}", scanned.entries);
    let newer = scanned
        .entries
        .iter()
        .find(|entry| entry.session.to_string_lossy() != session_path)
        .expect("the second swarm's row")
        .clone();
    assert!(
        newer.mtime > entry.mtime,
        "the second swarm's journal is newer (so the scan alone would show it first): \
         {} vs {}",
        newer.mtime,
        entry.mtime
    );
    assert_eq!(
        scanned.entries[0].session, newer.session,
        "newest first is what the scan does on its own"
    );

    // --- the app's recents: what was open at the last quit comes first -------
    // The desktop's own `app.json`, in a root of its own (the fixture's temp
    // directory), holding one recent: the session that was still open when the
    // app last quit, as the app records it — the row as the scan described it,
    // plus the flag only the app can set.
    let desktop = Root::at(fixture.dir.join("desktop"));
    let mut state = AppState::default();
    let mut recent = Recent::new(&entry.session, &entry.folder, 2).open_at_quit();
    recent.when = entry.when.clone();
    state.touch_recent(recent);
    state.save(&desktop).expect("app.json is written");

    let merged = history::load_history(&desktop, &sessions_dir, &ScanBudget::default());
    assert_eq!(merged.len(), 2, "the scan and the recents, deduped: {merged:?}");
    assert_eq!(
        merged[0].session,
        std::path::PathBuf::from(&session_path),
        "the session that was open at the last quit is first: {:?}",
        merged.iter().map(|e| (&e.session, e.open_at_quit, e.mtime)).collect::<Vec<_>>()
    );
    assert!(merged[0].open_at_quit, "and it carries the flag: {:?}", merged[0]);
    assert_eq!(
        merged[0].source,
        HistorySource::Scanned,
        "the scan knew the swarm; the app only added the flag: {:?}",
        merged[0]
    );
    assert_eq!(merged[0].lanes, 2, "the scan's own row, not a stub: {:?}", merged[0]);
    assert!(
        merged[0].mtime < merged[1].mtime,
        "the flagged row is the *older* one — the flag is what moved it up"
    );
    assert_eq!(merged[1].session, newer.session);
    assert!(!merged[1].open_at_quit, "only the tab that was open carries it");
    // What the round trip through app.json really said.
    let stored = AppState::load(&desktop);
    assert!(
        stored.recent_for(std::path::Path::new(&session_path)).is_some_and(|recent| recent.open_at_quit),
        "the flag survived the file: {:?}",
        stored.recents
    );
    eprintln!(
        "m2: app.json puts {} first (open at last quit, mtime {}), above {} (mtime {})",
        merged[0].session.display(),
        merged[0].mtime,
        merged[1].session.display(),
        merged[1].mtime
    );
    let entry = &merged[0];

    // --- resume it, with no --workers ---------------------------------------
    // A resumed swarm keeps the lane count its record has: the tab passes
    // `--resume` alone, exactly as (§7.2) does.
    let tab_dir = fixture.dir.join("resumed-tab");
    std::fs::create_dir_all(&tab_dir).expect("the resumed tab's directory");
    let resumed = spec_at(&fixture, &tab_dir).with_resume(entry.session.clone());
    let mut drive = Drive::start(resumed);

    // The earlier conversation is back: the first transcript carries the prompt.
    drive.wait_connected(Agent::Coordinator, deadline);
    drive.wait_model(deadline, "the resumed transcript", |model| {
        model.coordinator().rows().iter().any(|row| matches!(&row.kind, RowKind::User { text }
            if text.contains("m2 resume works")))
    });
    let rows = row_summary(drive.model.coordinator());
    assert!(
        rows.iter().any(|row| row.starts_with("assistant:")),
        "the earlier answer came back too: {rows:?}"
    );

    // Its lanes came back with it, and its own session is the one resumed.
    drive.wait_lanes_idle(deadline, 2);
    assert_eq!(
        drive.session.as_deref(),
        Some(session_path.as_str()),
        "a resume keeps writing to the same session"
    );

    // --- and it still works --------------------------------------------------
    drive.prompt(1, "m2 second turn");
    // The stub answers anything it was not told to do otherwise with
    // `ok: <the first 40 chars of the turn>`, so the reply names the new turn.
    drive.wait_for_update(deadline, "the answer to the second turn", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, data, .. }
            if kind == "text-delta" && data["text"].as_str().is_some_and(|text| text.contains("m2 second turn")))
    });
    let text: String = drive
        .events(Agent::Coordinator, "text-delta")
        .iter()
        .filter_map(|data| data.get("text").and_then(|t| t.as_str()))
        .collect();
    assert!(text.contains("m2 second turn"), "the new turn streamed: {text:?}");

    // The transcript after the resync carries both turns.
    drive.wait_model(deadline, "both turns in the transcript", |model| {
        let users = model
            .coordinator()
            .rows()
            .iter()
            .filter(|row| matches!(&row.kind, RowKind::User { text } if text.contains("m2 ")))
            .count();
        users >= 2
    });

    // --- nothing left running ----------------------------------------------
    drive.shutdown();
    drive.next(deadline, "Exited", |update| matches!(update, Update::Exited { .. }));
    drive.join_and_assert_gone(deadline);
}

/// One prompt in a swarm of this fixture's own, taken down again — the point is the
/// journal it leaves behind and how new that is. No lane is waited for: the
/// coordinator answers the prompt whether or not a lane is up, and this swarm is
/// here to be a *newer* row in the scan.
fn run_once(fixture: &Fixture, name: &str, workers: u16, prompt: &str, deadline: Instant) {
    let tab_dir = fixture.dir.join(name);
    std::fs::create_dir_all(&tab_dir).expect("the swarm's tab directory");
    let mut drive = Drive::start(spec_at(fixture, &tab_dir).with_workers(workers));
    drive.wait_connected(Agent::Coordinator, deadline);
    drive.prompt(1, prompt);
    drive.next(deadline, "the run settling", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, .. } if kind == "settled")
    });
    drive.wait_model(deadline, "the coordinator idle", |model| {
        model.activity() == session::Activity::Idle
    });
    drive.shutdown();
    drive.next(deadline, "Exited", |update| matches!(update, Update::Exited { .. }));
    drive.join_and_assert_gone(deadline);
}
