//! One tab, against a real fake `serve`: the ready file, the snapshot, the one
//! stream, the resets and the ops. The updates that come out are what the
//! `session` crate is fed, so the assertions are about frames and bodies rather
//! than rows.
//!
//! The tab spawns its own server, so these tests drive the server the tab is
//! really talking to through [`Control::attach`] — one of the fake's endpoints,
//! reached through the ready file in the tab directory.

use std::time::{Duration, Instant};

use async_channel::Receiver;
use serde_json::json;
use session::{AgentKey, Op, TabModel};
use swarm_client::harness::{Control, FakeSwarm, TempDir};
use swarm_client::ServerConfig;
use tab_engine::{EngineHandle, TabEngine, Update};

mod common;
use common::{serving, snapshot_of, Feed};

/// The config a tab is started from: the fake server in `dir`, with extra argv.
fn config(dir: &std::path::Path, extra: &[&str]) -> ServerConfig {
    swarm_client::harness::fake_config(dir, extra).expect("a fake server's config")
}

/// A tab in its own directory, with the fake swarm's binary, stopped when the
/// test ends.
fn tab(tag: &str, extra: &[&str]) -> (TempDir, EngineHandle, Receiver<Update>) {
    let dir = TempDir::new(tag).expect("a temp dir");
    let (handle, updates) = TabEngine::start(config(dir.path(), extra));
    (dir, handle, updates)
}

#[test]
fn a_tab_boots_snapshots_every_topic_and_streams() {
    let (dir, handle, updates) = tab("boot", &["--workers", "2"]);
    let mut feed = Feed::new(updates);
    feed.expect("Booting", |update| matches!(update, Update::Booting));
    let (epoch, pid) = serving(&mut feed);
    assert!(!epoch.is_empty());
    assert!(pid > 0);

    // One snapshot update per topic the server answered with: the coordinator's,
    // the swarm record's and each lane's.
    for topic in ["session", "swarm", "lane:1", "lane:2"] {
        let update = feed.expect(topic, snapshot_of(topic));
        let Update::Snapshot { body, .. } = update else {
            unreachable!()
        };
        assert!(body.get("state").is_some(), "{topic}: {body}");
    }

    // `serving` waited for the live stream, and the tab's own server answers.
    assert!(Control::attach(dir.path()).is_ok());
    drop(handle);
}

#[test]
fn a_frame_reaches_the_model_as_an_op() {
    let (dir, handle, updates) = tab("frames", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    control
        .emit(json!({
            "op": "item.add", "topic": "session", "after": null,
            "item": {"id": "e_1", "kind": "user", "ts": 1, "text": "hi", "status": "sent"}
        }))
        .unwrap();
    let update = feed.expect("the item.add", |update| {
        matches!(
            update,
            Update::Op {
                op: Op::ItemAdd { .. },
                ..
            }
        )
    });
    let Update::Op { topic, op } = update else {
        unreachable!()
    };
    assert_eq!(topic, "session");

    // It is the session crate's own op, and the session crate draws it.
    let mut model = TabModel::new();
    model.on_op(&topic, &op);
    assert_eq!(model.selected_items().len(), 1);
    assert_eq!(model.selected_items()[0].id, "e_1");
    drop(handle);
}

#[test]
fn a_topic_reset_re_reads_that_one_topic() {
    let (dir, handle, updates) = tab("topic-reset", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    feed.expect("the first snapshot", snapshot_of("session"));

    control
        .snapshot_body(json!({
            "session": {"state": {"status": "running"}, "items": [{"id": "e_9", "kind": "user", "ts": 9}]}
        }))
        .unwrap();
    control
        .emit(json!({"op": "topic.reset", "topic": "session", "reason": "leaf_moved"}))
        .unwrap();

    feed.expect("the topic.reset frame", |update| {
        matches!(
            update,
            Update::Op {
                op: Op::TopicReset { .. },
                ..
            }
        )
    });
    let update = feed.expect("the re-read topic", snapshot_of("session"));
    let Update::Snapshot { body, .. } = update else {
        unreachable!()
    };
    assert_eq!(body["items"][0]["id"], "e_9");
    assert_eq!(body["state"]["status"], "running");
    drop(handle);
}

#[test]
fn a_stream_reset_re_snapshots_everything_and_resumes_the_stream() {
    let (dir, handle, updates) = tab("stream-reset", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    feed.expect("the first snapshot", snapshot_of("session"));
    control.requests(); // forget the boot's requests

    control.stream_reset("restarted").unwrap();
    let update = feed.expect("the stream reset", |update| {
        matches!(
            update,
            Update::Op {
                op: Op::StreamReset { .. },
                ..
            }
        )
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
    // Everything is re-read, and the stream resumes from the snapshot's cursor —
    // not from the beginning, and not from nothing.
    feed.expect("the re-read session", snapshot_of("session"));
    let streams = control.requests_on("/stream");
    assert_eq!(
        streams.len(),
        1,
        "one resumed stream, not a second boot: {streams:?}"
    );
    let path = streams[0]["path"].as_str().unwrap();
    assert!(path.contains("since="), "{path}");
    assert!(path.contains("lane%3A%2A"), "{path}");
    drop(handle);
}

#[test]
fn a_refetch_asks_for_every_topic_again() {
    let (_dir, handle, updates) = tab("refetch", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    feed.expect("the first snapshot", snapshot_of("session"));

    assert!(handle.refetch());
    feed.expect("the second snapshot", snapshot_of("session"));
    drop(handle);
}

#[test]
fn a_request_from_the_model_becomes_a_post() {
    let (dir, handle, updates) = tab("ops", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    control.requests();

    // The model builds the op; the handle sends it and names the rid.
    let model = TabModel::new();
    let rid = handle
        .request(model.send_input("hello there", session::Queue::Now))
        .expect("the engine took the request");
    assert!(!rid.is_empty());

    let update = feed.expect("the reply", |update| {
        matches!(update, Update::OpReply { .. })
    });
    let Update::OpReply {
        rid: answered,
        op,
        reply,
    } = update
    else {
        unreachable!()
    };
    // The reply is matched to its request by the rid the handle minted, and names
    // the op so the UI knows which of its requests came back.
    assert_eq!(answered, rid);
    assert_eq!(op, "input.send");
    assert_eq!(reply.rid, rid);
    assert!(reply.ok, "{reply:?}");

    let posted = control.requests_on("/ops");
    assert_eq!(posted.len(), 1, "{posted:?}");
    let body = &posted[0]["body"];
    assert_eq!(body["op"], "input.send");
    assert_eq!(body["args"]["text"], "hello there");
    assert_eq!(body["args"]["queue"], "now");
    assert_eq!(body["rid"], json!(rid));
    drop(handle);
}

#[test]
fn the_model_can_be_driven_entirely_from_the_updates() {
    // The contract's bound: a tab's whole job is to feed the model. This is that
    // path, end to end, with the fake server on the other side.
    let (dir, handle, updates) = tab("model", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    let mut model = TabModel::new();

    control
        .emit(json!({
            "op": "item.add", "topic": "session", "after": null,
            "item": {"id": "e_1", "kind": "user", "ts": 1, "text": "hi", "status": "sent"}
        }))
        .unwrap();
    control
        .emit(json!({
            "op": "item.append", "topic": "session", "id": "e_1", "field": "text", "text": " there"
        }))
        .unwrap();

    feed.feed_model(&mut model, |model| {
        model
            .selected_items()
            .first()
            .map(|item| item.raw().clone())
            == Some(json!({
                "id": "e_1", "kind": "user", "ts": 1, "text": "hi there", "status": "sent"
            }))
    });
    assert_eq!(model.selected(), AgentKey::Coordinator);
    assert_eq!(model.selected_items().len(), 1);
    drop(handle);
}

#[test]
fn an_append_carries_its_id_field_and_text() {
    let (dir, handle, updates) = tab("append", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    control
        .emit(json!({
            "op": "item.append", "topic": "session", "id": "e_gone", "field": "text", "text": "x"
        }))
        .unwrap();
    let update = feed.expect("the append", |update| {
        matches!(
            update,
            Update::Op {
                op: Op::ItemAppend { .. },
                ..
            }
        )
    });
    let Update::Op { topic: _, op } = update else {
        unreachable!()
    };
    assert_eq!(
        op,
        Op::ItemAppend {
            id: "e_gone".to_string(),
            field: session::AppendField::Text,
            text: "x".to_string(),
        }
    );
    drop(handle);
}

#[test]
fn the_scrollback_pages_older_items_the_whole_item_and_an_image() {
    let (dir, handle, updates) = tab("reads", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();

    let items: Vec<_> = (1..=10)
        .map(|n| json!({"id": format!("e_{n}"), "kind": "user", "ts": n, "text": format!("line {n}")}))
        .collect();
    control
        .snapshot_body(json!({"session": {"state": {}, "items": items}}))
        .unwrap();

    // A page of the scrollback, the item a tool row expands to, and the bytes of
    // an image: three reads, three updates, each carrying the server's own body.
    assert!(handle.page("session", Some("e_8"), 3));
    let update = feed.expect("a page of items", |update| {
        matches!(update, Update::ItemsBefore { .. })
    });
    let Update::ItemsBefore { topic, body } = update else {
        unreachable!()
    };
    assert_eq!(topic, "session");
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e_5", "e_6", "e_7"]);
    assert_eq!(body["has_more"], true);

    assert!(handle.item("session", "e_3"));
    let update = feed.expect("one item", |update| matches!(update, Update::Item { .. }));
    let Update::Item { topic, body } = update else {
        unreachable!()
    };
    assert_eq!(topic, "session");
    assert_eq!(body["item"]["id"], "e_3");
    assert_eq!(body["item"]["text"], "line 3");

    assert!(handle.media("session", "e_3", 0));
    let update = feed.expect("image bytes", |update| {
        matches!(update, Update::Media { .. })
    });
    let Update::Media {
        topic,
        id,
        n,
        content_type,
        bytes,
    } = update
    else {
        unreachable!()
    };
    assert_eq!((topic.as_str(), id.as_str(), n), ("session", "e_3", 0));
    assert_eq!(content_type, "image/png");
    assert!(bytes.starts_with(b"fake-image"));
    drop(handle);
}

#[test]
fn a_read_that_fails_comes_back_as_a_fetch_failed() {
    let (dir, handle, updates) = tab("reads-fail", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    control.requests();

    // The server has no such item, so both reads are refusals — as an update,
    // never a panic and never a retry.
    assert!(handle.item("session", "e_missing"));
    assert!(handle.media("session", "e_missing", 3));
    let failed = |want: &'static str| {
        move |update: &Update| {
            matches!(update, Update::FetchFailed { what, reason }
                if what.starts_with(want) && !reason.is_empty())
        }
    };
    let update = feed.expect("the item failure", failed("item e_missing"));
    let Update::FetchFailed { what, reason } = update else {
        unreachable!()
    };
    assert!(what.starts_with("item e_missing"), "{what}");
    assert!(reason.contains("404") || !reason.is_empty(), "{reason}");
    let update = feed.expect("the media failure", failed("media 3 of e_missing"));
    let Update::FetchFailed { what, .. } = update else {
        unreachable!()
    };
    assert!(what.starts_with("media 3 of e_missing"), "{what}");
    drop(handle);
}

#[test]
fn the_sink_is_the_fire_and_forget_form_of_the_same_request() {
    let (dir, handle, updates) = tab("sink", &[]);
    let mut feed = Feed::new(updates);
    serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    control.requests();

    // `OpSink::send` mints a rid too — the reply just arrives with a rid nobody
    // kept, which is exactly what a fire-and-forget caller wants.
    let model = TabModel::new();
    session::OpSink::send(&handle, model.interrupt_swarm());
    let update = feed.expect("the reply", |update| {
        matches!(update, Update::OpReply { .. })
    });
    let Update::OpReply { rid, op, reply } = update else {
        unreachable!()
    };
    assert!(!rid.is_empty());
    assert_eq!(op, "run.interrupt");
    assert!(reply.ok, "{reply:?}");
    assert_eq!(reply.rid, rid);
    let posted = control.requests_on("/ops");
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0]["body"]["op"], "run.interrupt");
    assert_eq!(posted[0]["body"]["args"]["scope"], "swarm");
    drop(handle);
}

#[test]
fn a_supervisor_restart_is_a_reset_on_the_same_connection() {
    let (dir, handle, updates) = tab("restart", &[]);
    let mut feed = Feed::new(updates);
    let (epoch, _pid) = serving(&mut feed);
    let control = Control::attach(dir.path()).unwrap();
    let port = control.client().port();

    // The server re-execs itself: a new epoch, on the port it bound and with the
    // token it minted at launch — a supervisor restart (§1).
    control.restart().unwrap();

    // So there is nothing for the tab to be told and nothing to rebuild: the
    // stream reconnects where it was, the server answers a cursor from another
    // epoch with `stream.reset{restarted}`, and the tab reads itself again —
    // snapshot first, then the new lifetime's frames, on a stream that is live.
    feed.forget();
    let snapshot = feed.expect(
        "the new lifetime's session snapshot",
        snapshot_of("session"),
    );
    let Update::Snapshot { .. } = snapshot else {
        unreachable!()
    };
    feed.expect(
        "the new lifetime's live stream",
        |update| matches!(update, Update::Stream { status } if !status.is_reconnecting()),
    );
    let control = Control::attach(dir.path()).unwrap();
    assert_eq!(
        control.client().port(),
        port,
        "a restart keeps the port it bound"
    );
    control
        .emit(json!({"op": "state.patch", "topic": "session", "patch": {"thinking": "high"}}))
        .unwrap();
    feed.expect("a frame from the new server", |update| {
        matches!(
            update,
            Update::Op {
                op: Op::StatePatch { .. },
                ..
            }
        )
    });
    assert!(!epoch.is_empty());
    drop(handle);
}

#[test]
fn a_server_that_dies_is_reported_and_the_tab_stops() {
    let (_dir, handle, updates) = tab("dies", &[]);
    let mut feed = Feed::new(updates);
    let (_, pid) = serving(&mut feed);

    // Kill the process behind the tab: the stream cannot come back, and the tab
    // says so rather than reconnecting for ever.
    assert!(swarm_client::harness::kill_process(pid));
    feed.expect("ServerGone", |update| matches!(update, Update::ServerGone));
    feed.expect("Exited", |update| matches!(update, Update::Exited { .. }));
    handle.shutdown();
}

#[test]
fn dropping_the_handle_stops_the_server() {
    let (_dir, handle, updates) = tab("drop", &[]);
    let mut feed = Feed::new(updates);
    let (_, pid) = serving(&mut feed);
    assert!(swarm_client::process_alive(pid));

    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(10);
    while swarm_client::process_alive(pid) {
        assert!(Instant::now() < deadline, "the server outlived its tab");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn joining_waits_for_the_server_to_be_gone() {
    let (_dir, handle, updates) = tab("join", &[]);
    let mut feed = Feed::new(updates);
    let (_, pid) = serving(&mut feed);
    handle.join();
    assert!(!swarm_client::process_alive(pid));
}

#[test]
fn a_boot_that_fails_says_so_with_the_log_tail() {
    let dir = TempDir::new("boot-fail").unwrap();
    let bin =
        swarm_client::harness::script_that(dir.path(), "nope", "echo boom >&2; exit 3").unwrap();
    let config = ServerConfig::swarm(bin, dir.path(), dir.path());
    let argv = swarm_client::harness::serving_argv(&config.ready_file, &[]);
    let (handle, updates) = TabEngine::start(config.with_argv(argv));
    let mut feed = Feed::new(updates);
    let update = feed.expect("BootFailed", |update| {
        matches!(update, Update::BootFailed { .. })
    });
    let Update::BootFailed { reason, log_tail } = update else {
        unreachable!()
    };
    assert!(reason.contains("exited during startup"), "{reason}");
    assert!(log_tail.contains("boom"), "{log_tail}");
    drop(handle);
}

/// A compilation check on the shape the app uses: a `FakeSwarm` and a tab in the
/// same directory are two servers, so a test must attach to the tab's own.
#[test]
fn a_fake_swarm_can_still_be_driven_directly() {
    let dir = TempDir::new("direct").unwrap();
    let swarm = FakeSwarm::start(dir.path()).unwrap();
    swarm
        .control()
        .emit(json!({"op": "hello", "epoch": "e", "seq": 1}))
        .unwrap();
    assert!(swarm.control().requests_on("/_emit").is_empty());
    assert_eq!(swarm.server().port(), swarm.client().port());
}
