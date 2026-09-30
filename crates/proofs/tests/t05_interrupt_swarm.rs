//! t05 — a person stops the swarm: `run.interrupt` with scope `swarm` stops the
//! coordinator and its lanes, tells the coordinator what happened, and leaves the
//! lanes idle rather than working.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t05_interrupt_swarm
//! ```
//!
//! §7.4's narrow opening: a human may stop work, never redirect it. The one
//! action is `run.interrupt` — and the coordinator stays the only party that
//! directs lanes, so the interrupt is *also* an item on the coordinator's topic
//! (`human_action`, §4.1) rather than something that happens behind its back.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, lane_state, send, snapshot, try_op, Watcher};
use serde_json::json;
use store::launch::Program;

const WORKERS: u16 = 1;

#[test]
fn t05_interrupt_swarm() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t05");
    let mut server = fixture.spawn(&fixture.spec(Program::Swarm, WORKERS));
    let client = server.client().clone();

    let seeded = snapshot(&client, &["session", "swarm", "lane:*"], 200);
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().unwrap(),
    );
    let mut watcher = Watcher::start(&client, &["session", "swarm", "lane:*"], Some(cursor));
    watcher.reseed(&seeded);
    watcher.wait_mirror(deadline, "the lane up and idle", |mirror| {
        lane_state(mirror.state("swarm"), 1).as_deref() == Some("idle")
    });

    // --- work the swarm is in the middle of ------------------------------------
    // The lane is given ten seconds of work; the coordinator is held while it
    // works (`waiting`, §4.2).
    let task = format!(
        "DELAY10 CALL todo {}",
        json!({ "items": [{ "text": "t05 keep working", "status": "in-progress" }] })
    );
    send(
        &client,
        &format!("CALL delegate {}", json!({ "lane": 1, "task": task })),
    );
    watcher.wait_mirror(deadline, "lane 1 working", |mirror| {
        lane_state(mirror.state("swarm"), 1).as_deref() == Some("working")
    });
    println!("{NOTE} the swarm is busy after {:?}", started.elapsed());

    // --- the one human action ---------------------------------------------------
    let reply = try_op(&client, "run.interrupt", json!({ "scope": "swarm" }));
    assert_eq!(reply["ok"], json!(true), "{reply}");
    let interrupted: Vec<String> = reply["result"]["interrupted"]
        .as_array()
        .map(|topics| {
            topics
                .iter()
                .filter_map(|topic| topic.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        interrupted.iter().any(|topic| topic == "session")
            || interrupted.iter().any(|topic| topic.starts_with("lane:")),
        "the reply names what it stopped: {reply}"
    );
    println!("{NOTE} interrupted {interrupted:?}");

    // --- the coordinator is told, and the lanes settle --------------------------
    watcher.wait_mirror(
        deadline,
        "the interrupt on the coordinator's topic",
        |mirror| {
            mirror
                .last_of_kind("session", "human_action")
                .and_then(|item| item["action"].as_str())
                == Some("interrupt")
        },
    );
    let action = watcher
        .mirror
        .last_of_kind("session", "human_action")
        .cloned()
        .expect("the action");
    assert_eq!(
        action["lanes"].as_array().map(Vec::len),
        Some(usize::from(WORKERS)),
        "the item names the lanes it touched: {action}"
    );

    watcher.wait_mirror(deadline, "the lane stopped", |mirror| {
        matches!(
            lane_state(mirror.state("swarm"), 1).as_deref(),
            Some("idle") | Some("stopped") | Some("down")
        )
    });
    watcher.wait_mirror(deadline, "the swarm quiescent", |mirror| {
        matches!(
            mirror.state("session")["status"].as_str(),
            Some("idle") | Some("waiting")
        )
    });
    println!(
        "{NOTE} after the interrupt: lanes {:?}, status {:?} (t05 in {:?})",
        proofs::watch::lane_states(watcher.mirror.state("swarm")),
        watcher.mirror.state("session")["status"],
        started.elapsed()
    );

    let stopped = server.shutdown().expect("the ladder ran");
    println!("{NOTE} shutdown: {stopped:?}");
}
