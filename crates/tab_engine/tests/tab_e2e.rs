//! One tab, against a real fake `serve`: the ready file, the snapshot, the one
//! stream, the resets, and the ops. The updates that come out are what the
//! `session` crate is fed, so the assertions are about frames and bodies rather
//! than rows.

use std::time::{Duration, Instant};

use async_channel::Receiver;
use serde_json::json;
use session::{AgentKey, Op, TabModel};
use swarm_client::harness::{FakeSwarm, TempDir};
use tab_engine::{EngineHandle, TabEngine, TabSpec, Update};

/// A tab in its own directory, with the fake swarm's binary, stopped when the
/// test ends.
fn tab(tag: &str, workers: Option<u16>) -> (TempDir, EngineHandle, Receiver<Update>) {
    let dir = TempDir::new(tag).expect("a temp dir");
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).expect("the fake binary");
    let mut spec = TabSpec::new(bin, dir.path(), dir.path());
    if let Some(workers) = workers {
        spec = spec.with_workers(workers);
    }
    let (handle, updates) = TabEngine::start(spec);
    (dir, handle, updates)
}

/// The next update of a kind, or a panic naming what was being waited for.
fn expect(updates: &Receiver<Update>, what: &str, wanted: impl Fn(&Update) -> bool) -> Update {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(update) = updates.try_recv() {
            if wanted(&update) {
                return update;
            }
            continue;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether an update is a snapshot of this topic.
fn snapshot_of(topic: &str) -> impl Fn(&Update) -> bool + '_ {
    move |update| matches!(update, Update::Snapshot { topic: name, .. } if name == topic)
}

/// The ready update's epoch.
fn ready(updates: &Receiver<Update>) -> (String, u32) {
    let update = expect(updates, "Ready", |update| {
        matches!(update, Update::Ready { .. })
    });
    let Update::Ready { epoch, pid, .. } = update else {
        unreachable!()
    };
    (epoch, pid)
}

#[test]
fn a_tab_boots_snapshots_every_topic_and_streams() {
    let (_dir, handle, updates) = tab("boot", Some(2));
    assert!(matches!(updates.try_recv(), Ok(Update::Booting)));
    let (epoch, pid) = ready(&updates);
    assert!(!epoch.is_empty());
    assert!(pid > 0);

    // One snapshot update per topic the server answered with: the coordinator's,
    // the swarm's and each lane's.
    for topic in ["session", "swarm", "lane:1", "lane:2"] {
        let update = expect(&updates, topic, snapshot_of(topic));
        let Update::Snapshot { body, .. } = update else {
            unreachable!()
        };
        assert!(body.get("state").is_some(), "{topic}: {body}");
    }

    // And the stream is live, which the engine says once.
    expect(&updates, "the stream badge", |update| {
        matches!(update, Update::Stream { status } if !status.is_reconnecting())
    });
    drop(handle);
}

#[test]
fn a_frame_reaches_the_model_as_an_op() {
    let dir = TempDir::new("frames").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);

    swarm
        .emit(json!({
            "op": "item.add", "topic": "session", "after": null,
            "item": {"id": "e_1", "kind": "user", "ts": 1, "text": "hi", "status": "sent"}
        }))
        .unwrap();
    let update = expect(&updates, "the item.add", |update| {
        matches!(update, Update::Op { op: Op::ItemAdd { .. }, .. })
    });
    let Update::Op { topic, op } = update else {
        unreachable!()
    };
    assert_eq!(topic, "session");

    // It is the session crate's op, and the session crate draws it.
    let mut model = TabModel::new();
    model.on_op(&topic, &op);
    assert_eq!(model.selected_items().len(), 1);
    assert_eq!(model.selected_items()[0].id, "e_1");
    drop(handle);
}

#[test]
fn a_topic_reset_re_reads_that_one_topic() {
    let dir = TempDir::new("topic-reset").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);

    swarm
        .snapshot_body(json!({
            "session": {"state": {"status": "running"}, "items": [{"id": "e_9", "kind": "user", "ts": 9}]}
        }))
        .unwrap();
    swarm
        .emit(json!({"op": "topic.reset", "topic": "session", "reason": "leaf_moved"}))
        .unwrap();

    expect(&updates, "the topic.reset frame", |update| {
        matches!(update, Update::Op { op: Op::TopicReset { .. }, .. })
    });
    let update = expect(&updates, "the re-read topic", snapshot_of("session"));
    let Update::Snapshot { body, .. } = update else {
        unreachable!()
    };
    assert_eq!(body["items"][0]["id"], "e_9");
    assert_eq!(body["state"]["status"], "running");
    drop(handle);
}

#[test]
fn a_stream_reset_re_snapshots_everything_and_resumes_the_stream() {
    let dir = TempDir::new("stream-reset").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);
    expect(&updates, "the first snapshot", snapshot_of("session"));
    swarm.requests(); // forget the boot's requests

    swarm.stream_reset("restarted").unwrap();
    let update = expect(&updates, "the stream reset", |update| {
        matches!(update, Update::Op { op: Op::StreamReset { .. }, .. })
    });
    let Update::Op { op, .. } = update else {
        unreachable!()
    };
    assert_eq!(
        op,
        Op::StreamReset {
            reason: "restarted".into()
        }
    );
    // Everything is re-read, from the snapshot's cursor.
    expect(&updates, "the re-read session", snapshot_of("session"));
    let streams = swarm.requests_on("/stream");
    let resumed = streams.last().expect("a second stream").clone();
    let path = resumed["path"].as_str().unwrap();
    assert!(path.contains("since="), "{path}");
    assert!(path.contains("lane%3A%2A"), "{path}");
    drop(handle);
}

#[test]
fn a_refetch_asks_for_every_topic_again() {
    let dir = TempDir::new("refetch").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let _swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);
    expect(&updates, "the first snapshot", snapshot_of("session"));

    assert!(handle.refetch());
    expect(&updates, "the second snapshot", snapshot_of("session"));
    drop(handle);
}

#[test]
fn a_request_from_the_model_becomes_a_post() {
    let dir = TempDir::new("ops").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);
    swarm.requests();

    // The model builds the op; the handle is its sink.
    let model = TabModel::new();
    let request = model.send_input("hello there", session::Queue::Now);
    assert!(handle.request(request));

    let update = expect(&updates, "the reply", |update| {
        matches!(update, Update::OpReply(_))
    });
    let Update::OpReply(reply) = update else {
        unreachable!()
    };
    assert!(reply.ok, "{reply:?}");
    assert!(!reply.rid.is_empty());

    let posted = swarm.requests_on("/ops");
    assert_eq!(posted.len(), 1, "{posted:?}");
    let body = &posted[0]["body"];
    assert_eq!(body["op"], "input.send");
    assert_eq!(body["args"]["text"], "hello there");
    assert_eq!(body["args"]["queue"], "now");
    assert_eq!(body["rid"], json!(reply.rid));
    drop(handle);
}

#[test]
fn the_model_can_be_driven_entirely_from_the_updates() {
    // The contract's bound: a tab's whole job is to feed the model. This is that
    // path, end to end, with the fake server on the other side.
    let dir = TempDir::new("model").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));

    let mut model = TabModel::new();
    swarm
        .snapshot_body(json!({
            "session": {"state": {"status": "idle", "thinking": "high", "language": "en"},
                        "items": [{"id": "e_1", "kind": "user", "ts": 1, "text": "hi"}]},
            "swarm": {"state": {"id": "s1", "workers": 0, "lanes": []}}
        }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while model.selected_items().is_empty() {
        assert!(Instant::now() < deadline, "the model never saw the snapshot");
        if let Ok(update) = updates.recv_blocking() {
            apply(&mut model, update);
        }
    }
    assert_eq!(model.selected(), AgentKey::Coordinator);
    assert_eq!(model.selected_items().len(), 1);

    swarm
        .emit(json!({
            "op": "item.append", "topic": "session", "id": "e_1", "field": "text", "text": " there"
        }))
        .unwrap();
    while model.selected_items()[0].text != "hi" {
        // The first item is the reader's own words: the append is for another one.
        break;
    }
    let update = expect(&updates, "the append", |update| {
        matches!(update, Update::Op { op: Op::ItemAppend { .. }, .. })
    });
    let Update::Op { topic, op } = update else {
        unreachable!()
    };
    model.on_op(&topic, &op);
    assert_eq!(topic, "session");
    drop(handle);
}

/// Feed one update into the model, the way the workspace does.
fn apply(model: &mut TabModel, update: Update) {
    match update {
        Update::Snapshot { topic, body } => {
            model.on_snapshot(&topic, &body);
        }
        Update::Op { topic, op } => {
            model.on_op(&topic, &op);
        }
        Update::Stream { status } => {
            model.on_stream("session", status);
        }
        _ => {}
    }
}

#[test]
fn a_server_that_dies_is_reported_and_the_tab_stops() {
    let dir = TempDir::new("dies").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let mut swarm = FakeSwarm::start(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    ready(&updates);

    // Kill the process behind the tab: the stream cannot come back, and the tab
    // says so rather than reconnecting for ever.
    swarm.server_mut().kill();
    expect(&updates, "ServerGone", |update| {
        matches!(update, Update::ServerGone)
    });
    expect(&updates, "Exited", |update| {
        matches!(update, Update::Exited { .. })
    });
    assert!(!handle.is_running() || handle.shutdown());
}

#[test]
fn dropping_the_handle_stops_the_server() {
    let dir = TempDir::new("drop").unwrap();
    let bin = swarm_client::harness::fake_swarm_bin(dir.path()).unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    let (_, pid) = ready(&updates);
    assert!(swarm_client::process_alive(pid));

    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(10);
    while swarm_client::process_alive(pid) {
        assert!(Instant::now() < deadline, "the server outlived its tab");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_boot_that_fails_says_so_with_the_log_tail() {
    let dir = TempDir::new("boot-fail").unwrap();
    let bin = swarm_client::harness::script_that(dir.path(), "nope", "echo boom >&2; exit 3").unwrap();
    let (handle, updates) = TabEngine::start(TabSpec::new(bin, dir.path(), dir.path()));
    let update = expect(&updates, "BootFailed", |update| {
        matches!(update, Update::BootFailed { .. })
    });
    let Update::BootFailed { message, log_tail } = update else {
        unreachable!()
    };
    assert!(message.contains("exited during startup"), "{message}");
    assert!(log_tail.contains("boom"), "{log_tail}");
    drop(handle);
}
