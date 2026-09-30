//! t01 — launch → ready: one real `evo-swarm serve` in a chosen folder comes up,
//! says where it is in its ready file, and answers the snapshot a tab opens on.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t01_launch_ready -- --nocapture
//! ```
//!
//! What is proven here is the whole of §1 and §8's launch: `store::launch` built
//! the argv, `swarm_client` held the pipe and read the ready file, and there is
//! no port to pick, no token file to wait for and no `/health` poll anywhere in
//! the proof — because none of those exists any more. The session it starts is
//! under the fixture's own home, so nothing of the machine is touched.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, wait_for};
use store::launch::Program;
use swarm_client::{process_alive, ReadyFile};

#[test]
fn t01_launch_ready() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t01");

    // --- up, and the ready file says where -------------------------------------
    let mut server = fixture.spawn(&fixture.spec(Program::Swarm, 2));
    let ready: ReadyFile = server.ready().clone();
    assert_eq!(ready.program, "evo-swarm", "{ready:?}");
    assert!(ready.port > 0, "the child picked its own port: {ready:?}");
    assert_eq!(ready.url, format!("http://127.0.0.1:{}/", ready.port));
    assert_eq!(ready.token.len(), 64, "a 64-hex token, not logged here");
    assert_eq!(ready.restarts, 0);
    assert!(
        ready
            .session
            .path
            .starts_with(fixture.home.to_string_lossy().as_ref()),
        "the journal is under this fixture's home: {}",
        ready.session.path
    );
    println!("{NOTE} ready after {:?}: {ready:?}", started.elapsed());

    let client = server.client().clone();

    // --- the two reads a tab opens on (§4, §5.2) --------------------------------
    let health = client.get_json("/health").expect("health");
    assert_eq!(health["ok"], serde_json::json!(true));
    assert_eq!(health["epoch"], serde_json::json!(ready.epoch));
    assert_eq!(health["pid"], serde_json::json!(ready.pid));

    let snapshot = proofs::watch::snapshot(&client, &["session", "swarm"], 200);
    assert_eq!(
        snapshot["epoch"].as_str(),
        Some(ready.epoch.as_str()),
        "the snapshot is the ready file's own process epoch: {snapshot}"
    );
    let seq = snapshot["seq"].as_u64().expect("a seq");
    let session = &snapshot["topics"]["session"];
    assert!(
        session["state"].is_object(),
        "the session topic has a state: {session}"
    );
    assert!(
        session["items"].is_array(),
        "and a list of items: {session}"
    );
    println!(
        "{NOTE} snapshot at {}.{seq}: session status {:?}, {} item(s)",
        ready.epoch,
        session["state"]["status"],
        session["items"].as_array().map_or(0, Vec::len)
    );

    // --- one stream, from the snapshot's cursor (§5.3) --------------------------
    let cursor = snapshot["seq"]
        .as_u64()
        .map(|seq| swarm_client::Cursor::new(ready.epoch.clone(), seq));
    let mut watcher = proofs::watch::Watcher::start(&client, &["session", "swarm"], cursor.clone());
    let hello = watcher.wait_frame(deadline, "the hello frame", |frame| frame.is_hello());
    assert_eq!(
        hello.get("epoch").and_then(serde_json::Value::as_str),
        Some(ready.epoch.as_str()),
        "hello names the epoch a client forgets a cursor from: {:?}",
        hello.data
    );

    // --- and it stops when the pipe goes (§8) -----------------------------------
    let stopped = server.shutdown().expect("the ladder ran");
    println!("{NOTE} shutdown: {:?}", stopped);
    assert!(
        stopped.exit_code.is_some() || stopped.outcome != swarm_client::ShutdownOutcome::Killed,
        "it left without being killed: {stopped:?}"
    );
    let pid = ready.pid;
    wait_for(deadline, "the child to be gone", || {
        (!process_alive(pid)).then_some(())
    });
    println!("{NOTE} t01 done in {:?}", started.elapsed());
}
