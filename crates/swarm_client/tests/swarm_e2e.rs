//! swarm_client end to end, against a REAL `/usr/local/bin/evo-swarm serve`.
//!
//! The shape of `../evo-agent/tests/swarm-serve-e2e.py`: a temp `HOME` whose
//! `init.lisp` registers a stub provider, two lanes, a `SLOW` scripted model.
//! Nothing here stubs HTTP; the only thing faked is the model.
//!
//! Run with `CARGO_TARGET_DIR=target/swarm_client cargo test -p swarm_client`.
//! Set `EVO_SWARM_KEEP_TMP=1` to keep the temp homes of a failing run.

#![cfg(feature = "test-harness")]

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;
use swarm_client::harness::{Bins, Harness, HarnessConfig, TempDir, STUB_MODEL, STUB_SECRET};
use swarm_client::{
    Error, EventStream, LaneState, Server, ServerConfig, ShutdownOutcome, StatusError,
    StreamConfig, StreamMsg, StreamTarget,
};

/// One swarm at a time: a test run is not a load test, and cargo runs the tests
/// in this file on parallel threads.
static SWARM: Mutex<()> = Mutex::new(());

fn one_swarm() -> MutexGuard<'static, ()> {
    SWARM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn bins() -> Bins {
    let bins = Bins::installed();
    assert!(
        bins.available(),
        "no binaries to test against: {} / {} (set EVO_SWARM_BIN / EVO_AGENT_BIN)",
        bins.swarm.display(),
        bins.agent.display()
    );
    bins
}

/// The next stream message, or a panic naming the deadline.
fn next_message(stream: &EventStream, deadline: Instant) -> StreamMsg {
    loop {
        if let Some(message) = stream.try_recv() {
            return message;
        }
        assert!(
            Instant::now() < deadline,
            "no stream message within the deadline (path {})",
            stream.path()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// #1 — start a real swarm, read every endpoint of §4, refuse like serve does,
/// and shut it down the ladder way (§3).
#[test]
fn boot_reads_and_the_shutdown_ladder() {
    let _guard = one_swarm();
    let mut harness = Harness::start(HarnessConfig {
        workers: 2,
        ..Default::default()
    })
    .expect("the swarm should come up");
    let client = harness.client().clone();

    // --- /health names the server a swarm, with the SSE bootstrap cursor -----
    let health = client.health().expect("GET /health").typed;
    assert_eq!(health.name.as_deref(), Some("evo-swarm"), "{health:?}");
    assert!(health.has_feature("swarm"), "{health:?}");
    assert!(
        health.cursor.is_some(),
        "health carries a cursor: {health:?}"
    );
    assert!(health.pid > 0);
    assert!(!health.version.clone().unwrap_or_default().is_empty());

    // --- /state: idle, the stub model resolved -------------------------------
    let state = harness.state().expect("GET /state");
    assert_eq!(state.status, "idle", "{:?}", state.raw);
    assert_eq!(state.model.as_deref(), Some(STUB_MODEL), "{:?}", state.raw);
    assert_eq!(state.provider.as_deref(), Some("stub"), "{:?}", state.raw);
    assert_eq!(state.model_ready, Some(true), "{:?}", state.raw);
    assert_eq!(state.context_window, Some(200_000), "{:?}", state.raw);
    assert!(state.goal.is_none());
    assert!(state.todos.is_empty());
    let _ = state.activity();

    // --- /transcript: a fresh session has nothing to send yet ----------------
    let transcript = client.transcript(None).expect("GET /transcript");
    assert!(transcript.messages.is_empty(), "{:?}", transcript.raw);
    assert!(client.transcript(Some(5)).is_ok(), "?limit=N is accepted");

    // --- /registry: the stub model, the kernel's APIs, the commands ----------
    let registry = client.registry().expect("GET /registry");
    let model = registry
        .models
        .iter()
        .find(|model| model.id == STUB_MODEL)
        .unwrap_or_else(|| panic!("no {STUB_MODEL} in {:?}", registry.raw));
    assert_eq!(model.api.as_deref(), Some("anthropic-messages"));
    assert_eq!(model.context_window, Some(200_000));
    assert!(
        registry.apis.contains(&"anthropic-messages".to_owned()),
        "{:?}",
        registry.apis
    );
    assert!(
        registry.lane_ready(model),
        "a lane can register the stub model"
    );
    assert!(registry
        .providers
        .iter()
        .any(|provider| provider.key == "stub"));
    assert!(registry
        .commands
        .iter()
        .any(|command| command.name == "goal"));
    assert!(registry.tools.iter().any(|tool| tool.name == "bash"));

    // --- /journal: the coordinator's own session ----------------------------
    let journal = client.journal(Some(20)).expect("GET /journal");
    assert!(journal.path.ends_with(".sexp"), "{}", journal.path);
    assert_eq!(
        journal.header.get("type").and_then(Value::as_str),
        Some("session")
    );
    assert!(journal.leaf.is_some());
    // The swarm records itself as :custom state, which is what --resume reads.
    let swarm_record = journal
        .newest_custom("swarm")
        .unwrap_or_else(|| panic!("no swarm entry in {:?}", journal.raw));
    assert!(swarm_record.get("id").is_some(), "{swarm_record:?}");

    // --- /lanes: every lane, numbered, nothing private ----------------------
    let lanes = harness
        .wait_for_lanes_idle(Duration::from_secs(120))
        .expect("both lanes should come up idle");
    assert_eq!(lanes.swarm.workers, 2, "{:?}", lanes.raw);
    assert!(!lanes.swarm.id.is_empty());
    assert!(!lanes.swarm.cwd.trim_end_matches('/').is_empty());
    assert!(!lanes.swarm.is_stopping());
    assert_eq!(lanes.lanes.len(), 2, "{:?}", lanes.raw);
    let numbers: Vec<u32> = lanes.lanes.iter().map(|lane| lane.n).collect();
    assert_eq!(numbers, vec![1, 2]);
    let lane_pids: Vec<u32> = lanes.lanes.iter().filter_map(|lane| lane.pid).collect();
    assert_eq!(
        lane_pids.len(),
        2,
        "every lane reports a pid: {:?}",
        lanes.raw
    );
    let text = serde_json::to_string(&lanes.raw).unwrap();
    assert!(
        !text.contains("http"),
        "no lane URL reaches the client: {text}"
    );
    for lane in &lanes.lanes {
        assert_eq!(lane.state(), LaneState::Idle);
        assert!(lane.task.is_none());
    }

    // --- a lane's transcript, relayed (read-only, §D21) ---------------------
    let lane_transcript = client
        .lane_transcript(1, None)
        .expect("GET /lanes/1/transcript");
    assert!(
        lane_transcript.messages.is_empty(),
        "{:?}",
        lane_transcript.raw
    );

    // --- refusals are typed, and 409 is NotNow (§4) -------------------------
    match client.lane_transcript(99, None) {
        Err(Error::Status(StatusError::NotFound(error))) => {
            assert!(error.message.contains("no lane 99"), "{error:?}");
        }
        other => panic!("an unknown lane should be 404, got {other:?}"),
    }
    let refused = client.steer("is anyone there?").expect_err("nothing runs");
    assert!(refused.is_not_now(), "409 expected, got {refused:?}");
    assert_eq!(refused.status().map(|status| status.status()), Some(409));
    assert!(
        refused
            .status()
            .unwrap()
            .message()
            .contains("no run to steer"),
        "{refused}"
    );
    assert!(matches!(refused, Error::Status(StatusError::NotNow(_))));

    // --- the lane's own event stream, relayed and resumable -----------------
    let lane_stream = EventStream::start(
        StreamTarget::lane(&client, 1, Some(0)),
        StreamConfig::default(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    let mut lane_events = 0;
    while !connected || lane_events == 0 {
        match next_message(&lane_stream, deadline) {
            StreamMsg::Connected { .. } => connected = true,
            StreamMsg::Event { id, .. } => {
                assert!(id.is_some(), "a lane event carries its own id");
                lane_events += 1;
            }
            StreamMsg::Disconnected { error, .. } => panic!("lane stream dropped: {error}"),
            StreamMsg::Reset { .. } | StreamMsg::Ended => panic!("unexpected stream message"),
        }
    }
    drop(lane_stream); // stops the thread and its socket

    // --- no key ever reaches a journal, a log or a lane file ----------------
    let leaks = harness.files_containing(STUB_SECRET);
    assert!(
        leaks.is_empty(),
        "the provider secret leaked into {leaks:?}"
    );

    // --- the shutdown ladder: POST /shutdown, wait, and nothing is left -----
    let swarm_pid = harness.server.pid();
    let report = harness.shutdown().expect("POST /shutdown");
    assert_eq!(report.outcome, ShutdownOutcome::Graceful, "{report:?}");
    assert_eq!(report.exit_code, Some(0), "{report:?}");
    assert!(!swarm_client::process_alive(swarm_pid), "the swarm is gone");
    assert!(
        harness.wait_server_gone(Duration::from_secs(5)),
        "the swarm process is gone"
    );
    assert!(
        !harness.token_file.exists(),
        "a clean shutdown removes the token file (§serve.md)"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut alive = lane_pids.clone();
    while !alive.is_empty() && Instant::now() < deadline {
        alive.retain(|pid| swarm_client::process_alive(*pid));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        alive.is_empty(),
        "every lane is gone after shutdown, still: {alive:?}"
    );
}

/// #2 — a prompt runs through `/prompt`, and its text arrives on the SSE stream
/// as it streams (§5), which then resumes from an arbitrary cursor.
#[test]
fn prompt_streams_events_and_the_stream_resumes() {
    let _guard = one_swarm();
    let mut harness = Harness::start(HarnessConfig {
        workers: 1,
        ..Default::default()
    })
    .expect("swarm up");
    let client = harness.client().clone();

    // From the beginning: the connect is itself a replay.
    let stream = EventStream::start(
        StreamTarget::coordinator(&client, Some(0)),
        StreamConfig::default(),
    );
    let deadline = Instant::now() + Duration::from_secs(90);

    let posted = client
        .prompt("SLOW hello from the client")
        .expect("POST /prompt");
    assert!(posted.is_ok(), "{posted:?}");
    assert!(
        posted.task.is_some(),
        "the prompt started a run: {posted:?}"
    );
    assert_eq!(
        posted.data.get("queued"),
        Some(&Value::Bool(true)),
        "{posted:?}"
    );

    let mut kinds: Vec<String> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();
    let mut text = String::new();
    let mut usage = None;
    let mut settled = None;
    loop {
        match next_message(&stream, deadline) {
            StreamMsg::Event { id, kind, data } => {
                if let Some(id) = id {
                    ids.push(id);
                }
                if kind == "text-delta" {
                    text.push_str(data.get("text").and_then(Value::as_str).unwrap_or(""));
                }
                if kind == "message-end" {
                    assert_eq!(
                        data.get("stop_reason").and_then(Value::as_str),
                        Some("stop")
                    );
                    usage = data.get("usage").cloned();
                }
                if kind == "settled" {
                    settled = Some(data.clone());
                }
                kinds.push(kind);
                if settled.is_some() {
                    break;
                }
            }
            StreamMsg::Connected { .. }
            | StreamMsg::Disconnected { .. }
            | StreamMsg::Reset { .. } => {}
            StreamMsg::Ended => panic!("the stream ended before settled"),
        }
        assert!(
            Instant::now() < deadline,
            "no settled event; kinds so far: {kinds:?}"
        );
    }

    // The events §5 lists, in order.
    for wanted in ["message-start", "text-delta", "message-end", "settled"] {
        assert!(
            kinds.iter().any(|kind| kind == wanted),
            "no {wanted} event in {kinds:?}"
        );
    }
    assert!(
        kinds.iter().position(|kind| kind == "message-start")
            < kinds.iter().position(|kind| kind == "text-delta")
    );
    // The text streamed delta by delta, in order.
    assert!(
        text.starts_with("slow0 slow1 "),
        "streamed text was {text:?}"
    );
    assert!(
        text.contains("slow59"),
        "the whole message arrived: {text:?}"
    );
    // `usage` is what the status readout folds (§7.3).
    let usage = usage.expect("message-end carries usage");
    assert!(
        usage.get("input").and_then(Value::as_u64).is_some(),
        "{usage}"
    );
    assert!(usage.get("cache_read").is_some(), "{usage}");
    let settled = settled.unwrap();
    assert_eq!(
        settled.get("outcome").and_then(Value::as_str),
        Some("stop"),
        "{settled}"
    );
    assert!(settled.get("goal").is_some(), "{settled}");

    // Ids are consecutive from 1 within one process.
    assert!(
        ids.windows(2).all(|pair| pair[1] > pair[0]),
        "ids are increasing: {ids:?}"
    );
    assert_eq!(ids.first(), Some(&1), "?since=0 replays from the beginning");

    // The run is over, and the state says so.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if harness
            .state()
            .map(|state| state.is_idle())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the session never went idle again"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let transcript = client.transcript(None).expect("GET /transcript");
    assert_eq!(transcript.messages.len(), 2, "{:?}", transcript.raw);
    assert_eq!(
        transcript.messages[0].get("role").and_then(Value::as_str),
        Some("user")
    );

    // --- resume from the middle of the run ----------------------------------
    let pivot = ids[ids.len() / 2];
    let last = *ids.last().unwrap();
    drop(stream);

    let resumed = EventStream::start(
        StreamTarget::coordinator(&client, Some(pivot)),
        StreamConfig::default(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut got: Vec<i64> = Vec::new();
    while got.last().copied().unwrap_or(0) < last {
        match next_message(&resumed, deadline) {
            StreamMsg::Event { id: Some(id), .. } => got.push(id),
            StreamMsg::Event { id: None, .. } => {}
            StreamMsg::Connected { .. }
            | StreamMsg::Disconnected { .. }
            | StreamMsg::Reset { .. } => {}
            StreamMsg::Ended => panic!("the resumed stream ended early"),
        }
    }
    assert!(
        got.iter().all(|id| *id > pivot),
        "a resumed stream starts after the cursor: pivot {pivot}, got {got:?}"
    );
    assert!(
        got.contains(&last),
        "and reaches the event the run ended on: {got:?}"
    );
    drop(resumed);

    harness.shutdown().expect("POST /shutdown");
}

/// #3 — a server that never comes up carries its log tail, which is what the
/// tab shows (§3).
#[test]
fn boot_failure_carries_the_log_tail() {
    let _guard = one_swarm();
    let bins = bins();
    let dir = TempDir::new("swarm-client-bootfail").expect("temp dir");
    let config = ServerConfig::swarm(&bins.swarm, dir.path(), dir.path())
        .with_arg("--no-such-a-flag")
        .with_arg("--port-override-nonsense");
    let error = Server::start(&config).expect_err("a bad argument is a boot failure");
    match &error {
        Error::Boot(failure) => {
            assert_eq!(failure.exit_code, Some(64), "{failure:?}");
            assert!(
                failure.log_tail.contains("Unknown argument"),
                "the tail is what the server said: {:?}",
                failure.log_tail
            );
            assert_eq!(
                failure.log_path.as_deref(),
                Some(dir.join("swarm.log").as_path())
            );
        }
        other => panic!("expected a boot failure, got {other:?}"),
    }
    assert!(!error.log_tail().unwrap().is_empty());
    assert!(
        dir.join("swarm.log").is_file(),
        "the log is there to show next time"
    );
}

/// #4 — §9.4's probe: a throwaway `evo-agent serve` learns the model catalog,
/// with and without userspace.
#[test]
fn the_probe_learns_the_registry() {
    let _guard = one_swarm();
    let bins = bins();
    let dir = TempDir::new("swarm-client-probe").expect("temp dir");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("temp home");
    // A provider and a model, registered exactly as a user's init.lisp does.
    std::fs::write(
        home.join("init.lisp"),
        swarm_client::harness::stub_init_lisp(1, "probe-model"),
    )
    .expect("write init.lisp");

    let config_for = |no_userspace: bool| {
        ServerConfig::agent(&bins.agent, dir.path(), dir.path())
            .with_no_userspace(no_userspace)
            .with_env("EVO_HOME", home.to_string_lossy().into_owned())
            .with_env_removed("ANTHROPIC_API_KEY")
    };

    // With userspace: the session reports the model its init.lisp registered.
    let userspace = swarm_client::learn_registry_with(config_for(false)).expect("probe up");
    let model = userspace
        .models
        .iter()
        .find(|model| model.id == "probe-model")
        .unwrap_or_else(|| panic!("no probe-model in {:?}", userspace.raw));
    assert_eq!(model.api.as_deref(), Some("anthropic-messages"));
    assert!(userspace.apis.contains(&"anthropic-messages".to_owned()));
    assert!(userspace.lane_ready(model), "a lane can register it (§9.4)");

    // In quarantine: init.lisp is not read, so no model is registered — which is
    // exactly the catalog a lane starts from, and proves the flag is accepted.
    let quarantined = swarm_client::learn_registry_with(config_for(true)).expect("probe up");
    assert!(
        quarantined.models.is_empty(),
        "a --no-userspace probe registers no init.lisp models: {:?}",
        quarantined.raw
    );
    assert!(
        quarantined.apis.contains(&"anthropic-messages".to_owned()),
        "the kernel's own APIs are there: {:?}",
        quarantined.apis
    );

    // Both probes left nothing behind.
    assert!(
        !dir.join("token").exists(),
        "a clean shutdown removes its token file"
    );
}

/// #5 — `--port 0` lets the server choose, and the port it printed is the one
/// the client uses (§3).
#[test]
fn port_zero_reads_the_port_the_server_printed() {
    let _guard = one_swarm();
    let bins = bins();
    let dir = TempDir::new("swarm-client-port0").expect("temp dir");
    // No init.lisp: this test is about the port, not about a model.
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("temp home");
    let mut config = ServerConfig::swarm(&bins.swarm, dir.path(), dir.path())
        .with_workers(1)
        .with_evo(&bins.agent)
        .with_env("EVO_HOME", home.to_string_lossy().into_owned());
    config.port = Some(0);
    let mut server = Server::start(&config).expect("a `--port 0` swarm should come up");
    assert_ne!(server.port(), 0, "the port came from the server's own line");
    assert!(
        server.health().has_feature("swarm"),
        "{:?}",
        server.health()
    );
    assert!(
        server.log_tail(5).contains(&server.port().to_string()),
        "the log names the port it chose"
    );
    // This swarm is stopped while its lane is still starting, and a swarm walks
    // its just-booted lanes down before it exits — measured at 17.9 s for one
    // starting lane against 3.3 s for an idle one, so reaching `SIGTERM` here is
    // the ladder working, not failing. Either way it exits 0 on its own.
    let report = server.shutdown().expect("shutdown");
    assert_ne!(report.outcome, ShutdownOutcome::Killed, "{report:?}");
    assert_eq!(report.exit_code, Some(0), "{report:?}");
}

/// #6 — one app, several tabs: shutting every tab down at once (§3, §9.8).
#[test]
fn two_swarms_shut_down_in_parallel() {
    let _guard = one_swarm();
    let mut first = Harness::start(HarnessConfig {
        workers: 1,
        ..Default::default()
    })
    .expect("first swarm up");
    let mut second = Harness::start(HarnessConfig {
        workers: 1,
        ..Default::default()
    })
    .expect("second swarm up");
    let first_pid = first.server.pid();
    let second_pid = second.server.pid();
    assert_ne!(first_pid, second_pid);
    assert!(swarm_client::process_alive(first_pid) && swarm_client::process_alive(second_pid));

    let one = std::thread::spawn(move || first.shutdown());
    let two = std::thread::spawn(move || second.shutdown());
    let one = one.join().expect("thread").expect("POST /shutdown");
    let two = two.join().expect("thread").expect("POST /shutdown");
    // Both tabs shut down at once, each on its own thread; each stopped on its
    // own and exited 0 (see the note in the `--port 0` test on why a swarm whose
    // lanes are still starting can need `SIGTERM` after the 10 s grace).
    assert_ne!(one.outcome, ShutdownOutcome::Killed, "{one:?}");
    assert_ne!(two.outcome, ShutdownOutcome::Killed, "{two:?}");
    assert_eq!(one.exit_code, Some(0), "{one:?}");
    assert_eq!(two.exit_code, Some(0), "{two:?}");
    assert!(!swarm_client::process_alive(first_pid));
    assert!(!swarm_client::process_alive(second_pid));
}
