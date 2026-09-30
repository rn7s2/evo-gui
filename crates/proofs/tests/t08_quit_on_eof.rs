//! t08 — quitting reaches the server: closing the pipe a tab was started with is
//! what ends the swarm, and it has to work after a run, not only while idle.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t08_quit_on_eof
//! ```
//!
//! §8's first rung is the pipe, and the app's quit is only the pipe: `⌘Q` writes
//! `app.json`, closes each tab's stdin and exits — no signal, no pid, no waiting
//! (`docs/usage.md`, "Starting, and the second launch"). So a server that stops
//! hearing EOF is a server nothing stops: the app has exited, and the client that
//! would have escalated to `SIGTERM` went with it.
//!
//! The proof asks for the one thing the app needs — the child is gone, on its own,
//! a few seconds after its pipe closes — in both states a tab is quit from: one
//! that has never been used, and one that has answered a prompt.

use std::time::{Duration, Instant};

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, send, snapshot, Watcher};
use store::launch::Program;

/// A tab's server is a swarm; what this proof is about is the pipe, which the
/// agent a swarm is built on reads the same way.
const PROGRAM: Program = Program::Agent;

/// How long a quit may take before the tab has outlived the app: the pipe is
/// closed, so nothing but the child's own loop is left to notice.
const QUIT_WAIT: Duration = Duration::from_secs(10);

#[test]
fn t08_quit_on_eof() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);

    // --- a tab nothing has happened in yet -------------------------------------
    let fixture = Fixture::new("t08-idle");
    let mut server = fixture.spawn(&fixture.spec(PROGRAM, 0));
    server.close_stdin();
    let exit = server.wait_for_exit(QUIT_WAIT);
    println!("{NOTE} idle: exit on EOF alone = {exit:?}");
    assert!(
        exit.is_some(),
        "a server that has served nothing exits when its pipe closes (waited {QUIT_WAIT:?})"
    );

    // --- a tab that has had a turn, which is the tab a person quits ------------
    let fixture = Fixture::new("t08-used");
    let mut server = fixture.spawn(&fixture.spec(PROGRAM, 0));
    let client = server.client().clone();
    let seeded = snapshot(&client, &["session"], 200);
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().expect("a cursor"),
    );
    let mut watcher = Watcher::start(&client, &["session"], Some(cursor));
    send(&client, "ok: t08 a turn to quit after");
    watcher.wait_mirror(deadline, "the answer", |mirror| {
        mirror
            .last_of_kind("session", "assistant")
            .and_then(|item| item["status"].as_str())
            == Some("final")
    });

    server.close_stdin();
    let exit = server.wait_for_exit(QUIT_WAIT);
    println!("{NOTE} after a run: exit on EOF alone = {exit:?}");
    assert!(
        exit.is_some(),
        "a server that has answered still exits when its pipe closes — this is the \
         only thing a quit does (waited {QUIT_WAIT:?}, then gave up)"
    );
    println!("{NOTE} t08 done in {:?}", started.elapsed());
}
