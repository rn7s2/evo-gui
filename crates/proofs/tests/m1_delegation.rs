//! m1 — delegation: the coordinator hands lane 1 a task, the lane works, and what
//! it says comes back to the coordinator as input.
//!
//! What is real: one `evo-swarm serve`, its two lanes, and the model both talk to
//! (scripted — `CALL <tool> {json}` in the last user turn becomes that tool call,
//! `DELAY n` waits first). What is asserted on: the `session::TabModel` the tab
//! page renders, folded from the engine's updates exactly as the page folds them.
//!
//! Run with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m1_delegation`.

mod common;

use std::time::{Duration, Instant};

use common::{fixture, one_swarm, row_summary, spec, Drive};
use serde_json::json;
use session::{AgentKey, LaneStatus, RowKind};
use tab_engine::Agent;

#[test]
fn m1_delegation() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let mut drive = Drive::start(spec(&fixture, 2));
    let deadline = Instant::now() + Duration::from_secs(240);

    // --- the tab is up, and its lanes are idle -------------------------------
    drive.wait_connected(Agent::Coordinator, deadline);
    drive.wait_model(deadline, "two idle lanes", |model| {
        model.lanes().lanes.len() == 2
            && model.lanes().lanes.iter().all(|row| row.status == LaneStatus::Idle)
    });

    // --- watch lane 1, as showing it does ------------------------------------
    // `Command::WatchLane` opens that one lane's stream and seeds its transcript;
    // the lane's own events then land in its own model.
    drive.select(AgentKey::Lane(1));
    drive.next(deadline, "lane 1's transcript", |update| {
        matches!(update, tab_engine::Update::Transcript { agent: Agent::Lane(1), .. })
    });
    drive.wait_connected(Agent::Lane(1), deadline);

    // --- the coordinator delegates ------------------------------------------
    // The task is the lane's whole world: the lane cannot see the coordinator's
    // conversation, so the task text is what its model is asked to do. `DELAY3`
    // keeps it working long enough for the lane-state transitions to be observed.
    let lane_task = format!(
        "DELAY3 CALL todo {}",
        json!({ "items": [
            { "text": "m1 lane one step", "status": "in-progress" },
            { "text": "m1 lane one rest", "status": "pending" },
        ]})
    );
    drive.prompt(1, format!("CALL delegate {}", json!({ "lane": 1, "task": lane_task })));

    // --- the lane works, then goes idle, on the coordinator's own stream -----
    // The left column folds `lane-state`; assert the transition, not just the end.
    drive.wait_event_where(deadline, Agent::Coordinator, "lane-state", "lane 1 working", |data| {
        data["lane"] == 1 && data["state"] == "working"
    });
    assert_eq!(
        drive.model.lanes().lane(1).map(|row| row.status),
        Some(LaneStatus::Working),
        "the left column shows lane 1 working"
    );

    // The lane's own work: the todo tool call, and the checklist it left behind.
    drive.wait_event_where(deadline, Agent::Lane(1), "tool-call-start", "the lane's todo call", |data| {
        data["name"] == "todo"
    });
    let todo_event = drive.wait_event_where(deadline, Agent::Lane(1), "todo-changed", "todo-changed", |_| true);
    assert_eq!(
        todo_event["todos"].as_array().map(Vec::len),
        Some(2),
        "the lane's checklist came through its own stream: {todo_event}"
    );
    drive.wait_event_where(deadline, Agent::Lane(1), "settled", "the lane settling", |_| true);
    drive.wait_event_where(deadline, Agent::Coordinator, "lane-state", "lane 1 idle", |data| {
        data["lane"] == 1 && data["state"] == "idle"
    });

    // --- what the two models hold -------------------------------------------
    // The coordinator: the delegate call as a tool row, completed by its result,
    // and the lane's run-end notice arriving as input.
    let coordinator = drive.model.coordinator();
    let rows = row_summary(coordinator);
    let delegate = coordinator.rows().iter().find_map(|row| match &row.kind {
        RowKind::Tool { name, result, .. } if name == "delegate" => Some(result.clone()),
        _ => None,
    });
    let delegate = delegate.unwrap_or_else(|| panic!("no delegate tool row; rows: {rows:?}"));
    assert!(delegate.is_some(), "the delegate call was completed by its result; rows: {rows:?}");
    assert!(
        coordinator.rows().iter().any(|row| matches!(&row.kind, RowKind::Assistant { markdown, .. }
            if markdown.contains("Delegated to lane 1"))),
        "the lane's line is in the coordinator's transcript; rows: {rows:?}"
    );

    // The lane: its own rows, and the checklist its script made.
    let lane = drive.model.lane_model(1).expect("lane 1 has a model");
    let lane_rows = row_summary(lane);
    assert!(
        lane_rows.iter().any(|row| row.starts_with("user:")),
        "the lane's model holds its turn: {lane_rows:?}"
    );
    assert!(
        lane_rows.iter().any(|row| row == "tool:todo✓"),
        "the lane's todo call, with its result: {lane_rows:?}"
    );
    assert_eq!(
        lane.todos().iter().map(|todo| todo.text.as_str()).collect::<Vec<_>>(),
        vec!["m1 lane one step", "m1 lane one rest"],
        "the checklist the lane's script left behind"
    );

    // --- a lane that reports is heard ---------------------------------------
    // The second delegation's task is a `report` call: the lane's report is what
    // the swarm relays to the coordinator as input, which is how a lane is ever
    // heard from.
    let before = drive.cursor();
    drive.prompt(2, format!(
        "CALL delegate {}",
        json!({ "lane": 1, "task": "CALL report {\"done\":\"m1 lane one delivered\",\"evidence\":\"the stub ran\",\"goal\":\"active\"}" })
    ));
    drive.wait_model(deadline, "the lane's report in the coordinator's transcript", |model| {
        model.coordinator().rows().iter().any(|row| matches!(&row.kind, RowKind::User { text }
            if text.contains("[lane 1 report]") && text.contains("m1 lane one delivered")))
    });
    assert_eq!(drive.lane_state(1), Some("idle".to_string()), "the lane settled again");

    // ...and the lane's own report row is in its own model.
    drive.wait_model(deadline, "the lane's report row", |model| {
        model.lane_model(1).is_some_and(|lane| {
            lane.rows().iter().any(|row| matches!(&row.kind, RowKind::Report { done, .. }
                if done.contains("m1 lane one delivered")))
        })
    });

    // Nothing ran twice: one delegate row per attempt.
    let delegates = drive
        .model
        .coordinator()
        .rows()
        .iter()
        .filter(|row| matches!(&row.kind, RowKind::Tool { name, .. } if name == "delegate"))
        .count();
    eprintln!("delegate rows after two prompts: {delegates} (updates since the report attempt: {})",
        drive.updates().len() - before);

    // --- and the ladder leaves nothing behind -------------------------------
    drive.shutdown();
    drive.next(deadline, "Exited", |update| matches!(update, tab_engine::Update::Exited { .. }));
    drive.join_and_assert_gone(deadline);
}
