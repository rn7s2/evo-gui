//! The client against a real server process, speaking the real protocol: a fake
//! `serve` in python, spawned the way the app spawns the real one.
//!
//! Every test here is about the wire — the ready file, the snapshot, the stream's
//! cursors and resets, and `POST /ops` — and nothing about what an item means,
//! which is the `session` crate's business.

use std::time::Duration;

use serde_json::json;
use swarm_client::harness::{script_that, FakeSwarm, TempDir};
use swarm_client::{
    process_alive, Client, Cursor, Error, ErrorCode, EventStream, Server, ServerConfig,
    StreamConfig, StreamMsg, StreamResetReason,
};

/// A config for a binary of the test's own, with the protocol's argv.
fn config_for(dir: &std::path::Path, bin: std::path::PathBuf) -> ServerConfig {
    let config = ServerConfig::swarm(bin, dir, dir);
    let argv = swarm_client::harness::serving_argv(&config.ready_file, &[]);
    config.with_argv(argv)
}

/// A fake swarm in its own tab directory, stopped when the test ends.
fn swarm(tag: &str) -> (TempDir, FakeSwarm) {
    let dir = TempDir::new(tag).expect("a temp dir");
    let swarm = FakeSwarm::start(dir.path()).expect("the fake serve starts");
    (dir, swarm)
}

/// The next message of a kind, or a panic naming what was being waited for.
fn expect(stream: &EventStream, what: &str, wanted: impl Fn(&StreamMsg) -> bool) -> StreamMsg {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(message) = stream.try_recv() {
            if wanted(&message) {
                return message;
            }
            continue;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_ready_file_is_the_whole_handshake() {
    let (_dir, swarm) = swarm("ready");
    let ready = swarm.server().ready().clone();
    assert_eq!(ready.program, "evo-swarm");
    assert_eq!(ready.pid, swarm.server().pid());
    assert_eq!(ready.port, swarm.server().port());
    assert!(swarm.server().ready_file().exists());
    assert!(swarm.server().client().port() > 0);

    // The port is the one the child chose (`--port 0`), and the argv said so.
    assert!(swarm.server().ready().url.contains(&ready.port.to_string()));
}

#[test]
fn a_snapshot_is_atomic_and_names_its_epoch() {
    let (_dir, swarm) = swarm("snapshot");
    swarm
        .snapshot_body(json!({
            "session": {"state": {"status": "idle"}, "items": [{"id": "e_1", "kind": "user", "ts": 1}]}
        }))
        .unwrap();
    let snapshot = swarm
        .client()
        .snapshot(&["session".to_owned(), "swarm".to_owned()], None)
        .expect("a snapshot");
    assert_eq!(snapshot.epoch, swarm.epoch());
    assert_eq!(snapshot.topic("session").unwrap()["items"][0]["id"], "e_1");
    // The swarm topic is state only, and a topic the server does not have is
    // simply not in the answer.
    assert!(snapshot.topic("swarm").unwrap()["state"].is_object());
    assert!(snapshot.topic("lane:9").is_none());
}

#[test]
fn a_lane_wildcard_expands_to_every_lane() {
    let dir = TempDir::new("lanes").unwrap();
    let swarm = FakeSwarm::with_argv(dir.path(), &["--workers", "2"]).unwrap();
    let snapshot = swarm
        .client()
        .snapshot(&["lane:*".to_owned()], None)
        .unwrap();
    let mut names = snapshot.topic_names();
    names.sort();
    assert_eq!(names, ["lane:1", "lane:2"]);
}

#[test]
fn a_stream_starts_with_hello_and_carries_ops_verbatim() {
    let (_dir, swarm) = swarm("stream");
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    let connected = expect(&stream, "hello", |message| {
        matches!(message, StreamMsg::Connected { .. })
    });
    let StreamMsg::Connected { cursor } = connected else {
        unreachable!()
    };
    assert_eq!(cursor.epoch, swarm.epoch());

    swarm
        .emit(json!({
            "op": "item.add", "topic": "session",
            "item": {"id": "e_1", "kind": "user", "ts": 1, "text": "hi"}, "after": null
        }))
        .unwrap();
    let frame = expect(&stream, "the item.add frame", |message| {
        matches!(message, StreamMsg::Frame(_))
    });
    let StreamMsg::Frame(frame) = frame else {
        unreachable!()
    };
    assert_eq!(frame.op, "item.add");
    assert_eq!(frame.data["item"]["id"], "e_1");
    assert_eq!(frame.topic(), Some("session"));
    // The frame's own cursor is where a reconnect would resume.
    assert!(stream.cursor().is_some_and(|cursor| cursor.seq > 0));

    // The snapshot the stream was resumed from is what the tab had read: the
    // client asked for `since` only when it had one, and the fake saw the topics.
    let asked = swarm.requests_on("/stream");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0]["path"]
        .as_str()
        .unwrap()
        .contains("topics=session"));
}

#[test]
fn a_dropped_stream_reconnects_from_the_last_cursor() {
    let (_dir, swarm) = swarm("reconnect");
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    swarm
        .emit(json!({"op": "state.patch", "topic": "session", "patch": {"status": "running"}}))
        .unwrap();
    expect(&stream, "a frame", |m| matches!(m, StreamMsg::Frame(_)));

    swarm.drop_streams().unwrap();
    expect(&stream, "a reconnect", |m| {
        matches!(m, StreamMsg::Reconnecting { .. })
    });
    expect(&stream, "hello again", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    // The second connection resumed from where the first had read.
    let asked = swarm.requests_on("/stream");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1]["path"].as_str().unwrap().contains("since="));
}

#[test]
fn a_stream_reset_parks_the_stream_until_it_is_told_where_to_resume() {
    let (_dir, swarm) = swarm("reset");
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    swarm.stream_reset("restarted").unwrap();
    let reset = expect(&stream, "a stream reset", |m| {
        matches!(m, StreamMsg::Reset { .. })
    });
    assert_eq!(
        reset,
        StreamMsg::Reset {
            reason: StreamResetReason::Restarted
        }
    );
    // Parked: nothing else arrives until the consumer answers.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        stream.try_recv().is_none(),
        "the stream must not reconnect on its own"
    );

    let from = Cursor::new(swarm.epoch().to_owned(), 7);
    stream.resume_from(from.clone());
    expect(&stream, "hello after the resume", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    let asked = swarm.requests_on("/stream");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[1]["path"]
            .as_str()
            .unwrap()
            .contains(&format!("since={}", from.encode())),
        "{asked:?}"
    );
}

#[test]
fn a_retried_rid_is_answered_again_and_acts_once() {
    let (_dir, swarm) = swarm("ops");
    let client = swarm.client().clone();
    let reply = client
        .op("input.send", json!({"text": "hi"}))
        .expect("a reply");
    assert!(reply.ok);
    assert!(reply.rid.len() > 4);
    assert!(reply.seq > 0);

    let first = client
        .op_with_rid("fixed-rid", "goal.pause", json!({}))
        .unwrap();
    let again = client
        .op_with_rid("fixed-rid", "goal.pause", json!({}))
        .unwrap();
    assert_eq!(again, first, "the server remembers the reply by rid");

    // And the rid the client makes for itself is unique per call.
    let one = client.op("goal.resume", json!({})).unwrap();
    let two = client.op("goal.resume", json!({})).unwrap();
    assert_ne!(one.rid, two.rid);
}

#[test]
fn a_refused_op_is_a_code_not_a_status() {
    let (_dir, swarm) = swarm("refused");
    swarm
        .reply_for(
            "input.send",
            json!({"ok": false, "error": {"code": "busy", "message": "a run is in flight", "detail": {}}}),
        )
        .unwrap();
    let reply = swarm
        .client()
        .op("input.send", json!({"text": "hi"}))
        .expect("an answered op is HTTP 200, refusal or not");
    assert!(!reply.ok);
    assert_eq!(reply.code(), Some(ErrorCode::Busy));
    assert!(reply.error().unwrap().message.contains("in flight"));
}

#[test]
fn a_bad_token_is_a_transport_failure() {
    let (_dir, swarm) = swarm("token");
    let stranger = Client::loopback(swarm.server().port(), "not-the-token").unwrap();
    let error = stranger.get_json("/health").unwrap_err();
    assert!(matches!(error, Error::Status(_)), "{error:?}");
    assert!(error.is_unauthorized());
}

#[test]
fn older_items_and_media_come_from_the_server() {
    let (_dir, swarm) = swarm("items");
    let items: Vec<_> = (1..=10)
        .map(|n| json!({"id": format!("e_{n}"), "kind": "user", "ts": n}))
        .collect();
    swarm
        .snapshot_body(json!({
            "session": {"state": {}, "items": items}
        }))
        .unwrap();
    let client = swarm.client().clone();
    let page = client.items("session", Some("e_8"), 3).unwrap();
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e_5", "e_6", "e_7"]);

    let one = client.item("session", "e_3").unwrap();
    assert_eq!(one["item"]["id"], "e_3");
    assert_eq!(one["item"]["kind"], "user");

    let (bytes, content_type) = client.media("session", "e_3", 0).unwrap();
    assert_eq!(content_type, "image/png");
    assert!(bytes.starts_with(b"fake-image"));
}

#[test]
fn an_eof_on_stdin_stops_the_server() {
    let (_dir, mut swarm) = swarm("eof");
    let pid = swarm.server().pid();
    assert!(process_alive(pid));
    swarm.server_mut().close_stdin();
    let code = swarm.server_mut().wait_for_exit(Duration::from_secs(10));
    assert!(code.is_some(), "the server left on EOF");
    assert!(
        !swarm.server().ready_file().exists(),
        "and took its ready file"
    );
}

#[test]
fn a_clean_shutdown_is_the_ladder_and_removes_the_ready_file() {
    let (_dir, mut swarm) = swarm("shutdown");
    let shutdown = swarm.server_mut().shutdown().expect("the ladder ran");
    assert_eq!(shutdown.outcome, swarm_client::ShutdownOutcome::Graceful);
    assert!(!swarm.server().ready_file().exists());
    assert!(!process_alive(swarm.server().pid()));
}

#[test]
fn a_server_that_exits_during_boot_names_the_exit() {
    let dir = TempDir::new("boot-exit").unwrap();
    let bin = script_that(dir.path(), "exits", "exit 3").unwrap();
    let error = Server::start(&config_for(dir.path(), bin)).unwrap_err();
    let Error::Boot(failure) = &error else {
        panic!("{error:?}")
    };
    assert_eq!(failure.exit_code, Some(3));
    assert!(failure.message.contains("exited during startup"));
}

#[test]
fn a_server_that_never_writes_its_ready_file_times_out() {
    let dir = TempDir::new("boot-slow").unwrap();
    let bin = script_that(dir.path(), "sleeps", "sleep 60").unwrap();
    let mut config = config_for(dir.path(), bin);
    config.ready_timeout = Duration::from_millis(300);
    config.shutdown_grace = Duration::from_secs(2);
    config.term_grace = Duration::from_secs(2);
    let error = Server::start(&config).unwrap_err();
    let Error::Boot(failure) = &error else {
        panic!("{error:?}")
    };
    assert!(
        failure.message.contains("no ready file"),
        "{}",
        failure.message
    );
}

#[test]
fn a_cancelled_boot_stops_the_server_it_was_waiting_for() {
    let dir = TempDir::new("boot-cancel").unwrap();
    let bin = script_that(dir.path(), "sleeps", "sleep 60").unwrap();
    let mut config = config_for(dir.path(), bin);
    config.ready_timeout = Duration::from_secs(30);
    let cancel = swarm_client::BootCancel::new();
    cancel.cancel();
    let error = Server::start_with(&config, &cancel, Default::default()).unwrap_err();
    assert!(error.cancelled().is_some(), "{error:?}");
}
