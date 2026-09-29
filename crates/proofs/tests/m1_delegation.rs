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
use tab_engine::{Agent, Update};

#[test]
fn m1_delegation() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let mut drive = Drive::start(spec(&fixture, 2));
    let deadline = Instant::now() + Duration::from_secs(240);

    // --- the tab is up, and its lanes are really up ------------------------
    drive.wait_connected(Agent::Coordinator, deadline);
    drive.wait_for_lane_rows(deadline, 2);
    // The event stream never announces a lane going idle before it was ever given
    // work (`swarm/lanes.lisp`'s `sync-lane-state :announce nil`), so the swarm is
    // asked: a delegation needs its lane genuinely idle, and this is how a client
    // learns that.
    drive.wait_lanes_idle(deadline, 2);

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
    // The left column folds `lane-state`: assert the whole cycle, not just the end.
    drive.wait_model(deadline, "lane 1 working", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Working)
    });
    drive.wait_model(deadline, "lane 1 idle again", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });
    let states = drive.lane_states(1);
    let working = states.iter().position(|state| state == "working");
    let idle = states.iter().rposition(|state| state == "idle");
    assert!(
        matches!((working, idle), (Some(w), Some(i)) if w < i),
        "lane 1's cycle, as the stream carried it: {states:?}"
    );

    // The lane's own work: the todo tool call, and the checklist it left behind.
    drive.wait_for_update(deadline, "the lane's todo call", |update| {
        matches!(update, Update::Event { agent: Agent::Lane(1), kind, data, .. }
            if kind == "tool-call-start" && data["name"] == "todo")
    });
    let todo_event = drive.wait_for_update(deadline, "todo-changed", |update| {
        matches!(update, Update::Event { agent: Agent::Lane(1), kind, .. } if kind == "todo-changed")
    });
    let todo_event = match todo_event {
        Update::Event { data, .. } => data,
        _ => unreachable!(),
    };
    assert_eq!(
        todo_event["todos"].as_array().map(Vec::len),
        Some(2),
        "the lane's checklist came through its own stream: {todo_event}"
    );

    // --- what the two models hold -------------------------------------------
    // The coordinator: the delegate call as a tool row, completed by the swarm's
    // own answer.
    drive.wait_model(deadline, "the delegate row and its result", |model| {
        model.coordinator().rows().iter().any(|row| matches!(&row.kind, RowKind::Tool { name, result: Some(_), .. }
            if name == "delegate"))
    });
    let coordinator = drive.model.coordinator();
    let rows = row_summary(coordinator);
    let delegate = coordinator
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Tool { name, arguments, result, .. } if name == "delegate" => {
                Some((arguments.clone(), result.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no delegate tool row; rows: {rows:?}"));
    assert!(
        delegate.0.contains("m1 lane one step"),
        "the row carries the task the coordinator gave: {}",
        delegate.0
    );
    let result = delegate.1.unwrap_or_else(|| panic!("the delegate call has no result; rows: {rows:?}"));
    assert!(
        result.content.contains("Delegated to lane 1"),
        "the swarm answered the call: {}",
        result.content
    );
    assert!(!result.is_error, "the delegation succeeded: {result:?}");

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
    drive.wait_model(deadline, "lane 1 working on the report task", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Working)
    });
    drive.wait_model(deadline, "the lane's report in the coordinator's transcript", |model| {
        model.coordinator().rows().iter().any(|row| matches!(&row.kind, RowKind::User { text }
            if text.contains("[lane 1 report]") && text.contains("m1 lane one delivered")))
    });
    drive.wait_model(deadline, "lane 1 idle after reporting", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });

    // ...and the lane's own report reaches its own model too. It arrives as a
    // `report` event, but a report is also a *tool call*, so the resync that ends
    // the run rebuilds the lane's rows from its transcript and shows it there —
    // the event-only row it made is replaced, exactly as an `output` line is.
    drive.wait_model(deadline, "the lane's report call", |model| {
        model.lane_model(1).is_some_and(|lane| {
            lane.rows().iter().any(|row| matches!(&row.kind,
                RowKind::Tool { name, result: Some(result), .. }
                    if name == "report" && result.content.contains("Delivered to the coordinator")))
        })
    });
    assert!(
        !drive.events(Agent::Lane(1), "report").is_empty(),
        "the lane's `report` event came through its own stream"
    );

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
