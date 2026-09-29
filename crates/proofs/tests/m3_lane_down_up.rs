//! m3 — a lane that dies: the left column says so, the swarm brings it back, and
//! the tab's model follows both.
//!
//! The lane runs under evo's in-binary supervisor (`src/cli/supervisor.lisp`): the
//! `evo-agent serve` the swarm launched re-spawns itself as the child and restarts
//! it with `--resume` after a crash. So the proof kills only the lane's own pid —
//! the one `GET /lanes` reports, which is the supervised child — and the swarm's
//! lane watcher (`swarm/lanes.lisp`'s `recover-lane`) notices the new process,
//! re-initializes the lane, counts a restart, and tells the coordinator.
//!
//! Run with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m3_lane_down_up`.

mod common;

use std::time::{Duration, Instant};

use common::{fixture, one_swarm, row_summary, spec, Drive};
use session::{AgentKey, LaneStatus};
use tab_engine::Agent;

#[test]
fn m3_lane_down_up() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let mut drive = Drive::start(spec(&fixture, 2));
    let deadline = Instant::now() + Duration::from_secs(240);

    drive.wait_connected(Agent::Coordinator, deadline);
    drive.wait_lanes_idle(deadline, 2);
    // The coordinator says who its lanes are; one of them is watched here so its
    // own model is loaded (the reason a down row shows prefers the lane's account).
    drive.select(AgentKey::Lane(1));

    // The pid the swarm reports for lane 1 — the supervised child under evo's
    // in-binary supervisor, the process whose death the supervisor notices and the
    // lane watcher follows. It arrives in the lane list the left column folded.
    let lane_pid = drive
        .model
        .lanes()
        .lane(1)
        .and_then(|row| row.pid)
        .expect("lane 1 has a pid") as i32;
    let coordinator_pid = drive.coordinator_pid.expect("/health named the coordinator") as i32;
    assert_ne!(lane_pid, coordinator_pid, "the lane is its own process");
    let restarts_before = drive.model.lanes().lane(1).map(|row| row.restarts).unwrap_or(0);
    assert_eq!(restarts_before, 0, "a fresh swarm has restarted nothing");
    eprintln!("m3: killing lane 1's process {lane_pid}");

    // --- kill the lane's process, and only it -------------------------------
    let killed = unsafe { libc::kill(lane_pid, libc::SIGKILL) };
    assert_eq!(killed, 0, "could not kill lane 1 (pid {lane_pid})");

    // --- the left column shows it down --------------------------------------
    // The swarm publishes the state as its lane watcher sees the stream end
    // (`swarm/lanes.lisp`'s `recover-lane`) — the row is red from here until the
    // lane is up again.
    drive.wait_for_update(deadline, "a lane-state event saying lane 1 is down", |update| {
        matches!(update, tab_engine::Update::Event { agent: Agent::Coordinator, kind, data, .. }
            if kind == "lane-state" && data["lane"] == 1 && data["state"] == "down")
    });
    drive.wait_model(deadline, "lane 1 down", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Down)
    });

    // --- the swarm brings it back -------------------------------------------
    // The supervisor restarts it, the lane watcher sees a new pid, re-initializes
    // the lane and counts the restart; the lane-state events carry both.
    let recovered = drive.wait_for_update(deadline, "lane 1 back to idle", |update| {
        matches!(update, tab_engine::Update::Event { agent: Agent::Coordinator, kind, data, .. }
            if kind == "lane-state" && data["lane"] == 1 && data["state"] == "idle"
                && data["restarts"].as_u64().unwrap_or(0) >= 1)
    });
    let recovered = match recovered {
        tab_engine::Update::Event { data, .. } => data,
        _ => unreachable!(),
    };
    assert!(
        recovered["pid"].as_u64().is_some_and(|pid| pid as i32 != lane_pid),
        "the lane came back as a new process: {recovered}"
    );
    let states = drive.lane_states(1);
    let down = states.iter().position(|state| state == "down");
    let up = states.iter().rposition(|state| state == "idle");
    assert!(
        matches!((down, up), (Some(d), Some(u)) if d < u),
        "lane 1's cycle, as the stream carried it: {states:?}"
    );
    drive.wait_model(deadline, "the restarted lane in the list", |model| {
        model.lanes().lane(1).is_some_and(|row| {
            row.status == LaneStatus::Idle && row.restarts >= 1 && row.pid.is_some()
        })
    });

    // --- what the tab says about it -----------------------------------------
    // The swarm's own account of the lane going down is on the coordinator's
    // stream, error-styled, and it is what a red row shows — see m3's note in
    // docs/proofs.md for why the row itself is back to `idle` by the time the
    // line lands (the swarm announces the crash *after* it has already
    // re-initialized the lane).
    let outputs = drive.events(Agent::Coordinator, "output");
    let announcement = outputs
        .iter()
        .position(|data| {
            data["style"].as_str() == Some("error")
                && data["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("[lane 1] crashed and was restarted"))
        })
        .unwrap_or_else(|| panic!("no restart announcement among {} output lines", outputs.len()));
    eprintln!("m3: the swarm said {}", outputs[announcement]["text"]);
    // ...and the restarted lane's own complaint about its fresh state ("no model is
    // configured yet", until the swarm re-runs its baseline) comes *after* it:
    // that ordering is what `session::TabModel::lane_down_reason` prefers the
    // announcement for.
    let complaint = outputs.iter().position(|data| {
        data["text"]
            .as_str()
            .is_some_and(|text| text.contains("[lane 1]") && text.contains("No model is configured"))
    });
    assert!(
        complaint.is_none_or(|complaint| announcement < complaint),
        "the swarm's account comes before what the lane says about itself: {outputs:#?}"
    );

    // A lane that is up claims nothing: the red row's reason lives only while the
    // row is red (§9.7 as the coordinator decided it).
    assert_eq!(
        drive.model.lane_down_reason(1),
        None,
        "a lane that is back up has no reason"
    );

    // A restarted lane is told apart from a lane that was never up: `restarts` is
    // the counter the left column's tooltip shows.
    let tooltip = drive.model.lanes().lane(1).expect("lane 1").tooltip();
    assert!(tooltip.contains("restart"), "the row says it restarted: {tooltip:?}");
    // ...and the row is no longer red, so nothing is claimed about it any more.
    assert_eq!(
        drive.model.lane_down_reason(1),
        None,
        "a lane that is up has no reason"
    );

    // --- work still reaches it ----------------------------------------------
    // The lane is re-initialized, so it can be given work again: the coordinator
    // delegates, and the lane's own stream shows it running.
    drive.prompt(1, format!("CALL delegate {}", serde_json::json!({
        "lane": 1, "task": "SLOW m3 lane one after the restart"
    })));
    drive.wait_event_where(deadline, Agent::Lane(1), "text-delta", "the restarted lane working", |_| true);
    drive.wait_model(deadline, "lane 1 idle after its new task", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });
    assert!(
        fixture.stub.find("lane 1", "m3 lane one after the restart", 0.0).is_some(),
        "the restarted lane really ran the task"
    );
    assert!(
        row_summary(drive.model.coordinator()).iter().any(|row| row.starts_with("tool:delegate")),
        "the coordinator's delegate row is there"
    );

    // --- nothing left running ----------------------------------------------
    drive.shutdown();
    drive.next(deadline, "Exited", |update| matches!(update, tab_engine::Update::Exited { .. }));
    drive.join_and_assert_gone(deadline);
}
