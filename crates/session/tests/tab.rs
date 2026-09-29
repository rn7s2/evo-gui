//! `TabModel`: the whole tab driven by the captured fixtures, one update at a time.
//!
//! The coordinator capture (`events-coordinator.sse`) is a real run — two lanes, a goal
//! that completes, a delegation, a `todo-changed`, and `settled` at the end — so a tab fed
//! it covers the whole reducer path end to end; the lane captures drive the per-lane side.

mod common;

use common::{fixture, sse_events};
use serde_json::json;
use session::{
    Activity, AgentKey, Changes, LaneStatus, RowChanges, RowKind, StreamStatus, TabModel, TodoStatus,
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
    assert!(!changes.activity, "state.json is idle and a fresh model is idle too");
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
    assert!(changes.needs_resync.contains(&COORDINATOR), "settled asks for a resync");
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
    assert!(tab.readout_text().contains("58k/200k (29%) · 17% cached"), "line: {}", tab.readout_text());
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
    assert_eq!(changes.rows_for(COORDINATOR), Some(&RowChanges::Changed(vec![streaming])));
    assert!(!changes.rows_for(COORDINATOR).unwrap().is_rebuilt());

    // A delta that carries nothing is no change at all.
    let changes = tab.on_event(COORDINATOR, 3, "text-delta", &json!({ "text": "" }));
    assert!(changes.is_empty());

    // A tool result completes a row the stream opened earlier.
    tab.on_event(COORDINATOR, 4, "message-end", &json!({ "usage": null, "error": null }));
    tab.on_event(COORDINATOR, 5, "tool-call-start", &json!({ "name": "bash", "id": "t1", "arguments_json": "{}" }));
    let call = tab.coordinator().rows().last().expect("the tool row").id;
    let changes = tab.on_event(COORDINATOR, 6, "tool-result", &json!({ "name": "bash", "id": "t1", "content": "ok" }));
    assert_eq!(changes.rows_for(COORDINATOR), Some(&RowChanges::Changed(vec![call])));
}

/// §9.1: a late transcript from an older revision must not overwrite a newer one.
#[test]
fn a_stale_transcript_is_dropped() {
    let mut tab = TabModel::new();
    // Revision 7 arrives first (the display symbols, a later fetch).
    let rev7 = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "seven" }] }] });
    assert_eq!(
        tab.on_transcript(COORDINATOR, 7, &rev7).rows_for(COORDINATOR),
        Some(&RowChanges::Rebuilt)
    );
    let rows = tab.coordinator().rows().len();

    // An answer to an older request lands late: dropped, and the rows are untouched.
    let stale = json!({ "messages": [{ "role": "user", "content": [{ "type": "text", "text": "stale" }] }] });
    let changes = tab.on_transcript(COORDINATOR, 6, &stale);
    assert!(changes.is_empty(), "a stale answer changes nothing: {changes:?}");
    assert_eq!(tab.coordinator().rows().len(), rows);
    match &tab.selected_rows()[0].kind {
        RowKind::User { text } => assert_eq!(text, "seven"),
        other => panic!("the newer transcript is still on screen: {other:?}"),
    }

    // The same revision again is applied — the transcript is the truth, and re-reading it
    // is harmless (a fresh id space, the same rows).
    let changes = tab.on_transcript(COORDINATOR, 7, &rev7);
    assert_eq!(changes.rows_for(COORDINATOR), Some(&RowChanges::Rebuilt));
    assert_eq!(tab.coordinator().rows().len(), rows);
}

/// §3: the supervisor restarts a crashed coordinator, and the new server's ids start at 1.
/// The tab must keep working — no duplicate rows, and a fresh transcript must not be
/// refused as stale.
#[test]
fn hello_after_a_restart_keeps_one_set_of_rows() {
    let mut tab = TabModel::new();
    // The lane's stream is the simplest complete log: ids 1..N, a tool call, todos.
    feed(&mut tab, lane(1), "lane1-events.sse");
    let first_rows: Vec<RowKind> =
        tab.lane_model(1).expect("the lane's model").rows().iter().map(|row| row.kind.clone()).collect();
    assert!(!first_rows.is_empty());

    // The old server's transcript was stamped 9; the restarted one answers with its own
    // revision 1. `hello` is what says the numbering restarted.
    tab.on_transcript(lane(1), 9, &fixture("transcript-empty.json"));
    assert!(tab.lane_model(1).unwrap().rows().is_empty());
    let changes = tab.on_event(lane(1), 1, "hello", &json!({ "pid": 4242 }));
    assert!(changes.needs_resync.contains(&lane(1)), "hello refetches this agent");
    let changes = tab.on_transcript(lane(1), 1, &fixture("lane1-transcript.json"));
    assert!(changes.rows_for(lane(1)).unwrap().is_rebuilt(), "the fresh transcript applies");

    // The restarted server replays the same log, ids 1..N again: a second set of rows, no
    // id handed out twice within the fresh space.
    let changes = feed(&mut tab, lane(1), "lane1-events.sse");
    assert!(changes.rows_for(lane(1)).is_some());
    let ids: Vec<u64> = tab.lane_model(1).unwrap().rows().iter().map(|row| row.id).collect();
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

/// §9.7: a lane that is down says why, from its own transcript's `output` lines.
#[test]
fn lane_down_reason_comes_from_the_lanes_own_output_rows() {
    let mut tab = TabModel::new();
    // No transcript loaded: nothing to say.
    assert_eq!(tab.lane_down_reason(1), None);

    tab.select(lane(1));
    // The lane's model init fails: serve says so as an error-styled output line, exactly
    // the shape `swarm/init.lisp`'s check produces (`✗ lane 1 cannot use its model ...`).
    tab.on_event(lane(1), 1, "output", &json!({
        "style": "error",
        "text": "✗ lane 1 cannot use its model st-1: its API :openai-completions is not in the lane"
    }));
    // Work goes on after the error; the reason stays the error line, not the newest line.
    tab.on_event(lane(1), 2, "task-start", &json!({ "task_id": "t", "kind": "run" }));
    tab.on_event(lane(1), 3, "output", &json!({ "style": "dim", "text": "queued" }));

    // The lane goes down (a lane-state on the coordinator's stream).
    let changes = tab.on_event(COORDINATOR, 1, "lane-state", &json!({
        "lane": 1, "state": "down", "task": null, "goal": null, "restarts": 0, "pid": null
    }));
    assert!(changes.lanes);
    let row = tab.lanes().lane(1).unwrap();
    assert_eq!(row.status, LaneStatus::Down);
    assert_eq!(row.glyph(), '✗');
    assert_eq!(
        tab.lane_down_reason(1).as_deref(),
        Some("✗ lane 1 cannot use its model st-1: its API :openai-completions is not in the lane")
    );

    // A lane with no error line has no reason.
    tab.select(lane(2));
    tab.on_event(lane(2), 1, "output", &json!({ "style": "dim", "text": "just talking" }));
    assert_eq!(tab.lane_down_reason(2), None);
}

#[test]
fn the_stream_badge_tracks_reconnecting() {
    let mut tab = TabModel::new();
    // The default is connected: a tab that has heard nothing shows no badge.
    assert_eq!(tab.stream_status(lane(1)), StreamStatus::Connected);
    assert!(!tab.is_reconnecting(lane(1)));
    assert!(tab.on_stream(lane(1), StreamStatus::Connected).is_empty());

    // A drop: the badge changes, and the retry delay comes through.
    let changes = tab.on_stream(lane(1), StreamStatus::Reconnecting { retry_in: Duration::from_millis(500) });
    assert_eq!(changes.stream, std::collections::BTreeSet::from([lane(1)]));
    assert_eq!(
        tab.stream_status(lane(1)),
        StreamStatus::Reconnecting { retry_in: Duration::from_millis(500) }
    );
    assert!(tab.is_reconnecting(lane(1)));
    assert!(!tab.is_reconnecting(COORDINATOR), "the badge is per agent");

    // A different delay is news; the same one is not.
    assert!(
        tab.on_stream(lane(1), StreamStatus::Reconnecting { retry_in: Duration::from_secs(2) })
            .stream
            .contains(&lane(1))
    );
    assert!(tab.on_stream(lane(1), StreamStatus::Reconnecting { retry_in: Duration::from_secs(2) }).is_empty());
    assert!(tab.on_stream(lane(1), StreamStatus::Connected).stream.contains(&lane(1)));
}

#[test]
fn a_cache_seed_lands_in_the_readout() {
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    assert!(!tab.readout_text().contains("cached"), "the capture's journal is all input");

    // The seed can arrive as the whole /journal body…
    let seed = json!({ "entries": [
        { "type": "custom", "key": "cache-stats", "data": { "input": 300, "cache_read": 9700, "cache_write": 0 } }
    ]});
    assert!(tab.on_cache_seed(Some(&seed)).readout);
    assert!(tab.readout_text().contains("97% cached"), "line: {}", tab.readout_text());

    // …as the entry itself…
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    let entry = json!({ "type": "custom", "key": "cache-stats", "data": { "input": 0, "cache_read": 5, "cache_write": 5 } });
    assert!(tab.on_cache_seed(Some(&entry)).readout);
    assert!(tab.readout_text().contains("50% cached"));

    // …or as the totals object alone.
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    assert!(tab.on_cache_seed(Some(&json!({ "input": 0, "cache_read": 95, "cache_write": 5 }))).readout);
    assert!(tab.readout_text().contains("95% cached"));

    // "Not found, grow the limit": nothing to seed, and live totals are not cleared.
    let mut tab = TabModel::new();
    tab.on_state(&fixture("state-final.json"));
    tab.on_cache_seed(Some(&seed));
    let line = tab.readout_text();
    assert!(tab.on_cache_seed(Some(&fixture("journal-empty.json"))).is_empty());
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
    assert!(tab.on_transcript(COORDINATOR, 1, &fixture("transcript.json")).rows_for(COORDINATOR).is_some());
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
    assert!(changes.is_empty(), "unknown events are ignored: {changes:?}");

    // What the server does send for a failed compaction (capture: events-compact.sse): the
    // error line becomes a row, and the task ends.
    let mut tab = TabModel::new();
    let changes = feed(&mut tab, COORDINATOR, "events-compact.sse");
    assert!(matches!(changes.rows_for(COORDINATOR), Some(RowChanges::Changed(_))));
    let error = tab.coordinator().rows().iter().find_map(|row| match &row.kind {
        RowKind::Dim { style: session::DimStyle::Error, text } => Some(text.clone()),
        _ => None,
    });
    assert!(error.is_some_and(|text| text.starts_with("✗ compact:")), "rows: {:?}", tab.coordinator().rows());
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
    assert_eq!(session::lane_task_label(&sixty), sixty, "60 characters is not truncated");
    let sixty_one: String = "a".repeat(61);
    assert_eq!(session::lane_task_label(&sixty_one), format!("{}…", "a".repeat(60)));
    assert_eq!(session::lane_task_label("two\nlines"), "two lines");

    // A real lane task from the capture, longer than 60 characters.
    let list = session::LaneList::from_lanes(&fixture("lanes-lane1-working.json"));
    let lane1 = list.lane(1).unwrap();
    let label = lane1.task_label().expect("a task");
    assert_eq!(label.chars().count(), 61, "60 characters plus the ellipsis: {label}");
    assert!(label.ends_with('…'));
    assert!(label.starts_with("DELAY3 CALL todo"));
    assert_eq!(lane1.step_clock().as_deref(), Some("0s"));
    assert_eq!(list.lane(2).unwrap().step_clock(), None, "an idle lane has no step clock");
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
