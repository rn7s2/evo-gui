//! t06 — history and resume: a session the app's own history lists is a session
//! the app can come back to.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t06_history_resume
//! ```
//!
//! The history list is `evo-agent sessions --json` (§2, §9): one process, one
//! JSON document, no journal is read or parsed here. And resume is
//! `--resume <that exact path>` (§1) — the same path the ready file named before,
//! which is what makes the turn the person had already had come back with it.

use std::path::Path;
use std::time::Instant;

use proofs::fixture::{same_path, Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, send, snapshot, wait_for, Watcher};
use store::history::{self, SessionsQuery};
use store::launch::Program;

/// A tab's server is a swarm; what this proof is about is the index and the
/// resume of one session, which the agent records the same way.
const PROGRAM: Program = Program::Agent;

const TURN: &str = "t06 the turn that has to come back";

#[test]
fn t06_history_resume() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t06");
    // The offline CLIs run with *this* process's environment, so the index they
    // write and read is the fixture's.
    fixture.enter();

    // --- a session, and a turn in it -------------------------------------------
    let mut server = fixture.spawn(&fixture.spec(PROGRAM, 0));
    let path = server.ready().session.path.clone();
    let client = server.client().clone();
    let seeded = snapshot(&client, &["session"], 200);
    let mut watcher = Watcher::start(
        &client,
        &["session"],
        Some(swarm_client::Cursor::new(
            server.ready().epoch.clone(),
            seeded["seq"].as_u64().unwrap(),
        )),
    );
    watcher.reseed(&seeded);
    send(&client, TURN);
    watcher.wait_mirror(deadline, "the answer", |mirror| {
        mirror
            .last_of_kind("session", "assistant")
            .and_then(|item| item["status"].as_str())
            == Some("final")
    });
    let stopped = server.shutdown().expect("the ladder ran");
    println!("{NOTE} stopped {path} ({stopped:?})");

    // --- the index lists it -----------------------------------------------------
    // No `--program evo-swarm` here: this proof's session is a lone agent's, and
    // the empty tab's own query is `SessionsQuery::swarms()`.
    let query = SessionsQuery {
        all: true,
        cwd: None,
        program: None,
    };
    let entries = history::load(&fixture.root, &fixture.bins.agent, &query)
        .expect("evo-agent sessions --json answered");
    let listed = entries
        .iter()
        .find(|entry| same_path(&entry.session, Path::new(&path)));
    let listed = listed.unwrap_or_else(|| {
        panic!(
            "{path} is not in the index: {:?}",
            entries
                .iter()
                .map(|entry| entry.session.display().to_string())
                .collect::<Vec<_>>()
        )
    });
    assert!(
        listed.when_epoch.is_some(),
        "the row has a time to sort and phrase by: {listed:?}"
    );
    println!(
        "{NOTE} the index row after {:?}: {} ({})",
        started.elapsed(),
        listed.label(),
        listed.when
    );

    // --- and resuming it brings the turn back ------------------------------------
    let mut resumed = fixture.spawn(&fixture.resume_spec(PROGRAM, Path::new(&path)));
    assert!(
        same_path(Path::new(&resumed.ready().session.path), Path::new(&path)),
        "the resumed server serves the session it was given: {:?}",
        resumed.ready().session
    );
    let client = resumed.client().clone();
    let seeded = snapshot(&client, &["session"], 200);
    let mut watcher = Watcher::start(
        &client,
        &["session"],
        Some(swarm_client::Cursor::new(
            resumed.ready().epoch.clone(),
            seeded["seq"].as_u64().unwrap(),
        )),
    );
    watcher.reseed(&seeded);
    assert!(
        watcher
            .mirror
            .items("session")
            .iter()
            .any(|item| item["kind"] == "user" && item["text"].as_str() == Some(TURN)),
        "the turn the person had is back: {:?}",
        watcher.mirror.kinds("session")
    );

    let stopped = resumed.shutdown().expect("the ladder ran");
    let pid = resumed.ready().pid;
    wait_for(deadline, "the resumed server gone", || {
        (!swarm_client::process_alive(pid)).then_some(())
    });
    println!(
        "{NOTE} resumed and stopped {stopped:?} (t06 in {:?})",
        started.elapsed()
    );
}
