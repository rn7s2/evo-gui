//! m4 — the coordinator dies: the swarm restarts it, the view resyncs once, and
//! nothing is doubled.
//!
//! Killing the coordinator's process (`/health`'s pid — the child under evo's
//! in-binary supervisor) drops its stream. The supervisor restarts it with
//! `--resume`; the tab's stream reconnects, meets a process it has never seen, and
//! replays from the start: `hello` (ids begin again at 1), then a fresh
//! `/transcript` + `/state` at a higher revision. The model must end up with the
//! conversation once — the resync is a rebuild, not an append.
//!
//! Run with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m4_coordinator_restart`.

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::{fixture, one_swarm, row_summary, signature, spec, Drive};
use session::RowKind;
use tab_engine::{Agent, Update};

#[test]
fn m4_coordinator_restart() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let mut drive = Drive::start(spec(&fixture, 1));
    let deadline = Instant::now() + Duration::from_secs(240);

    // --- a conversation, then a dead coordinator -----------------------------
    drive.wait_connected(Agent::Coordinator, deadline);
    drive.prompt(1, "m4 before the restart");
    drive.next(deadline, "the run settling", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, .. } if kind == "settled")
    });
    drive.wait_model(deadline, "the answer in the transcript", |model| {
        model.coordinator().rows().iter().any(|row| matches!(&row.kind, RowKind::User { text }
            if text.contains("m4 before the restart")))
    });

    let revision_before = drive
        .updates()
        .iter()
        .filter_map(|update| match update {
            Update::Transcript { agent: Agent::Coordinator, revision, .. } => Some(*revision),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let coordinator_pid = drive.coordinator_pid.expect("/health named the coordinator");
    let swarm_pid = drive.swarm_pid.expect("the engine named the swarm");
    assert_ne!(coordinator_pid, swarm_pid, "the coordinator is its own process");
    eprintln!("m4: killing the coordinator {coordinator_pid} (swarm {swarm_pid})");

    let killed = unsafe { libc::kill(coordinator_pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "could not kill the coordinator");

    // --- the stream drops and comes back ------------------------------------
    drive.next(deadline, "the stream reconnecting", |update| {
        matches!(update, Update::Stream { agent: Agent::Coordinator, status: tab_engine::StreamStatus::Reconnecting { .. } })
    });
    drive.next(deadline, "the stream back", |update| {
        matches!(update, Update::Stream { agent: Agent::Coordinator, status: tab_engine::StreamStatus::Connected })
    });
    // A process the tab has never seen announces itself: `hello`, ids from 1 again.
    let hello = drive.next(deadline, "hello from the restarted coordinator", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, id: Some(1), .. } if kind == "hello")
    });
    let new_pid = match &hello {
        Update::Event { data, .. } => data["pid"].as_u64(),
        _ => unreachable!(),
    };
    assert!(
        new_pid.is_some_and(|pid| pid as u32 != coordinator_pid),
        "the restarted coordinator is a new process: {hello:?}"
    );
    // ...and the view is refetched at a higher revision.
    drive.next(deadline, "a resynced transcript", |update| {
        matches!(update, Update::Transcript { agent: Agent::Coordinator, revision, .. } if *revision > revision_before)
    });

    // --- the resync is a rebuild, not a second copy -------------------------
    let rows = drive.model.coordinator().rows();
    let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
    let unique: BTreeSet<u64> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "row ids are unique: {ids:?}");
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "row ids are in order: {ids:?}");

    let signatures: Vec<String> = rows.iter().map(signature).collect();
    let unique: BTreeSet<&String> = signatures.iter().collect();
    assert_eq!(
        signatures.len(),
        unique.len(),
        "no row is listed twice: {}",
        row_summary(drive.model.coordinator()).join(" | ")
    );
    let before: Vec<&String> = signatures
        .iter()
        .filter(|line| line.starts_with("user:") && line.contains("m4 before the restart"))
        .collect();
    assert_eq!(before.len(), 1, "the turn before the crash is there exactly once");
    assert!(
        signatures.iter().any(|line| line.starts_with("assistant:")),
        "the answer survived the restart: {signatures:?}"
    );
    // The step the old process was in is not the new process's (§5).
    assert_eq!(drive.model.coordinator_step_started(), None, "the old step is gone");

    // --- and the tab still works --------------------------------------------
    let before = drive.cursor();
    drive.prompt(2, "m4 after the restart");
    drive.next(deadline, "the new run settling", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, .. } if kind == "settled")
    });
    let text: String = drive
        .events(Agent::Coordinator, "text-delta")
        .iter()
        .filter_map(|data| data.get("text").and_then(|t| t.as_str()))
        .collect();
    assert!(text.contains("m4 after the restart"), "the new turn streamed: {text:?}");
    assert!(drive.updates().len() > before, "the engine is still delivering updates");

    drive.wait_model(deadline, "both turns, once each", |model| {
        let rows = model.coordinator().rows();
        let first = rows
            .iter()
            .filter(|row| matches!(&row.kind, RowKind::User { text } if text.contains("m4 before the restart")))
            .count();
        let second = rows
            .iter()
            .filter(|row| matches!(&row.kind, RowKind::User { text } if text.contains("m4 after the restart")))
            .count();
        first == 1 && second == 1
    });

    // --- nothing left running ----------------------------------------------
    drive.shutdown();
    drive.next(deadline, "Exited", |update| matches!(update, Update::Exited { .. }));
    drive.join_and_assert_gone(deadline);
}
