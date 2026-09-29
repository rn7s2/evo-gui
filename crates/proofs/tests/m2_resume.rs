//! m2 — resume: a swarm that ran once is found by the history scan and comes back
//! with its conversation and its lanes.
//!
//! The chain proven here is the empty tab's: `store::history::scan` over the
//! sessions directory finds the journal, describes it (folder, lane count,
//! coordinator model) and hands back the `--resume` argument, which a new tab
//! starts with. What comes back is a real swarm reading its own journal.
//!
//! Run with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m2_resume`.

mod common;

use std::time::{Duration, Instant};

use common::{fixture, one_swarm, row_summary, spec, Drive};
use session::RowKind;
use store::history::{self, HistorySource, ScanBudget};
use swarm_client::harness::STUB_MODEL;
use tab_engine::{Agent, TabSpec, Update};

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

    // --- resume it, with no --workers ---------------------------------------
    // A resumed swarm keeps the lane count its record has: the tab passes
    // `--resume` alone, exactly as (§7.2) does.
    let tab_dir = fixture.dir.join("resumed-tab");
    std::fs::create_dir_all(&tab_dir).expect("the resumed tab's directory");
    let mut resumed = TabSpec::new(&fixture.bins.swarm, &entry.folder, &tab_dir)
        .with_agent_bin(&fixture.bins.agent)
        .with_resume(entry.session.clone());
    for (key, value) in fixture.env() {
        resumed = resumed.with_env(key, value);
    }
    for key in fixture.env_remove() {
        resumed = resumed.with_env_removed(key);
    }
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
