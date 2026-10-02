//! The real thing: a client against `build/evo-agent` (or `evo-swarm`), the
//! binary the app will run, speaking the protocol of CONTRACT.md §5.
//!
//! Skipped unless `EVO_AGENT_BIN` (or `EVO_SWARM_BIN`) names a build, so the
//! ordinary `cargo test` runs the fake and stays fast:
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_TEST_HOME=/tmp/stub \
//!   cargo test -p swarm_client --test real_server -- --nocapture
//! ```
//!
//! (`EVO_TEST_HOME` is a stub home — `evo-gui/scripts/stub_home.sh start DIR` —
//! and is handed to the *child* as its `HOME`/`EVO_HOME`, so the model answers
//! without a key; this process's own HOME is left alone, since rustup needs it.)

use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::json;
use swarm_client::harness::{serving_argv, with_stub_home, TempDir};
use swarm_client::{
    Client, Cursor, Error, EventStream, Server, ServerConfig, StreamConfig, StreamMsg,
};

/// The binary under test, or a note that this run has none.
fn real_bin() -> Option<PathBuf> {
    for name in ["EVO_AGENT_BIN", "EVO_SWARM_BIN"] {
        if let Some(path) = std::env::var_os(name) {
            return Some(PathBuf::from(path));
        }
    }
    eprintln!("real_server: no EVO_AGENT_BIN/EVO_SWARM_BIN — skipping");
    None
}

/// A real server in its own directory, stopped when the test ends.
fn server(tag: &str) -> Option<(TempDir, Server)> {
    let bin = real_bin()?;
    let dir = TempDir::new(tag).expect("a temp dir");
    let config = ServerConfig::swarm(bin, dir.path(), dir.path());
    let argv = serving_argv(&config.ready_file, &[]);
    // The stub home goes to the child only: `HOME` is how this machine finds
    // rustup, and it must survive.
    let config = with_stub_home(config.with_argv(argv));
    match Server::start(&config) {
        Ok(server) => Some((dir, server)),
        Err(error) => panic!("the real server did not start: {error}"),
    }
}

/// The next message of a kind, or a panic naming what was being waited for.
fn expect(stream: &EventStream, what: &str, wanted: impl Fn(&StreamMsg) -> bool) -> StreamMsg {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_real_server_answers_the_reads_the_client_makes() {
    let Some((_dir, server)) = server("real-reads") else {
        return;
    };
    let client = server.client().clone();

    // §1: the ready file names the process that is *serving*.
    let ready = server.ready().clone();
    assert!(!ready.epoch.is_empty());
    assert!(
        swarm_client::process_alive(ready.pid),
        "the pid in the ready file is a live process: {ready:?}"
    );
    // The pid serving is the coordinator's, which in this build is a child of a
    // supervisor the launcher holds — and the file names that supervisor only
    // when the supervisor sets `EVO_SUPERVISOR_PID` (not merged yet). The client
    // compares no pids: the epoch is what identifies a lifetime.
    if let Some(supervisor) = ready.supervisor_pid {
        assert_eq!(supervisor, server.pid(), "the process we hold supervises");
    }
    assert_eq!(ready.port, server.port());
    assert!(ready.session.path.contains(".evo/sessions/"), "{ready:?}");

    // §5.2: one snapshot, topic by topic, with the session's own state.
    let snapshot = client
        .snapshot(&["session".to_owned(), "swarm".to_owned()], Some(20))
        .expect("a snapshot");
    assert_eq!(snapshot.epoch, ready.epoch);
    let session = snapshot.topic("session").expect("topic session");
    assert!(session["state"]["model"]["id"].is_string(), "{session}");
    assert!(session["items"].is_array(), "{session}");
    // §4.1: `durable` is a bool on every notice — present even when false.
    for item in session["items"].as_array().expect("items") {
        if item["kind"] == json!("notice") {
            assert!(item["durable"].is_boolean(), "{item}");
        }
    }
    // §5.6: the catalog, which the model choosers read.
    let catalog = client.catalog().expect("a catalog");
    assert!(catalog["models"].is_array(), "{catalog}");
    assert!(catalog["ops"].is_array(), "{catalog}");

    // §5.2: `lane:*` on a server with no lanes is empty, not an error.
    let lanes = client
        .snapshot(&["lane:*".to_owned()], Some(1))
        .expect("a snapshot");
    assert!(lanes.topic("lane:1").is_none());

    // §5.4: paging and one whole item.
    let body = client.items("session", None, 3).expect("a page");
    let items = body["items"].as_array().expect("items").clone();
    assert!(items.len() <= 3, "{body}");
    if let Some(id) = items.last().and_then(|item| item["id"].as_str()) {
        let whole = client.item("session", id).expect("one item");
        assert_eq!(whole["item"]["id"], id);
    }
}

#[test]
fn the_real_server_streams_a_turn_end_to_end() {
    let Some((_dir, server)) = server("real-turn") else {
        return;
    };
    let client = server.client().clone();
    let snapshot = client
        .snapshot(&["session".to_owned()], None)
        .expect("a snapshot");
    let stream = EventStream::start(
        client.clone(),
        StreamConfig::new(["session".to_owned()]).from(snapshot.cursor()),
    );

    // §5.3: hello first, and it is where the stream starts.
    let hello = expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    let StreamMsg::Connected { cursor } = hello else {
        unreachable!()
    };
    assert_eq!(cursor.epoch, server.epoch());
    assert!(cursor.seq >= snapshot.seq);

    // §5.5: an op whose effects are visible in the stream the client is already
    // reading — the ordering the whole design rests on.
    let reply = client
        .op(
            "input.send",
            json!({"text": "hello from the real-server test", "queue": "now"}),
        )
        .expect("a reply");
    assert!(reply.ok, "{reply:?}");
    let item_id = reply.result["item_id"].as_str().expect("an item id");

    let mut ops = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        let Some(message) = stream.try_recv() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let StreamMsg::Frame(frame) = message else {
            continue;
        };
        let seen = frame.get("seq").and_then(|seq| seq.as_u64()).unwrap_or(0);
        ops.push(frame.op.clone());
        // The turn is over once the assistant item is final.
        if frame.op == "item.patch"
            && frame.get("id").and_then(|id| id.as_str()) == Some(item_id)
            && frame.get("patch").and_then(|p| p.get("status")).is_some()
        {
            let _ = seen;
        }
        if ops.iter().any(|op| op == "item.append") && ops.len() > 12 {
            break;
        }
    }
    assert!(
        ops.iter().any(|op| op == "item.add"),
        "the turn's items arrive: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| op == "state.patch"),
        "the run's state arrives: {ops:?}"
    );
    assert!(
        ops.iter().any(|op| op == "item.append"),
        "the answer streams: {ops:?}"
    );

    // §5.2: what the snapshot says now, and the user item the turn created.
    let whole = client.item("session", item_id).expect("the queued item");
    assert_eq!(whole["item"]["kind"], "user");
    assert_eq!(whole["item"]["text"], "hello from the real-server test");
    drop(stream);
}

#[test]
fn a_cursor_from_another_lifetime_is_a_reset_not_a_gap() {
    let Some((_dir, server)) = server("real-reset") else {
        return;
    };
    let client = server.client().clone();
    let stream = EventStream::start(
        client.clone(),
        StreamConfig::new(["session".to_owned()]).from(Cursor::new("deadbeef", 3)),
    );
    // §5.3: a `since` from another epoch gets hello and then stream.reset.
    let reset = expect(&stream, "a stream reset", |m| {
        matches!(m, StreamMsg::Reset { .. })
    });
    let StreamMsg::Reset { reason } = reset else {
        unreachable!()
    };
    assert_eq!(reason.as_str(), "restarted");
    drop(stream);
}

#[test]
fn a_supervisor_restart_keeps_the_address_and_moves_the_epoch() {
    let Some((_dir, server)) = server("real-restart") else {
        return;
    };
    // §1 restarts the child from the exact session it was serving — including one
    // that has never been written to, which is what a tab shows before its first
    // turn.
    let epoch = server.epoch().to_owned();
    let port = server.port();
    let before = server.ready().clone();

    // Kill the process that is serving: the supervisor (the process we hold)
    // brings it back and rewrites the ready file. What it keeps is the whole point
    // — the port it bound and the token it minted at launch — so a client that was
    // built from the first file keeps working, and the new epoch arrives in band as
    // a `stream.reset` rather than as a new address to connect to.
    assert!(swarm_client::harness::kill_process(before.pid));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let restarted = loop {
        if let Some(ready) = swarm_client::read_ready(server.ready_file()) {
            if ready.epoch != epoch {
                break ready;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the ready file never named a new lifetime (still {epoch})"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_ne!(restarted.epoch, epoch, "a restart is a new epoch");
    assert!(restarted.pid > 0);
    assert_eq!(
        restarted.port, port,
        "§1: a restart keeps the port it bound"
    );
    assert_eq!(restarted.restarts, 1, "§1: and counts itself");
    assert_eq!(
        restarted.session.path, before.session.path,
        "§1: --resume <the exact session it was serving>"
    );
    assert_eq!(
        restarted.token, before.token,
        "§1: the token is minted once per launch, so a restart keeps it"
    );
    // Which is what makes the client the tab already had still the right one: it
    // reads the new lifetime with the token it was built with in the first place.
    let client = server.client().clone();
    let snapshot = client
        .snapshot(&["session".to_owned()], Some(5))
        .expect("the new lifetime answers");
    assert_eq!(snapshot.epoch, restarted.epoch);

    // §3: an epoch change is a reset on the connection that already exists, not a
    // new address. This is exactly what a tab that was streaming when its server
    // was killed does: it reconnects to the same URL with the cursor it had, and
    // is told the epoch moved.
    let stream = EventStream::start(
        client,
        StreamConfig::new(["session".to_owned()]).from(Cursor::new(&epoch, snapshot.seq)),
    );
    let connected = expect(&stream, "hello", |m| {
        matches!(m, StreamMsg::Connected { .. })
    });
    let StreamMsg::Connected { cursor } = connected else {
        unreachable!()
    };
    assert_eq!(cursor.epoch, restarted.epoch, "hello names the new epoch");
    let reset = expect(&stream, "a stream reset", |m| {
        matches!(m, StreamMsg::Reset { .. })
    });
    let StreamMsg::Reset { reason } = reset else {
        unreachable!()
    };
    assert_eq!(reason.as_str(), "restarted");
}

#[test]
fn the_real_server_refuses_the_way_the_contract_says() {
    let Some((_dir, server)) = server("real-refusals") else {
        return;
    };
    let client = server.client().clone();

    // §5.5: an answered op is HTTP 200 with a code, never a status.
    let unknown = client.op("no.such.op", json!({})).expect("an answer");
    assert!(!unknown.ok);
    assert_eq!(unknown.code(), Some(swarm_client::ErrorCode::UnknownOp));

    let bad = client.op("input.send", json!({})).expect("an answer");
    assert!(!bad.ok);
    assert_eq!(bad.code(), Some(swarm_client::ErrorCode::InvalidArgs));

    // §5.5: a retried rid is answered from the cache and does nothing twice.
    let first = client
        .op_with_rid("real-rid", "goal.set", json!({"objective": "x"}))
        .unwrap();
    let again = client
        .op_with_rid("real-rid", "goal.set", json!({"objective": "x"}))
        .unwrap();
    assert_eq!(first, again, "the second call is the first reply");
    client.op("goal.clear", json!({})).expect("a reply");

    // §5.5 / §5: a wrong token is the transport's refusal, not an op code.
    let stranger = Client::loopback(server.port(), "not-the-token").expect("a client");
    let refused = stranger
        .snapshot(&["session".to_owned()], Some(1))
        .unwrap_err();
    assert!(matches!(refused, Error::Status(_)), "{refused:?}");
    assert!(refused.is_unauthorized(), "{refused:?}");

    // §5.6 / §2: an op the server does not have is a refusal, and shutdown is a
    // clean exit.
    let shutdown = server_shutdown(&client);
    assert!(shutdown.is_ok(), "{shutdown:?}");
}

/// §5.5: a turn carrying a pasted picture is *answered*, not timed out.
///
/// Measured 2026-10-02 against `/usr/local/bin/evo-agent`: a turn whose image is
/// 314 KB of PNG (419 KB of base64) is answered 8.7 s in, and one of 640 KB (854 KB
/// of base64 — the payload the journal held) 36 s in, while a body of the same size
/// made of *words* is answered in 41 ms. The cost is the server's own handling of a
/// base64 image, and it grows faster than the image: twice the picture, four times
/// the wait. A client whose patience is the base 30 s therefore reports a *refusal*
/// for a turn the session has already taken and journalled — which is how the same
/// pasted picture ended up in the session three times. The patience is the body's
/// own size since then (`HttpClient::patience_for_body`), and this is that, on the
/// binary the app runs.
///
/// Slow on purpose — the answer lands most of a minute after the request — and run
/// only where the other real-binary tests run (`EVO_AGENT_BIN` + a `stub_home.sh`
/// home). The picture is a GIF by its magic and a filler: what the server charges
/// for is how much base64 it decodes, and evo is told the media type by the
/// client's own field.
#[test]
fn a_turn_carrying_a_screenshot_is_answered_not_timed_out() {
    let Some((_dir, server)) = server("real-big-send") else {
        return;
    };
    let client = server.client().clone();
    client
        .snapshot(&["session".to_owned()], Some(1))
        .expect("a snapshot");

    // 640 KB of picture: 854 KB of base64, the size the journal held for the
    // screenshot that reported this.
    let mut picture = b"GIF89a".to_vec();
    picture.resize(640 * 1024, 0);
    let data = BASE64.encode(&picture);
    assert!(data.len() > 850 * 1024, "{}", data.len());

    let started = std::time::Instant::now();
    let reply = client
        .op(
            "input.send",
            json!({
                "text": "look at this",
                "images": [{"name": "pasted image.gif", "media_type": "image/gif",
                            "data": data}],
                "queue": "now",
                "topic": "session",
            }),
        )
        .expect("the send is answered, not dropped");
    let took = started.elapsed();
    assert!(reply.ok, "{reply:?}");
    assert!(
        reply.result["item_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "and it names the turn it took: {reply:?}"
    );
    eprintln!("note: a 854 KB base64 picture was answered {took:?} after it was sent");
    let _ = client.op("run.interrupt", json!({"scope": "session"}));
}

fn server_shutdown(client: &Client) -> Result<(), Error> {
    let reply = client.op("server.shutdown", json!({}))?;
    assert!(reply.ok, "{reply:?}");
    // The ready file goes with the process, and the socket closes.
    for _ in 0..200 {
        if client.snapshot(&["session".to_owned()], Some(1)).is_err() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(Error::Timeout("the server did not stop".into()))
}
