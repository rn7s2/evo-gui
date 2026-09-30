//! `TabModel`: the topics, the ops that move them, the lane list and the changes a UI
//! needs to re-set (CONTRACT §4.2, §4.3, §5.2, §5.3).

mod common;

use common::{fixture, ops, topic_body};
use serde_json::json;
use session::{
    AgentKey, ItemKind, LaneStatus, Op, Scope, Status, StreamStatus, TabModel, UserStatus,
};

fn open_tab() -> TabModel {
    let mut tab = TabModel::new();
    tab.on_snapshot(
        "session",
        &topic_body(&fixture("snapshot-session.json"), "session"),
    );
    tab.on_snapshot(
        "swarm",
        &topic_body(&fixture("snapshot-swarm.json"), "swarm"),
    );
    tab.on_snapshot(
        "lane:1",
        &topic_body(&fixture("snapshot-lane1.json"), "lane:1"),
    );
    tab
}

#[test]
fn a_snapshot_of_each_topic_seeds_it() {
    let tab = open_tab();
    assert_eq!(tab.items(AgentKey::Coordinator).len(), 18);
    assert!(tab.selected_state().unwrap().is_busy());
    assert_eq!(tab.activity(), Status::Running);

    // The segments are the server's, rendered as they are: nothing recomposes them.
    let texts: Vec<&str> = tab
        .selected_segments()
        .iter()
        .map(|segment| segment.text.as_str())
        .collect();
    assert_eq!(
        texts,
        [
            "stub-a",
            "high",
            "ctx 48k/936k (5%)",
            "97% cached",
            "goal a1b2c3d4 (active) 12k/50k",
            "2 lanes",
        ]
    );
    // Left and right sides keep their own order.
    let (left, right) = session::ordered_segments(tab.selected_segments());
    assert_eq!(left.len(), 5);
    assert_eq!(right.len(), 1);
    assert_eq!(right[0].name, "swarm");

    // The checklist rides the state.
    assert_eq!(tab.selected_todos().len(), 2);
    assert_eq!(tab.queue(), ["e_queued_1"]);
}

#[test]
fn the_lane_list_comes_from_the_swarm_topic() {
    let tab = open_tab();
    let lanes = tab.lane_rows();
    assert_eq!(lanes.len(), 3);
    assert_eq!(lanes[0].n, 1);
    assert_eq!(lanes[0].status, LaneStatus::Working);
    assert_eq!(lanes[0].glyph(), '●');
    assert_eq!(
        lanes[0].task_label().as_deref(),
        Some("port the view model")
    );
    assert_eq!(lanes[0].context_label().as_deref(), Some("21k/200k"));
    assert_eq!(lanes[0].model.as_deref(), Some("stub-b (openai)"));
    assert_eq!(lanes[0].goal_status.as_deref(), Some("active"));
    assert_eq!(lanes[2].status, LaneStatus::Down);
    assert_eq!(lanes[1].restarts, 1);

    // The step clock is arithmetic on the absolute start the swarm published, never a
    // stamped age.
    assert_eq!(lanes[0].step_clock(1759200020000).as_deref(), Some("0s"));
    assert_eq!(lanes[0].step_clock(1759200035000).as_deref(), Some("15s"));
    assert_eq!(lanes[1].step_clock(1759200035000), None, "idle: no step");

    // The swarm's own count is the button's business, and it counts the rows too.
    assert_eq!(tab.swarm().unwrap().busy, 2);
    assert!(tab.lanes_busy(), "waiting_on_lanes is the swarm being busy");

    // The header's tooltip is the swarm's line, plus what the new topic adds.
    let tooltip = lanes[0].tooltip(1759200035000);
    assert!(tooltip.contains("lane 1  working"), "{tooltip}");
    assert!(tooltip.contains("2 reports"), "{tooltip}");
    assert!(tooltip.contains("ctx 21k/200k"), "{tooltip}");
}

#[test]
fn a_lanes_own_mirror_gives_its_row_a_live_activity_line() {
    let tab = open_tab();
    let lane = tab.lane_rows().iter().find(|lane| lane.n == 1).unwrap();
    // The lane's mirror is newer than the swarm's summary: the last item it holds is what
    // the row says the lane is doing.
    assert_eq!(lane.label(), "Now the transcript.");
    assert!(lane.activity.is_some());
    assert_eq!(
        lane.last_item.as_ref().map(|(kind, _)| kind.as_str()),
        Some("assistant")
    );

    // A lane whose mirror is not held at all falls back to the swarm's own summary.
    let mut tab = open_tab();
    tab.on_snapshot("swarm", &json!({ "state": { "id": "sw", "workers": 1, "status": { "busy": 0, "waiting_on_lanes": false }, "lanes": [
        { "n": 7, "state": "idle", "task": "waiting for work", "reports": 0,
          "last_item": { "kind": "lane_report", "summary": "report: done" } }
    ]}}));
    let lane = tab.lane_rows().iter().find(|lane| lane.n == 7).unwrap();
    assert_eq!(lane.label(), "report: done");
}

#[test]
fn an_op_moves_exactly_what_it_touched() {
    let mut tab = open_tab();
    let before = tab.items(AgentKey::Coordinator).len();
    let mut added = 0;
    for (name, topic, data) in ops("ops.json") {
        let Some(topic) = topic else { continue };
        let Some(op) = Op::from_json(&name, &data) else {
            continue;
        };
        let changes = tab.on_op(&topic, &op);
        match name.as_str() {
            "item.add" => {
                assert!(
                    changes
                        .for_topic(&topic)
                        .is_some_and(|c| c.items().len() == 1),
                    "{name} on {topic} reports one row"
                );
                added += 1;
            }
            "item.remove" => assert!(changes
                .for_topic(&topic)
                .is_some_and(|c| matches!(c.items(), [session::ItemChange::Remove { .. }]))),
            "state.patch" => assert!(changes.for_topic(&topic).is_some_and(|c| c.state)),
            "item.append" | "item.patch" => assert!(changes
                .for_topic(&topic)
                .is_some_and(|c| c.items().len() == 1)),
            // A topic reset moves no rows: the transport re-snapshots, and that answer
            // is what the UI drops rows for.
            "topic.reset" => {}
            _ => assert!(changes.is_empty(), "{name} changed something"),
        }
    }
    assert_eq!(added, 4);

    // The session topic grew by two of the three adds (one was removed again) and the
    // lane topic took its own.
    assert_eq!(tab.items(AgentKey::Coordinator).len(), before + 2);
    assert_eq!(tab.items(AgentKey::Lane(1)).len(), 4);
    // The state patch was merged, not replaced: the fields it did not name are still there.
    let state = tab.selected_state().unwrap();
    assert_eq!(state.status, Status::Waiting);
    assert!(state.queue.is_empty());
    assert_eq!(state.context.as_ref().unwrap().tokens, 49000);
    assert_eq!(
        state.model.as_ref().unwrap().id,
        "stub-a",
        "untouched fields survive"
    );
    assert_eq!(state.segments.len(), 6);

    // The lane topic took its own op: a lane's status is its own state, and the lane list
    // follows the *swarm* topic, which has not published the change yet.
    assert_eq!(tab.state(AgentKey::Lane(1)).unwrap().status, Status::Idle);
    assert!(tab.lanes_busy(), "the swarm topic still says a lane works");

    // A lane that was restarted drops its mirror: the next snapshot is a fresh one.
    let changes = tab.on_op(
        "lane:2",
        &Op::from_json("topic.reset", &json!({"reason": "lane_restarted"})).unwrap(),
    );
    assert!(changes.is_empty(), "the reset itself moves nothing");
    let dropped = tab.on_lane_restarted(2);
    assert!(dropped.for_topic("lane:2").is_some_and(|c| c.reset));
}

#[test]
fn a_stream_reset_stales_every_topic() {
    let mut tab = open_tab();
    let changes = tab.on_op(
        "session",
        &Op::from_json("stream.reset", &json!({ "reason": "restarted" })).unwrap(),
    );
    for topic in ["session", "swarm", "lane:1"] {
        assert!(
            changes.for_topic(topic).is_some_and(|c| c.reset && c.state),
            "{topic} must be re-snapshotted"
        );
    }
}

#[test]
fn selecting_an_agent_switches_the_center_column() {
    let mut tab = open_tab();
    assert_eq!(tab.selected(), AgentKey::Coordinator);
    let changes = tab.select(AgentKey::Lane(1));
    assert!(changes.selection);
    assert!(changes.for_topic("lane:1").is_some_and(|c| c.reset));
    assert_eq!(tab.selected_items().len(), 3);
    assert_eq!(tab.selected_state().unwrap().status, Status::Compacting);
    assert_eq!(tab.selected_segments()[1].text, "ctx 21k/200k (11%)");

    // Selecting the same agent again is not a change.
    assert!(tab.select(AgentKey::Lane(1)).is_empty());

    // A lane the tab holds no mirror of is created empty, so the column has somewhere to
    // land: no items, and nothing to put on the status line.
    let changes = tab.select(AgentKey::Lane(4));
    assert!(changes.selection);
    assert!(tab.selected_items().is_empty());
    assert!(tab.selected_segments().is_empty());
    assert!(tab.selected_todos().is_empty());
}

#[test]
fn the_tab_turns_a_ui_action_into_one_op_request() {
    let tab = open_tab();
    let send = tab.send_input("hello", session::Queue::AfterRun);
    assert_eq!(send.op, "input.send");
    assert_eq!(send.args["queue"], "after_run");
    assert_eq!(send.args["topic"], "session");

    let cancel = tab.cancel_input("e_queued_1");
    assert_eq!(cancel.op, "input.cancel");
    assert_eq!(cancel.args["item_id"], "e_queued_1");

    // The stop that means the whole swarm, and the one that means a lane.
    let swarm = tab.interrupt_swarm();
    assert_eq!(swarm.op, "run.interrupt");
    assert_eq!(swarm.args["scope"], "swarm");
    assert!(swarm.args.get("lane").is_none());
    let lane = tab.interrupt_lane(2);
    assert_eq!(lane.args["scope"], "lane");
    assert_eq!(lane.args["lane"], 2);
    assert_eq!(Scope::Lane.name(), "lane");
}

#[test]
fn the_queue_and_the_queued_rows_are_one_thing() {
    let mut tab = open_tab();
    // The state's queue names a user item the tab holds, still queued.
    let id = tab.queue()[0].clone();
    let item = tab
        .items(AgentKey::Coordinator)
        .iter()
        .find(|item| item.id == id)
        .expect("the queued item is in the topic");
    let ItemKind::User(user) = &item.kind else {
        panic!("a queue holds user items")
    };
    assert!(user.is_queued());
    assert_eq!(user.status, UserStatus::Queued);

    // The op that takes it back is the one the row's cancel button sends.
    let request = tab.cancel_input(&id);
    assert_eq!(request.op, "input.cancel");
    assert_eq!(request.args["item_id"], id);

    // ...and the answer to that op is a patch on the item, which is what the row draws.
    tab.on_op(
        "session",
        &Op::from_json(
            "item.patch",
            &json!({ "id": id, "patch": { "status": "cancelled", "queue": null } }),
        )
        .unwrap(),
    );
    let item = tab
        .items(AgentKey::Coordinator)
        .iter()
        .find(|item| item.id == id)
        .unwrap();
    let ItemKind::User(user) = &item.kind else {
        panic!("still a user item")
    };
    assert_eq!(user.status, UserStatus::Cancelled);
    assert!(!user.is_queued());
}

#[test]
fn older_items_page_into_the_topic_the_ui_asked_for() {
    let mut tab = open_tab();
    let changes = tab.on_items_before("session", &fixture("items-before.json"));
    assert_eq!(changes.for_topic("session").unwrap().prepended, 2);
    assert_eq!(tab.items(AgentKey::Coordinator).len(), 20);
    assert!(!tab.topic("session").unwrap().has_older());
}

#[test]
fn a_stream_badge_is_per_topic() {
    let mut tab = open_tab();
    assert_eq!(
        tab.stream_status("session"),
        StreamStatus::Connected,
        "no badge until something says so"
    );
    tab.on_stream(
        "session",
        StreamStatus::Reconnecting {
            retry_in: std::time::Duration::from_secs(2),
        },
    );
    assert!(tab.stream_status("session").is_reconnecting());
    assert!(!tab.stream_status("swarm").is_reconnecting());
}
