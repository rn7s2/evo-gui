//! The event reducer, driven by the captured fixtures.
//!
//! Events that the stub provider cannot produce (`thinking-delta`, `provider-retry`,
//! `gap`) are built here in the exact shape the servers send them, and the test that
//! uses one says where the shape comes from. Everything else comes off the wire.

mod common;

use common::{apply_capture, fixture, sse_events, transcript_rows, RowView};
use serde_json::json;
use session::{Activity, AgentModel, DimStyle, Effect, RowKind, TodoStatus};

fn kinds(model: &AgentModel) -> Vec<&RowKind> {
    model.rows().iter().map(|row| &row.kind).collect()
}

fn dim_texts(model: &AgentModel) -> Vec<String> {
    model
        .rows()
        .iter()
        .filter_map(|row| match &row.kind {
            RowKind::Dim { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn rebuild_pairs_tool_calls_with_their_results() {
    let mut model = AgentModel::new();
    let effect = model.rebuild_from_transcript(&fixture("transcript.json"));
    assert_eq!(effect, Effect::REBUILT);
    assert_eq!(model.revision(), 1);

    // 21 messages in the capture: user turns, assistant messages (text and/or tool
    // calls), and one tool result per call.
    let rows = transcript_rows(&model);
    assert!(rows.len() > 10, "rows: {rows:#?}");
    assert!(matches!(rows[0], RowView::User(_)), "first row: {:#?}", rows[0]);

    let ids: Vec<u64> = model.rows().iter().map(|row| row.id).collect();
    let mut unique = ids.clone();
    unique.dedup();
    assert_eq!(ids, unique, "row ids are unique and in order");
    assert_eq!(ids.first(), Some(&1), "a fresh model numbers rows from 1");

    // Every tool call was completed by its result, by call id.
    let tools: Vec<&session::Row> = model
        .rows()
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Tool { .. }))
        .collect();
    assert_eq!(tools.len(), 4, "four tool calls in the capture: {tools:#?}");
    for row in &tools {
        match &row.kind {
            RowKind::Tool { call_id, name, arguments, result } => {
                assert!(!call_id.is_empty());
                assert!(!name.is_empty());
                assert!(arguments.starts_with('{'), "{name} arguments: {arguments}");
                let result = result.as_ref().unwrap_or_else(|| panic!("{name} has no result"));
                assert!(!result.is_error, "{name} result: {result:?}");
                assert!(!result.content.is_empty());
            }
            _ => unreachable!(),
        }
    }

    // The tool that ran the checklist carries what the todo tool printed.
    let todo = tools
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Tool { name, result, .. } if name == "todo" => result.as_ref(),
            _ => None,
        })
        .expect("the todo call");
    assert!(todo.content.contains("Todo list updated: 3 items"));
}

#[test]
fn a_rebuild_starts_a_fresh_id_space_and_bumps_the_revision() {
    let mut model = AgentModel::new();
    model.rebuild_from_transcript(&fixture("transcript.json"));
    let first: Vec<RowView> = transcript_rows(&model);
    let revision = model.revision();

    model.rebuild_from_transcript(&fixture("transcript.json"));
    assert_eq!(model.revision(), revision + 1);
    assert_eq!(transcript_rows(&model), first, "the same transcript, the same rows");
    assert_eq!(model.rows().first().map(|row| row.id), Some(1), "ids restart at 1");
    assert_eq!(model.rows().len(), first.len(), "no duplicate rows");
}

/// The heart of §9.1: the events and the transcript describe the same session. Feed the
/// coordinator's whole captured event log to one model and the captured `/transcript` to
/// another; the transcript rows must be identical.
#[test]
fn the_event_stream_and_the_transcript_agree() {
    let mut from_events = AgentModel::new();
    let effect = apply_capture(&mut from_events, "events-coordinator.sse");
    assert!(effect.contains(Effect::ROWS));
    assert!(effect.contains(Effect::RESYNC), "settled asks for a resync");
    assert!(effect.contains(Effect::TODOS), "todo-changed arrives on this stream");
    assert!(effect.contains(Effect::LANES), "lane-state events go by on this stream");

    let mut from_transcript = AgentModel::new();
    from_transcript.rebuild_from_transcript(&fixture("transcript.json"));

    assert_eq!(
        transcript_rows(&from_events),
        transcript_rows(&from_transcript),
        "the event log and /transcript describe different rows"
    );

    // ...and the event-only rows are the ones the transcript cannot carry.
    assert!(!dim_texts(&from_events).is_empty(), "output lines become dim rows");
    assert!(dim_texts(&from_transcript).is_empty());
}

#[test]
fn output_lines_take_the_style_the_server_sent() {
    let mut model = AgentModel::new();
    apply_capture(&mut model, "events-coordinator.sse");
    let dims: Vec<(DimStyle, String)> = model
        .rows()
        .iter()
        .filter_map(|row| match &row.kind {
            RowKind::Dim { style, text } => Some((*style, text.clone())),
            _ => None,
        })
        .collect();
    assert!(
        dims.iter().any(|(style, text)| *style == DimStyle::Notice && text.starts_with("◆ goal")),
        "dims: {dims:#?}"
    );
    assert!(
        dims.iter().any(|(_, text)| text.starts_with("[lane 1]")),
        "the lane's run ending is reported in the coordinator's transcript"
    );
}

#[test]
fn a_lane_run_gives_its_todo_list_and_its_tool_pair() {
    let mut model = AgentModel::new();
    let effect = apply_capture(&mut model, "lane1-events.sse");
    assert!(effect.contains(Effect::TODOS));

    assert_eq!(
        model.todos(),
        &[
            session::Todo { text: "lane step one".into(), status: TodoStatus::InProgress },
            session::Todo { text: "lane step two".into(), status: TodoStatus::Pending },
        ]
    );

    let tool = model
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Tool { name, arguments, result, .. } if name == "todo" => {
                Some((arguments.clone(), result.clone()))
            }
            _ => None,
        })
        .expect("the todo call");
    assert!(tool.0.contains("lane step one"), "arguments: {}", tool.0);
    assert!(!tool.1.expect("a result").is_error);

    // The lane's own text, appended from its deltas.
    assert!(transcript_rows(&model)
        .iter()
        .any(|row| matches!(row, RowView::Assistant { markdown, .. } if markdown == "tool done")));
}

#[test]
fn a_report_event_becomes_a_report_row() {
    let mut model = AgentModel::new();
    apply_capture(&mut model, "lane2-events.sse");
    let report = model
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Report { done, evidence, next, blocked, requests } => {
                Some((done.clone(), evidence.clone(), next.clone(), blocked.clone(), requests.clone()))
            }
            _ => None,
        })
        .expect("a report row");
    assert_eq!(report.0, "lane 2 finished the fixture work");
    assert_eq!(report.1, "lane2-transcript.json");
    assert_eq!(report.2, "nothing");
    assert_eq!(report.3, "");
    assert_eq!(report.4, "none");
}

#[test]
fn message_start_and_deltas_build_one_streaming_row() {
    let mut model = AgentModel::new();
    // Seed the readout with a context window, so the usage that ends the message moves
    // the rendered line (that is what the UI has to hear about).
    model.readout_mut().apply_state(&json!({"context_tokens": 48000, "context_window": 200000}));
    assert_eq!(model.apply_event(1, "turn-start", &json!({"run_id": "r", "turn": 0})), Effect::NONE);
    assert_eq!(model.apply_event(2, "message-start", &json!({"run_id": "r", "turn": 0})), Effect::ROWS);
    let id = model.streaming_row().expect("a streaming row");

    for (n, text) in ["## head", "ing\n\n"] .iter().enumerate() {
        assert_eq!(model.apply_event(3 + n as u64, "text-delta", &json!({"text": text})), Effect::ROWS);
    }
    let row = model.row(id).expect("the row");
    match &row.kind {
        RowKind::Assistant { markdown, thinking, streaming, error } => {
            assert_eq!(markdown, "## heading\n\n", "the whole source so far, ready to render");
            assert!(thinking.is_empty());
            assert!(*streaming);
            assert!(error.is_none());
        }
        other => panic!("unexpected row: {other:?}"),
    }
    assert!(row.version >= 3, "every delta bumps the version: {}", row.version);

    // An empty delta changes nothing at all.
    let version = row.version;
    assert_eq!(model.apply_event(9, "text-delta", &json!({"text": ""})), Effect::NONE);
    assert_eq!(model.row(id).unwrap().version, version);

    // The provider's usage re-anchors the context figure: 48211 + 100 + 9700 + 200.
    assert_eq!(
        model.apply_event(10, "message-end", &json!({"stop_reason": "stop", "usage": {"input": 48211, "output": 100, "cache_read": 9700, "cache_write": 200}, "error": null})),
        Effect::ROWS | Effect::READOUT
    );
    assert_eq!(model.readout().context_tokens(), 58211);
    assert_eq!(model.readout().text(), "ctx 58k/200k (29%) · 17% cached");
    assert!(model.streaming_row().is_none());
    match &model.row(id).unwrap().kind {
        RowKind::Assistant { streaming, .. } => assert!(!streaming, "message-end closes the row"),
        _ => unreachable!(),
    }
}

#[test]
fn a_tool_only_turn_leaves_no_assistant_row() {
    // The stub answers a tool call with an empty text block: `message-start`, then
    // `message-end`, and the call itself. The transcript fold carries no assistant
    // message for that, so neither does the model.
    let mut model = AgentModel::new();
    model.apply_event(1, "message-start", &json!({}));
    assert_eq!(model.rows().len(), 1, "the streaming row exists while it streams");
    model.apply_event(2, "tool-call-start", &json!({"name": "todo", "id": "toolu_1", "arguments": {}, "arguments_json": "{}"}));
    model.apply_event(3, "message-end", &json!({"stop_reason": "tool-use", "usage": null, "error": null}));
    assert_eq!(model.rows().len(), 1, "rows: {:#?}", kinds(&model));
    assert!(matches!(model.rows()[0].kind, RowKind::Tool { .. }));
}

#[test]
fn thinking_deltas_accumulate_on_the_open_row() {
    // Shape from the anthropic adapter's emit and docs/serve.md §5: `thinking-delta`
    // carries `text`, exactly like `text-delta`.
    let mut model = AgentModel::new();
    model.apply_event(1, "message-start", &json!({}));
    model.apply_event(2, "thinking-delta", &json!({"text": "weighing "}));
    model.apply_event(3, "thinking-delta", &json!({"text": "options"}));
    model.apply_event(4, "text-delta", &json!({"text": "answer"}));
    match &model.rows()[0].kind {
        RowKind::Assistant { markdown, thinking, .. } => {
            assert_eq!(thinking, "weighing options");
            assert_eq!(markdown, "answer");
        }
        other => panic!("unexpected row: {other:?}"),
    }
    // A thinking-only message is kept: it is not an empty row.
    let mut model = AgentModel::new();
    model.apply_event(1, "message-start", &json!({}));
    model.apply_event(2, "thinking-delta", &json!({"text": "hmm"}));
    model.apply_event(3, "message-end", &json!({"usage": null, "error": null}));
    assert_eq!(model.rows().len(), 1);
}

#[test]
fn a_failed_message_keeps_its_error_on_the_row() {
    let mut model = AgentModel::new();
    model.apply_event(1, "message-start", &json!({}));
    model.apply_event(2, "text-delta", &json!({"text": "half a thou"}));
    let effect = model.apply_event(3, "message-end", &json!({
        "stop_reason": "error",
        "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
        "error": "overloaded_error: Overloaded"
    }));
    assert_eq!(effect, Effect::ROWS, "a zero usage moves nothing");
    match &model.rows()[0].kind {
        RowKind::Assistant { markdown, streaming, error, .. } => {
            assert_eq!(markdown, "half a thou");
            assert!(!streaming);
            assert_eq!(error.as_deref(), Some("overloaded_error: Overloaded"));
        }
        other => panic!("unexpected row: {other:?}"),
    }

    // ...and a failed message whose `message-start` this stream never saw still shows up.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "message-end", &json!({"stop_reason": "error", "usage": null, "error": "boom"})),
        Effect::ROWS
    );
    match &model.rows()[0].kind {
        RowKind::Assistant { markdown, error, .. } => {
            assert!(markdown.is_empty());
            assert_eq!(error.as_deref(), Some("boom"));
        }
        other => panic!("unexpected row: {other:?}"),
    }
}

#[test]
fn a_manual_compaction_drives_activity_and_dim_rows() {
    let mut model = AgentModel::new();
    let events = sse_events("events-compact.sse");
    let mut effects = Vec::new();
    for (id, kind, data) in &events {
        let effect = model.apply_event(*id, kind, data);
        if kind == "task-start" {
            assert_eq!(effect, Effect::ACTIVITY);
            assert_eq!(model.activity(), Activity::Compacting, "a compact task is compacting");
        }
        if kind == "task-end" {
            assert_eq!(effect, Effect::ACTIVITY);
            assert_eq!(model.activity(), Activity::Idle);
        }
        if kind == "settled" {
            assert!(effect.contains(Effect::RESYNC), "settled is a resync point");
        }
        effects.push((kind.clone(), effect));
    }
    assert_eq!(model.activity(), Activity::Idle);

    let dims = dim_texts(&model);
    assert!(dims.contains(&"compacting...".to_string()), "dims: {dims:#?}");
    assert!(dims.contains(&"compaction finished".to_string()), "dims: {dims:#?}");
    let failure = model.rows().iter().find_map(|row| match &row.kind {
        RowKind::Dim { style: DimStyle::Error, text } => Some(text.clone()),
        _ => None,
    });
    assert!(
        failure.is_some_and(|text| text.starts_with("✗ compact:")),
        "the compact task's failure is an error line: {dims:#?}"
    );
}

#[test]
fn a_coordinator_run_start_stops_compacting() {
    let mut model = AgentModel::new();
    model.apply_event(1, "task-start", &json!({"task_id": "t", "kind": "compact"}));
    assert_eq!(model.activity(), Activity::Compacting);
    // A run's turn boundary starts inside the compaction of the same run: the task is
    // still the compaction.
    assert_eq!(model.apply_event(2, "run-start", &json!({"run_id": "r", "turn": 0})), Effect::NONE);
    assert_eq!(model.activity(), Activity::Compacting);

    let mut model = AgentModel::new();
    assert_eq!(model.apply_event(1, "task-start", &json!({"task_id": "t", "kind": "run"})), Effect::ACTIVITY);
    assert_eq!(model.activity(), Activity::Running);
    assert_eq!(model.apply_event(2, "run-start", &json!({"run_id": "r", "turn": 0})), Effect::NONE);
    assert_eq!(model.apply_event(3, "task-end", &json!({"task_id": "t", "kind": "run", "outcome": "stop"})), Effect::ACTIVITY);
    assert_eq!(model.activity(), Activity::Idle);
}

#[test]
fn provider_retry_is_a_dim_notice() {
    // Shape from `src/provider/core.lisp`'s retry emit: attempt, max, delay, reason.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "provider-retry", &json!({"attempt": 2, "max": 4, "delay": 1.5, "reason": "overloaded_error: Overloaded\n(second line)"})),
        Effect::ROWS
    );
    match &model.rows()[0].kind {
        RowKind::Dim { style, text } => {
            assert_eq!(*style, DimStyle::Notice);
            assert_eq!(text, "⟲ retry 2/4 · overloaded_error: Overloaded");
        }
        other => panic!("unexpected row: {other:?}"),
    }
}

#[test]
fn resync_events_and_lifecycle() {
    for kind in ["hello", "gap", "session-switched", "settled"] {
        let mut model = AgentModel::new();
        let data = if kind == "session-switched" { json!({"session": "/x.sexp"}) } else { json!({}) };
        assert!(model.apply_event(1, kind, &data).contains(Effect::RESYNC), "{kind}");
    }
    // A `lane-state` event is the tab's, not this agent's.
    let mut model = AgentModel::new();
    assert_eq!(model.apply_event(1, "lane-state", &json!({"lane": 1, "state": "working"})), Effect::LANES);
    // An event serve could not map: logged, not rendered.
    assert_eq!(model.apply_event(1, "unprintable-event", &json!({"original_type": "x"})), Effect::NONE);
    // Lifecycle and anything newer than this build.
    for kind in ["ready", "shutdown", "bye", "some-future-event"] {
        let mut model = AgentModel::new();
        assert_eq!(model.apply_event(7, kind, &json!({})), Effect::NONE, "{kind}");
    }
}

#[test]
fn an_orphan_tool_result_still_gets_a_row() {
    // A stream that joined after the call (a resync, a `?since=` inside a turn). The
    // field spellings are the wire's (`src/serve/json.lisp`'s `keyword->json-key` turns
    // `:is-error` into `is_error`), taken from a captured `tool-result` event.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "tool-result", &json!({"name": "bash", "id": "toolu_9", "is_error": true, "content_chars": 12, "content": "no such file"})),
        Effect::ROWS
    );
    match &model.rows()[0].kind {
        RowKind::Tool { call_id, name, arguments, result } => {
            assert_eq!(call_id, "toolu_9");
            assert_eq!(name, "bash");
            assert!(arguments.is_empty());
            let result = result.as_ref().unwrap();
            assert!(result.is_error);
            assert_eq!(result.content, "no such file");
            assert_eq!(result.content_chars, Some(12));
        }
        other => panic!("unexpected row: {other:?}"),
    }
}

#[test]
fn a_replayed_tool_call_updates_its_row_instead_of_stacking_a_copy() {
    let mut model = AgentModel::new();
    model.apply_event(1, "tool-call-start", &json!({"name": "bash", "id": "toolu_1", "arguments": {}, "arguments_json": "{}"}));
    model.apply_event(2, "tool-call-start", &json!({"name": "bash", "id": "toolu_1", "arguments": {"command": "ls"}, "arguments_json": "{\"command\": \"ls\"}"}));
    assert_eq!(model.rows().len(), 1);
    match &model.rows()[0].kind {
        RowKind::Tool { arguments, .. } => assert_eq!(arguments, "{\"command\": \"ls\"}"),
        other => panic!("unexpected row: {other:?}"),
    }
}

/// §3: a restarted coordinator is a new event log whose ids start again at 1, announced by
/// `hello`. The view must keep working — no duplicated rows, no lost events.
#[test]
fn event_ids_restarting_at_one_do_not_disturb_the_rows() {
    let capture: Vec<(u64, String, serde_json::Value)> = sse_events("lane1-events.sse");
    let mut model = AgentModel::new();
    for (id, kind, data) in &capture {
        model.apply_event(*id, kind, data);
    }
    let first_pass = model.rows().len();
    let revision = model.revision();

    // The restarted server replays the same log, ids 1..N again.
    model.rebuild_from_transcript(&fixture("transcript-empty.json"));
    assert_eq!(model.rows().len(), 0, "the restarted session starts empty");
    assert!(model.revision() > revision);
    for (id, kind, data) in &capture {
        model.apply_event(*id, kind, data);
    }
    assert_eq!(model.rows().len(), first_pass);
    let mut ids: Vec<u64> = model.rows().iter().map(|row| row.id).collect();
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count, "no row id was handed out twice");
    assert_eq!(model.rows()[0].id, 1, "the fresh id space starts at 1");
}

#[test]
fn apply_state_seeds_readout_todos_and_activity() {
    // This capture is a run in flight: the goal is active and the session is running.
    let mut model = AgentModel::new();
    let effect = model.apply_state(&fixture("state-with-goal.json"));
    assert!(effect.contains(Effect::READOUT));
    assert!(effect.contains(Effect::ACTIVITY));
    assert_eq!(model.activity(), Activity::Running, "the fixture was captured mid-run");
    assert_eq!(model.readout().goal_label().unwrap(), "goal g-4cb9 (active) 0k");

    // A settled session: idle, with the checklist the todo tool left behind.
    let mut idle = AgentModel::new();
    let effect = idle.apply_state(&fixture("state-final.json"));
    assert!(effect.contains(Effect::READOUT));
    assert!(effect.contains(Effect::TODOS));
    assert_eq!(idle.activity(), Activity::Idle);
    assert_eq!(idle.readout().text(), "stub-a · medium · ctx 0k/200k (0%) · goal g-4cb9 (complete) 0k");
    assert_eq!(idle.todos().len(), 3);
    assert_eq!(idle.todos()[0].status, TodoStatus::InProgress);
    assert_eq!(idle.todos()[1].status, TodoStatus::Pending);
    assert_eq!(idle.todos()[2].status, TodoStatus::Done);
}

#[test]
fn a_todo_change_replaces_the_whole_list() {
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "todo-changed", &json!({"todos": [{"text": "one", "status": "in_progress"}]})),
        Effect::TODOS
    );
    assert_eq!(model.todos()[0].status, TodoStatus::InProgress, "the model's own spelling");
    assert_eq!(model.apply_event(2, "todo-changed", &json!({"todos": [{"text": "one", "status": "done"}]})), Effect::TODOS);
    assert_eq!(model.todos().len(), 1);
    assert_eq!(model.todos()[0].status, TodoStatus::Done);
    assert_eq!(model.apply_event(3, "todo-changed", &json!({"todos": []})), Effect::TODOS);
    assert!(model.todos().is_empty());
}
