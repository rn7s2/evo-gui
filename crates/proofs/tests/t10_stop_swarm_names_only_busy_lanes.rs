//! t10 — Stop swarm names the lanes it stopped, and only them: a lane that was
//! idle is not one of them.
//!
//! ```sh
//! EVO_SWARM_BIN=/tmp/evo-agent-fix/build/evo-swarm \
//! EVO_AGENT_BIN=/tmp/evo-agent-fix/build/evo-agent \
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs \
//!   --test t10_stop_swarm_names_only_busy_lanes -- --nocapture
//! ```
//!
//! t05 stops a swarm whose only lane is at work, so every lane it names is one it
//! stopped and the bug this proof exists for cannot show there. Here two lanes
//! run and one of them works: `run.interrupt` with scope `swarm` must answer
//! `lane:1` alone, and the coordinator must read "stopped lane 1", not "stopped
//! lanes 1, 2".
//!
//! The cause it guards: a lane's `run.interrupt` always answers
//! `{interrupted: [...]}` (CONTRACT §5.5), `[]` for an idle lane, and an empty
//! array is a vector — non-NIL — so a swarm that asked `(getf result :interrupted)`
//! counted every lane as stopped. Against the pre-fix binaries this proof fails
//! naming both lanes.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, lane_state, lane_states, send, snapshot, try_op, Watcher};
use serde_json::{json, Value};
use store::launch::Program;

/// The lanes the swarm runs, and which of them is given work.
const WORKERS: u16 = 2;
const BUSY: u64 = 1;
const IDLE: u64 = 2;

/// The note the coordinator reads when a human stops lane work.
const NOTE_START: &str = "[human] stopped";

#[test]
fn t10_stop_swarm_names_only_busy_lanes() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t10");
    println!(
        "{NOTE} swarm {}, lanes {}",
        fixture.bins.swarm.display(),
        fixture.bins.agent.display()
    );
    let mut server = fixture.spawn(&fixture.spec(Program::Swarm, WORKERS));
    let client = server.client().clone();

    let seeded = snapshot(&client, &["session", "swarm", "lane:*"], 200);
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().unwrap(),
    );
    let mut watcher = Watcher::start(&client, &["session", "swarm", "lane:*"], Some(cursor));
    watcher.reseed(&seeded);
    watcher.wait_mirror(deadline, "both lanes up and idle", |mirror| {
        let lanes = lane_states(mirror.state("swarm"));
        lanes.len() == usize::from(WORKERS) && lanes.iter().all(|(_, state)| state == "idle")
    });

    // --- one lane works, the other does not ------------------------------------
    // t05's ten seconds of work, handed to lane 1 alone: `DELAY10` keeps it busy
    // while the human presses Stop, and lane 2 is never delegated to.
    let task = format!(
        "DELAY10 CALL todo {}",
        json!({ "items": [{ "text": "t10 keep working", "status": "in-progress" }] })
    );
    send(
        &client,
        &format!("CALL delegate {}", json!({ "lane": BUSY, "task": task })),
    );
    watcher.wait_mirror(deadline, "lane 1 working", |mirror| {
        lane_state(mirror.state("swarm"), BUSY).as_deref() == Some("working")
    });
    assert_eq!(
        lane_state(watcher.mirror.state("swarm"), IDLE).as_deref(),
        Some("idle"),
        "the second lane was never given work: {:?}",
        lane_states(watcher.mirror.state("swarm"))
    );
    println!(
        "{NOTE} lanes before the interrupt: {:?}",
        lane_states(watcher.mirror.state("swarm"))
    );

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

    // The note is a queued after-run row (§3), so it is read from the frames: the
    // row that carries the sentence becomes the `human_action` item — fields, no
    // text — as soon as the turn it waits for ends.
    let frame = watcher.wait_frame(deadline, "the coordinator's note", |frame| {
        note_in(&frame.data).is_some()
    });
    let note = note_in(&frame.data).expect("the note");
    println!("{NOTE} interrupted {interrupted:?}, the note: {note:?}");

    assert!(
        interrupted.iter().any(|topic| topic == "lane:1"),
        "the lane that was working is one of them: reply {reply}, note {note:?}"
    );
    assert!(
        !interrupted.iter().any(|topic| topic == "lane:2"),
        "an idle lane stops nothing, so the reply must not name it: reply {reply}, note {note:?}"
    );
    assert!(
        note.contains("[human] stopped lane 1 (interrupt)"),
        "the note names the lane that was stopped: {note:?}"
    );
    assert!(
        !note.contains("lane 2") && !note.contains("lanes"),
        "one lane stopped reads 'lane 1', not 'lanes 1, 2': {note:?}"
    );

    // --- and the item the note becomes says the same -----------------------------
    watcher.wait_mirror(deadline, "the human_action item", |mirror| {
        mirror.last_of_kind("session", "human_action").is_some()
    });
    let action = watcher
        .mirror
        .last_of_kind("session", "human_action")
        .cloned()
        .expect("the action");
    let named: Vec<u64> = action["lanes"]
        .as_array()
        .map(|lanes| lanes.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    assert_eq!(
        named,
        vec![BUSY],
        "the item names the lane that was stopped, and only it: {action}"
    );

    // --- the lanes settle --------------------------------------------------------
    watcher.wait_mirror(deadline, "lane 1 stopped", |mirror| {
        matches!(
            lane_state(mirror.state("swarm"), BUSY).as_deref(),
            Some("idle") | Some("stopped") | Some("down")
        )
    });
    println!(
        "{NOTE} after the interrupt: lanes {:?}, status {:?} (t10 in {:?})",
        lane_states(watcher.mirror.state("swarm")),
        watcher.mirror.state("session")["status"],
        started.elapsed()
    );

    let stopped = server.shutdown().expect("the ladder ran");
    println!("{NOTE} shutdown: {stopped:?}");
}

/// The note a frame carries, if it carries one: the queued row's text, wherever
/// the payload nests it (an `item.add`'s item, an `item.patch`'s patch).
fn note_in(data: &Value) -> Option<String> {
    strings(data)
        .into_iter()
        .find(|text| text.starts_with(NOTE_START))
}

/// Every string a JSON value holds, in order.
fn strings(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_strings(value, &mut found);
    found
}

fn collect_strings(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_strings(item, found)),
        Value::Object(fields) => fields
            .values()
            .for_each(|field| collect_strings(field, found)),
        _ => {}
    }
}
