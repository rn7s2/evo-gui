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

    // --- the tab is up, and its lanes are up ---------------------------------
    drive.wait_connected(Agent::Coordinator, deadline);
    // The left column's own model says so: the engine reads the lane list back when
    // a launch announcement says a lane is still `starting`, because the swarm
    // never announces a lane coming up idle after that (`swarm/lanes.lisp`'s
    // `sync-lane-state :announce nil`).
    drive.wait_lanes_idle(deadline, 2);

    // --- watch lane 1, as showing it does ------------------------------------
    // `Command::WatchLane` opens that one lane's stream and seeds its transcript;
    // the lane's own events then land in its own model.
    drive.select(AgentKey::Lane(1));
    drive.next(deadline, "lane 1's transcript", |update| {
        matches!(
            update,
            tab_engine::Update::Transcript {
                agent: Agent::Lane(1),
                ..
            }
        )
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
    drive.prompt(
        1,
        format!("CALL delegate {}", json!({ "lane": 1, "task": lane_task })),
    );

    // --- the lane works, then goes idle, on the coordinator's own stream -----
    // The left column folds `lane-state`: assert the whole cycle, not just the end.
    drive.wait_model(deadline, "lane 1 working", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Working)
    });
    drive.wait_model(deadline, "lane 1 idle again", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });
    // The row can reach idle a moment before the swarm's idle *announcement* does: a
    // lane's own `settled` makes the engine read `/lanes` (see
    // `m1_the_lane_list_follows_a_lane_to_idle`), and that snapshot says idle before
    // the coordinator's stream carries the event. The cycle below is the stream's, so
    // wait for the event itself.
    drive.wait_for_update(deadline, "lane 1 idle on the stream", |update| {
        matches!(update, Update::Event { agent: Agent::Coordinator, kind, data, .. }
            if kind == "lane-state" && data["lane"] == 1 && data["state"] == "idle")
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
        model.coordinator().rows().iter().any(|row| {
            matches!(&row.kind, RowKind::Tool { name, result: Some(_), .. }
            if name == "delegate")
        })
    });
    let coordinator = drive.model.coordinator();
    let rows = row_summary(coordinator);
    let delegate = coordinator
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Tool {
                name,
                arguments,
                result,
                ..
            } if name == "delegate" => Some((arguments.clone(), result.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no delegate tool row; rows: {rows:?}"));
    assert!(
        delegate.0.contains("m1 lane one step"),
        "the row carries the task the coordinator gave: {}",
        delegate.0
    );
    let result = delegate
        .1
        .unwrap_or_else(|| panic!("the delegate call has no result; rows: {rows:?}"));
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
        lane.todos()
            .iter()
            .map(|todo| todo.text.as_str())
            .collect::<Vec<_>>(),
        vec!["m1 lane one step", "m1 lane one rest"],
        "the checklist the lane's script left behind"
    );

    // --- a lane that reports is heard ---------------------------------------
    // The second delegation's task is a `report` call: the lane's report is what
    // the swarm relays to the coordinator as input, which is how a lane is ever
    // heard from. The relay arrives as a user-role message — `[lane 1 report] done:
    // m1 lane one delivered\nevidence: the stub ran` (`report-text`,
    // `swarm/lanes.lisp:220`) — and the session reads it as the report it is, naming
    // the lane it came from rather than as one of the lane's own rows (whose reports
    // carry no lane, having come from its stream).
    let before = drive.cursor();
    drive.prompt(2, format!(
        "CALL delegate {}",
        json!({ "lane": 1, "task": "CALL report {\"done\":\"m1 lane one delivered\",\"evidence\":\"the stub ran\",\"goal\":\"active\"}" })
    ));
    drive.wait_model(deadline, "lane 1 working on the report task", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Working)
    });
    drive.wait_model(
        deadline,
        "the lane's report in the coordinator's transcript",
        |model| {
            model.coordinator().rows().iter().any(|row| {
                matches!(&row.kind, RowKind::Report { lane: Some(1), done, evidence, .. }
                    if done == "m1 lane one delivered" && evidence == "the stub ran")
            })
        },
    );
    drive.wait_model(deadline, "lane 1 idle after reporting", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });

    // ...and the lane's own report reaches its own model too. It arrives as a
    // `report` event, but a report is also a *tool call*, so the resync that ends
    // the run rebuilds the lane's rows from its transcript and shows it there —
    // the event-only row it made is replaced, exactly as an `output` line is.
    drive.wait_model(deadline, "the lane's report call", |model| {
        model.lane_model(1).is_some_and(|lane| {
            lane.rows().iter().any(|row| {
                matches!(&row.kind,
                RowKind::Tool { name, result: Some(result), .. }
                    if name == "report" && result.content.contains("Delivered to the coordinator"))
            })
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
    eprintln!(
        "delegate rows after two prompts: {delegates} (updates since the report attempt: {})",
        drive.updates().len() - before
    );

    // --- and the ladder leaves nothing behind -------------------------------
    drive.shutdown();
    drive.next(deadline, "Exited", |update| {
        matches!(update, tab_engine::Update::Exited { .. })
    });
    drive.join_and_assert_gone(deadline);
}

/// The left column keeps up with a lane's *work*, not only with the list it was
/// last handed.
///
/// A row's glyph follows the `lane-state` events, which the coordinator's stream
/// carries; the header's count and the row's clocks (`swarm.busy`, the step clock)
/// are `GET /lanes`' alone, and nothing read that list when a lane's own run ended —
/// so the header could still count a lane busy while its row said idle. An earlier
/// capture showed that frame: "2 lanes · 1 busy" over a lane
/// whose run was over. What ends a lane's run is its own `settled` (§7.3), which the
/// tab hears because the lane is the one it watches.
#[test]
fn m1_the_lane_list_follows_a_lane_to_idle() {
    let _guard = one_swarm();
    let fixture = fixture(2);
    let mut drive = Drive::start(spec(&fixture, 2));
    let deadline = Instant::now() + Duration::from_secs(240);

    drive.wait_connected(Agent::Coordinator, deadline);
    drive.select(AgentKey::Lane(1));
    drive.wait_lanes_idle(deadline, 2);

    // A lane's own turn, and a slow one: `SLOW` makes the stub stream its answer
    // over seconds, so the row is working for a while rather than for a millisecond.
    drive.prompt(
        1,
        r#"CALL delegate {"lane":1,"task":"SLOW write up what you changed"}"#,
    );
    drive.wait_for_update(deadline, "the lane streaming", |update| {
        matches!(update, Update::Event { agent: Agent::Lane(1), kind, .. } if kind == "text-delta")
    });
    // While it streams, its row is the working glyph — the centre header's too.
    drive.wait_model(deadline, "lane 1 working", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Working)
    });

    // And the header counts it: `/lanes` is read while the lane is still working
    // (the coordinator's run settled first — its own read-back).
    drive.wait_model(deadline, "the header counting the busy lane", |model| {
        model.lanes().busy() == 1
    });

    // The run ends. The lane says so on the stream being watched.
    drive.wait_for_update(deadline, "the lane settling", |update| {
        matches!(update, Update::Event { agent: Agent::Lane(1), kind, .. } if kind == "settled")
    });
    drive.wait_model(deadline, "lane 1 idle", |model| {
        model.lanes().lane(1).map(|row| row.status) == Some(LaneStatus::Idle)
    });
    // The header has to agree with the rows it sits above: the lane is up and idle,
    // so nothing is busy.
    drive.wait_model(deadline, "the header back to nothing busy", |model| {
        model.lanes().busy() == 0
    });

    // …and the tab asks the swarm, rather than going on looking at the list it read
    // while the lane was working: a fresh `/lanes` arrives, with lane 1 idle and the
    // swarm's own count at zero. (The old engine read the list at boot and on a
    // launch announcement, so nothing it held could have moved on its own.)
    let mark = drive.updates().len();
    let read = drive.wait_for_update_since(
        mark,
        deadline,
        "a lane list read after the lane went idle",
        |update| {
            matches!(update, Update::Lanes { raw }
            if raw["swarm"]["busy"].as_u64() == Some(0)
                && raw["lanes"].as_array().into_iter().flatten().any(|lane| {
                    lane["n"] == 1 && lane["state"] == "idle"
                }))
        },
    );
    let Update::Lanes { raw } = read else {
        unreachable!()
    };
    eprintln!(
        "m1: the list came back with busy {}: {}",
        raw["swarm"]["busy"], raw["lanes"]
    );

    drive.shutdown();
    drive.next(deadline, "Exited", |update| {
        matches!(update, Update::Exited { .. })
    });
    drive.join_and_assert_gone(deadline);
}
