//! The event reducer, driven by the captured fixtures.
//!
//! Events that the stub provider cannot produce (`thinking-delta`, `provider-retry`,
//! `gap`) are built here in the exact shape the servers send them, and the test that
//! uses one says where the shape comes from. Everything else comes off the wire.

mod common;

use common::{apply_capture, fixture, sse_events, transcript_rows, RowView};
use serde_json::json;
use session::{Activity, AgentModel, DimStyle, Effect, RowKind, StepClock, TodoStatus};

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
    assert!(
        matches!(rows[0], RowView::User(_)),
        "first row: {:#?}",
        rows[0]
    );

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
            RowKind::Tool {
                call_id,
                name,
                arguments,
                result,
            } => {
                assert!(!call_id.is_empty());
                assert!(!name.is_empty());
                assert!(arguments.starts_with('{'), "{name} arguments: {arguments}");
                let result = result
                    .as_ref()
                    .unwrap_or_else(|| panic!("{name} has no result"));
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
    assert_eq!(
        transcript_rows(&model),
        first,
        "the same transcript, the same rows"
    );
    assert_eq!(
        model.rows().first().map(|row| row.id),
        Some(1),
        "ids restart at 1"
    );
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
    assert!(
        effect.contains(Effect::TODOS),
        "todo-changed arrives on this stream"
    );
    assert!(
        effect.contains(Effect::LANES),
        "lane-state events go by on this stream"
    );

    let mut from_transcript = AgentModel::new();
    from_transcript.rebuild_from_transcript(&fixture("transcript.json"));

    assert_eq!(
        transcript_rows(&from_events),
        transcript_rows(&from_transcript),
        "the event log and /transcript describe different rows"
    );

    // ...and the event-only rows are the ones the transcript cannot carry.
    assert!(
        !dim_texts(&from_events).is_empty(),
        "output lines become dim rows"
    );
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
        dims.iter()
            .any(|(style, text)| *style == DimStyle::Notice && text.starts_with("◆ goal")),
        "dims: {dims:#?}"
    );
    assert!(
        dims.iter().any(|(_, text)| text.starts_with("[lane 1]")),
        "the lane's run ending is reported in the coordinator's transcript"
    );
    // ...and an output line is the server's text, whole: no prefix names the event it came
    // from, because the reader is reading what evo said, not which event carried it.
    assert!(
        dims.contains(&(
            DimStyle::Notice,
            "◆ goal created: fixture goal: show the goal segment FINISH".to_string()
        )),
        "dims: {dims:#?}"
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
            session::Todo {
                text: "lane step one".into(),
                status: TodoStatus::InProgress
            },
            session::Todo {
                text: "lane step two".into(),
                status: TodoStatus::Pending
            },
        ]
    );

    let tool = model
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Tool {
                name,
                arguments,
                result,
                ..
            } if name == "todo" => Some((arguments.clone(), result.clone())),
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
    let row = model
        .rows()
        .iter()
        .find(|row| matches!(row.kind, RowKind::Report { .. }))
        .expect("a report row");
    let RowKind::Report {
        done,
        evidence,
        next,
        blocked,
        requests,
        goal,
        lane,
    } = &row.kind
    else {
        unreachable!("the row above is a report")
    };
    assert_eq!(done, "lane 2 finished the fixture work");
    assert_eq!(evidence, "lane2-transcript.json");
    assert_eq!(next, "nothing");
    assert_eq!(blocked, "");
    assert_eq!(requests, "none");
    assert_eq!(*goal, None, "the captured lane had no goal");
    assert_eq!(*lane, None, "the lane's own stream does not name its lane");
}

/// The memory extension injects a snapshot into a fresh session with
/// `evo:inject-context`, which journals a `:custom-message` carrying a `:key`; /transcript
/// sends it back as a user-role message with `"meta": {"key": "<key>"}`. The reader typed
/// neither snapshot, so neither is one of their turns: both are context rows, and the
/// turns around them are what the transcript counts.
///
/// The capture is `tests/fixtures/context-transcript.json`, recorded by
/// `tests/capture_context_fixture.py` from a real server.
/// The swarm talks to the coordinator by steering it (`tell-coordinator` →
/// `evo:steer`, `swarm/lanes.lisp:19`), so its own words arrive as user-role messages.
/// This is the coordinator's captured transcript from a run that delegated to two
/// lanes: the three lane messages in it are the swarm's, not the reader's, and none of
/// them is a turn.
#[test]
fn the_swarms_words_about_a_lane_are_not_the_readers() {
    let mut model = AgentModel::new();
    model.rebuild_from_transcript(&fixture("transcript.json"));
    let rows = transcript_rows(&model);

    assert!(
        !rows
            .iter()
            .any(|row| matches!(row, RowView::User(text) if text.starts_with("[lane "))),
        "the swarm's words stop looking like the reader's: {rows:#?}"
    );

    let lane_rows: Vec<&RowView> = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                RowView::LaneNotice { .. } | RowView::Report { lane: Some(_), .. }
            )
        })
        .collect();
    assert_eq!(lane_rows.len(), 3, "lane rows: {lane_rows:#?}");

    match lane_rows[0] {
        RowView::LaneNotice { lane, text, tone } => {
            assert_eq!(*lane, 1);
            // evo truncates the task itself (`truncate-string … 80 "…"`, lanes.lisp:241).
            assert!(
                text.starts_with("run ended (stop) — task: DELAY3"),
                "{text:?}"
            );
            assert_eq!(*tone, DimStyle::Notice, "a run that ended as asked");
        }
        other => panic!("lane 1's run end is not a notice: {other:#?}"),
    }
    match lane_rows[1] {
        RowView::Report {
            done,
            evidence,
            next,
            blocked,
            requests,
            goal,
            lane,
        } => {
            assert_eq!(*lane, Some(2));
            assert_eq!(done, "lane 2 finished the fixture work");
            assert_eq!(evidence, "lane2-transcript.json");
            assert_eq!(next, "nothing");
            // evo writes the line for an empty value too (the report's `blocked` was "").
            assert_eq!(blocked, "");
            assert_eq!(requests, "none");
            assert_eq!(*goal, None, "the captured lane had no goal");
        }
        other => panic!("lane 2's report is not a report row: {other:#?}"),
    }
    match lane_rows[2] {
        RowView::LaneNotice { lane, text, .. } => {
            assert_eq!(*lane, 2);
            assert!(
                text.starts_with("run ended (stop) — task: CALL report"),
                "{text:?}"
            );
        }
        other => panic!("lane 2's run end is not a notice: {other:#?}"),
    }
}

/// Every line the swarm writes, in evo's own words: the formats are quoted from
/// `swarm/lanes.lisp`, which is where these strings come from — there is no `meta` key
/// and no event of their own, so the wording is all the app has to go on.
#[test]
fn every_line_the_swarm_writes_becomes_a_lane_row() {
    let mut model = AgentModel::new();
    let steering = |model: &mut AgentModel, id: u64, text: &str| {
        model.apply_event(id, "steering", &json!({ "text": text }));
    };

    // :193 `"[lane ~d] failed to start — see ~a"`, ~a the lane's log.
    steering(
        &mut model,
        1,
        "[lane 1] failed to start — see /tmp/evo/lanes/1/lane.log",
    );
    // :202 `"[lane ~d] initialization failed: ~a"`.
    steering(
        &mut model,
        2,
        "[lane 3] initialization failed: no model is configured",
    );
    // :237 `"[lane ~d] run ended (~a)~@[ — goal: ~a~]~@[ — task: ~a~]"`.
    steering(&mut model, 3, "[lane 2] run ended (stop) — goal: active, but the lane is idle until steered — task: port the readout");
    // :286 `"[lane ~d] error: ~a"`.
    steering(
        &mut model,
        4,
        "[lane 1] error: eval failed: undefined function",
    );
    // :295 `"[lane ~d] ~a"`, a lane's own error output, which evo only forwards when the
    // output itself was styled an error.
    steering(&mut model, 5, "[lane 1] Error: no such package");
    // :341 `"[lane ~d] is down: its process exited. restart_lane brings it back."`
    steering(
        &mut model,
        6,
        "[lane 2] is down: its process exited. restart_lane brings it back.",
    );
    // :359 `"[lane ~d] crashed and was restarted by its supervisor (pid ~a → ~a); …"`
    steering(&mut model, 7, "[lane 2] crashed and was restarted by its supervisor (pid 4711 → 4712); its session was resumed and it was re-initialized.");
    // :220 `"[lane ~d report] done: ~a~@[~%evidence: ~a~]~@[~%next: ~a~]~@[~%blocked: ~a~]~@[~%requests: ~a~]~@[~%goal: ~a~]"`
    // with a `done` of more than one line, a blank line inside it, an empty `blocked`,
    // and a goal.
    steering(&mut model, 8, "[lane 2 report] done: ported the readout\n\nand its tests\nevidence: cargo test -p session\nnext: wait for the review\nblocked: \nrequests: none\ngoal: active, but the lane is idle until steered");

    let kinds: Vec<&RowKind> = model.rows().iter().map(|row| &row.kind).collect();
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, RowKind::User { .. })),
        "the swarm steering the coordinator is not the reader typing: {kinds:#?}"
    );

    let notices: Vec<(u32, &str, DimStyle)> = kinds
        .iter()
        .filter_map(|kind| match kind {
            RowKind::LaneNotice { lane, text, tone } => Some((*lane, text.as_str(), *tone)),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        vec![
            (1, "failed to start — see /tmp/evo/lanes/1/lane.log", DimStyle::Error),
            (3, "initialization failed: no model is configured", DimStyle::Error),
            (2, "run ended (stop) — goal: active, but the lane is idle until steered — task: port the readout", DimStyle::Notice),
            (1, "error: eval failed: undefined function", DimStyle::Error),
            (1, "Error: no such package", DimStyle::Error),
            (2, "is down: its process exited. restart_lane brings it back.", DimStyle::Error),
            (2, "crashed and was restarted by its supervisor (pid 4711 → 4712); its session was resumed and it was re-initialized.", DimStyle::Error),
        ],
        "one line per thing the swarm said"
    );

    let Some(RowKind::Report {
        done,
        evidence,
        next,
        blocked,
        requests,
        goal,
        lane,
    }) = kinds.last()
    else {
        panic!("the last row is not the report: {kinds:#?}")
    };
    assert_eq!(*lane, Some(2));
    assert_eq!(done, "ported the readout\n\nand its tests");
    assert_eq!(evidence, "cargo test -p session");
    assert_eq!(next, "wait for the review");
    assert_eq!(blocked, "");
    assert_eq!(requests, "none");
    assert_eq!(
        goal.as_deref(),
        Some("active, but the lane is idle until steered")
    );
}

/// The same lines on the rebuild path, where they arrive as the user-role messages
/// `/transcript` carries — and a line the reader wrote that happens to open with the
/// prefix is still theirs.
#[test]
fn a_rebuilt_transcript_reads_the_swarms_words_too() {
    let mut model = AgentModel::new();
    model.rebuild_from_transcript(&json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "[lane 4] failed to start — see /tmp/evo/lanes/4/lane.log"}]},
            {"role": "user", "content": [{"type": "text", "text": "[lane 1 report] done: nothing to do\nrequests: none"}]},
            {"role": "user", "content": [{"type": "text", "text": "[lane four] is that right?"}]},
            {"role": "user", "content": [{"type": "text", "text": "over to you"}]},
        ]
    }));
    assert_eq!(
        transcript_rows(&model),
        vec![
            RowView::LaneNotice {
                lane: 4,
                text: "failed to start — see /tmp/evo/lanes/4/lane.log".to_string(),
                tone: DimStyle::Error,
            },
            RowView::Report {
                done: "nothing to do".to_string(),
                evidence: String::new(),
                next: String::new(),
                blocked: String::new(),
                requests: "none".to_string(),
                goal: None,
                lane: Some(1),
            },
            RowView::User("[lane four] is that right?".to_string()),
            RowView::User("over to you".to_string()),
        ]
    );
}

#[test]
fn an_injected_message_is_context_rather_than_a_user_turn() {
    let mut model = AgentModel::new();
    model.rebuild_from_transcript(&fixture("context-transcript.json"));

    let rows = transcript_rows(&model);
    assert_eq!(rows.len(), 4, "rows: {rows:#?}");
    let (global, project, typed, assistant) = (&rows[0], &rows[1], &rows[2], &rows[3]);
    match (global, project, typed, assistant) {
        (
            RowView::Context { key, text },
            RowView::Context {
                key: project_key,
                text: project_text,
            },
            RowView::User(said),
            RowView::Assistant { .. },
        ) => {
            assert_eq!(key, "global-memory");
            assert!(text.starts_with("<global-memory>"), "{text:?}");
            assert_eq!(project_key, "project-memory");
            assert!(
                project_text.starts_with("<project-memory>"),
                "{project_text:?}"
            );
            assert_eq!(said, "Say hello.");
        }
        other => panic!("unexpected rows: {other:#?}"),
    }
}

/// A `meta` is not enough on its own: a message with no key, or with an empty one, is
/// anything else the fold carries and stays the reader's.
#[test]
fn a_message_without_a_context_key_is_still_a_user_turn() {
    let mut model = AgentModel::new();
    model.rebuild_from_transcript(&json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "typed"}]},
            {"role": "user", "content": [{"type": "text", "text": "tagged"}], "meta": {"key": ""}},
            {"role": "user", "content": [{"type": "text", "text": "labelled"}], "meta": {"key": null}},
            {"role": "user", "content": [{"type": "text", "text": "other"}], "meta": {"other": "x"}},
        ]
    }));
    assert_eq!(
        transcript_rows(&model),
        vec![
            RowView::User("typed".to_string()),
            RowView::User("tagged".to_string()),
            RowView::User("labelled".to_string()),
            RowView::User("other".to_string()),
        ]
    );
}

#[test]
fn message_start_and_deltas_build_one_streaming_row() {
    let mut model = AgentModel::new();
    // Seed the readout with a context window, so the usage that ends the message moves
    // the rendered line (that is what the UI has to hear about).
    model
        .readout_mut()
        .apply_state(&json!({"context_tokens": 48000, "context_window": 200000}));
    // A turn opening is a step boundary: the clock starts, and this model joined mid-run.
    assert_eq!(
        model.apply_event(1, "turn-start", &json!({"run_id": "r", "turn": 0})),
        Effect::STEP | Effect::ACTIVITY
    );
    assert_eq!(
        model.apply_event(2, "message-start", &json!({"run_id": "r", "turn": 0})),
        Effect::ROWS
    );
    let id = model.streaming_row().expect("a streaming row");

    for (n, text) in ["## head", "ing\n\n"].iter().enumerate() {
        assert_eq!(
            model.apply_event(3 + n as u64, "text-delta", &json!({"text": text})),
            Effect::ROWS
        );
    }
    let row = model.row(id).expect("the row");
    match &row.kind {
        RowKind::Assistant {
            markdown,
            thinking,
            streaming,
            error,
        } => {
            assert_eq!(
                markdown, "## heading\n\n",
                "the whole source so far, ready to render"
            );
            assert!(thinking.is_empty());
            assert!(*streaming);
            assert!(error.is_none());
        }
        other => panic!("unexpected row: {other:?}"),
    }
    assert!(
        row.version >= 3,
        "every delta bumps the version: {}",
        row.version
    );

    // An empty delta changes nothing at all.
    let version = row.version;
    assert_eq!(
        model.apply_event(9, "text-delta", &json!({"text": ""})),
        Effect::NONE
    );
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
    assert_eq!(
        model.rows().len(),
        1,
        "the streaming row exists while it streams"
    );
    model.apply_event(
        2,
        "tool-call-start",
        &json!({"name": "todo", "id": "toolu_1", "arguments": {}, "arguments_json": "{}"}),
    );
    model.apply_event(
        3,
        "message-end",
        &json!({"stop_reason": "tool-use", "usage": null, "error": null}),
    );
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
        RowKind::Assistant {
            markdown, thinking, ..
        } => {
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
    let effect = model.apply_event(
        3,
        "message-end",
        &json!({
            "stop_reason": "error",
            "usage": {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
            "error": "overloaded_error: Overloaded"
        }),
    );
    assert_eq!(effect, Effect::ROWS, "a zero usage moves nothing");
    match &model.rows()[0].kind {
        RowKind::Assistant {
            markdown,
            streaming,
            error,
            ..
        } => {
            assert_eq!(markdown, "half a thou");
            assert!(!streaming);
            assert_eq!(error.as_deref(), Some("overloaded_error: Overloaded"));
        }
        other => panic!("unexpected row: {other:?}"),
    }

    // ...and a failed message whose `message-start` this stream never saw still shows up.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(
            1,
            "message-end",
            &json!({"stop_reason": "error", "usage": null, "error": "boom"})
        ),
        Effect::ROWS
    );
    match &model.rows()[0].kind {
        RowKind::Assistant {
            markdown, error, ..
        } => {
            assert!(markdown.is_empty());
            assert_eq!(error.as_deref(), Some("boom"));
        }
        other => panic!("unexpected row: {other:?}"),
    }
}

/// The step clock (§5's "activity state, step clock" for `run-start`/`turn-start`, and the
/// TUI's own `begin-step`, which fires at a turn opening and at both ends of a compaction).
/// The model holds no clock: the arrival time is the caller's, and it is optional.
#[test]
fn the_step_clock_follows_the_step_boundaries() {
    let mut model = AgentModel::new();
    assert_eq!(model.step_started(), None, "nothing runs yet");

    assert!(model
        .apply_event_at(1, "run-start", &json!({"run_id": "r", "turn": 3}), 1_000)
        .contains(Effect::STEP));
    assert_eq!(
        model.step_started(),
        Some(StepClock {
            turn: 3,
            event_id: 1,
            started_at_millis: Some(1_000)
        })
    );

    // A turn opening restarts it, and keeps the turn the event carries.
    model.apply_event_at(2, "turn-start", &json!({"run_id": "r", "turn": 4}), 2_000);
    assert_eq!(
        model.step_started(),
        Some(StepClock {
            turn: 4,
            event_id: 2,
            started_at_millis: Some(2_000)
        })
    );

    // A message ending inside the step is not a boundary.
    model.apply_event_at(
        3,
        "message-end",
        &json!({"usage": null, "error": null}),
        2_500,
    );
    assert_eq!(model.step_started().unwrap().event_id, 2);

    // A compaction is its own step, and handing the turn back starts the clock again — the
    // TUI calls `begin-step` at both ends for exactly this reason.
    model.apply_event_at(
        4,
        "compaction-start",
        &json!({"run_id": "r", "turn": 4}),
        3_000,
    );
    assert_eq!(model.step_started().unwrap().event_id, 4);
    model.apply_event_at(
        5,
        "compaction-end",
        &json!({"run_id": "r", "turn": 4}),
        4_000,
    );
    assert_eq!(model.step_started().unwrap().started_at_millis, Some(4_000));

    // The run ending ends the step.
    assert!(model
        .apply_event(6, "run-end", &json!({"outcome": "stop"}))
        .contains(Effect::STEP));
    assert_eq!(model.step_started(), None);

    // Unstamped: the step is still recorded, without a time for the frontend to count from.
    let mut model = AgentModel::new();
    model.apply_event(1, "turn-start", &json!({"turn": 7}));
    assert_eq!(
        model.step_started(),
        Some(StepClock {
            turn: 7,
            event_id: 1,
            started_at_millis: None
        })
    );

    // The clock is formatted the way the swarm formats a lane's step age, from the same
    // `short_duration`, so the two columns of the left list count alike.
    let clock = StepClock {
        turn: 0,
        event_id: 1,
        started_at_millis: Some(1_000),
    };
    assert_eq!(clock.elapsed(1_000), Some(std::time::Duration::ZERO));
    assert_eq!(clock.elapsed(3_500).unwrap().as_millis(), 2_500);
    assert_eq!(clock.clock_label(1_000).as_deref(), Some("0s"));
    assert_eq!(clock.clock_label(46_000).as_deref(), Some("45s"));
    assert_eq!(clock.clock_label(91_000).as_deref(), Some("1m"));
    assert_eq!(clock.clock_label(3_601_000).as_deref(), Some("1h0m"));
    // A clock behind the stamp (two clocks disagreeing) reads as zero, not as a panic.
    assert_eq!(clock.elapsed(0), Some(std::time::Duration::ZERO));
    assert_eq!(clock.clock_label(0).as_deref(), Some("0s"));
    // No stamp, no elapsed time — and no label either.
    let unstamped = StepClock {
        turn: 0,
        event_id: 2,
        started_at_millis: None,
    };
    assert_eq!(unstamped.elapsed(9_999), None);
    assert_eq!(unstamped.clock_label(9_999), None);

    // A restarted server has no step running, and neither does a switched session.
    let mut model = AgentModel::new();
    model.apply_event_at(1, "turn-start", &json!({"turn": 0}), 10);
    assert!(model
        .apply_event(2, "hello", &json!({"pid": 9}))
        .contains(Effect::STEP));
    assert_eq!(
        model.step_started(),
        None,
        "the old process's step is not ours"
    );
    let mut model = AgentModel::new();
    model.apply_event_at(1, "turn-start", &json!({"turn": 0}), 10);
    model.apply_event(2, "session-switched", &json!({"session": "/x.sexp"}));
    assert_eq!(model.step_started(), None);
    // ...but a `gap` is this session's own missed events: the step stands, and the reader
    // is told what the missing events mean instead of being shown the event name.
    let mut model = AgentModel::new();
    model.apply_event_at(1, "turn-start", &json!({"turn": 0}), 10);
    model.apply_event(2, "gap", &json!({}));
    assert!(model.step_started().is_some());
    match &model.rows().last().unwrap().kind {
        RowKind::Dim { style, text } => {
            assert_eq!(*style, DimStyle::Notice);
            assert_eq!(text, "Reconnected — some events may be missing");
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
            assert_eq!(
                model.activity(),
                Activity::Compacting,
                "a compact task is compacting"
            );
        }
        if kind == "task-end" {
            // The run is over: idle, and the step it was in is gone.
            assert_eq!(effect, Effect::ACTIVITY | Effect::STEP);
            assert_eq!(model.activity(), Activity::Idle);
        }
        if kind == "settled" {
            assert!(effect.contains(Effect::RESYNC), "settled is a resync point");
        }
        effects.push((kind.clone(), effect));
    }
    assert_eq!(model.activity(), Activity::Idle);

    let dims = dim_texts(&model);
    assert!(
        dims.contains(&"Compacting context…".to_string()),
        "dims: {dims:#?}"
    );
    assert!(
        dims.contains(&"Context compacted".to_string()),
        "dims: {dims:#?}"
    );
    let failure = model.rows().iter().find_map(|row| match &row.kind {
        RowKind::Dim {
            style: DimStyle::Error,
            text,
        } => Some(text.clone()),
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
    // A run's turn boundary starts inside the compaction of the same run: the step clock
    // restarts, but the task is still the compaction.
    assert_eq!(
        model.apply_event(2, "run-start", &json!({"run_id": "r", "turn": 0})),
        Effect::STEP
    );
    assert_eq!(model.activity(), Activity::Compacting);

    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "task-start", &json!({"task_id": "t", "kind": "run"})),
        Effect::ACTIVITY
    );
    assert_eq!(model.activity(), Activity::Running);
    assert_eq!(
        model.apply_event(2, "run-start", &json!({"run_id": "r", "turn": 0})),
        Effect::STEP,
        "the activity was already running; the step is what changed"
    );
    assert_eq!(
        model.apply_event(
            3,
            "task-end",
            &json!({"task_id": "t", "kind": "run", "outcome": "stop"})
        ),
        Effect::ACTIVITY | Effect::STEP
    );
    assert_eq!(model.activity(), Activity::Idle);
}

#[test]
fn provider_retry_is_a_dim_notice() {
    // Shape from `src/provider/core.lisp`'s retry emit: attempt, max, delay (seconds,
    // serialized as a double: `retry-delay` is `2^attempt` plus a random fraction, or a
    // whole `Retry-After`) and the reason the attempt died.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "provider-retry", &json!({"attempt": 2, "max": 4, "delay": 1.5, "reason": "overloaded_error: Overloaded\n(second line)"})),
        Effect::ROWS
    );
    match &model.rows()[0].kind {
        RowKind::Dim { style, text } => {
            assert_eq!(*style, DimStyle::Notice);
            // The reader's words: no event name, no field name, and the reason is one line.
            assert_eq!(
                text,
                "Retrying provider (2/4) in 1.5 s — overloaded_error: Overloaded"
            );
        }
        other => panic!("unexpected row: {other:?}"),
    }

    // The delay is spoken in the unit that reads best: sub-second in milliseconds, a whole
    // second in seconds. A retry with no delay (an older server) or no reason still reads.
    let texts: Vec<String> = [
        json!({"attempt": 1, "max": 3, "delay": 0.5, "reason": "HTTP 503"}),
        json!({"attempt": 3, "max": 4, "delay": 3.0, "reason": "HTTP 529"}),
        json!({"attempt": 1, "max": 4, "delay": 30, "reason": "HTTP 429"}),
        json!({"attempt": 1, "max": 4, "reason": "boom"}),
        json!({"attempt": 1, "max": 4, "delay": 2.0}),
    ]
    .iter()
    .map(|data| {
        let mut model = AgentModel::new();
        model.apply_event(1, "provider-retry", data);
        match &model.rows()[0].kind {
            RowKind::Dim { text, .. } => text.clone(),
            other => panic!("unexpected row: {other:?}"),
        }
    })
    .collect();
    assert_eq!(
        texts,
        vec![
            "Retrying provider (1/3) in 500 ms — HTTP 503",
            "Retrying provider (3/4) in 3 s — HTTP 529",
            "Retrying provider (1/4) in 30 s — HTTP 429",
            "Retrying provider (1/4) — boom",
            "Retrying provider (1/4) in 2 s",
        ]
    );
}

/// Task 4: a run that ended badly is the one thing `run-end` says. `run-start` never
/// emits a row, a clean stop emits none either, and the ones that do carry the outcome
/// itself as a typed field rather than a word to be sniffed back out of a string.
#[test]
fn a_run_that_ended_badly_gets_an_outcome_row() {
    // `stop` is a run finishing as asked — the turn boundary already says so.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "run-start", &json!({"run_id": "r", "turn": 1})),
        Effect::STEP | Effect::ACTIVITY
    );
    assert!(model.rows().is_empty(), "a run starting is not a row");
    assert_eq!(
        model.apply_event(
            2,
            "run-end",
            &json!({"outcome": "stop", "run_id": "r", "turn": 1})
        ),
        Effect::STEP | Effect::ACTIVITY
    );
    assert!(
        model.rows().is_empty(),
        "a clean run end is not a row either"
    );

    // An outcome the event does not carry at all reads as a clean one; `ok` is the older
    // spelling of the same thing.
    for outcome in [json!({}), json!({"outcome": "ok"})] {
        let mut model = AgentModel::new();
        assert_eq!(model.apply_event(1, "run-end", &outcome), Effect::STEP);
        assert!(model.rows().is_empty(), "{outcome}");
    }

    // Aborted: the row says so, and the outcome rides along for the UI to style with.
    let mut model = AgentModel::new();
    let effect = model.apply_event(1, "run-end", &json!({"outcome": "aborted"}));
    assert!(effect.contains(Effect::ROWS), "the row is news: {effect:?}");
    match &model.rows()[0].kind {
        RowKind::RunOutcome { outcome, text } => {
            assert_eq!(outcome, "aborted");
            assert_eq!(text, "Run aborted");
        }
        other => panic!("unexpected row: {other:?}"),
    }

    // A failed run names what it failed with: the failing assistant message's error, which
    // is the message whose stop reason *is* the outcome.
    let mut model = AgentModel::new();
    model.apply_event(
        1,
        "message-end",
        &json!({"stop_reason": "error", "usage": null, "error": "HTTP 529: overloaded"}),
    );
    model.apply_event(2, "run-end", &json!({"outcome": "error"}));
    assert!(
        model.rows().iter().any(|row| matches!(
            &row.kind,
            RowKind::RunOutcome { outcome, text }
                if outcome == "error" && text == "Run failed: HTTP 529: overloaded"
        )),
        "rows: {:?}",
        model.rows()
    );

    // ..., and a run that failed with nothing to quote still says it failed.
    let mut model = AgentModel::new();
    model.apply_event(1, "run-end", &json!({"outcome": "error"}));
    assert!(model.rows().iter().any(|row| matches!(
        &row.kind,
        RowKind::RunOutcome { text, .. } if text == "Run failed"
    )));

    // The other two outcomes evo has words for, and one it does not.
    let cases = [
        ("length", "Run stopped at the length limit"),
        ("?", "Run ended: ?"),
    ];
    for (outcome, expected) in cases {
        let mut model = AgentModel::new();
        model.apply_event(1, "run-end", &json!({ "outcome": outcome }));
        match &model.rows()[0].kind {
            RowKind::RunOutcome { outcome: got, text } => {
                assert_eq!(got, outcome);
                assert_eq!(text, expected);
            }
            other => panic!("unexpected row: {other:?}"),
        }
    }
}

#[test]
fn resync_events_and_lifecycle() {
    for kind in ["hello", "gap", "session-switched", "settled"] {
        let mut model = AgentModel::new();
        let data = if kind == "session-switched" {
            json!({"session": "/x.sexp"})
        } else {
            json!({})
        };
        assert!(
            model.apply_event(1, kind, &data).contains(Effect::RESYNC),
            "{kind}"
        );
    }
    // A `lane-state` event is the tab's, not this agent's.
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(1, "lane-state", &json!({"lane": 1, "state": "working"})),
        Effect::LANES
    );
    // An event serve could not map: logged, not rendered.
    assert_eq!(
        model.apply_event(1, "unprintable-event", &json!({"original_type": "x"})),
        Effect::NONE
    );
    // Lifecycle and anything newer than this build.
    for kind in ["ready", "shutdown", "bye", "some-future-event"] {
        let mut model = AgentModel::new();
        assert_eq!(
            model.apply_event(7, kind, &json!({})),
            Effect::NONE,
            "{kind}"
        );
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
        RowKind::Tool {
            call_id,
            name,
            arguments,
            result,
        } => {
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
    model.apply_event(
        1,
        "tool-call-start",
        &json!({"name": "bash", "id": "toolu_1", "arguments": {}, "arguments_json": "{}"}),
    );
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
    assert_eq!(
        model.activity(),
        Activity::Running,
        "the fixture was captured mid-run"
    );
    assert_eq!(
        model.readout().goal_label().unwrap(),
        "goal g-4cb9 (active) 0k"
    );

    // A settled session: idle, with the checklist the todo tool left behind.
    let mut idle = AgentModel::new();
    let effect = idle.apply_state(&fixture("state-final.json"));
    assert!(effect.contains(Effect::READOUT));
    assert!(effect.contains(Effect::TODOS));
    assert_eq!(idle.activity(), Activity::Idle);
    assert_eq!(
        idle.readout().text(),
        "stub-a · medium · ctx 0k/200k (0%) · goal g-4cb9 (complete) 0k"
    );
    assert_eq!(idle.todos().len(), 3);
    assert_eq!(idle.todos()[0].status, TodoStatus::InProgress);
    assert_eq!(idle.todos()[1].status, TodoStatus::Pending);
    assert_eq!(idle.todos()[2].status, TodoStatus::Done);
}

#[test]
fn a_todo_change_replaces_the_whole_list() {
    let mut model = AgentModel::new();
    assert_eq!(
        model.apply_event(
            1,
            "todo-changed",
            &json!({"todos": [{"text": "one", "status": "in_progress"}]})
        ),
        Effect::TODOS
    );
    assert_eq!(
        model.todos()[0].status,
        TodoStatus::InProgress,
        "the model's own spelling"
    );
    assert_eq!(
        model.apply_event(
            2,
            "todo-changed",
            &json!({"todos": [{"text": "one", "status": "done"}]})
        ),
        Effect::TODOS
    );
    assert_eq!(model.todos().len(), 1);
    assert_eq!(model.todos()[0].status, TodoStatus::Done);
    assert_eq!(
        model.apply_event(3, "todo-changed", &json!({"todos": []})),
        Effect::TODOS
    );
    assert!(model.todos().is_empty());
}

/// A failed run is published twice — the kernel's own error line, then the run's
/// outcome — and the tab says it once.
///
/// `docs/screens/09-bad-run-light.png`: the error-styled `output` row
/// "error: Provider request failed after 4 attempts: …" and, immediately under it,
/// the outcome row "Run failed: Provider request failed after 4 attempts: …". The row
/// that goes is the output one; the outcome stays, because it says the same thing and
/// says what it was (a run that ended badly).
#[test]
fn a_failed_run_says_it_once() {
    const FAILURE: &str =
        "Provider request failed after 4 attempts: Stream ended without a terminal event";

    let mut model = AgentModel::new();
    model.apply_event(
        1,
        "message-end",
        &json!({ "stop_reason": "error", "usage": null, "error": FAILURE }),
    );
    model.apply_event(
        2,
        "output",
        &json!({ "style": "error", "text": format!("error: {FAILURE}") }),
    );
    let effect = model.apply_event(3, "run-end", &json!({ "outcome": "error" }));
    assert!(effect.contains(Effect::ROWS), "the row is news: {effect:?}");

    let says: Vec<&RowKind> = kinds(&model)
        .into_iter()
        .filter(|kind| match kind {
            RowKind::Dim { text, .. } => text.contains(FAILURE),
            RowKind::RunOutcome { text, .. } => text.contains(FAILURE),
            _ => false,
        })
        .collect();
    assert_eq!(says.len(), 1, "one row, not two: {:?}", model.rows());
    assert!(
        matches!(says[0], RowKind::RunOutcome { outcome, text }
            if outcome == "error" && text == &format!("Run failed: {FAILURE}")),
        "the outcome row is the one left: {:?}",
        says[0]
    );

    // A different failure is a different sentence: both rows stay where they are.
    let mut model = AgentModel::new();
    model.apply_event(
        1,
        "message-end",
        &json!({ "stop_reason": "error", "usage": null, "error": "the second failure" }),
    );
    model.apply_event(
        2,
        "output",
        &json!({ "style": "error", "text": "error: the first failure" }),
    );
    model.apply_event(3, "run-end", &json!({ "outcome": "error" }));
    assert_eq!(
        mentions(&model, "the first failure"),
        1,
        "{:?}",
        model.rows()
    );
    assert_eq!(
        mentions(&model, "the second failure"),
        1,
        "{:?}",
        model.rows()
    );

    // And a line that is not the *error* kind of output is not the same publication.
    let mut model = AgentModel::new();
    model.apply_event(
        1,
        "message-end",
        &json!({ "stop_reason": "error", "usage": null, "error": "M" }),
    );
    model.apply_event(
        2,
        "output",
        &json!({ "style": "notice", "text": "error: M" }),
    );
    model.apply_event(3, "run-end", &json!({ "outcome": "error" }));
    assert_eq!(
        mentions(&model, "M"),
        2,
        "a notice is not the run's own error line: {:?}",
        model.rows()
    );
}

/// How many rows say NEEDLE, counting only the two kinds that can: an output line
/// and a run's outcome.
fn mentions(model: &AgentModel, needle: &str) -> usize {
    model
        .rows()
        .iter()
        .filter(|row| match &row.kind {
            RowKind::Dim { text, .. } | RowKind::RunOutcome { text, .. } => text.contains(needle),
            _ => false,
        })
        .count()
}
