//! The lane list: `GET /lanes` plus the coordinator stream's `lane-state` events.
//!
//! The captures are real: `lanes.json` is the swarm before any lane was given work,
//! `lanes-lane1-working.json` is it with lane 1 in a step, `lanes-final.json` is it at rest
//! with both lanes keeping the task they last ran. The `lane-state` events come from the
//! same session's `events-coordinator.sse`.

mod common;

use common::{fixture, sse_events};
use serde_json::json;
use session::{LaneList, LaneStatus};

/// §7.3: the step clock keeps moving between reads.
///
/// `GET /lanes` reports a step *age*, not a step start, so the row remembers when it saw
/// that age (`step_age_at_millis`) and the clock counts on from there. Without that, a
/// lane running for five minutes sat at `0s` for all five — which is exactly the number
/// the clock exists to tell apart from a wedged lane.
#[test]
fn a_step_clock_counts_on_from_the_moment_the_age_was_seen() {
    let seen = 1_700_000_000_000u64;
    let list = LaneList::from_lanes_at(&fixture("lanes-lane1-working.json"), Some(seen));
    let lane1 = list.lane(1).expect("lane 1");
    assert_eq!(lane1.step_age, Some(0), "the swarm's own number");
    assert_eq!(lane1.step_age_at_millis, Some(seen));
    assert_eq!(lane1.step_clock_at(seen).as_deref(), Some("0s"));
    assert_eq!(lane1.step_clock_at(seen + 45_000).as_deref(), Some("45s"));
    assert_eq!(lane1.step_clock_at(seen + 180_000).as_deref(), Some("3m"));
    assert_eq!(
        lane1.step_clock_at(seen + 3_780_000).as_deref(),
        Some("1h3m")
    );
    // The age itself is still there for a reader with no moment to count from.
    assert_eq!(lane1.step_clock().as_deref(), Some("0s"));

    // An idle lane has no step clock, stamped or not.
    assert_eq!(list.lane(2).unwrap().step_clock_at(seen + 45_000), None);

    // A read nobody stamped counts from nothing: the age stands where the swarm left it.
    let unstamped = LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    let lane1 = unstamped.lane(1).unwrap();
    assert_eq!(lane1.step_age_at_millis, None);
    assert_eq!(lane1.step_clock_at(seen + 45_000).as_deref(), Some("0s"));

    // A stamp ahead of `now` (two clocks disagreeing) reads as the reported age, not as
    // half a century.
    assert_eq!(
        list.lane(1)
            .unwrap()
            .step_clock_at(seen - 60_000)
            .as_deref(),
        Some("0s")
    );
}

/// A `lane-state` names the new step and carries no age: a lane that *enters* working or
/// compacting starts its clock at zero from the moment the event was seen, and the
/// lane-list read the engine schedules behind every such event corrects it to the swarm's
/// own number. A step already under way keeps the age and the moment it has — a task or
/// goal change publishes a `lane-state` too, which is the same step, and re-stamping there
/// would freeze the clock it is meant to move.
#[test]
fn a_lane_state_that_enters_a_step_starts_its_clock() {
    let seen = 1_700_000_000_000u64;
    let mut list = LaneList::from_lanes_at(&fixture("lanes.json"), Some(seen));
    assert_eq!(
        list.lane(1).unwrap().step_age,
        None,
        "an idle lane has none"
    );

    assert!(list.apply_lane_state_at(
        &json!({ "lane": 1, "state": "working", "task": "x", "goal": null, "restarts": 0, "pid": 1 }),
        Some(seen),
    ));
    let lane1 = list.lane(1).unwrap();
    assert_eq!(lane1.step_age, Some(0), "the step begins here");
    assert_eq!(lane1.step_age_at_millis, Some(seen));
    assert_eq!(lane1.step_clock_at(seen + 12_000).as_deref(), Some("12s"));

    // A task change mid-step: the clock counts from the read, not from the event.
    let mut list = LaneList::from_lanes_at(&fixture("lanes-lane1-working.json"), Some(seen));
    let read_age = list.lane(1).unwrap().step_age;
    assert!(list.apply_lane_state_at(
        &json!({ "lane": 1, "state": "working", "task": "another", "goal": null, "restarts": 0, "pid": 1 }),
        Some(seen + 30_000),
    ));
    let lane1 = list.lane(1).unwrap();
    assert_eq!(lane1.step_age, read_age, "the step went on");
    assert_eq!(
        lane1.step_age_at_millis,
        Some(seen),
        "counted from the read"
    );
    assert_eq!(lane1.step_clock_at(seen + 30_000).as_deref(), Some("30s"));

    // Idle and working again is a new step, from the moment it started.
    let mut list = LaneList::from_lanes_at(&fixture("lanes-lane1-working.json"), Some(seen));
    list.apply_lane_state_at(
        &json!({ "lane": 1, "state": "idle", "task": "x", "goal": null, "restarts": 0, "pid": 1 }),
        Some(seen + 90_000),
    );
    list.apply_lane_state_at(
        &json!({ "lane": 1, "state": "working", "task": "x", "goal": null, "restarts": 0, "pid": 1 }),
        Some(seen + 100_000),
    );
    let lane1 = list.lane(1).unwrap();
    assert_eq!(lane1.step_age, Some(0), "a new step starts at zero");
    assert_eq!(lane1.step_age_at_millis, Some(seen + 100_000));
    assert_eq!(lane1.step_clock_at(seen + 105_000).as_deref(), Some("5s"));
}

#[test]
fn a_captured_lanes_reply_holds_the_swarm_and_its_lanes() {
    let list = LaneList::from_lanes(&fixture("lanes.json"));
    let swarm = list.swarm.as_ref().expect("the swarm object");
    assert_eq!(swarm.id, "20260929T092544-fc3c");
    assert_eq!(swarm.workers, 2);
    assert_eq!(swarm.busy, 0);
    assert!(
        !swarm.stopping,
        "the capture's `stopping` is null, which is not stopping"
    );
    assert!(swarm.cwd.ends_with('/'), "cwd: {}", swarm.cwd);

    assert_eq!(list.lanes.len(), 2);
    for (index, row) in list.lanes.iter().enumerate() {
        assert_eq!(row.n, index as u64 + 1);
        assert_eq!(row.status, LaneStatus::Idle);
        assert_eq!(row.state, "idle");
        // A lane that has never been given work carries no task and no clocks.
        assert_eq!(row.task, None);
        assert_eq!(row.task_age, None);
        assert_eq!(row.step_age, None);
        assert_eq!(row.restarts, 0);
        assert_eq!(row.reports, 0);
        assert_eq!(row.goal_status, None);
        assert!(row.pid.is_some(), "a brought-up lane knows its pid");
        assert_eq!(row.glyph(), '○');
    }
    assert_eq!(list.busy(), 0, "the swarm's own busy count");
}

#[test]
fn a_working_lane_carries_its_task_and_its_clocks() {
    let list = LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    assert_eq!(list.busy(), 1);

    let lane1 = list.lane(1).expect("lane 1");
    assert_eq!(lane1.status, LaneStatus::Working);
    assert!(lane1.is_busy());
    assert_eq!(lane1.glyph(), '●');
    assert!(lane1
        .task
        .as_deref()
        .unwrap()
        .starts_with("DELAY3 CALL todo"));
    assert_eq!(lane1.task_age, Some(0), "seconds in the task");
    assert_eq!(lane1.step_age, Some(0), "seconds in the current step");

    // The idle lane beside it has its pid but no step.
    let lane2 = list.lane(2).expect("lane 2");
    assert_eq!(lane2.status, LaneStatus::Idle);
    assert_eq!(lane2.step_age, None);
}

#[test]
fn an_idle_lane_keeps_the_task_it_last_ran() {
    // The swarm leaves `task` in place when a lane goes idle (`lanes.lisp`), so the left
    // column can still say what the lane did.
    let list = LaneList::from_lanes(&fixture("lanes-final.json"));
    assert_eq!(list.busy(), 0);
    let lane1 = list.lane(1).unwrap();
    assert_eq!(lane1.status, LaneStatus::Idle);
    assert!(lane1
        .task
        .as_deref()
        .unwrap()
        .starts_with("DELAY3 CALL todo"));
    assert_eq!(lane1.task_age, Some(8));
    let lane2 = list.lane(2).unwrap();
    let task = lane2.task.as_deref().expect("lane 2's task");
    assert!(
        task.starts_with("CALL report {\"done\":\"lane 2 finished the fixture work\""),
        "task: {task}"
    );
    assert_eq!(lane2.reports, 1, "lane 2 filed one report");
}

/// The lane row's composed tooltip, against the swarm's own `lane-status-line`
/// (`swarm/tools.lisp`) — the expected strings below are that `format` string evaluated on
/// the same fields in a real Common Lisp.
#[test]
fn the_tooltip_is_the_swarms_own_lane_line() {
    let list = LaneList::from_lanes(&fixture("lanes-final.json"));
    let lane2 = list.lane(2).unwrap();
    assert_eq!(
        lane2.tooltip(),
        r#"lane 2  idle · task: CALL report {"done":"lane 2 finished the fixture work","evid… · 1 report · pid 87894"#
    );

    // Lane 1 of the working capture: the step clock is in the line while it works.
    let working = LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    let lane1 = working.lane(1).unwrap();
    assert_eq!(
        lane1.tooltip(),
        r#"lane 1  working · step 0s · task: DELAY3 CALL todo {"items":[{"text":"lane step one","status":… · 0 reports · pid 87897"#
    );

    // `lane-status-line` writes the fields it has and skips the ones it has not, and `~:p`
    // pluralises anything but one.
    let row = LaneList::from_lanes(&json!({ "lanes": [ {
        "n": 3, "state": "starting", "worktree": "/w", "branch": "lanes/x",
        "restarts": 2, "reports": 1, "pid": 12,
        "goal": { "goal_id": "g", "status": "active" }
    }] }))
    .lane(3)
    .unwrap()
    .clone();
    assert_eq!(
        row.tooltip(),
        "lane 3  starting · worktree /w (lanes/x) · 1 report · 2 restarts · pid 12 · goal active"
    );

    // A lane that has never been given work: state, the report count, its pid.
    let bare = LaneList::from_lanes(&fixture("lanes.json"))
        .lane(1)
        .unwrap()
        .clone();
    assert_eq!(bare.tooltip(), "lane 1  idle · 0 reports · pid 87897");
}

/// The state vocabulary (`swarm/state.lisp`: `:starting :idle :working :compacting :down
/// :stopped`) and the swarm TUI's own glyphs (`swarm/tui.lisp`: everything else is ✗).
#[test]
fn every_lane_state_maps_to_the_swarms_own_glyph() {
    for (state, status, glyph) in [
        ("starting", LaneStatus::Starting, '◌'),
        ("idle", LaneStatus::Idle, '○'),
        ("working", LaneStatus::Working, '●'),
        ("compacting", LaneStatus::Compacting, '◐'),
        ("down", LaneStatus::Down, '✗'),
        ("stopped", LaneStatus::Down, '✗'),
        ("something-new", LaneStatus::Down, '✗'),
    ] {
        assert_eq!(LaneStatus::from_state(state), status, "{state}");
        assert_eq!(LaneStatus::from_state(state).glyph(), glyph, "{state}");
    }
}

#[test]
fn a_lane_state_event_updates_its_row() {
    let mut list = LaneList::from_lanes(&fixture("lanes.json"));

    // Lane 1 is given work (id 36 of the capture).
    assert!(list.apply_lane_state(&json!({
        "type": "lane-state", "lane": 1, "state": "working",
        "task": "DELAY3 CALL todo {\"items\":[]}", "goal": null, "restarts": 0, "pid": 87897
    })));
    let lane1 = list.lane(1).unwrap();
    assert_eq!(lane1.status, LaneStatus::Working);
    assert_eq!(lane1.state, "working");
    assert_eq!(
        lane1.task.as_deref(),
        Some("DELAY3 CALL todo {\"items\":[]}")
    );
    assert_eq!(lane1.pid, Some(87897));

    // The same event again changes nothing.
    assert!(!list.apply_lane_state(&json!({
        "type": "lane-state", "lane": 1, "state": "working",
        "task": "DELAY3 CALL todo {\"items\":[]}", "goal": null, "restarts": 0, "pid": 87897
    })));

    // Back to idle with the task kept, as the swarm publishes it (id 46).
    assert!(list.apply_lane_state(&json!({
        "type": "lane-state", "lane": 1, "state": "idle",
        "task": "DELAY3 CALL todo {\"items\":[]}", "goal": null, "restarts": 0, "pid": 87897
    })));
    assert_eq!(list.lane(1).unwrap().status, LaneStatus::Idle);
    assert_eq!(
        list.lane(1).unwrap().task.as_deref(),
        Some("DELAY3 CALL todo {\"items\":[]}")
    );

    // A lane-state carries no clocks, so the ones a /lanes reply gave are untouched.
    let mut list = LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    list.apply_lane_state(&json!({ "lane": 1, "state": "compacting", "task": "x", "goal": null, "restarts": 0, "pid": 1 }));
    assert_eq!(list.lane(1).unwrap().status, LaneStatus::Compacting);
    assert_eq!(
        list.lane(1).unwrap().step_age,
        Some(0),
        "the step clock stays from /lanes"
    );
}

#[test]
fn a_lane_state_for_an_unseen_lane_adds_a_starting_row() {
    let mut list = LaneList::from_lanes(&fixture("lanes.json"));
    assert!(list.apply_lane_state(&json!({ "lane": 3, "state": "starting", "task": null, "goal": null, "restarts": 0, "pid": null })));
    assert_eq!(list.lanes.len(), 3);
    let lane3 = list.lane(3).expect("the new lane");
    assert_eq!(lane3.status, LaneStatus::Starting);
    assert_eq!(lane3.task, None);
    assert_eq!(lane3.pid, None);
    // Rows stay ordered by lane number.
    let numbers: Vec<u64> = list.lanes.iter().map(|row| row.n).collect();
    assert_eq!(numbers, vec![1, 2, 3]);

    // An event with no lane number is not a lane.
    assert!(!list.apply_lane_state(&json!({ "state": "working" })));
    assert_eq!(list.lanes.len(), 3);
}

#[test]
fn a_lane_state_goal_is_reduced_to_its_status() {
    let mut list = LaneList::from_lanes(&fixture("lanes.json"));
    // `lane-state` carries the goal's status alone (`swarm/api.lisp`).
    list.apply_lane_state(&json!({ "lane": 1, "state": "idle", "task": null, "goal": "active", "restarts": 0, "pid": 1 }));
    assert_eq!(list.lane(1).unwrap().goal_status.as_deref(), Some("active"));

    // `/lanes` carries the lane's cached goal plist; the status is what the list shows.
    let list = LaneList::from_lanes(&json!({
        "swarm": { "id": "s", "workers": 1, "busy": 0 },
        "lanes": [ { "n": 1, "state": "idle",
                     "goal": { "goal_id": "g-1", "status": "complete", "tokens_used": 15 },
                     "restarts": 3, "reports": 2 } ]
    }));
    let lane = list.lane(1).unwrap();
    assert_eq!(lane.goal_status.as_deref(), Some("complete"));
    assert_eq!(lane.restarts, 3);
    assert_eq!(lane.reports, 2);

    // A null goal is no goal at all, and a restart is a restart the list carries up.
    let mut list = LaneList::from_lanes(&fixture("lanes.json"));
    assert!(list.apply_lane_state(&json!({ "lane": 2, "state": "down", "task": null, "goal": null, "restarts": 2, "pid": 87894 })));
    assert_eq!(list.lane(2).unwrap().goal_status, None);
    assert_eq!(list.lane(2).unwrap().restarts, 2);
    assert_eq!(list.lane(2).unwrap().status, LaneStatus::Down);
    assert_eq!(list.lane(2).unwrap().glyph(), '✗');
}

#[test]
fn apply_lanes_replaces_wholesale_and_says_when_it_changed() {
    let mut list = LaneList::new();
    assert!(list.apply_lanes(&fixture("lanes.json")));
    assert!(
        !list.apply_lanes(&fixture("lanes.json")),
        "the same reply changes nothing, so the UI need not redraw"
    );
    assert!(list.apply_lanes(&fixture("lanes-lane1-working.json")));
    assert_eq!(list.lane(1).unwrap().status, LaneStatus::Working);
    // A later reply that drops a lane drops the row too: `GET /lanes` is the whole list.
    assert!(
        list.apply_lanes(&json!({ "swarm": { "id": "s", "workers": 1, "busy": 0 }, "lanes": [] }))
    );
    assert!(list.lanes.is_empty());
}

/// The coordinator's own stream carries every lane's state, so the left column follows the
/// whole swarm from the one stream the tab already holds.
#[test]
fn the_coordinator_stream_drives_the_whole_lane_list() {
    let mut list = LaneList::new();
    let mut lane_states = 0;
    let mut changed = 0;
    for (_, kind, data) in sse_events("events-coordinator.sse") {
        if kind != "lane-state" {
            continue;
        }
        lane_states += 1;
        if list.apply_lane_state(&data) {
            changed += 1;
        }
    }
    // Every `lane-state` in the capture is a real change — `maybe-publish-lane-state`
    // publishes on a change and only then — and they are the two lanes the swarm has.
    assert!(lane_states > 0, "the capture carries lane-state events");
    assert_eq!(changed, lane_states);
    let numbers: Vec<u64> = list.lanes.iter().map(|row| row.n).collect();
    assert_eq!(numbers, vec![1, 2]);
    // The capture ends with both lanes back at idle, each still holding its task.
    for row in &list.lanes {
        assert_eq!(row.status, LaneStatus::Idle, "lane {}", row.n);
        assert!(row.task.is_some(), "lane {}", row.n);
        assert!(row.pid.is_some());
    }
}

/// The header counts the rows under it, not the number its last snapshot came with.
///
/// An earlier capture caught the two disagreeing: `2 lanes · 1 busy`
/// over two idle rows. The list had been read while lane 1 was working
/// (`lanes-lane1-working.json`, whose `swarm.busy` is 1) and the lane then went idle on
/// the stream — which carries no count at all — so between that event and the next read
/// the header said one thing and the column under it another.
#[test]
fn the_header_counts_the_rows_it_labels() {
    let mut list = LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    assert_eq!(
        list.busy(),
        1,
        "the snapshot's own count, over its one working row"
    );

    // The lane settles: the captured `lane-state` event for exactly that, task and all.
    let idle = sse_events("events-coordinator.sse")
        .into_iter()
        .map(|(_, _, data)| data)
        .find(|data| data["type"] == "lane-state" && data["lane"] == 1 && data["state"] == "idle")
        .expect("the capture holds lane 1 going idle");
    assert!(list.apply_lane_state(&idle), "the row changed");
    let lane1 = list.lane(1).expect("lane 1");
    assert_eq!(lane1.status, LaneStatus::Idle);
    assert!(
        lane1
            .task
            .as_deref()
            .is_some_and(|task| task.starts_with("DELAY3 CALL todo")),
        "the task it last ran stays on the row: {lane1:?}"
    );
    assert_eq!(
        lane1.step_clock(),
        None,
        "a lane that stopped has no step clock to show"
    );

    assert_eq!(
        list.swarm.as_ref().unwrap().busy,
        1,
        "the snapshot it was built from still says one lane is busy…"
    );
    assert_eq!(list.busy(), 0, "…and the header says what its rows say");

    // Which is what a *fresh* read of the list would have said, without waiting for one.
    let fresh = LaneList::from_lanes(&fixture("lanes-final.json"));
    assert_eq!(fresh.lane(1).unwrap().status, LaneStatus::Idle);
    assert_eq!(list.busy(), fresh.busy(), "the same count, a read later");
}
