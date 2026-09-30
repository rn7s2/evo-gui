//! t03 — the swarm delegates, and the lanes stream: the coordinator hands lane 1
//! a task, the lane's own topic shows it working, and what the lane says comes
//! back to the coordinator as an item.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t03_delegate_and_lanes
//! ```
//!
//! This one is a swarm: lanes are the subject. What it proves is §4.3 and §7.3 —
//! every lane transition is published on the `swarm` topic, the lane's own items
//! arrive on `lane:<n>` in the coordinator's own stream, and the report comes back
//! as a `lane_report` item rather than as a sentence in a transcript.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, lane_state, lane_states, send, snapshot, wait_for, Watcher};
use serde_json::json;
use store::launch::Program;

const WORKERS: u16 = 2;

/// The lane's whole world is the task text: it cannot see the coordinator's
/// conversation. `DELAY3` keeps it working long enough for the transitions to be
/// observed, and `CALL todo` gives it something that shows up in its own items.
fn lane_task() -> String {
    format!(
        "DELAY3 CALL todo {}",
        json!({ "items": [
            { "text": "t03 lane one step", "status": "in-progress" },
            { "text": "t03 lane one rest", "status": "pending" },
        ]})
    )
}

#[test]
fn t03_delegate_and_lanes() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t03");
    let mut server = fixture.spawn(&fixture.spec(Program::Swarm, WORKERS));
    let client = server.client().clone();

    // --- the swarm says who it is, and what lanes it has ------------------------
    let health = client.get_json("/health").expect("health");
    assert_eq!(health["program"], json!("evo-swarm"));
    let seeded = snapshot(&client, &["session", "swarm", "lane:1", "lane:2"], 200);
    assert_eq!(
        seeded["topics"]["swarm"]["state"]["workers"].as_u64(),
        Some(u64::from(WORKERS)),
        "the swarm topic names the pool it runs: {seeded}"
    );
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().unwrap(),
    );
    let mut watcher = Watcher::start(&client, &["session", "swarm", "lane:*"], Some(cursor));

    // --- every lane is published, up and idle (§4.3) ----------------------------
    watcher.wait_mirror(deadline, "the lanes up and idle", |mirror| {
        let lanes = lane_states(mirror.state("swarm"));
        lanes.len() == usize::from(WORKERS) && lanes.iter().all(|(_, state)| state == "idle")
    });
    println!(
        "{NOTE} lanes after {:?}: {:?}",
        started.elapsed(),
        lane_states(watcher.mirror.state("swarm"))
    );

    // --- the coordinator delegates ---------------------------------------------
    let task = lane_task();
    let reply = send(
        &client,
        &format!("CALL delegate {}", json!({ "lane": 1, "task": task })),
    );
    assert_eq!(reply["blocked"], json!(null), "{reply}");

    // The lane works: the swarm topic says so, and the lane's own topic has the
    // task it was given.
    watcher.wait_mirror(deadline, "lane 1 working", |mirror| {
        lane_state(mirror.state("swarm"), 1).as_deref() == Some("working")
    });
    watcher.wait_mirror(deadline, "the lane's own task item", |mirror| {
        mirror
            .items("lane:1")
            .iter()
            .any(|item| item["kind"] == "user" && item["text"].as_str() == Some(task.as_str()))
    });

    // The lane's own work, on its own topic: a tool item for the todo call and the
    // checklist the call left behind.
    watcher.wait_mirror(deadline, "the lane's todo item", |mirror| {
        mirror
            .items("lane:1")
            .iter()
            .any(|item| item["kind"] == "tool" && item["name"] == json!("todo"))
    });
    // The checklist is a later `state.patch` on that topic than the item that
    // wrote it, so this waits for it rather than reading what happens to be there.
    watcher.wait_mirror(deadline, "the lane's checklist", |mirror| {
        mirror
            .state("lane:1")
            .get("todos")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|todos| todos.len() == 2)
    });
    let todos = watcher
        .mirror
        .state("lane:1")
        .get("todos")
        .cloned()
        .unwrap_or(json!(null));
    println!("{NOTE} lane 1's todos: {todos}");

    // And it finishes: the whole cycle is on the swarm topic, not just the end.
    watcher.wait_mirror(deadline, "lane 1 idle again", |mirror| {
        lane_state(mirror.state("swarm"), 1).as_deref() == Some("idle")
    });
    let states = watcher
        .mirror
        .state("swarm")
        .get("lanes")
        .cloned()
        .unwrap_or(json!(null));
    println!("{NOTE} lanes after the task: {states}");

    // --- what the lane said comes back to the coordinator ------------------------
    //
    // A lane reports by *calling the `report` tool*: that item is the only channel
    // (§4.1), so the second delegation is the one that makes the lane report.
    let asked = send(
        &client,
        &format!(
            "CALL delegate {}",
            json!({ "lane": 1, "task": format!("CALL report {}", json!({
                "done": "t03 the lane's own words",
                "evidence": "crates/proofs/tests/t03_delegate_and_lanes.rs",
                "next": "nothing",
            })) })
        ),
    );
    assert_eq!(asked["blocked"], json!(null), "{asked}");

    // §4.1's `lane_report`: fields, not prose — a GUI renders it, it does not read
    // `[lane 1 report]` out of a sentence.
    watcher.wait_mirror(deadline, "the lane's report", |mirror| {
        mirror.last_of_kind("session", "lane_report").is_some()
    });
    let report = watcher
        .mirror
        .last_of_kind("session", "lane_report")
        .cloned()
        .expect("the report");
    assert_eq!(report["lane"].as_u64(), Some(1));
    for field in ["done", "evidence", "next", "blocked", "requests", "goal"] {
        assert!(
            report.get(field).is_some(),
            "a lane_report carries `{field}`: {report}"
        );
    }
    println!(
        "{NOTE} the report after {:?}: {}",
        started.elapsed(),
        report["done"].as_str().unwrap_or_default()
    );
    assert_eq!(
        report["done"].as_str(),
        Some("t03 the lane's own words"),
        "the lane's own words, as data: {report}"
    );
    assert_eq!(
        watcher.mirror.state("swarm")["lanes"][0]["reports"].as_u64(),
        Some(1),
        "and the swarm counts it: {:?}",
        watcher.mirror.state("swarm")["lanes"]
    );

    // --- and the coordinator is only held while the lanes work (§4.2) ------------
    // The turn that asked for the report is still running when the report lands:
    // this waits for it to settle rather than reading whatever it was doing.
    watcher.wait_mirror(deadline, "the coordinator to settle", |mirror| {
        matches!(
            mirror.state("session")["status"].as_str(),
            Some("idle") | Some("waiting")
        )
    });
    let status = watcher.mirror.state("session")["status"].clone();
    assert!(
        matches!(status.as_str(), Some("idle") | Some("waiting")),
        "the coordinator ends idle, or held while its lanes work: {status}"
    );

    let stopped = server.shutdown().expect("the ladder ran");
    println!(
        "{NOTE} shutdown: {stopped:?} (t03 in {:?})",
        started.elapsed()
    );
    wait_for(deadline, "nothing left", || {
        (!swarm_client::process_alive(server.pid())).then_some(())
    });
}
