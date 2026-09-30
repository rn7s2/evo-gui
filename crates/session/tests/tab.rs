//! `TabModel`: the whole tab driven by the captured fixtures, one update at a time.
//!
//! The coordinator capture (`events-coordinator.sse`) is a real run — two lanes, a goal
//! that completes, a delegation, a `todo-changed`, and `settled` at the end — so a tab fed
//! it covers the whole reducer path end to end; the lane captures drive the per-lane side.

mod common;

use common::{fixture, sse_events};
use serde_json::json;
use session::{
    Activity, AgentKey, Changes, DimStyle, LaneStatus, RowChanges, RowKind, StreamStatus, TabModel,
    TodoStatus,
};
use std::time::Duration;

const COORDINATOR: AgentKey = AgentKey::Coordinator;

fn lane(n: u32) -> AgentKey {
    AgentKey::Lane(n)
}

/// Feed one capture into a tab as AGENT's stream, returning the union of the changes.
fn feed(tab: &mut TabModel, agent: AgentKey, capture: &str) -> Changes {
    let mut changes = Changes::default();
    for (id, kind, data) in sse_events(capture) {
        changes = merge(changes, tab.on_event(agent, id, &kind, &data));
    }
    changes
}

/// Two `Changes` as one: what the UI would do if it batched the updates.
fn merge(a: Changes, b: Changes) -> Changes {
    Changes {
        rows: a.rows.into_iter().chain(b.rows).collect(),
        todos: a.todos.union(&b.todos).copied().collect(),
        lanes: a.lanes || b.lanes,
        readout: a.readout || b.readout,
        activity: a.activity || b.activity,
        stream: a.stream.union(&b.stream).copied().collect(),
        selection: a.selection || b.selection,
        step: a.step.union(&b.step).copied().collect(),
        needs_resync: a.needs_resync.union(&b.needs_resync).copied().collect(),
    }
}

#[test]
fn a_coordinator_run_drives_the_whole_tab() {
    let mut tab = TabModel::new();
    assert_eq!(tab.selected(), COORDINATOR);
    assert!(tab.selected_rows().is_empty(), "a fresh tab has no rows");

    // The seeds a tab opens with.
    tab.on_registry(&fixture("registry.json"));
    let changes = tab.on_state(&fixture("state.json"));
    assert!(changes.readout, "the readout is seeded from /state");
    assert!(
        !changes.activity,
        "state.json is idle and a fresh model is idle too"
    );
    assert!(tab.on_lanes(&fixture("lanes.json")).lanes);

    // The transcript, stamped revision 1.
    let changes = tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));
    assert_eq!(changes.rows_for(COORDINATOR), Some(&RowChanges::Rebuilt));
    let rows = tab.selected_rows().len();
    assert!(rows > 10, "the capture's transcript: {rows} rows");

    // ...and then the live run.
    let changes = feed(&mut tab, COORDINATOR, "events-coordinator.sse");
    assert!(changes.lanes, "lane-state events reach the lane list");
    assert!(changes.todos.contains(&COORDINATOR), "todo-changed");
    assert!(changes.activity, "task-start/task-end move the activity");
    assert!(
        changes.needs_resync.contains(&COORDINATOR),
        "settled asks for a resync"
    );
    assert!(
        !changes.readout,
        "the capture's requests are 15 tokens of a 200k window: the line does not move, so \
         there is nothing to redraw"
    );
    assert_eq!(
        tab.readout_text(),
        "stub-a · medium · ctx 0k/200k (0%)",
        "the seed describes a session with no goal yet: the goal is created by the run"
    );
    assert!(
        matches!(changes.rows_for(COORDINATOR), Some(RowChanges::Changed(ids)) if !ids.is_empty()),
        "the run touched rows: {:?}",
        changes.rows_for(COORDINATOR)
    );

    // A usage that does move the line says so.
    let before = tab.readout_text();
    let changes = tab.on_event(
        COORDINATOR,
        999,
        "message-end",
        &json!({ "usage": { "input": 48211, "output": 100, "cache_read": 9700, "cache_write": 200 } }),
    );
    assert!(changes.readout);
    assert!(
        tab.readout_text().contains("58k/200k (29%) · 17% cached"),
        "line: {}",
        tab.readout_text()
    );
    assert_ne!(tab.readout_text(), before);

    // The goal the run created reaches the line the way it always does: a resync's
    // `/state`, applied to the same readout.
    let changes = tab.on_state(&fixture("state-final.json"));
    assert!(changes.readout);
    assert_eq!(
        tab.readout_text(),
        "stub-a · medium · ctx 0k/200k (0%) · 17% cached · goal g-4cb9 (complete) 0k"
    );

    // The transcript on screen is exactly what the seed had, plus the run's rows.
    let rebuilt = tab.coordinator().rows().len();
    assert!(rebuilt > rows, "the run added rows: {rebuilt} vs {rows}");

    // The lane list followed the stream: both lanes exist and are back at idle with the
    // task they last ran.
    assert_eq!(tab.lane_rows().len(), 2);
    assert_eq!(tab.lane_rows()[0].status, LaneStatus::Idle);
    assert!(tab.lane_rows()[0].task_label().is_some());

    // The checklist came from `todo-changed` and lands in the panel.
    assert_eq!(tab.selected_todos().len(), 3);
    assert_eq!(tab.selected_todos()[0].status, TodoStatus::InProgress);

    // The activity settled back to idle: the composer's button reads Send again.
    assert_eq!(tab.activity(), Activity::Idle);
}

#[test]
fn a_changed_row_is_named_not_the_whole_transcript() {
    let mut tab = TabModel::new();
    tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));

    // A delta touches one row: the UI re-sets that row only.
    tab.on_event(COORDINATOR, 1, "message-start", &json!({}));
    let streaming = tab.coordinator().streaming_row().expect("a streaming row");
    let changes = tab.on_event(COORDINATOR, 2, "text-delta", &json!({ "text": "hello" }));
    assert_eq!(
        changes.rows_for(COORDINATOR),
        Some(&RowChanges::Changed(vec![streaming]))
    );
    assert!(!changes.rows_for(COORDINATOR).unwrap().is_rebuilt());

    // A delta that carries nothing is no change at all.
    let changes = tab.on_event(COORDINATOR, 3, "text-delta", &json!({ "text": "" }));
    assert!(changes.is_empty());

    // A tool result completes a row the stream opened earlier.
    tab.on_event(
        COORDINATOR,
        4,
        "message-end",
        &json!({ "usage": null, "error": null }),
    );
    tab.on_event(
        COORDINATOR,
        5,
        "tool-call-start",
        &json!({ "name": "bash", "id": "t1", "arguments_json": "{}" }),
    );
    let call = tab.coordinator().rows().last().expect("the tool row").id;
    let changes = tab.on_event(
        COORDINATOR,
        6,
        "tool-result",
        &json!({ "name": "bash", "id": "t1", "content": "ok" }),
    );
    assert_eq!(
        changes.rows_for(COORDINATOR),
        Some(&RowChanges::Changed(vec![call]))
    );
}

/// §9.1: a late transcript from an older revision must not overwrite a newer one.
#[test]
fn a_stale_transcript_is_dropped() {
    let mut tab = TabModel::new();
    // Revision 7 arrives first (the display symbols, a later fetch).
    let rev7 = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "seven" }] }] });
    assert_eq!(
        tab.on_transcript(COORDINATOR, 7, &rev7)
            .rows_for(COORDINATOR),
        Some(&RowChanges::Rebuilt)
    );
    let rows = tab.coordinator().rows().len();

    // An answer to an older request lands late: dropped, and the rows are untouched.
    let stale = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "stale" }] }] });
    let changes = tab.on_transcript(COORDINATOR, 6, &stale);
    assert!(
        changes.is_empty(),
        "a stale answer changes nothing: {changes:?}"
    );
    assert_eq!(tab.coordinator().rows().len(), rows);
    match &tab.selected_rows()[0].kind {
        RowKind::User { text } => assert_eq!(text, "seven"),
        other => panic!("the newer transcript is still on screen: {other:?}"),
    }

    // The same revision again is a duplicate, not news: the I/O layer's revisions are
    // monotone per agent, so applying it twice would only rebuild the same rows.
    let changes = tab.on_transcript(COORDINATOR, 7, &rev7);
    assert!(
        changes.is_empty(),
        "an equal revision is a duplicate: {changes:?}"
    );
    assert_eq!(tab.coordinator().rows().len(), rows);

    // A newer one applies.
    let rev8 = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "eight" }] }] });
    assert_eq!(
        tab.on_transcript(COORDINATOR, 8, &rev8)
            .rows_for(COORDINATOR),
        Some(&RowChanges::Rebuilt)
    );
    match &tab.selected_rows()[0].kind {
        RowKind::User { text } => assert_eq!(text, "eight"),
        other => panic!("the newest transcript is on screen: {other:?}"),
    }
}

/// §3: the supervisor restarts a crashed coordinator, and the new server's ids start at 1.
/// The tab must keep working — no duplicate rows, and a fresh transcript must not be
/// refused as stale.
#[test]
fn hello_after_a_restart_keeps_one_set_of_rows() {
    let mut tab = TabModel::new();
    // The lane's stream is the simplest complete log: ids 1..N, a tool call, todos.
    feed(&mut tab, lane(1), "lane1-events.sse");
    let first_rows: Vec<RowKind> = tab
        .lane_model(1)
        .expect("the lane's model")
        .rows()
        .iter()
        .map(|row| row.kind.clone())
        .collect();
    assert!(!first_rows.is_empty());

    // The old server's transcript was stamped 9; the restarted one answers with its own
    // revision 1. `hello` is what says the numbering restarted.
    tab.on_transcript(lane(1), 9, &fixture("transcript-empty.json"));
    assert!(tab.lane_model(1).unwrap().rows().is_empty());
    let changes = tab.on_event(lane(1), 1, "hello", &json!({ "pid": 4242 }));
    assert!(
        changes.needs_resync.contains(&lane(1)),
        "hello refetches this agent"
    );
    let changes = tab.on_transcript(lane(1), 1, &fixture("lane1-transcript.json"));
    assert!(
        changes.rows_for(lane(1)).unwrap().is_rebuilt(),
        "the fresh transcript applies"
    );

    // The restarted server replays the same log, ids 1..N again: a second set of rows, no
    // id handed out twice within the fresh space.
    let changes = feed(&mut tab, lane(1), "lane1-events.sse");
    assert!(changes.rows_for(lane(1)).is_some());
    let ids: Vec<u64> = tab
        .lane_model(1)
        .unwrap()
        .rows()
        .iter()
        .map(|row| row.id)
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "row ids are unique: {ids:?}");
    assert_eq!(ids[0], 1, "the rebuilt id space starts at 1");
}

#[test]
fn selecting_a_lane_switches_the_center_column() {
    let mut tab = TabModel::new();
    tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));
    let coordinator_rows = tab.selected_rows().len();

    // Selecting a lane creates its model and tells the UI to rebuild the row views.
    let changes = tab.select(lane(1));
    assert!(changes.selection);
    assert_eq!(changes.rows_for(lane(1)), Some(&RowChanges::Rebuilt));
    assert_eq!(tab.selected(), lane(1));
    assert!(tab.selected_rows().is_empty(), "not fetched yet");

    // Selecting it again is not a change.
    assert!(tab.select(lane(1)).is_empty());

    // Its transcript lands, and the center column is the lane's.
    tab.on_transcript(lane(1), 1, &fixture("lane1-transcript.json"));
    assert!(!tab.selected_rows().is_empty());
    assert!(tab.selected_rows().len() < coordinator_rows);

    // Back to the coordinator: its rows are still there, untouched.
    let changes = tab.select(COORDINATOR);
    assert!(changes.selection);
    assert_eq!(tab.selected_rows().len(), coordinator_rows);

    // An unwatched lane has no model at all — nothing to render, nothing invented.
    assert!(tab.lane_model(2).is_none());
    assert!(tab.agent_model(lane(2)).is_none());
}

#[test]
fn the_todo_panel_follows_the_selected_agent() {
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-with-todos.json"));
    assert_eq!(tab.selected_todos().len(), 3, "the coordinator's checklist");

    // The lane's checklist arrives on its own stream.
    tab.select(lane(1));
    let changes = feed(&mut tab, lane(1), "lane1-events.sse");
    assert!(changes.todos.contains(&lane(1)));
    assert_eq!(tab.selected_todos().len(), 2);
    assert_eq!(tab.selected_todos()[0].text, "lane step one");

    // ...and the coordinator's is unaffected: each agent keeps its own.
    assert_eq!(tab.coordinator().todos().len(), 3);
    tab.select(COORDINATOR);
    assert_eq!(tab.selected_todos().len(), 3);
    assert_eq!(tab.selected_todos()[2].status, TodoStatus::Done);
}

/// §9.7: a lane that is down says why. The swarm's own account of the lane going
/// down comes first — what a restarted lane says about itself is the noise of being
/// restarted — and the lane's own transcript speaks when the swarm has said nothing.
#[test]
fn lane_down_reason_comes_from_the_lanes_own_output_rows() {
    let mut tab = TabModel::new();
    // No transcript loaded anywhere: nothing to say.
    assert_eq!(tab.lane_down_reason(1), None);

    tab.select(lane(1));
    // The lane's model init fails: serve says so as an error-styled output line, exactly
    // the shape `swarm/init.lisp`'s check produces (`✗ lane 1 cannot use its model ...`).
    tab.on_event(lane(1), 1, "output", &json!({
        "style": "error",
        "text": "✗ lane 1 cannot use its model st-1: its API :openai-completions is not in the lane"
    }));
    // Work goes on after the error; the reason stays the error line, not the newest line.
    tab.on_event(
        lane(1),
        2,
        "task-start",
        &json!({ "task_id": "t", "kind": "run" }),
    );
    tab.on_event(
        lane(1),
        3,
        "output",
        &json!({ "style": "dim", "text": "queued" }),
    );
    // The coordinator relays what the lane said, prefixed with the lane it is about.
    tab.on_event(COORDINATOR, 1, "output", &json!({
        "style": "error",
        "text": "[lane 1] ✗ lane 1 cannot use its model st-1: its API :openai-completions is not in the lane"
    }));
    // Nothing is shown until the lane is actually down: a lane that is up is not down.
    assert_eq!(tab.lane_down_reason(1), None, "the lane is not down yet");

    // The lane goes down (a lane-state on the coordinator's stream).
    let changes = tab.on_event(
        COORDINATOR,
        2,
        "lane-state",
        &json!({
            "lane": 1, "state": "down", "task": null, "goal": null, "restarts": 0, "pid": null
        }),
    );
    assert!(changes.lanes);
    let row = tab.lanes().lane(1).unwrap();
    assert_eq!(row.status, LaneStatus::Down);
    assert_eq!(row.glyph(), '✗');
    // The lane's own account is what the tab has: the swarm has announced nothing.
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some("✗ lane 1 cannot use its model st-1: its API :openai-completions is not in the lane")
    );

    // The newest error line of the winning source is the one shown.
    tab.on_event(
        lane(1),
        4,
        "output",
        &json!({ "style": "error", "text": "✗ lane 1: second failure" }),
    );
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some("✗ lane 1: second failure")
    );

    // ...but once the swarm says what happened to the lane, *that* is the reason: a
    // restarted lane complains about its own fresh state (no model registered yet),
    // and those lines are symptoms of the restart, not why the row is red.
    tab.on_event(lane(1), 5, "output", &json!({
        "style": "error",
        "text": "✗ No model is configured — set one in init.lisp: (evo:set-setting :model \"...\")"
    }));
    tab.on_event(COORDINATOR, 3, "output", &json!({
        "style": "error",
        "text": "[lane 1] crashed and was restarted by its supervisor (pid 1 → 2); its session was resumed and it was re-initialized."
    }));
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some("[lane 1] crashed and was restarted by its supervisor (pid 1 → 2); its session was resumed and it was re-initialized."),
        "the swarm's account outranks what the lane said about itself"
    );
    // Even with the lane's noise arriving later still.
    tab.on_event(
        lane(1),
        6,
        "output",
        &json!({
            "style": "error",
            "text": "✗ No model is configured — set one in init.lisp"
        }),
    );
    assert!(tab
        .lane_down_reason(1)
        .is_some_and(|reason| reason.contains("crashed and was restarted")));

    // Back up: the row is not down any more, and nothing is claimed about it.
    tab.on_event(
        COORDINATOR,
        4,
        "lane-state",
        &json!({
            "lane": 1, "state": "idle", "task": null, "goal": null, "restarts": 1, "pid": 2
        }),
    );
    assert_eq!(
        tab.lanes().lane(1).map(|row| row.status),
        Some(LaneStatus::Idle)
    );
    assert_eq!(
        tab.lane_down_reason(1),
        None,
        "a lane that is up has no reason"
    );

    // A lane whose own transcript is not loaded falls back to the coordinator's line about
    // it — the only evidence the tab has. It is down first: a reason needs a red row.
    assert!(tab.lane_model(2).is_none());
    assert_eq!(
        tab.lane_down_reason(2),
        None,
        "the coordinator has said nothing about lane 2"
    );
    tab.on_event(
        COORDINATOR,
        5,
        "output",
        &json!({
            "style": "error",
            "text": "[lane 2] is down: its process exited. restart_lane brings it back."
        }),
    );
    assert_eq!(
        tab.lane_down_reason(2),
        None,
        "lane 2 is not down: nothing is shown for a lane that is up"
    );
    tab.on_event(
        COORDINATOR,
        6,
        "lane-state",
        &json!({
            "lane": 2, "state": "down", "task": null, "goal": null, "restarts": 0, "pid": null
        }),
    );
    assert_eq!(
        tab.lane_down_reason(2).as_deref(),
        Some("[lane 2] is down: its process exited. restart_lane brings it back.")
    );

    // A lane whose own transcript has no error keeps the fallback too.
    tab.select(lane(3));
    tab.on_event(
        lane(3),
        1,
        "output",
        &json!({ "style": "dim", "text": "just talking" }),
    );
    assert_eq!(tab.lane_down_reason(3), None);

    // `[lane 1]` is not `[lane 10]`: the match is the whole bracket token.
    tab.on_event(
        COORDINATOR,
        7,
        "output",
        &json!({
            "style": "error",
            "text": "[lane 10] is down: its process exited. restart_lane brings it back."
        }),
    );
    assert!(
        tab.lane_down_reason(1).is_none(),
        "lane 10's line is not lane 1's, and lane 1 is up"
    );
    assert_eq!(
        tab.lane_down_reason(10),
        None,
        "lane 10 is not down either: the line alone does not make a red row"
    );
}

/// A lane that failed to start is announced by the swarm as such, and that line is
/// the reason — `swarm/lanes.lisp`'s bring-up failure.
#[test]
fn a_lane_that_failed_to_start_says_so() {
    let mut tab = TabModel::new();
    let changes = tab.on_event(
        COORDINATOR,
        1,
        "lane-state",
        &json!({
            "lane": 1, "state": "down", "task": null, "goal": null, "restarts": 0, "pid": null
        }),
    );
    assert!(changes.lanes);
    assert_eq!(tab.lane_down_reason(1), None, "nothing has been said yet");
    tab.on_event(
        COORDINATOR,
        2,
        "output",
        &json!({
            "style": "error",
            "text": "[lane 1] failed to start — see /tmp/swarm/lane-1/lane.log"
        }),
    );
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some("[lane 1] failed to start — see /tmp/swarm/lane-1/lane.log")
    );
}

/// The reason outlives a resync. `settled` refetches and rebuilds the coordinator's
/// rows from `/transcript`, which carries no `output` lines: a reason read only out of
/// those rows would leave the red row mute from the first resync on — and a lane
/// crash is *always* followed by one, because the swarm wakes the coordinator with it.
#[test]
fn a_down_reason_survives_a_resync() {
    let mut tab = TabModel::new();
    tab.on_event(
        COORDINATOR,
        1,
        "lane-state",
        &json!({
            "lane": 1, "state": "down", "task": null, "goal": null, "restarts": 0, "pid": null
        }),
    );
    tab.on_event(COORDINATOR, 2, "output", &json!({
        "style": "error",
        "text": "[lane 1] crashed and was restarted by its supervisor (pid 1 → 2); its session was resumed and it was re-initialized."
    }));
    let reason = tab.lane_down_reason(1).expect("the swarm's account");

    // The resync: the rows are rebuilt from a transcript, and the `output` line is not
    // in it (`Dim` rows are events, not messages).
    tab.on_transcript(
        COORDINATOR,
        1,
        &json!({"messages": [
            { "role": "user", "content": [ { "type": "text", "text": "delegate this" } ] },
        ]}),
    );
    assert!(tab
        .coordinator()
        .rows()
        .iter()
        .all(|row| !matches!(row.kind, RowKind::Dim { .. })));
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some(reason.as_str()),
        "the red row keeps what the swarm said"
    );

    // The lane comes up: the row is not red, and the reason goes with it.
    tab.on_event(
        COORDINATOR,
        3,
        "lane-state",
        &json!({
            "lane": 1, "state": "idle", "task": null, "goal": null, "restarts": 1, "pid": 2
        }),
    );
    assert_eq!(tab.lane_down_reason(1), None);
}

#[test]
fn the_stream_badge_tracks_reconnecting() {
    let mut tab = TabModel::new();
    // The default is connected: a tab that has heard nothing shows no badge.
    assert_eq!(tab.stream_status(lane(1)), StreamStatus::Connected);
    assert!(!tab.is_reconnecting(lane(1)));
    assert!(tab.on_stream(lane(1), StreamStatus::Connected).is_empty());

    // A drop: the badge changes, and the retry delay comes through.
    let changes = tab.on_stream(
        lane(1),
        StreamStatus::Reconnecting {
            retry_in: Duration::from_millis(500),
        },
    );
    assert_eq!(changes.stream, std::collections::BTreeSet::from([lane(1)]));
    assert_eq!(
        tab.stream_status(lane(1)),
        StreamStatus::Reconnecting {
            retry_in: Duration::from_millis(500)
        }
    );
    assert!(tab.is_reconnecting(lane(1)));
    assert!(!tab.is_reconnecting(COORDINATOR), "the badge is per agent");

    // A different delay is news; the same one is not.
    assert!(tab
        .on_stream(
            lane(1),
            StreamStatus::Reconnecting {
                retry_in: Duration::from_secs(2)
            }
        )
        .stream
        .contains(&lane(1)));
    assert!(tab
        .on_stream(
            lane(1),
            StreamStatus::Reconnecting {
                retry_in: Duration::from_secs(2)
            }
        )
        .is_empty());
    assert!(tab
        .on_stream(lane(1), StreamStatus::Connected)
        .stream
        .contains(&lane(1)));
}

#[test]
fn a_cache_seed_lands_in_the_readout() {
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    assert!(
        !tab.readout_text().contains("cached"),
        "the capture's journal is all input"
    );

    // The seed can arrive as the whole /journal body…
    let seed = json!({ "entries": [
        { "type": "custom", "key": "cache-stats", "data": { "input": 300, "cache_read": 9700, "cache_write": 0 } }
    ]});
    assert!(tab.on_cache_seed(Some(&seed)).readout);
    assert!(
        tab.readout_text().contains("97% cached"),
        "line: {}",
        tab.readout_text()
    );

    // …as the entry itself…
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    let entry = json!({ "type": "custom", "key": "cache-stats", "data": { "input": 0, "cache_read": 5, "cache_write": 5 } });
    assert!(tab.on_cache_seed(Some(&entry)).readout);
    assert!(tab.readout_text().contains("50% cached"));

    // …or as the totals object alone.
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    assert!(
        tab.on_cache_seed(Some(
            &json!({ "input": 0, "cache_read": 95, "cache_write": 5 })
        ))
        .readout
    );
    assert!(tab.readout_text().contains("95% cached"));

    // "Not found, grow the limit": nothing to seed, and live totals are not cleared.
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    tab.on_cache_seed(Some(&seed));
    let line = tab.readout_text();
    assert!(tab
        .on_cache_seed(Some(&fixture("journal-empty.json")))
        .is_empty());
    assert!(tab.on_cache_seed(None).is_empty());
    assert_eq!(tab.readout_text(), line);
}

#[test]
fn a_reset_refetches_every_agent_that_is_loaded() {
    let mut tab = TabModel::new();
    tab.select(lane(1));
    tab.on_transcript(lane(1), 3, &fixture("lane1-transcript.json"));
    tab.on_transcript(COORDINATOR, 5, &fixture("transcript.json"));

    let changes = tab.on_reset();
    assert_eq!(
        changes.needs_resync,
        std::collections::BTreeSet::from([COORDINATOR, lane(1)]),
        "the coordinator and every lane with a model"
    );
    // The rows are kept until the refetch replaces them.
    assert!(!tab.selected_rows().is_empty());
    assert!(!tab.coordinator().rows().is_empty());

    // The revision gates are gone: the new numbering starts wherever it likes.
    let changes = tab.on_transcript(lane(1), 1, &fixture("lane1-transcript.json"));
    assert_eq!(changes.rows_for(lane(1)), Some(&RowChanges::Rebuilt));
    assert!(tab
        .on_transcript(COORDINATOR, 1, &fixture("transcript.json"))
        .rows_for(COORDINATOR)
        .is_some());
}

/// `compact-result` is a **TUI-internal** event, not a server one: `src/tui/tui.lisp` pushes
/// it onto the TUI's own queue (`push-event tui …`) around the `compaction-start`/`-end`
/// pair it *does* `emit-event`, and nothing else in evo-agent mentions it. The server never
/// sends it, so the tab ignores it instead of growing a branch the app can never reach. The
/// compaction outcome it would carry reaches the app as the `output` line the server does
/// emit (`✗ compact: …`, `style: error`) plus `task-end`'s `outcome`.
#[test]
fn a_tui_internal_event_is_not_a_server_event() {
    let mut tab = TabModel::new();
    let changes = tab.on_event(
        COORDINATOR,
        1,
        "compact-result",
        &json!({ "task-id": "t", "outcome": "error", "text": "Nothing to compact" }),
    );
    assert!(
        changes.is_empty(),
        "unknown events are ignored: {changes:?}"
    );

    // What the server does send for a failed compaction (capture: events-compact.sse): the
    // error line becomes a row, and the task ends.
    let mut tab = TabModel::new();
    let changes = feed(&mut tab, COORDINATOR, "events-compact.sse");
    assert!(matches!(
        changes.rows_for(COORDINATOR),
        Some(RowChanges::Changed(_))
    ));
    let error = tab
        .coordinator()
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Dim {
                style: session::DimStyle::Error,
                text,
            } => Some(text.clone()),
            _ => None,
        });
    assert!(
        error.is_some_and(|text| text.starts_with("✗ compact:")),
        "rows: {:?}",
        tab.coordinator().rows()
    );
}

/// The step clock the tab hands to a frontend: `on_event_at` carries the arrival time in,
/// `on_event` leaves it unset, and `Changes::step` says when to restart a clock of one's own.
#[test]
fn the_step_clock_is_stamped_by_the_caller() {
    let mut tab = TabModel::new();
    assert_eq!(tab.coordinator_step_started(), None);

    let changes = tab.on_event_at(
        COORDINATOR,
        1,
        "run-start",
        &json!({"run_id": "r", "turn": 2}),
        5_000,
    );
    assert!(changes.step.contains(&COORDINATOR));
    let clock = tab.coordinator_step_started().expect("a step");
    assert_eq!(clock.turn, 2);
    assert_eq!(clock.event_id, 1);
    assert_eq!(clock.started_at_millis, Some(5_000));

    // A lane's step is its own: the coordinator's clock is untouched by the lane's events.
    let changes = tab.on_event_at(lane(1), 1, "turn-start", &json!({"turn": 0}), 6_000);
    assert_eq!(changes.step, std::collections::BTreeSet::from([lane(1)]));
    assert_eq!(tab.coordinator_step_started().unwrap().event_id, 1);
    assert_eq!(
        tab.lane_model(1)
            .unwrap()
            .step_started()
            .unwrap()
            .started_at_millis,
        Some(6_000)
    );

    // Without a stamp the step is still announced and recorded — the frontend starts its own
    // clock when this arrives.
    let changes = tab.on_event(COORDINATOR, 2, "turn-start", &json!({"turn": 3}));
    assert!(changes.step.contains(&COORDINATOR));
    let clock = tab.coordinator_step_started().unwrap();
    assert_eq!(
        (clock.turn, clock.event_id, clock.started_at_millis),
        (3, 2, None)
    );

    // The run ending ends the clock.
    let changes = tab.on_event(
        COORDINATOR,
        3,
        "task-end",
        &json!({"task_id": "t", "kind": "run"}),
    );
    assert!(changes.step.contains(&COORDINATOR));
    assert_eq!(tab.coordinator_step_started(), None);
}

/// The lane step clock is stamped by the caller the same way: a `/lanes` read carries the
/// moment the tab applied it, and a `lane-state` that starts a step the moment the event
/// arrived — so the left column's clock counts on between reads (§7.3).
#[test]
fn the_lane_step_clock_is_stamped_by_the_caller() {
    let seen = 1_700_000_000_000u64;

    let mut tab = TabModel::new();
    tab.on_lanes_at(&fixture("lanes-lane1-working.json"), Some(seen));
    let lane1 = tab.lanes().lane(1).expect("lane 1").clone();
    assert_eq!(lane1.step_age_at_millis, Some(seen));
    assert_eq!(lane1.step_clock_at(seen + 61_000).as_deref(), Some("1m"));

    // Without the stamp the clock stands where the swarm left it; a frontend with its own
    // clock is what `Changes::step` is for, not this.
    let mut tab = TabModel::new();
    tab.on_lanes(&fixture("lanes-lane1-working.json"));
    let lane1 = tab.lanes().lane(1).expect("lane 1").clone();
    assert_eq!(lane1.step_age_at_millis, None);
    assert_eq!(lane1.step_clock_at(seen + 61_000).as_deref(), Some("0s"));

    // A lane-state that enters a step is stamped with the moment the tab saw it.
    let mut tab = TabModel::new();
    tab.on_lanes_at(&fixture("lanes.json"), Some(seen));
    assert!(
        tab.on_event_at(
            COORDINATOR,
            36,
            "lane-state",
            &json!({ "lane": 1, "state": "working", "task": "x", "goal": null, "restarts": 0, "pid": 1 }),
            seen + 5_000,
        )
        .lanes
    );
    let lane1 = tab.lanes().lane(1).expect("lane 1").clone();
    assert_eq!(lane1.step_age, Some(0));
    assert_eq!(lane1.step_age_at_millis, Some(seen + 5_000));
    assert_eq!(lane1.step_clock_at(seen + 25_000).as_deref(), Some("20s"));
}

/// The lane row's two formatters, against the swarm's own (`short-duration` and the
/// `lane-status-line` truncation), checked value for value with Common Lisp's `floor` and
/// `length`.
#[test]
fn the_lane_row_formats_as_the_swarm_does() {
    for (seconds, expected) in [
        (0u64, "0s"),
        (45, "45s"),
        (59, "59s"),
        (60, "1m"),
        (90, "1m"),
        (599, "9m"),
        (3599, "59m"),
        (3600, "1h0m"),
        (3660, "1h1m"),
        (7325, "2h2m"),
        (36000, "10h0m"),
    ] {
        assert_eq!(session::short_duration(seconds), expected, "{seconds}s");
    }

    let short = "lane step one";
    assert_eq!(session::lane_task_label(short), short);
    let sixty: String = "a".repeat(60);
    assert_eq!(
        session::lane_task_label(&sixty),
        sixty,
        "60 characters is not truncated"
    );
    let sixty_one: String = "a".repeat(61);
    assert_eq!(
        session::lane_task_label(&sixty_one),
        format!("{}…", "a".repeat(60))
    );
    assert_eq!(session::lane_task_label("two\nlines"), "two lines");

    // A real lane task from the capture, longer than 60 characters.
    let list = session::LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    let lane1 = list.lane(1).unwrap();
    let label = lane1.task_label().expect("a task");
    assert_eq!(
        label.chars().count(),
        61,
        "60 characters plus the ellipsis: {label}"
    );
    assert!(label.ends_with('…'));
    assert!(label.starts_with("DELAY3 CALL todo"));
    assert_eq!(lane1.step_clock().as_deref(), Some("0s"));
    assert_eq!(
        list.lane(2).unwrap().step_clock(),
        None,
        "an idle lane has no step clock"
    );
}

/// The event stream and the transcript are the same session, so a tab that rebuilt from
/// the transcript and a tab that folded the events agree — including which rows are dirty.
#[test]
fn the_two_assembly_paths_agree_through_the_tab() {
    let mut from_events = TabModel::new();
    feed(&mut from_events, COORDINATOR, "events-coordinator.sse");

    let mut from_transcript = TabModel::new();
    from_transcript.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));

    let rows = |tab: &TabModel| -> Vec<RowKind> {
        tab.coordinator()
            .rows()
            .iter()
            .filter(|row| !matches!(row.kind, RowKind::Dim { .. }))
            .map(|row| row.kind.clone())
            .collect()
    };
    assert_eq!(rows(&from_events), rows(&from_transcript));
}

/// §9.7 — a lane that cannot register its model says so.
///
/// The wording is the swarm's own, from the model check it runs in a lane's init
/// (`swarm/init.lisp`): the lane keeps running, so nothing about it is *down* — but
/// what it says arrives on the coordinator's stream, error-styled, and the tab shows
/// it verbatim. The lane row stays what it is (a lane that is up); the failure is the
/// line, and this pins both halves: the wording is not ours to paraphrase, and it is
/// not swallowed into a row that says nothing.
#[test]
fn a_lane_that_cannot_register_its_model_says_so() {
    let mut model = TabModel::new();
    model.on_lanes(&json!({
        "swarm": { "id": "s", "dir": "/sw", "cwd": "/p", "workers": 1, "busy": 0, "stopping": false },
        "lanes": [ { "n": 1, "state": "idle", "restarts": 0, "reports": 0 } ],
    }));

    // The line the swarm sends: `swarm/init.lisp`'s check, wrapped by the lane's own
    // init failure report (`swarm/lanes.lisp`), as it goes over the wire.
    let line = "lane 1 cannot use its model stub-a: its API \"anthropic-messages\" is not in \
the lane — an extension defines it, so load that extension in the lanes with \
(evo.swarm:in-lanes ...) in swarm.lisp";
    model.on_event_at(
        AgentKey::Coordinator,
        1,
        "output",
        &json!({ "style": "error", "text": format!("[lane 1] initialization failed: {line}") }),
        1_000,
    );

    let said: Vec<&RowKind> = model
        .coordinator()
        .rows()
        .iter()
        .map(|row| &row.kind)
        .filter(|kind| matches!(kind, RowKind::Dim { text, .. } if text.contains("cannot use its model")))
        .collect();
    assert_eq!(
        said.len(),
        1,
        "the lane's own words, once: {:?}",
        model.coordinator().rows()
    );
    match said[0] {
        RowKind::Dim { style, text } => {
            assert_eq!(*style, DimStyle::Error, "it is a failure: {text}");
            assert!(text
                .starts_with("[lane 1] initialization failed: lane 1 cannot use its model stub-a"));
            assert!(text.contains("in-lanes"), "the remedy survives too: {text}");
        }
        other => panic!("expected the error line, got {other:?}"),
    }

    // The lane itself is up — a model it cannot use is not a crash — so nothing about
    // it claims a reason, and what it said is where a reader can see it: the line above.
    assert_eq!(
        model.lanes().lane(1).map(|row| row.status),
        Some(LaneStatus::Idle)
    );
    assert_eq!(
        model.lane_down_reason(1),
        None,
        "a lane that is up claims nothing"
    );
}

/// The status row under the center column reads the **selected** agent's metrics: the
/// coordinator's on `main`, the lane's own when a lane is selected, and nothing while that
/// lane has not been read yet — which the UI writes as its own muted "no metrics yet".
///
/// A lane's line is built from what the API carries for a lane: the model and provider its
/// transcript's assistant messages name, the usage they report, and the window its model
/// has in `/registry`. There is no per-lane `/state`, so the segments that live only there
/// (the thinking level, the goal) are not invented for it.
#[test]
fn the_status_line_follows_the_selection() {
    let mut tab = TabModel::new();
    tab.on_registry(&fixture("registry.json"));
    tab.on_state(&fixture("state.json"));
    assert_eq!(
        tab.selected_readout_text().as_deref(),
        Some("stub-a · medium · ctx 0k/200k (0%)"),
        "`main` shows the coordinator's line"
    );

    // The coordinator's rebuild leaves that line alone: `/state` is its seed, and the
    // transcript's usage (15 tokens a message) is not newer news than the state's.
    tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));
    assert_eq!(
        tab.selected_readout_text().as_deref(),
        Some("stub-a · medium · ctx 0k/200k (0%)")
    );

    // A lane selected before anything about it has been read: the UI has nothing to show.
    let changes = tab.select(lane(1));
    assert!(changes.selection);
    assert!(
        changes.readout,
        "the row under the column is a different one"
    );
    assert_eq!(tab.selected_readout_text(), None);
    assert_eq!(tab.selected(), lane(1));

    // Its transcript is what it has to say: the model, the provider, the usage, and the
    // window from the catalog.
    let changes = tab.on_transcript(lane(1), 1, &fixture("lane1-transcript.json"));
    assert!(changes.readout, "the lane's first line is news");
    assert_eq!(
        tab.selected_readout_text().as_deref(),
        Some("stub-a · ctx 0k/200k (0%)")
    );

    // Its own stream keeps the line live…
    let changes = tab.on_event(
        lane(1),
        99,
        "message-end",
        &json!({ "usage": { "input": 48211, "output": 100, "cache_read": 9700, "cache_write": 200 } }),
    );
    assert!(changes.readout, "the selected lane's line moved");
    assert_eq!(
        tab.selected_readout_text().as_deref(),
        Some("stub-a · ctx 58k/200k (29%) · 17% cached")
    );

    // …and stays quiet while the line on screen belongs to someone else.
    tab.select(COORDINATOR);
    let changes = tab.on_event(
        lane(1),
        100,
        "message-end",
        &json!({ "usage": { "input": 90000, "output": 0, "cache_read": 0, "cache_write": 0 } }),
    );
    assert!(!changes.readout, "nothing shown moved");
    assert_eq!(
        tab.selected_readout_text().as_deref(),
        Some("stub-a · medium · ctx 0k/200k (0%)")
    );

    // The lane kept its own figure all the same, and `main`'s is untouched.
    assert_eq!(
        tab.lane_model(1)
            .map(|model| model.readout().context_tokens()),
        Some(90000)
    );
    assert_eq!(tab.readout().context_tokens(), 0);

    // A lane's own events never move the coordinator's activity, and never its line.
    assert!(!changes.activity);
}

/// §9.1: the row a tool-only step opened is **dropped** at `message-end`, and the tab
/// says so the only way it can — the dropped id comes back in `Changed` while the model
/// no longer holds it. That pair is the contract with the UI: an id in `Changed` with no
/// row behind it is what tells the transcript view to `remove` the row instead of
/// refreshing it. Skip it and the empty streaming row stays on screen, drawing its
/// waiting dots over the tool rows that follow until the run's `settled` resync finally
/// rebuilds the transcript.
#[test]
fn a_tool_only_step_reports_the_row_it_dropped() {
    let mut tab = TabModel::new();
    tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json"));
    let before = tab.coordinator().rows().len();

    // A step that calls a tool: `message-start` opens a row, `message-end` ends it
    // carrying no text, no thinking and no error.
    tab.on_event(COORDINATOR, 2, "message-start", &json!({}));
    let opened = tab.coordinator().streaming_row().expect("the opened row");
    let changes = tab.on_event(
        COORDINATOR,
        3,
        "message-end",
        &json!({ "usage": null, "error": null }),
    );
    assert_eq!(
        changes.rows_for(COORDINATOR),
        Some(&RowChanges::Changed(vec![opened])),
        "the dropped row is named, so the UI can drop its own copy"
    );
    assert!(
        tab.coordinator().row(opened).is_none(),
        "and it is gone from the model: an id in `Changed` with no row behind it"
    );
    assert_eq!(
        tab.coordinator().rows().len(),
        before,
        "a tool-only step leaves no row behind"
    );
    assert_eq!(
        tab.coordinator().streaming_row(),
        None,
        "and nothing is left open"
    );

    // The tool row the step goes on to open follows the row above it: the id the
    // dropped row had is never handed out again.
    tab.on_event(
        COORDINATOR,
        4,
        "tool-call-start",
        &json!({ "name": "bash", "id": "t1", "arguments_json": "{\"command\":\"ls\"}" }),
    );
    let tool = tab.coordinator().rows().last().expect("the tool row");
    assert!(tool.id > opened, "the dropped id is not reused");

    // The next step's message opens a row of its own — a new id, and the only row
    // that is streaming (whatever the step goes on to say).
    let changes = tab.on_event(COORDINATOR, 5, "message-start", &json!({}));
    assert_eq!(
        changes.rows_for(COORDINATOR),
        Some(&RowChanges::Changed(vec![tab
            .coordinator()
            .streaming_row()
            .expect("the next row")])),
        "and it is announced as the streaming one"
    );
    let streaming: Vec<session::RowId> = tab
        .coordinator()
        .rows()
        .iter()
        .filter(|row| {
            matches!(
                &row.kind,
                RowKind::Assistant { markdown, thinking, streaming: true, .. }
                    if markdown.is_empty() && thinking.is_empty()
            )
        })
        .map(|row| row.id)
        .collect();
    assert_eq!(
        streaming,
        vec![tab.coordinator().rows().last().expect("a row").id],
        "exactly one empty streaming row, and it is the last"
    );
    assert!(streaming[0] > opened, "it is not the row that was dropped");
}
