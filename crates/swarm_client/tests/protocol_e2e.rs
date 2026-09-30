//! The client against a real server process, speaking the real protocol: a fake
//! `serve` in python, spawned the way the app spawns the real one.
//!
//! Every test here is about the wire — the ready file, the snapshot, the stream's
//! cursors and resets, and `POST /ops` — and nothing about what an item means,
//! which is the `session` crate's business.
//!
//! `EVO_SWARM_BIN` points the same tests at the real `evo-swarm`, and then every
//! assertion here holds of a server the test cannot script as well as of the
//! fake; the four that cannot are marked where they stand. A real run needs a
//! stub home and no tie to the session that started it:
//!
//! ```sh
//! EVO_SWARM_BIN=…/build/evo-swarm EVO_AGENT_BIN=…/build/evo-agent \
//! EVO_TEST_HOME=/tmp/evostub \
//! env -u EVO_SESSIONS_DIR -u EVO_SERVE_TOKEN -u EVO_SUPERVISED_CHILD \
//!     -u EVO_HEARTBEAT_FILE -u EVO_PID -u EVO_IDE_CONTEXT \
//!   cargo test -p swarm_client --test protocol_e2e
//! ```

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
///
/// With `EVO_SWARM_BIN` set this is the real `evo-swarm` instead, driven through
/// the same client and the same argv — and every assertion below is written to
/// hold either way. What cannot hold is a test whose *setup* needs an endpoint
/// only the fake has, and those say so through [`needs_the_fake`].
fn swarm(tag: &str) -> (TempDir, FakeSwarm) {
    let dir = TempDir::new(tag).expect("a temp dir");
    let swarm = FakeSwarm::start(dir.path()).expect("the fake serve starts");
    if !swarm.is_fake() {
        eprintln!(
            "protocol_e2e: {tag} against the real server ({})",
            swarm.bin().display()
        );
    }
    (dir, swarm)
}

/// Whether a test can drive the server it was given.
///
/// Four of these tests need a server that can be *made* to do something a client
/// cannot ask for — drop a live stream, move the epoch under it, forget a cursor
/// — and only the fake has those controls. On the real server the same events
/// happen when the supervisor restarts the child, which is asserted the way a
/// client can observe it in `real_server.rs`; here the test says what it is
/// skipping rather than pretending.
fn needs_the_fake(swarm: &FakeSwarm, what: &str) -> bool {
    if swarm.is_fake() {
        return true;
    }
    eprintln!("protocol_e2e: {what} needs the fake server's controls — skipped");
    false
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
    assert_eq!(ready.port, swarm.server().port());
    assert!(swarm.server().ready_file().exists());
    assert!(swarm.server().client().port() > 0);
    assert!(
        process_alive(ready.pid),
        "the pid the file names is a live process: {ready:?}"
    );

    // §1: the pid in the file is the process that is *serving*, and a file that
    // names a supervisor names one whose child is serving — which is what the
    // real `evo-swarm` is, and what the fake is not: it serves in the very
    // process the harness spawned.
    if let Some(supervisor) = ready.supervisor_pid {
        assert_eq!(
            supervisor,
            swarm.server().pid(),
            "the process we spawned supervises"
        );
    }
    if swarm.is_fake() {
        assert_eq!(
            ready.pid,
            swarm.server().pid(),
            "the fake serves in the process we spawned"
        );
    } else {
        assert_ne!(
            ready.pid,
            swarm.server().pid(),
            "a real server serves in a child of the process we hold"
        );
    }

    // The port is the one the child chose (`--port 0`), and the argv said so.
    assert!(swarm.server().ready().url.contains(&ready.port.to_string()));
}

#[test]
fn a_snapshot_is_atomic_and_names_its_epoch() {
    let (_dir, swarm) = swarm("snapshot");
    // The fake serves whatever a test tells it to; the real one serves the
    // session it is running, whose items are its own.
    if swarm.is_fake() {
        swarm
            .control()
            .snapshot_body(json!({
                "session": {"state": {"status": "idle"}, "items": [{"id": "e_1", "kind": "user", "ts": 1}]}
            }))
            .unwrap();
    }
    let snapshot = swarm
        .client()
        .snapshot(
            &[
                "session".to_owned(),
                "swarm".to_owned(),
                "lane:9".to_owned(),
            ],
            None,
        )
        .expect("a snapshot");
    assert_eq!(snapshot.epoch, swarm.epoch());

    let session = snapshot.topic("session").expect("topic session");
    assert!(session["state"].is_object(), "{session}");
    let items = session["items"].as_array().expect("items");
    assert!(!items.is_empty(), "a session says something: {session}");
    // §4.1: every item carries the id the server minted and the kind it is.
    for item in items {
        assert!(item["id"].is_string(), "{item}");
        assert!(item["kind"].is_string(), "{item}");
    }
    if swarm.is_fake() {
        assert_eq!(items[0]["id"], "e_1");
    }

    // The swarm's own record is state only, and a topic the server does not have
    // is simply not in the answer.
    let swarm_topic = snapshot.topic("swarm").expect("topic swarm");
    assert!(swarm_topic["state"].is_object(), "{swarm_topic}");
    let held = swarm_topic["items"]
        .as_array()
        .map(|items| items.len())
        .unwrap_or(0);
    assert_eq!(
        held, 0,
        "the swarm topic is its record, with no items of its own: {swarm_topic}"
    );
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

    // Something for the server to say: the fake echoes an op a test wrote —
    // nothing interprets it — and the real one gets a prompt.
    if swarm.is_fake() {
        swarm
            .control()
            .emit(json!({
                "op": "item.add", "topic": "session",
                "item": {"id": "e_1", "kind": "user", "ts": 1, "text": "hi"}, "after": null
            }))
            .unwrap();
    } else {
        let reply = swarm
            .client()
            .op("input.send", json!({"text": "hi"}))
            .expect("a reply");
        assert!(reply.ok, "{reply:?}");
    }

    // The frame is the server's op, with the topic it was published on: the
    // client copies what it is given and interprets nothing.
    let frame = expect(&stream, "an item.add frame", |message| match message {
        StreamMsg::Frame(frame) => frame.op == "item.add",
        _ => false,
    });
    let StreamMsg::Frame(frame) = frame else {
        unreachable!()
    };
    assert_eq!(frame.topic(), Some("session"));
    assert!(frame.data["item"]["id"].is_string(), "{:?}", frame.data);
    if swarm.is_fake() {
        assert_eq!(
            frame.data["item"]["id"], "e_1",
            "the op is the one that was written, as it was written"
        );
    }
    // The frame's own cursor is where a reconnect would resume.
    assert!(stream.cursor().is_some_and(|cursor| cursor.seq > 0));

    // The snapshot the stream was resumed from is what the tab had read: the
    // client asked for `since` only when it had one, and the fake saw the
    // topics. The real server keeps no log of what a client asked for.
    if swarm.is_fake() {
        let asked = swarm.control().requests_on("/stream");
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0]["path"]
            .as_str()
            .unwrap()
            .contains("topics=session"));
    }
}

#[test]
fn a_dropped_stream_reconnects_from_the_last_cursor() {
    let (_dir, swarm) = swarm("reconnect");
    if !needs_the_fake(&swarm, "a dropped stream") {
        return;
    }
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    swarm
        .control()
        .emit(json!({"op": "state.patch", "topic": "session", "patch": {"status": "running"}}))
        .unwrap();
    expect(&stream, "a frame", |m| matches!(m, StreamMsg::Frame(_)));

    swarm.control().drop_streams().unwrap();
    expect(&stream, "a reconnect", |m| {
        matches!(m, StreamMsg::Reconnecting { .. })
    });
    expect(&stream, "hello again", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    // The second connection resumed from where the first had read.
    let asked = swarm.control().requests_on("/stream");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1]["path"].as_str().unwrap().contains("since="));
}

#[test]
fn a_stream_reset_parks_the_stream_until_it_is_told_where_to_resume() {
    let (_dir, swarm) = swarm("reset");
    if !needs_the_fake(&swarm, "a stream reset") {
        return;
    }
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    swarm.control().stream_reset("restarted").unwrap();
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
    let asked = swarm.control().requests_on("/stream");
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
fn a_reconnect_to_another_lifetime_is_a_reset() {
    let (_dir, swarm) = swarm("restart");
    if !needs_the_fake(&swarm, "a moved epoch") {
        return;
    }
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    // A different process lifetime, and our connection goes with it.
    let epoch = swarm.control().new_epoch().unwrap();
    swarm.control().drop_streams().unwrap();
    let reset = expect(&stream, "a stream reset", |m| {
        matches!(m, StreamMsg::Reset { .. })
    });
    assert!(
        matches!(reset, StreamMsg::Reset { reason } if reason.as_str() == "restarted"),
        "{reset:?}"
    );

    // Resuming from the snapshot of that lifetime continues it.
    stream.resume_from(Cursor::new(epoch.clone(), 0));
    expect(&stream, "hello in the new lifetime", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    assert_eq!(
        stream.cursor().map(|cursor| cursor.epoch),
        Some(epoch),
        "the stream is in the new lifetime"
    );
}

#[test]
fn a_cursor_older_than_the_retention_is_a_reset_too() {
    let (_dir, swarm) = swarm("forgotten");
    if !needs_the_fake(&swarm, "a forgotten cursor") {
        return;
    }
    let stream = EventStream::start(
        swarm.client().clone(),
        StreamConfig::new(["session".to_owned()]),
    );
    expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });

    swarm.control().forget_cursors().unwrap();
    swarm.control().drop_streams().unwrap();
    let reset = expect(&stream, "a cursor_too_old reset", |m| {
        matches!(
            m,
            StreamMsg::Reset {
                reason: StreamResetReason::CursorTooOld
            }
        )
    });
    assert_eq!(
        reset,
        StreamMsg::Reset {
            reason: StreamResetReason::CursorTooOld
        }
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
    // An op the server has to refuse: the fake is told to refuse `input.send`,
    // and the real server refuses what it has nothing to act on — pausing a goal
    // when there is no goal.
    if swarm.is_fake() {
        swarm
            .control()
            .reply_for(
                "input.send",
                json!({"ok": false, "error": {"code": "busy", "message": "a run is in flight", "detail": {}}}),
            )
            .unwrap();
    }
    let (op, args) = if swarm.is_fake() {
        ("input.send", json!({"text": "hi"}))
    } else {
        ("goal.pause", json!({}))
    };
    let reply = swarm
        .client()
        .op(op, args)
        .expect("an answered op is HTTP 200, refusal or not");
    assert!(!reply.ok, "{reply:?}");
    let error = reply.error().expect("a refusal carries an error");
    assert!(!error.message.is_empty(), "{error:?}");
    // §5.5: the code is the machine's word for why, and it is one a client knows.
    assert!(reply.code().is_some(), "{reply:?}");
    if swarm.is_fake() {
        assert_eq!(reply.code(), Some(ErrorCode::Busy));
        assert!(error.message.contains("in flight"));
    } else {
        assert_eq!(reply.code(), Some(ErrorCode::GoalState), "{reply:?}");
    }
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
    if swarm.is_fake() {
        let items: Vec<_> = (1..=10)
            .map(|n| json!({"id": format!("e_{n}"), "kind": "user", "ts": n}))
            .collect();
        swarm
            .control()
            .snapshot_body(json!({
                "session": {"state": {}, "items": items}
            }))
            .unwrap();
    }
    let client = swarm.client().clone();

    // §5.4: a page of the session's items, oldest first.
    let page = client.items("session", None, 20).expect("a page");
    let held: Vec<&str> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(!held.is_empty(), "a session has items: {page}");

    if swarm.is_fake() {
        // Ten items, and a page that ends *before* the item named.
        assert_eq!(held.len(), 10);
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
    } else {
        // The real session's own items: one of them, whole, and a page behind it.
        // Media needs an image in the topic, which a session gets from a paste —
        // `real_server.rs` drives that — so what is asserted here is that an item
        // with no picture is refused rather than answered with bytes.
        let newest = *held.last().unwrap();
        let behind = client.items("session", Some(newest), 1).unwrap();
        assert!(behind["items"].as_array().unwrap().len() <= 1, "{behind}");
        let one = client.item("session", newest).unwrap();
        assert_eq!(one["item"]["id"], newest);
        let error = client.media("session", newest, 0).unwrap_err();
        assert!(matches!(error, Error::Status(_)), "{error:?}");
    }
}

#[test]
fn an_eof_on_stdin_stops_the_server() {
    let (_dir, mut swarm) = swarm("eof");
    let pid = swarm.server().pid();
    assert!(process_alive(pid));
    swarm.server_mut().close_stdin();
    // A real swarm stops its lanes one at a time, and these tests run in
    // parallel: the patience is the harness's, not ten seconds.
    let code = swarm
        .server_mut()
        .wait_for_exit(swarm_client::harness::STOP_PATIENCE);
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
    assert!(failure.reason.contains("exited during startup"));
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
        failure.reason.contains("no ready file"),
        "{}",
        failure.reason
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
