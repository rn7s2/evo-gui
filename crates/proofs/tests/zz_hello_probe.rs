//! Throwaway: what does the watcher see first on a swarm's stream?

use std::time::Duration;

use proofs::fixture::{Fixture, NOTE};
use proofs::watch::{snapshot, Watcher};
use store::launch::Program;

#[test]
fn zz_hello_probe() {
    let fixture = Fixture::new("zz-hello");
    let mut server = fixture.spawn(&fixture.spec(Program::Swarm, 2));
    let client = server.client().clone();
    println!("{NOTE} ready: {:?}", server.ready().epoch);

    let snapshot = snapshot(&client, &["session", "swarm"], 200);
    let seq = snapshot["seq"].as_u64().expect("a seq");
    println!("{NOTE} snapshot at {}.{seq}", server.ready().epoch);

    let cursor = swarm_client::Cursor::new(server.ready().epoch.clone(), seq);
    let mut watcher = Watcher::start(&client, &["session", "swarm"], Some(cursor));
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        watcher.pump();
        std::thread::sleep(Duration::from_millis(50));
    }
    let ops: Vec<&str> = watcher.frames.iter().map(|f| f.op.as_str()).collect();
    println!("{NOTE} ops seen: {ops:?}");
    let hello = watcher.frames.iter().find(|f| f.is_hello()).cloned();
    println!("{NOTE} hello: {hello:?}");
    let _ = server.shutdown();
}
