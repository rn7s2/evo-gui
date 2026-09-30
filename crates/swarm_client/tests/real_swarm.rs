//! The real swarm: `build/evo-swarm` with its lanes, the topics `evo-agent` does
//! not have (CONTRACT §4.3, §6, §7).
//!
//! Skipped unless `EVO_SWARM_BIN` names a build:
//!
//! ```sh
//! EVO_SWARM_BIN=…/build/evo-swarm EVO_AGENT_BIN=…/build/evo-agent \
//!   EVO_TEST_HOME=/tmp/stub cargo test -p swarm_client --test real_swarm -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use swarm_client::harness::{kill_process, serving_argv, with_stub_home, TempDir};
use swarm_client::{Client, EventStream, Server, ServerConfig, StreamConfig, StreamMsg};

/// The binary under test, or a note that this run has none.
fn swarm_bin() -> Option<PathBuf> {
    match std::env::var_os("EVO_SWARM_BIN") {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            eprintln!("real_swarm: no EVO_SWARM_BIN — skipping");
            None
        }
    }
}

/// The `evo-agent` the lanes run: beside the swarm binary unless told otherwise.
fn agent_bin(swarm: &std::path::Path) -> Option<PathBuf> {
    std::env::var_os("EVO_AGENT_BIN")
        .map(PathBuf::from)
        .or_else(|| Some(swarm.parent()?.join("evo-agent")))
}

/// A swarm in its own directory, with `workers` lanes, stopped when the test ends.
fn swarm(tag: &str, workers: u32) -> Option<(TempDir, Server)> {
    let bin = swarm_bin()?;
    let dir = TempDir::new(tag).expect("a temp dir");
    let config = ServerConfig::swarm(bin.clone(), dir.path(), dir.path());
    let mut extra = vec!["--workers".to_owned(), workers.to_string()];
    if let Some(agent) = agent_bin(&bin) {
        extra.push("--evo".to_owned());
        extra.push(agent.display().to_string());
    }
    let borrowed: Vec<&str> = extra.iter().map(String::as_str).collect();
    let argv = serving_argv(&config.ready_file, &borrowed);
    let config = with_stub_home(config.with_argv(argv));
    match Server::start(&config) {
        Ok(server) => Some((dir, server)),
        Err(error) => panic!("the real swarm did not start: {error}"),
    }
}

/// The swarm topic's state, once every lane has finished starting (§4.3: a lane
/// is published as it starts, and again when it is idle).
fn wait_for_lanes(client: &Client, workers: usize) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Ok(snapshot) = client.snapshot(&["swarm".to_owned()], Some(1)) {
            if let Some(state) = snapshot.topic("swarm").map(|topic| topic["state"].clone()) {
                let idle = state["lanes"]
                    .as_array()
                    .map(|lanes| {
                        lanes.len() == workers
                            && lanes.iter().all(|lane| lane["state"] == json!("idle"))
                    })
                    .unwrap_or(false);
                if idle {
                    return state;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the swarm never reported {workers} idle lanes"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The next op of a kind, or a panic naming what was being waited for.
fn expect(stream: &EventStream, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(StreamMsg::Frame(frame)) = stream.try_recv() {
            if wanted(&frame.data) {
                return frame.data;
            }
            continue;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_swarm_topic_and_its_lane_mirrors_are_what_the_contract_says() {
    let Some((_dir, server)) = swarm("swarm-topics", 2) else {
        return;
    };
    let client = server.client().clone();
    let state = wait_for_lanes(&client, 2);

    // §4.3: the swarm record.
    assert!(state["id"].is_string(), "{state}");
    assert_eq!(state["workers"], json!(2));
    assert!(state["status"]["busy"].is_number(), "{state}");
    let lanes = state["lanes"].as_array().expect("lanes");
    assert_eq!(lanes.len(), 2);
    for (index, lane) in lanes.iter().enumerate() {
        assert_eq!(lane["n"], json!(index + 1));
        assert!(lane["state"].is_string(), "{lane}");
        assert!(lane["pid"].as_u64().unwrap_or(0) > 0, "{lane}");
        assert_eq!(lane["restarts"], json!(0));
        assert!(lane["model"]["id"].is_string(), "{lane}");
        assert!(lane["todos"].is_array(), "{lane}");
        assert!(lane["reports"].is_number(), "{lane}");
    }

    // §4.3: `lane:N` is that lane's own items and state — one topic each, mirrored
    // by the coordinator, so a client reads every lane through one endpoint.
    let snapshot = client
        .snapshot(&["session".to_owned(), "lane:*".to_owned()], Some(5))
        .expect("a snapshot");
    for n in 1..=2 {
        let topic = snapshot
            .topic(&format!("lane:{n}"))
            .unwrap_or_else(|| panic!("no lane:{n}"));
        assert!(
            topic["state"]["session"]["program"] == json!("lane"),
            "{topic}"
        );
        assert!(
            !topic["items"].as_array().unwrap().is_empty(),
            "a lane's own items (its notices): {topic}"
        );
    }
    // The coordinator's own topic is still the session's, with the swarm's session
    // id beside it (§4.2).
    assert!(snapshot.topic("session").is_some());

    // §5.6: `evo-swarm catalog --json` computes a lane view without a lane; what a
    // *live* swarm answers is worth knowing either way.
    let catalog = client.catalog().expect("a catalog");
    eprintln!("note: /catalog lanes on a live swarm: {}", catalog["lanes"]);
}

#[test]
fn a_lane_restart_is_published_and_mirrored() {
    let Some((_dir, server)) = swarm("swarm-lane-restart", 1) else {
        return;
    };
    let client = server.client().clone();
    let state = wait_for_lanes(&client, 1);
    let pid = state["lanes"][0]["pid"].as_u64().expect("a lane pid") as u32;

    let snapshot = client
        .snapshot(
            &[
                "session".to_owned(),
                "swarm".to_owned(),
                "lane:*".to_owned(),
            ],
            Some(5),
        )
        .expect("a snapshot");
    let stream = EventStream::start(
        client.clone(),
        StreamConfig::new([
            "session".to_owned(),
            "swarm".to_owned(),
            "lane:*".to_owned(),
        ])
        .from(snapshot.cursor()),
    );

    // §7.2/§4.3: kill the lane's process; the coordinator restarts it from the
    // exact session it was running.
    assert!(kill_process(pid));
    let crashed = expect(&stream, "a lane_event (crashed)", |data| {
        data["op"] == json!("item.add") && data["item"]["kind"] == json!("lane_event")
    });
    assert_eq!(crashed["item"]["lane"], json!(1), "{crashed}");

    // §4.3/§5.3: the lane's topic is stale, so it is reset — and re-read.
    let reset = expect(&stream, "topic.reset lane:1", |data| {
        data["op"] == json!("topic.reset") && data["topic"] == json!("lane:1")
    });
    assert_eq!(reset["reason"], json!("lane_restarted"), "{reset}");

    // And the swarm record counts it, with a new process.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let state = client
            .snapshot(&["swarm".to_owned()], Some(1))
            .unwrap()
            .topic("swarm")
            .unwrap()["state"]
            .clone();
        let lane = &state["lanes"][0];
        if lane["restarts"] == json!(1) {
            assert_ne!(lane["pid"].as_u64().unwrap_or(0) as u32, pid, "{lane}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the restart was never counted: {state}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(stream);
}

#[test]
fn the_two_interrupt_scopes_a_person_has() {
    let Some((_dir, server)) = swarm("swarm-interrupts", 2) else {
        return;
    };
    let client = server.client().clone();
    wait_for_lanes(&client, 2);

    // §5.5/§7.4: stopping the swarm stops every lane; stopping one lane stops it.
    let swarm_scope = client
        .op("run.interrupt", json!({"scope": "swarm"}))
        .unwrap();
    assert!(swarm_scope.ok, "{swarm_scope:?}");
    let stopped = swarm_scope.result["interrupted"]
        .as_array()
        .expect("a list");
    for n in 1..=2 {
        assert!(
            stopped
                .iter()
                .any(|name| name == &json!(format!("lane:{n}"))),
            "{stopped:?}"
        );
    }
    let lane_scope = client
        .op("run.interrupt", json!({"scope": "lane", "lane": 2}))
        .unwrap();
    assert!(lane_scope.ok, "{lane_scope:?}");
    assert_eq!(lane_scope.result["interrupted"], json!(["lane:2"]));

    // A lane that does not exist is refused, not obeyed.
    let missing = client
        .op("run.interrupt", json!({"scope": "lane", "lane": 9}))
        .unwrap();
    assert!(!missing.ok, "{missing:?}");
    assert!(missing.error().is_some(), "{missing:?}");
}

/// Run one turn and wait for it to be over: a session that has never been written
/// is a session a restart does not resume (reported to the serve lane).
fn run_a_turn(client: &Client) {
    let reply = client
        .op(
            "input.send",
            json!({"text": "a turn before the restart", "queue": "now"}),
        )
        .expect("a reply");
    assert!(reply.ok, "{reply:?}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = client
            .snapshot(&["session".to_owned()], Some(1))
            .unwrap()
            .topic("session")
            .unwrap()["state"]["status"]
            .clone();
        if status == json!("idle") {
            return;
        }
        assert!(Instant::now() < deadline, "the turn never ended ({status})");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_coordinator_restart_keeps_the_port_and_counts_itself() {
    let Some((_dir, mut server)) = swarm("swarm-restart", 1) else {
        return;
    };
    wait_for_lanes(&server.client().clone(), 1);
    run_a_turn(server.client());
    let before = server.ready().clone();
    let epoch = before.epoch.clone();
    let port = before.port;
    let token = before.token.clone();

    // §1/§8: the supervisor restarts the serving child on the port it bound, from
    // the exact session, and says so in the ready file.
    assert!(kill_process(before.pid));
    let deadline = Instant::now() + Duration::from_secs(60);
    let restarted = loop {
        if let Some(ready) = server.follow_ready() {
            break ready;
        }
        assert!(
            Instant::now() < deadline,
            "the ready file never named a new lifetime (still {epoch})"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_ne!(restarted.epoch, epoch);
    assert_eq!(restarted.port, port, "a restart keeps the port it bound");
    assert_eq!(restarted.restarts, 1, "and counts itself");
    assert_eq!(
        restarted.supervisor_pid,
        Some(server.pid()),
        "the supervisor is the process we hold"
    );
    assert_eq!(
        restarted.session.path, before.session.path,
        "the same session"
    );

    // The client the ready file now names works — which is what the tab does on a
    // re-read: whatever a restart changes (the epoch certainly, and in this build
    // the token too), the file is where a client learns it.
    if restarted.token != token {
        eprintln!(
            "note: the restart re-minted the token ({}… → {}…)",
            &token[..8],
            &restarted.token[..8]
        );
    }
    let client = server.client().clone();
    let snapshot = client
        .snapshot(&["swarm".to_owned(), "lane:*".to_owned()], Some(1))
        .expect("the restarted swarm answers");
    assert_eq!(snapshot.epoch, restarted.epoch);
    assert_eq!(
        snapshot.topic("swarm").unwrap()["state"]["workers"],
        json!(1),
        "its lanes came back with it"
    );
}
