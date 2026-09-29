//! tab_engine against a REAL `/usr/local/bin/evo-swarm serve`.
//!
//! The environment is `swarm_client::harness`'s `Fixture`: a temp `HOME` whose
//! `init.lisp` registers the stub provider, a temp project, and `stub-messages.py`
//! as the model both the coordinator and the lanes talk to. Nothing here fakes
//! HTTP; the only thing scripted is the model.
//!
//! Run with `CARGO_TARGET_DIR=target/tab_engine cargo test -p tab_engine`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use async_channel::Receiver;
use serde_json::Value;
use swarm_client::harness::{Bins, Fixture, HarnessConfig, TempDir, STUB_MODEL};
use tab_engine::catalog::{self, CatalogUpdate};
use tab_engine::{Agent, Command, ShutdownReport, StreamStatus, TabEngine, TabSpec, Update};

/// One swarm at a time: a test run is not a load test, and each swarm is a
/// supervisor plus a lane process each.
static SWARM: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_swarm() -> std::sync::MutexGuard<'static, ()> {
    SWARM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fixture(workers: u16) -> Fixture {
    let bins = Bins::installed();
    assert!(
        bins.available(),
        "no binaries to test against: {} / {}",
        bins.swarm.display(),
        bins.agent.display()
    );
    Fixture::new(HarnessConfig {
        workers,
        ..Default::default()
    })
    .expect("the fixture should come up")
}

/// The tab spec a [`Fixture`] describes: its project, its tab directory, and the
/// hermetic environment the fixture sets up.
fn spec(fixture: &Fixture, workers: u16) -> TabSpec {
    let mut spec = TabSpec::new(&fixture.bins.swarm, &fixture.project, fixture.tab_dir())
        .with_agent_bin(&fixture.bins.agent)
        .with_workers(workers);
    for (key, value) in fixture.env() {
        spec = spec.with_env(key, value);
    }
    for key in fixture.env_remove() {
        spec = spec.with_env_removed(key);
    }
    spec
}

/// An ordered log of a tab's updates, with a cursor so a test can ask for "the
/// next transcript" rather than "any transcript".
struct Updates {
    rx: Receiver<Update>,
    all: Vec<Update>,
    cursor: usize,
}

impl Updates {
    fn new(rx: Receiver<Update>) -> Updates {
        Updates {
            rx,
            all: Vec::new(),
            cursor: 0,
        }
    }

    fn pump(&mut self) {
        while let Ok(update) = self.rx.try_recv() {
            self.all.push(update);
        }
    }

    /// The next update, from the cursor, that matches — consuming it.
    fn next(&mut self, deadline: Instant, pred: impl Fn(&Update) -> bool) -> Update {
        loop {
            self.pump();
            if let Some(offset) = self.all[self.cursor..].iter().position(&pred) {
                let at = self.cursor + offset;
                self.cursor = at + 1;
                return self.all[at].clone();
            }
            assert!(
                Instant::now() < deadline,
                "no matching update before the deadline; saw {} updates: {:?}",
                self.all.len(),
                self.all.iter().map(kind_of).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait for the coordinator's stream to be up.
    fn wait_connected(&mut self, agent: Agent, deadline: Instant) -> Update {
        loop {
            self.pump();
            let found = self.all.iter().any(|update| {
                matches!(update, Update::Stream { agent: a, status: StreamStatus::Connected } if *a == agent)
            });
            if found {
                // Advance the cursor past everything so far.
                self.cursor = self.all.len();
                return self.all.last().cloned().unwrap();
            }
            assert!(Instant::now() < deadline, "the stream never connected");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Everything from `from` on, for "must not happen" checks.
    fn since(&self, from: usize) -> &[Update] {
        &self.all[from.min(self.all.len())..]
    }

    fn len(&self) -> usize {
        self.all.len()
    }

    fn events(&self, agent: Agent, kind: &str) -> Vec<(Option<i64>, Value)> {
        self.all
            .iter()
            .filter_map(|update| match update {
                Update::Event {
                    agent: a,
                    id,
                    kind: k,
                    data,
                } if *a == agent && k == kind => Some((*id, data.clone())),
                _ => None,
            })
            .collect()
    }
}

fn kind_of(update: &Update) -> &'static str {
    match update {
        Update::Booting => "Booting",
        Update::Ready { .. } => "Ready",
        Update::BootFailed { .. } => "BootFailed",
        Update::Transcript { .. } => "Transcript",
        Update::State { .. } => "State",
        Update::Registry { .. } => "Registry",
        Update::Lanes { .. } => "Lanes",
        Update::Event { .. } => "Event",
        Update::Stream { .. } => "Stream",
        Update::CacheSeed { .. } => "CacheSeed",
        Update::PostResult { .. } => "PostResult",
        Update::ServerGone => "ServerGone",
        Update::Exited { .. } => "Exited",
    }
}

fn lane_state_is(updates: &Updates, lane: u64, state: &str) -> bool {
    updates.all.iter().any(|update| match update {
        Update::Event {
            agent: Agent::Coordinator,
            kind,
            data,
            ..
        } if kind == "lane-state" => {
            let n = data
                .get("lane")
                .or_else(|| data.get("n"))
                .and_then(Value::as_u64);
            n == Some(lane) && data.get("state").and_then(Value::as_str) == Some(state)
        }
        _ => false,
    })
}

/// Wait for a `lane-state` event on the coordinator's stream (§9.1's lane list).
fn wait_lane_state(updates: &mut Updates, lane: u64, state: &str, deadline: Instant) {
    loop {
        updates.pump();
        if lane_state_is(updates, lane, state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "lane {lane} never reported {state:?}; saw {:?}",
            updates
                .events(Agent::Coordinator, "lane-state")
                .iter()
                .map(|(_, data)| (data.get("lane").cloned(), data.get("state").cloned()))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// #1 — boot assembles the view (§9.1): Booting, Ready, registry, lanes,
/// transcript + state, the cache seed, and the live stream.
#[test]
fn boot_assembles_the_view() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let (handle, rx) = TabEngine::start(spec(&fixture, 2));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    updates.next(deadline, |u| matches!(u, Update::Booting));
    let ready = updates.next(deadline, |u| matches!(u, Update::Ready { .. }));
    let Update::Ready { health, pid, port } = ready else {
        unreachable!()
    };
    assert_eq!(health.name.as_deref(), Some("evo-swarm"), "{health:?}");
    assert!(health.has_feature("swarm"), "{health:?}");
    assert!(pid > 0 && port > 0, "{health:?} {pid} {port}");

    let registry = updates.next(deadline, |u| matches!(u, Update::Registry { .. }));
    let Update::Registry { raw } = registry else {
        unreachable!()
    };
    assert!(
        raw["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == STUB_MODEL),
        "{raw}"
    );

    let lanes = updates.next(deadline, |u| matches!(u, Update::Lanes { .. }));
    let Update::Lanes { raw } = lanes else {
        unreachable!()
    };
    assert_eq!(raw["lanes"].as_array().unwrap().len(), 2, "{raw}");
    assert_eq!(raw["swarm"]["workers"].as_u64(), Some(2), "{raw}");
    // Lanes are started by the coordinator and are idle within a moment; the
    // snapshot may catch them still starting, so their states are not asserted
    // here — the lane-state events that say otherwise arrive on the stream
    // (tests #3) and `session::LaneList` folds them.

    let transcript = updates.next(deadline, |u| matches!(u, Update::Transcript { .. }));
    match transcript {
        Update::Transcript {
            agent: Agent::Coordinator,
            revision,
            raw,
        } => {
            assert_eq!(revision, 1);
            assert!(raw["messages"].as_array().unwrap().is_empty(), "{raw}");
        }
        other => panic!(
            "expected the coordinator's transcript, got {}",
            kind_of(&other)
        ),
    }

    let state = updates.next(deadline, |u| matches!(u, Update::State { .. }));
    let Update::State { revision, raw } = state else {
        unreachable!()
    };
    assert_eq!(revision, 1);
    assert_eq!(raw["status"].as_str(), Some("idle"), "{raw}");
    assert_eq!(raw["model"].as_str(), Some(STUB_MODEL), "{raw}");

    // The cache seed arrives; without `340-cache-stats.lisp` in the temp HOME
    // there is no entry, which is the "extension absent" case.
    let seed = updates.next(deadline, |u| matches!(u, Update::CacheSeed { .. }));
    let Update::CacheSeed { entry } = seed else {
        unreachable!()
    };
    if let Some(entry) = entry {
        assert_eq!(entry["key"].as_str(), Some("cache-stats"), "{entry}");
    }

    // The live stream: Connected, then lane-state events as the lanes start.
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                status: StreamStatus::Connected,
                ..
            }
        )
    });

    // A refetch command is a fresh view at a higher revision.
    assert!(handle.send(Command::Refetch(Agent::Coordinator)));
    let refetched = updates.next(
        deadline,
        |u| matches!(u, Update::Transcript { revision, .. } if *revision >= 2),
    );
    assert!(matches!(refetched, Update::Transcript { revision, .. } if revision >= 2));

    handle.join();
}

/// #2 — a prompt runs, its text streams in, and `settled` triggers a resync with
/// a higher revision.
#[test]
fn prompt_streams_and_settles_with_a_resync() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let (handle, rx) = TabEngine::start(spec(&fixture, 1));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    updates.wait_connected(Agent::Coordinator, deadline);

    assert!(handle.prompt(7, "SLOW hello from the engine"));
    let posted = updates.next(deadline, |u| matches!(u, Update::PostResult { .. }));
    match posted {
        Update::PostResult {
            req_id,
            result: Ok(envelope),
        } => {
            assert_eq!(req_id, 7);
            assert!(envelope.ok);
            assert_eq!(
                envelope.data.get("queued"),
                Some(&Value::Bool(true)),
                "{envelope:?}"
            );
            assert!(envelope.task.is_some(), "{envelope:?}");
        }
        other => panic!("the prompt should have succeeded, got {}", kind_of(&other)),
    }

    // The run: message-start, the deltas, message-end, settled.
    updates.next(
        deadline,
        |u| matches!(u, Update::Event { kind, .. } if kind == "message-start"),
    );
    let last_delta = updates.next(
        deadline,
        |u| matches!(u, Update::Event { kind, .. } if kind == "settled"),
    );
    assert!(matches!(last_delta, Update::Event { kind, .. } if kind == "settled"));

    let text: String = updates
        .events(Agent::Coordinator, "text-delta")
        .iter()
        .filter_map(|(_, data)| data.get("text").and_then(Value::as_str))
        .collect();
    assert!(
        text.starts_with("slow0 slow1 "),
        "streamed text was {text:?}"
    );
    assert!(
        text.contains("slow59"),
        "the whole message arrived: {text:?}"
    );

    let usage = updates
        .events(Agent::Coordinator, "message-end")
        .first()
        .and_then(|(_, data)| data.get("usage").cloned())
        .expect("message-end carries usage");
    assert!(
        usage.get("input").and_then(Value::as_u64).is_some(),
        "{usage}"
    );

    // `settled` resyncs: a second transcript + state, revision 2.
    let transcript = updates.next(
        deadline,
        |u| matches!(u, Update::Transcript { revision, .. } if *revision >= 2),
    );
    match transcript {
        Update::Transcript {
            agent: Agent::Coordinator,
            revision,
            raw,
        } => {
            assert!(revision >= 2);
            let messages = raw["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 2, "{raw}");
            assert_eq!(messages[0]["role"].as_str(), Some("user"));
        }
        other => panic!("expected a resync transcript, got {}", kind_of(&other)),
    }
    let state = updates.next(
        deadline,
        |u| matches!(u, Update::State { revision, .. } if *revision >= 2),
    );
    assert!(matches!(state, Update::State { .. }));

    handle.join();
}

/// #3 — at most one lane stream: watching lane 1 streams its work, switching to
/// lane 2 closes it, and unwatching closes everything.
#[test]
fn watching_a_lane_switches_the_single_stream() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let (handle, rx) = TabEngine::start(spec(&fixture, 2));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(240);

    updates.wait_connected(Agent::Coordinator, deadline);

    // --- watch lane 1 --------------------------------------------------------
    assert!(handle.watch_lane(Some(1)));
    let transcript = updates.next(deadline, |u| {
        matches!(
            u,
            Update::Transcript {
                agent: Agent::Lane(1),
                ..
            }
        )
    });
    assert!(
        matches!(transcript, Update::Transcript { revision: 1, .. }),
        "a lane's first transcript is its first revision: {transcript:?}"
    );
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                agent: Agent::Lane(1),
                status: StreamStatus::Connected
            }
        )
    });

    // Delegate work to lane 1 and watch its events arrive.
    assert!(handle.prompt(
        1,
        r#"CALL delegate {"lane":1,"task":"engine lane one work"}"#
    ));
    updates.next(
        deadline,
        |u| matches!(u, Update::Event { agent: Agent::Lane(1), kind, .. } if kind == "text-delta"),
    );
    // The coordinator's stream carries the lane-state transition the lane list folds.
    wait_lane_state(&mut updates, 1, "working", deadline);

    // --- switch to lane 2 ----------------------------------------------------
    assert!(handle.watch_lane(Some(2)));
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Transcript {
                agent: Agent::Lane(2),
                ..
            }
        )
    });
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                agent: Agent::Lane(2),
                status: StreamStatus::Connected
            }
        )
    });
    let after_switch = updates.len();

    // Lane 1 runs again, delegated while we watch lane 2: its events must NOT
    // reach us — the old stream is closed.
    let t = now();
    assert!(handle.prompt(
        2,
        r#"CALL delegate {"lane":1,"task":"engine lane one again"}"#
    ));
    updates.next(
        deadline,
        |u| matches!(u, Update::Event { agent: Agent::Coordinator, kind, .. } if kind == "settled"),
    );
    assert!(
        fixture
            .stub
            .find("lane 1", "engine lane one again", t)
            .is_some(),
        "lane 1 should have run again (the assertion is otherwise vacuous)"
    );
    assert!(
        !updates.since(after_switch).iter().any(|u| matches!(
            u,
            Update::Event {
                agent: Agent::Lane(1),
                ..
            }
        )),
        "a lane-1 event arrived after the stream was switched away"
    );

    // --- unwatch -------------------------------------------------------------
    assert!(handle.watch_lane(None));
    let after_unwatch = updates.len();
    std::thread::sleep(Duration::from_millis(500));
    updates.pump();
    assert!(
        !updates.since(after_unwatch).iter().any(|u| matches!(
            u,
            Update::Event {
                agent: Agent::Lane(_),
                ..
            }
        )),
        "a lane event arrived after unwatching"
    );

    handle.join();
}

/// #4 — a refusal is a typed `PostResult`: `409 Not now` for a steer with no
/// run, and `400` for an empty prompt.
#[test]
fn refusals_come_back_typed() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let (handle, rx) = TabEngine::start(spec(&fixture, 1));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    updates.wait_connected(Agent::Coordinator, deadline);

    // 409: `/steer` with nothing running.
    assert!(handle.steer(1, "is anyone there?"));
    let refused = updates.next(deadline, |u| {
        matches!(u, Update::PostResult { req_id: 1, .. })
    });
    match refused {
        Update::PostResult {
            result: Err(error), ..
        } => {
            assert_eq!(error.status, Some(409), "{error:?}");
            assert!(error.not_now, "{error:?}");
            assert!(error.message.contains("no run to steer"), "{error:?}");
        }
        other => panic!("a steer with no run should be 409, got {}", kind_of(&other)),
    }

    // 400: an empty prompt.
    assert!(handle.prompt(2, ""));
    let empty = updates.next(deadline, |u| {
        matches!(u, Update::PostResult { req_id: 2, .. })
    });
    match empty {
        Update::PostResult {
            result: Err(error), ..
        } => {
            assert_eq!(error.status, Some(400), "{error:?}");
            assert!(!error.not_now, "{error:?}");
        }
        other => panic!("an empty prompt should be 400, got {}", kind_of(&other)),
    }

    // `/interrupt` with nothing to interrupt is *not* a refusal.
    assert!(handle.interrupt(3));
    let interrupted = updates.next(deadline, |u| {
        matches!(u, Update::PostResult { req_id: 3, .. })
    });
    assert!(
        matches!(interrupted, Update::PostResult { result: Ok(_), .. }),
        "interrupt with nothing running should still succeed"
    );

    handle.join();
}

/// #5 — the shutdown ladder runs on the handle's drop and leaves no process.
#[test]
fn shutdown_leaves_no_process() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let (mut handle, rx) = TabEngine::start(spec(&fixture, 1));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    let ready = updates.next(deadline, |u| matches!(u, Update::Ready { .. }));
    let Update::Ready { pid, .. } = ready else {
        unreachable!()
    };
    assert!(swarm_client::process_alive(pid));

    assert!(handle.shutdown());
    let exited = updates.next(deadline, |u| matches!(u, Update::Exited { .. }));
    let Update::Exited { outcome } = exited else {
        unreachable!()
    };
    assert_ne!(
        outcome,
        swarm_client::ShutdownOutcome::Killed,
        "{outcome:?}"
    );
    handle.join();
    assert!(
        !swarm_client::process_alive(pid),
        "the swarm process is gone"
    );
}

/// #6 — a server that never comes up carries its log tail (§3).
#[test]
fn boot_failure_carries_the_log_tail() {
    let _guard = one_swarm();
    let bins = Bins::installed();
    assert!(bins.available(), "no binaries to test against");
    let dir = TempDir::new("tab-engine-bootfail").expect("temp dir");
    let project = dir.join("proj");
    std::fs::create_dir_all(&project).expect("temp project");

    let spec = TabSpec::new(&bins.swarm, &project, dir.path())
        .with_agent_bin(&bins.agent)
        .with_arg("--no-such-a-flag");
    let (handle, rx) = TabEngine::start(spec);
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(60);

    updates.next(deadline, |u| matches!(u, Update::Booting));
    let failed = updates.next(deadline, |u| matches!(u, Update::BootFailed { .. }));
    match failed {
        Update::BootFailed { message, log_tail } => {
            assert!(
                log_tail.contains("Unknown argument"),
                "log tail was {log_tail:?} ({message})"
            );
        }
        other => panic!("expected a boot failure, got {}", kind_of(&other)),
    }
    handle.join();
}

/// #7 — the coordinator is restarted by its supervisor: the stream reconnects,
/// `hello` arrives, and the view resyncs (§5, §3).
#[test]
fn a_restarted_coordinator_resyncs() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let (handle, rx) = TabEngine::start(spec(&fixture, 1));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(240);

    let ready = updates.next(deadline, |u| matches!(u, Update::Ready { .. }));
    let Update::Ready { health, pid, .. } = ready else {
        unreachable!()
    };
    updates.wait_connected(Agent::Coordinator, deadline);
    // The coordinator runs as its supervisor's child, so its pid is not the
    // process this tab started.
    assert_ne!(
        health.pid, pid,
        "the coordinator should be a separate process"
    );
    let revision_before = updates
        .all
        .iter()
        .filter_map(|u| match u {
            Update::Transcript {
                agent: Agent::Coordinator,
                revision,
                ..
            } => Some(*revision),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    // Kill the coordinator; the supervisor restarts it with --resume.
    let killed = unsafe { libc::kill(health.pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "could not kill the coordinator");

    // The stream drops, then comes back...
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                agent: Agent::Coordinator,
                status: StreamStatus::Reconnecting { .. }
            }
        )
    });
    updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                agent: Agent::Coordinator,
                status: StreamStatus::Connected
            }
        )
    });
    // ...and the view is refetched with a higher revision.
    let resynced = updates.next(deadline, |u| {
        matches!(u, Update::Transcript { agent: Agent::Coordinator, revision, .. } if *revision > revision_before)
    });
    assert!(matches!(resynced, Update::Transcript { .. }));
    // The restart is announced in band by `hello` (ids begin again at 1), which
    // the stream replays from the start because `/health` named a new process.
    updates.next(deadline, |u| {
        matches!(u, Update::Event { agent: Agent::Coordinator, kind, id: Some(1), .. } if kind == "hello")
    });

    handle.join();
}

/// #8 — a server dying on its own is reported, and nothing is left running.
#[test]
fn a_dead_server_is_reported() {
    let _guard = one_swarm();
    let fixture = fixture(1);
    let (handle, rx) = TabEngine::start(spec(&fixture, 1));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    let ready = updates.next(deadline, |u| matches!(u, Update::Ready { .. }));
    let Update::Ready { pid, .. } = ready else {
        unreachable!()
    };
    updates.wait_connected(Agent::Coordinator, deadline);

    // The supervisor puts itself and everything it starts in one process group
    // (swarm_client spawns it that way), so one signal reaches the coordinator
    // and the lanes too.
    let killed = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "could not kill the swarm's process group");

    updates.next(deadline, |u| matches!(u, Update::ServerGone));
    updates.next(deadline, |u| matches!(u, Update::Exited { .. }));

    let gone = Instant::now() + Duration::from_secs(10);
    while swarm_client::process_alive(pid) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !swarm_client::process_alive(pid),
        "the swarm process is gone"
    );
    handle.join();
}

/// #9 — a tab shut down while it is still booting stops within about a second,
/// and nothing is left running.
#[test]
fn a_handle_stops_within_a_second_while_booting() {
    let _guard = one_swarm();
    let bins = Bins::installed();
    assert!(bins.available(), "no binaries to test against");
    let dir = TempDir::new("tab-engine-slowboot").expect("temp dir");
    let project = dir.join("proj");
    std::fs::create_dir_all(&project).expect("temp project");
    // A "server" that never writes a token and never exits: a boot that will
    // never become ready, which is exactly what the abort is for.
    let script = dir.join("slow-server.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 600\n").expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let (mut handle, rx) = TabEngine::start(TabSpec::new(&script, &project, dir.path()));
    let mut updates = Updates::new(rx);
    updates.next(Instant::now() + Duration::from_secs(20), |u| {
        matches!(u, Update::Booting)
    });
    // Let readiness polling get going.
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    assert!(handle.shutdown());
    handle.join();
    let took = started.elapsed();
    eprintln!("aborted boot took {took:?}");
    assert!(
        took < Duration::from_secs(2),
        "the aborted boot took {took:?}"
    );

    updates.pump();
    assert!(
        !updates
            .all
            .iter()
            .any(|u| matches!(u, Update::BootFailed { .. })),
        "an aborted boot is not a failure to show: {:?}",
        updates.all.iter().map(kind_of).collect::<Vec<_>>()
    );
    assert!(
        updates
            .all
            .iter()
            .any(|u| matches!(u, Update::Exited { .. })),
        "the tab says it stopped"
    );
    assert!(!running(&script), "the slow server is gone");
}

/// #10 — the §9.4 catalog probe: a userspace `evo-agent serve` for the registry
/// and a `--no-userspace` one for the kernel's api set, both stopped again.
#[test]
fn the_catalog_probe_learns_registry_and_kernel_apis() {
    let _guard = one_swarm();
    let bins = Bins::installed();
    assert!(bins.available(), "no binaries to test against");
    let dir = TempDir::new("tab-engine-catalog").expect("temp dir");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("temp home");
    // The model the userspace probe should find, registered as init.lisp does.
    std::fs::write(
        home.join("init.lisp"),
        swarm_client::harness::stub_init_lisp(1, "catalog-model"),
    )
    .expect("write init.lisp");
    let probe_dir = dir.join("probe");

    let updates = catalog::learn_with(
        &bins.agent,
        &probe_dir,
        vec![("EVO_HOME".to_owned(), home.to_string_lossy().into_owned())],
    );
    let update = updates.recv_blocking().expect("the probe should answer");
    match update {
        CatalogUpdate::Done {
            registry,
            kernel_apis,
        } => {
            let models = registry["models"].as_array().expect("models");
            assert!(
                models.iter().any(|model| model["id"] == "catalog-model"),
                "the userspace registry should carry the init.lisp model: {registry}"
            );
            assert!(
                registry["apis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|api| api == "anthropic-messages"),
                "{registry}"
            );
            let apis = kernel_apis.expect("the --no-userspace probe should have answered");
            assert!(
                apis.iter().any(|api| api == "anthropic-messages"),
                "{apis:?}"
            );
        }
        CatalogUpdate::Failed { message, log_tail } => {
            panic!("catalog failed: {message}\n{log_tail}")
        }
    }

    // Both probes stopped the ladder's way: it removes the token it wrote.
    assert!(
        !probe_dir.join("userspace/token").exists(),
        "the userspace probe kept its token"
    );
    assert!(
        !probe_dir.join("kernel/token").exists(),
        "the kernel probe kept its token"
    );
}

/// #11 — every tab stops at once (§3, §9.8).
#[test]
fn shutdown_all_stops_every_tab() {
    let _guard = one_swarm();
    let first = fixture(1);
    let second = fixture(1);
    let (first_handle, first_rx) = TabEngine::start(spec(&first, 1));
    let (second_handle, second_rx) = TabEngine::start(spec(&second, 1));
    let mut first_updates = Updates::new(first_rx);
    let mut second_updates = Updates::new(second_rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    let first_pid = ready_pid(&mut first_updates, deadline);
    let second_pid = ready_pid(&mut second_updates, deadline);
    assert_ne!(first_pid, second_pid);

    let report: ShutdownReport =
        tab_engine::shutdown_all(vec![first_handle, second_handle], Duration::from_secs(60));
    assert!(report.all_exited(), "{report:?}");
    assert_eq!(report.exited.len(), 2, "{report:?}");
    assert!(
        !swarm_client::process_alive(first_pid),
        "the first swarm is gone"
    );
    assert!(
        !swarm_client::process_alive(second_pid),
        "the second swarm is gone"
    );
}

/// #12 — the lane list is read again when a lane's launch announcement says it is
/// still `starting`.
///
/// The swarm publishes `starting` when it launches a lane and then never announces
/// that the lane came up (`swarm/lanes.lisp`'s `sync-lane-state :announce nil`), so a
/// client that only folds events shows every lane as `starting` (◌) for the rest of
/// the session — the assembly snapshot is read while they boot.
#[test]
fn a_lane_that_came_up_is_read_back_idle() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let (handle, rx) = TabEngine::start(spec(&fixture, 2));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(150);

    updates.wait_connected(Agent::Coordinator, deadline);

    // An idle list arrives: both lanes up, which the assembly snapshot did not say.
    let idle = updates.next(deadline, |update| match update {
        Update::Lanes { raw } => raw["lanes"].as_array().is_some_and(|lanes| {
            lanes.len() == 2 && lanes.iter().all(|lane| lane["state"] == "idle")
        }),
        _ => false,
    });
    assert!(matches!(idle, Update::Lanes { .. }));

    // It is a *second* read, not the first: the engine asked again after the launch
    // announcement rather than the snapshot happening to catch them idle.
    let reads = updates
        .all
        .iter()
        .filter(|update| matches!(update, Update::Lanes { .. }))
        .count();
    assert!(reads >= 2, "the lane list was read {reads} time(s)");

    handle.join();
}

/// #13 — a lane shown while it is *down* gets its rows when it comes back.
///
/// The lane rows are on screen from the lane list on, and a red one is exactly the
/// row a reader clicks to see what happened — while the lane's own server is dead,
/// so the fetch that showing a lane makes cannot answer. That fetch must not be the
/// end of it: `resync` spends a revision only on a view that came back, so a lane no
/// view was ever read for is still at revision zero, and the first connect of its
/// stream is the moment to read it. Without that, the lane showed an empty middle
/// column until some unrelated reconnect happened to resync it.
#[test]
fn a_lane_shown_while_it_is_down_fills_its_rows_when_it_returns() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let (handle, rx) = TabEngine::start(spec(&fixture, 2));
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(240);

    updates.wait_connected(Agent::Coordinator, deadline);
    // Both lanes up and idle before anything is delegated: `delegate` refuses a lane
    // that is not idle, and a lane still booting is not.
    updates.next(deadline, |u| match u {
        Update::Lanes { raw } => raw["lanes"].as_array().is_some_and(|lanes| {
            lanes.len() == 2 && lanes.iter().all(|lane| lane["state"] == "idle")
        }),
        _ => false,
    });

    // Lane 1 runs something first, so its transcript has rows to come back with —
    // and so a transcript that arrives empty cannot pass for the real thing.
    let t = now();
    assert!(handle.prompt(
        1,
        r#"CALL delegate {"lane":1,"task":"engine lane one pre-crash"}"#
    ));
    let ran = Instant::now() + Duration::from_secs(120);
    while fixture
        .stub
        .find("lane 1", "engine lane one pre-crash", t)
        .is_none()
    {
        assert!(Instant::now() < ran, "lane 1 never ran the delegated task");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Crash it the way a crash does.
    let pid = lane_pid(&mut updates, deadline, 1);
    let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "could not kill lane 1");

    // Show it now, while it is red: this is the fetch with nothing behind it.
    let before = updates.len();
    assert!(handle.watch_lane(Some(1)));
    std::thread::sleep(Duration::from_millis(750));
    updates.pump();
    assert!(
        !updates.since(before).iter().any(|u| matches!(
            u,
            Update::Transcript {
                agent: Agent::Lane(1),
                ..
            }
        )),
        "the lane's server is down, so there are no rows to fetch yet"
    );

    // Its supervisor brings it back, its stream connects, and *that* is when the
    // rows arrive — as the lane's first revision, since no view of it was ever read.
    let transcript = updates.next(deadline, |u| {
        matches!(
            u,
            Update::Transcript {
                agent: Agent::Lane(1),
                ..
            }
        )
    });
    match &transcript {
        Update::Transcript {
            agent: Agent::Lane(1),
            revision,
            raw,
        } => {
            assert_eq!(
                *revision, 1,
                "a revision means a view was read: {transcript:?}"
            );
            let messages = raw["messages"]
                .as_array()
                .expect("a transcript carries messages");
            assert!(!messages.is_empty(), "the resumed lane's own rows: {raw}");
        }
        other => panic!("expected lane 1's transcript, got {}", kind_of(other)),
    }

    handle.join();
}

/// The pid the lane list gives for LANE — the process to kill.
fn lane_pid(updates: &mut Updates, deadline: Instant, lane: u64) -> u32 {
    let found = updates.next(deadline, |u| match u {
        Update::Lanes { raw } => raw["lanes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row["n"].as_u64() == Some(lane) && row["pid"].as_u64().is_some()),
        _ => false,
    });
    match found {
        Update::Lanes { raw } => raw["lanes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["n"].as_u64() == Some(lane))
            .and_then(|row| row["pid"].as_u64())
            .map(|pid| pid as u32)
            .expect("the lane list names its pid"),
        _ => unreachable!(),
    }
}

/// #14 — a server that goes silent is noticed, and a `POST` does not hold the tab.
///
/// `SIGSTOP` is not a crash: the swarm is alive and answers nothing at all — no
/// keepalives, no replies — so the only thing that can end the tab's belief that it
/// is connected is the client's own patience. Two of them, independent: the stream's
/// (no bytes for the stream timeout ⇒ reconnecting) and the request's (no reply for
/// the request timeout ⇒ the `POST` comes back as an error). The `POST` runs on a
/// thread of its own, so the stream's news reaches the UI *while* the `POST` is still
/// waiting — which is what the order of the two updates below says, with the stream's
/// patience deliberately far shorter than the request's.
#[test]
fn a_silent_swarm_is_noticed_while_a_post_waits() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let spec = spec(&fixture, 2).with_http_timeouts(Duration::from_secs(6), Duration::from_secs(1));
    let (mut handle, rx) = TabEngine::start(spec);
    let mut updates = Updates::new(rx);
    let deadline = Instant::now() + Duration::from_secs(120);

    let pid = ready_pid(&mut updates, deadline);
    updates.wait_connected(Agent::Coordinator, deadline);

    // Freeze it: every process this test started, and nothing else.
    let stopped = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGSTOP) };
    assert_eq!(stopped, 0, "could not stop the swarm");

    // A turn typed at a frozen swarm: the POST goes out and nothing comes back.
    assert!(handle.prompt(1, "the frozen swarm cannot take this"));

    let reconnecting = updates.next(deadline, |u| {
        matches!(
            u,
            Update::Stream {
                agent: Agent::Coordinator,
                status: StreamStatus::Reconnecting { .. }
            }
        )
    });
    assert!(matches!(reconnecting, Update::Stream { .. }));

    // …and the request ends as a typed failure rather than hanging.
    let posted = updates.next(deadline, |u| matches!(u, Update::PostResult { .. }));
    match posted {
        Update::PostResult { req_id, result } => {
            assert_eq!(req_id, 1);
            let error = result.expect_err("a POST nobody answered is not a success");
            eprintln!("the frozen POST said: {error:?}");
            assert!(
                error.status.is_none(),
                "a silence is not a status: {error:?}"
            );
        }
        other => panic!("expected a post result, got {}", kind_of(&other)),
    }

    // Which came first: the stream's notice, while the POST was still waiting. Had
    // the POST been issued on the engine's own loop, its answer would have landed
    // before the stream's queued message was ever looked at.
    let stream_at = updates
        .all
        .iter()
        .position(|u| {
            matches!(
                u,
                Update::Stream {
                    agent: Agent::Coordinator,
                    status: StreamStatus::Reconnecting { .. }
                }
            )
        })
        .expect("the stream gave up");
    let post_at = updates
        .all
        .iter()
        .position(|u| matches!(u, Update::PostResult { .. }))
        .expect("the POST came back");
    assert!(
        stream_at < post_at,
        "the stream's news arrived while the POST was still waiting"
    );

    // Let it breathe, then leave the ladder to do its work.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGCONT) };
    handle.shutdown();
    updates.next(deadline, |u| matches!(u, Update::Exited { .. }));
    let gone = Instant::now() + Duration::from_secs(60);
    while swarm_client::process_alive(pid) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!swarm_client::process_alive(pid), "the swarm is gone");
    handle.join();
}

fn ready_pid(updates: &mut Updates, deadline: Instant) -> u32 {
    match updates.next(deadline, |u| matches!(u, Update::Ready { .. })) {
        Update::Ready { pid, .. } => pid,
        _ => unreachable!(),
    }
}

/// Whether a process whose command line names PATH is running.
fn running(path: &Path) -> bool {
    let Ok(output) = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(path)
        .output()
    else {
        // No pgrep: the timing assertion above is the real evidence.
        return false;
    };
    output.status.success() && !output.stdout.is_empty()
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
