//! t02 — a prompt streams: `input.send` answers with the item it minted, and the
//! stream grows that item until its status says the answer is finished.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t02_prompt_streams -- --nocapture
//! ```
//!
//! The subject is the protocol, so this runs against one real
//! `evo-agent serve` (the same server a swarm's coordinator is); a tab's server
//! is `Program::Swarm` and nothing else here changes.
//!
//! What is proven: §6.4's `input.send` result carries the **pre-minted item id**
//! (CONTRACT §3), that id is the item the stream adds, the assistant's text grows
//! as `item.append` ops on that one item rather than as re-sent items (§5.3), the
//! status changes are `item.patch`es, and the topic's `status` is `running` while
//! an answer is being written and `idle` when it is done.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, send, snapshot, wait_for, Watcher};
use serde_json::Value;
use store::launch::Program;

/// A tab's server is a swarm; this proof's subject is the prompt path, and a
/// swarm's server is a coordinator plus lanes. Switch to `Program::Swarm` and
/// give it workers to run the same assertions through one.
const PROGRAM: Program = Program::Agent;
const WORKERS: u16 = 0;

/// A prompt the scripted model answers at once, and one it answers in sixty
/// deltas over six seconds — long enough for a client to see the item growing.
const QUICK: &str = "hello from t02";
const SLOW: &str = "SLOW t02";

#[test]
fn t02_prompt_streams() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t02");
    let mut server = fixture.spawn(&fixture.spec(PROGRAM, WORKERS));
    let client = server.client().clone();

    // --- from a snapshot, follow the stream ------------------------------------
    let seeded = snapshot(&client, &["session"], 200);
    assert_eq!(
        seeded["topics"]["session"]["state"]["status"].as_str(),
        Some("idle"),
        "a fresh server is idle: {seeded}"
    );
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().unwrap(),
    );
    let mut watcher = Watcher::start(&client, &["session"], Some(cursor));

    // --- an answer the model gives at once -------------------------------------
    let reply = send(&client, QUICK);
    let quick_id = reply["item_id"]
        .as_str()
        .expect("input.send mints the item's id")
        .to_owned();
    assert_eq!(
        reply["blocked"],
        Value::Null,
        "the model is ready, so nothing blocked it: {reply}"
    );
    println!("{NOTE} input.send -> {reply}");

    // The reader's own words, under the id the reply minted.
    watcher.wait_mirror(deadline, "the user item", |mirror| {
        mirror.item("session", &quick_id).is_some()
    });
    let user = watcher
        .mirror
        .item("session", &quick_id)
        .cloned()
        .expect("the item");
    assert_eq!(user["kind"].as_str(), Some("user"));
    assert_eq!(user["text"].as_str(), Some(QUICK));
    assert!(
        matches!(user["status"].as_str(), Some("sent") | Some("queued")),
        "it was taken or queued, not cancelled: {user}"
    );

    watcher.wait_mirror(deadline, "the answer", |mirror| {
        mirror
            .last_of_kind("session", "assistant")
            .and_then(|item| item["status"].as_str())
            == Some("final")
    });
    let quick = watcher
        .mirror
        .last_of_kind("session", "assistant")
        .cloned()
        .expect("the answer");
    assert_eq!(
        quick["text"].as_str(),
        Some(format!("ok: {QUICK}").as_str()),
        "the scripted model echoes the prompt: {quick}"
    );
    assert_eq!(quick["model"].as_str(), Some("stub-a"));
    assert!(
        quick["usage"]["output"].as_u64().unwrap_or(0) > 0,
        "a finished answer carries its usage: {quick}"
    );

    // --- an answer that is still being written ---------------------------------
    let slow_reply = send(&client, SLOW);
    let slow_id = slow_reply["item_id"]
        .as_str()
        .expect("an item id")
        .to_owned();
    let mut saw_streaming = false;
    let mut saw_running = false;
    watcher.wait_mirror(deadline, "the slow answer to start", |mirror| {
        saw_running |= mirror.state("session")["status"].as_str() == Some("running");
        saw_streaming |= mirror
            .last_of_kind("session", "assistant")
            .and_then(|item| item["status"].as_str())
            == Some("streaming");
        saw_streaming
    });
    // The item is on the stream while it is still short: that is what a client
    // draws, and it is why the appends exist at all.
    let partial = watcher
        .mirror
        .last_of_kind("session", "assistant")
        .cloned()
        .expect("the streaming answer");
    let partial_id = partial["id"].as_str().expect("an item id").to_owned();
    let partial_text = partial["text"].as_str().unwrap_or_default().to_owned();
    println!(
        "{NOTE} streaming after {:?}: {} chars of {partial_id}",
        started.elapsed(),
        partial_text.chars().count()
    );

    watcher.wait_mirror(deadline, "the slow answer to finish", |mirror| {
        mirror
            .last_of_kind("session", "assistant")
            .and_then(|item| item["status"].as_str())
            == Some("final")
    });
    let slow = watcher
        .mirror
        .last_of_kind("session", "assistant")
        .cloned()
        .expect("the answer");
    let slow_text = slow["text"].as_str().unwrap_or_default();
    assert!(
        slow_text.starts_with("slow0 slow1"),
        "sixty deltas arrived in order: {slow_text:?}"
    );
    assert!(
        slow_text.len() > partial_text.len(),
        "it grew after the first frame: {} then {}",
        partial_text.len(),
        slow_text.len()
    );
    assert!(
        saw_running,
        "the topic said running while the answer was being written"
    );
    assert_ne!(slow_id, quick_id, "each send mints its own item");

    // CONTRACT §3, §4.1: the assistant item's id is minted at `message-start` and
    // used when the entry is written, so a client never has to re-key a row it is
    // already drawing. If this fails the answer was substituted — an `item.remove`
    // of the streaming id followed by an `item.add` of another — which is what a
    // view does while the kernel does not mint the entry id up front.
    let final_id = slow["id"].as_str().expect("an item id").to_owned();
    assert_eq!(
        final_id,
        partial_id,
        "the finished answer keeps the id it streamed under: streamed as \
         {partial_id}, finished as {final_id}; the ops for it were {:?}",
        ops_for(&watcher, &final_id)
    );

    // The answer grew where it stands (§5.3): one `item.add` mints it, ops after
    // that grow or patch that same id, and the item is never re-sent whole —
    // which is what keeps a long answer cheap.
    let ops = ops_for(&watcher, &final_id);
    assert_eq!(
        ops.first().map(|(op, _)| op.as_str()),
        Some("item.add"),
        "the first op for an item is the one that adds it: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|(op, _)| op == "item.add").count(),
        1,
        "an item is added once and then grown: {ops:?}"
    );
    let appends = ops.iter().filter(|(op, _)| op == "item.append").count();
    assert!(
        appends >= 2,
        "a long answer is many appends, not one big one: {ops:?}"
    );
    assert_eq!(
        ops.last().map(|(op, _)| op.as_str()),
        Some("item.patch"),
        "the end of an answer is a patch on its item: {ops:?}"
    );
    let mut last_seq = 0;
    for frame in &watcher.frames {
        let seq = frame.data["seq"].as_u64().unwrap_or(0);
        assert!(
            seq >= last_seq,
            "ops arrive in seq order: {last_seq} then {seq}"
        );
        last_seq = seq;
    }

    // --- and the topic says it is over -----------------------------------------
    wait_for(deadline, "the run to end", || {
        watcher.pump();
        (watcher.mirror.state("session")["status"].as_str() == Some("idle")).then_some(())
    });
    println!(
        "{NOTE} t02 done in {:?}: {} item(s), {appends} appends",
        started.elapsed(),
        watcher.mirror.items("session").len()
    );

    let stopped = server.shutdown().expect("the ladder ran");
    println!("{NOTE} shutdown: {stopped:?}");
}

/// Every op the stream carried for one item, in order: `(op, seq)`. An `item.add`
/// names its item in `item.id`; every other op names it in `id`.
fn ops_for(watcher: &Watcher, id: &str) -> Vec<(String, u64)> {
    watcher
        .frames
        .iter()
        .filter(|frame| {
            frame.data["id"].as_str() == Some(id) || frame.data["item"]["id"].as_str() == Some(id)
        })
        .map(|frame| (frame.op.clone(), frame.data["seq"].as_u64().unwrap_or(0)))
        .collect()
}
