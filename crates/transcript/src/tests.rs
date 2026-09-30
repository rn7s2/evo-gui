//! `TestAppContext` tests: row building, the retained markdown documents, the
//! interactive rows, and the todo panel.

use gpui_kit::base::{Root, TextViewState};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::Keystroke;
use gpui_kit::{
    div, point, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollDelta, Styled as _, TestAppContext, TestSupportExt as _,
    VisualTestContext, Window,
};
use session::{DimStyle, GoalNudgeKind, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

use gpui_kit::px;

use crate::rows::{
    cap_fields, cap_note_text, cap_text, json_fields, looks_like_code, run_outcome_style,
    take_chars, Cap, Field, FieldValue, ARGUMENTS_LIMIT, COLUMN_GAP, CONTEXT_BLOCK_LINES,
    KEY_WIDTH, MAX_ARRAY, MAX_DEPTH, NEST_INDENT, PAYLOAD_LINE_HEIGHT, RESULT_LIMIT,
    TOOL_ROW_HEIGHT, VALUE_LIMIT,
};
use crate::style::Palette;
use crate::style::MEASURE;
use crate::todo::MAX_LIST_HEIGHT;
use crate::KEPT_DOCUMENTS;
use crate::{TodoPanel, TranscriptView};

/// The element ids of a tool row's two blocks: its arguments and its result.
const ARGUMENTS: &str = "transcript-tool-arguments";
const RESULT: &str = "transcript-tool-result";

/// The element ids of an opened context row: the block it draws its text in, and
/// the text itself inside it.
const CONTEXT_TEXT: &str = "transcript-context-text";
const CONTEXT_CONTENT: &str = "transcript-context-text-content";

/// A dim row of the given style.
fn dim(id: RowId, style: DimStyle, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Dim {
            style,
            text: text.into(),
        },
    }
}

/// One unbroken token, with no space and no punctuation to break a line on.
fn long_token() -> String {
    "0123456789abcdef".repeat(17) + "01234567"
}

/// A path long enough to need several lines at the reading measure.
fn long_path() -> String {
    let mut path = String::from("~/coding/evo-gui/crates/transcript");
    for index in 0..10 {
        path.push_str(&format!("/nested-{index:02}"));
    }
    path.push_str("/src/rows.rs");
    path
}

fn user(id: RowId, version: u64, text: &str) -> Row {
    Row {
        id,
        version,
        kind: RowKind::User { text: text.into() },
    }
}

/// The reader's words while evo still has them queued (§9.1).
fn pending_user(id: RowId, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::PendingUser { text: text.into() },
    }
}

fn assistant(id: RowId, version: u64, markdown: &str) -> Row {
    Row {
        id,
        version,
        kind: RowKind::Assistant {
            markdown: markdown.into(),
            thinking: String::new(),
            streaming: true,
            error: None,
        },
    }
}

fn assistant_with_thinking(id: RowId, markdown: &str, thinking: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Assistant {
            markdown: markdown.into(),
            thinking: thinking.into(),
            streaming: true,
            error: None,
        },
    }
}

/// Content an extension injected with `evo:inject-context`, as `/transcript` carries it
/// back: the key the extension tagged it with, and the text it injected.
fn context(id: RowId, key: &str, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Context {
            key: key.into(),
            text: text.into(),
        },
    }
}

/// A command's answer is one quiet line naming the command the reader ran — and the
/// whole of what the extension said is a click away, in the same capped block every
/// quiet row opens onto.
#[gpui_kit::test]
fn a_command_note_is_one_quiet_line_that_opens(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let request = "The user invoked `/global-memory` with an intention or query about global \
                  memory. Use the `global_memory` tool to inspect the current store.\n\n\
                  <memory-request>\nwhat is on floor 3\n</memory-request>";
    let doctor: String = (1..=40)
        .map(|line| format!("step {line}: check, then adapt\n"))
        .collect();
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    command_note(1, "/global-memory", request),
                    user(2, 1, "What is on floor 3?"),
                    command_note(3, "/notify doctor", &doctor),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let note = window.find(("transcript-command", 1u64));
        assert_eq!(note.label(), Some("Command · /global-memory"));
        assert_eq!(
            window.find(("transcript-command", 3u64)).label(),
            Some("Command · /notify doctor")
        );
        assert_eq!(note.expanded(), Some(false));
        assert!(
            window.try_find(("transcript-command-text", 1u64)).is_none(),
            "a command's answer is one line until it is opened"
        );
        assert_eq!(
            note.bounds().size.height,
            TOOL_ROW_HEIGHT,
            "the note wrapped: {:?}",
            note.bounds()
        );
        for id in [1u64, 3] {
            assert_eq!(
                window.find(("transcript-measure", id)).bounds().size.width,
                px(MEASURE),
                "row {id} keeps the measure"
            );
        }

        // The reader's own message is still the only turn.
        let first = window
            .try_find(("transcript-turn", 1usize))
            .expect("the reader's turn is turn 1");
        assert!(
            first.bounds().origin.y > note.bounds().origin.y,
            "no turn opens above the note"
        );
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_none(),
            "a command's answer is not a turn"
        );

        // Opened: the whole message, capped the way every quiet row's block is.
        window.click(("transcript-command", 3u64), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(("transcript-command", 3u64)).expanded(),
            Some(true)
        );
        let block = window.find(("transcript-command-text", 3u64)).bounds();
        let content = window
            .find(("transcript-command-text-content", 3u64))
            .bounds();
        assert!(
            content.size.height > block.size.height,
            "forty steps run past the block: {} in {}",
            content.size.height,
            block.size.height
        );
        let cap = Palette::from_app(cx).payload_size
            * (PAYLOAD_LINE_HEIGHT * CONTEXT_BLOCK_LINES as f32)
            + px(20.);
        assert!(
            block.size.height <= cap,
            "the block grew to {}",
            block.size.height
        );
    });
}

/// A goal nudge is one quiet line naming the goal and its budget, closed until it is
/// asked for — not the walls of text evo sent.
#[gpui_kit::test]
fn a_goal_nudge_is_one_quiet_line(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let objective = "read the tab page against §7.3, then check the goal segment and the \
                     page below it, requirement by requirement, to the end of the section";
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    goal_nudge(
                        1,
                        GoalNudgeKind::Continue,
                        objective,
                        "12,345 tokens used of 50,000 (37,655 remaining)",
                        "You are idle but your goal is still active. Continue working toward it now.",
                    ),
                    goal_nudge(
                        2,
                        GoalNudgeKind::Wrapup,
                        "read the tab page against §7.3",
                        "45,001 used of 45,000",
                        "Your goal's token budget is exhausted (45,001 used of 45,000). Do not start new work.",
                    ),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        let nudge = window.find(("transcript-goal", 1u64));
        // The whole of what it is about, and what the budget stands at, as the row's
        // own words — the line itself is cut to the measure.
        assert_eq!(
            nudge.label(),
            Some(format!("Goal · continue — {objective} · 12,345 tokens used of 50,000 (37,655 remaining)").as_str())
        );
        assert_eq!(nudge.expanded(), Some(false));
        assert!(
            window.try_find(("transcript-goal-text", 1u64)).is_none(),
            "a nudge is one line until it is opened"
        );
        // The wrap-up says which nudge it is in its own words.
        assert_eq!(
            window.find(("transcript-goal", 2u64)).label(),
            Some("Goal · budget exhausted — wrap up")
        );

        // One line, never wider than the reading measure — however long the objective
        // evo wrote is. The line's own height is the hit target every quiet row shares.
        assert_eq!(
            nudge.bounds().size.height,
            TOOL_ROW_HEIGHT,
            "the nudge wrapped: {:?}",
            nudge.bounds()
        );
        // The objective is what gives way: the budget stays on the line, at its end,
        // even though the objective above is longer than the measure.
        let budget = window.find(("transcript-goal-trailing", 1u64));
        assert!(
            budget.visible() && budget.bounds().right() <= nudge.bounds().right(),
            "the budget left the line: {:?} in {:?}",
            budget.bounds(),
            nudge.bounds()
        );
        for id in [1u64, 2] {
            let measure = window.find(("transcript-measure", id)).bounds();
            assert_eq!(measure.size.width, px(MEASURE), "row {id} keeps the measure");
        }
    });
}

/// Its whole message is a click away: the same capped mono block a context row opens
/// onto, which is where evo's rules are read from.
#[gpui_kit::test]
fn an_opened_goal_nudge_shows_the_message_evo_sent(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let text: String = (1..=40)
        .map(|line| format!("rule {line}: keep going\n"))
        .collect();
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![goal_nudge(
                    1,
                    GoalNudgeKind::Continue,
                    "ship the transcript",
                    "10 tokens used (no limit)",
                    &text,
                )],
                cx,
            );
            view.set_expanded(1, true, cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("transcript-goal", 1u64)).expanded(),
            Some(true)
        );
        let block = window.find(("transcript-goal-text", 1u64)).bounds();
        let content = window.find(("transcript-goal-text-content", 1u64)).bounds();
        assert!(
            content.size.height > block.size.height,
            "forty rules run past the block: {} in {}",
            content.size.height,
            block.size.height
        );
        // Twelve lines of it, no more: the block is the same cap a context row uses.
        let cap = Palette::from_app(cx).payload_size
            * (PAYLOAD_LINE_HEIGHT * CONTEXT_BLOCK_LINES as f32)
            + px(20.);
        assert!(
            block.size.height <= cap,
            "the block grew to {}",
            block.size.height
        );
    });
}

/// A nudge is not a turn either: evo keeping its goal going is not the reader speaking.
#[gpui_kit::test]
fn a_goal_nudge_opens_no_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    goal_nudge(
                        1,
                        GoalNudgeKind::Continue,
                        "ship the transcript",
                        "10 tokens used (no limit)",
                        "You are idle but your goal is still active. Continue working toward it now.",
                    ),
                    user(2, 1, "Ship it."),
                    assistant(3, 1, "Working on it."),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let first = window
            .try_find(("transcript-turn", 1usize))
            .expect("the reader's turn is turn 1");
        assert!(
            first.bounds().origin.y > window.find(("transcript-goal", 1u64)).bounds().origin.y,
            "the nudge is above the first turn and opens none of its own"
        );
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_none(),
            "a continuation is not a second turn"
        );
    });
}

/// A line the swarm wrote to the coordinator about one of its lanes, as the session
/// reads it out of the coordinator's own input (`[lane N] …`, `swarm/lanes.lisp`).
fn lane_notice(id: RowId, lane: u32, text: &str, tone: DimStyle) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::LaneNotice {
            lane,
            text: text.into(),
            tone,
        },
    }
}

/// A lane's report as the swarm passed it to the coordinator: the lane's own fields,
/// headed with the lane they belong to.
fn lane_report(id: RowId, lane: u32, done: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Report {
            done: done.into(),
            evidence: String::new(),
            next: String::new(),
            blocked: String::new(),
            requests: "none".into(),
            goal: None,
            lane: Some(lane),
        },
    }
}

/// A command the reader ran, answered with instructions for the agent: what the session
/// reads out of that message (`scoped-memory-command`, `src/core-ext/memory.lisp:242`).
fn command_note(id: RowId, command: &str, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::CommandNote {
            command: command.into(),
            text: text.into(),
        },
    }
}

/// A goal nudge: what evo steers into its own agent when a goal outlives the run, as
/// the session reads it out of that message (`goal-continuation-message`,
/// `src/kernel/goal.lisp:69`).
fn goal_nudge(id: RowId, kind: GoalNudgeKind, objective: &str, budget: &str, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::GoalNudge {
            kind,
            objective: objective.into(),
            budget: budget.into(),
            text: text.into(),
        },
    }
}

/// A tool row with the arguments and result a call actually carries.
fn tool_with(id: RowId, name: &str, arguments: &str, result: Option<ToolResult>) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Tool {
            call_id: format!("call-{id}"),
            name: name.into(),
            arguments: arguments.into(),
            result,
        },
    }
}

fn tool(id: RowId, version: u64) -> Row {
    Row {
        id,
        version,
        kind: RowKind::Tool {
            call_id: format!("call-{id}"),
            name: "read_file".into(),
            arguments: "{\"path\":\"src/lib.rs\"}".into(),
            result: None,
        },
    }
}

fn document(view: &Entity<TranscriptView>, cx: &App, id: RowId) -> Entity<TextViewState> {
    view.read(cx)
        .data
        .read(cx)
        .documents
        .get(&id)
        .expect("every assistant row keeps a document")
        .clone()
}

fn row_kinds(view: &Entity<TranscriptView>, cx: &App) -> Vec<RowKind> {
    view.read(cx)
        .rows(cx)
        .iter()
        .map(|row| row.kind.clone())
        .collect()
}

#[gpui_kit::test]
fn building_rows_replace_upsert_and_the_revision_guard(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let view = cx.new(TranscriptView::new);

    // A rebuild at revision 1 installs the whole transcript.
    view.update(cx, |view, cx| {
        assert!(view.replace(1, vec![user(1, 1, "hello")], cx));
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).revision(), 1);
        assert_eq!(view.read(cx).rows(cx).len(), 1);
    });

    // A rebuild at the same revision replaces the rows and drops the document
    // of a row that is gone.
    view.update(cx, |view, cx| {
        assert!(view.replace(1, vec![user(1, 1, "hello"), assistant(2, 1, "# Title")], cx));
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).rows(cx).len(), 2);
        assert_eq!(row_kinds(&view, cx)[1], assistant(2, 1, "# Title").kind);
    });

    // A whole rebuild from an older revision is dropped, rows and all.
    view.update(cx, |view, cx| {
        assert!(!view.replace(0, vec![user(9, 1, "stale")], cx));
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).rows(cx).len(), 2);
        assert_eq!(view.read(cx).revision(), 1);
    });

    // upsert appends a row it has not seen, and bumps the revision.
    view.update(cx, |view, cx| {
        assert!(view.upsert(2, user(3, 1, "later"), cx));
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).revision(), 2);
        assert_eq!(view.read(cx).rows(cx).len(), 3);
        assert_eq!(row_kinds(&view, cx)[2], user(3, 1, "later").kind);
    });

    // The same version means "unchanged": nothing is replaced.
    view.update(cx, |view, cx| {
        assert!(!view.upsert(2, user(3, 1, "content without a version bump"), cx));
    });
    cx.read(|cx| assert_eq!(row_kinds(&view, cx)[2], user(3, 1, "later").kind));

    // An older revision is dropped, including when only one row was sent.
    view.update(cx, |view, cx| {
        assert!(!view.upsert(1, user(4, 1, "stale"), cx));
    });
    cx.read(|cx| assert_eq!(view.read(cx).rows(cx).len(), 3));

    // A newer version replaces the row in place.
    view.update(cx, |view, cx| {
        assert!(view.upsert(3, user(3, 2, "edited"), cx));
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).rows(cx).len(), 3);
        assert_eq!(row_kinds(&view, cx)[2], user(3, 2, "edited").kind);
    });

    // A row the model dropped (an assistant message that ended empty) leaves
    // the view; the rows around it keep their order. Removing it again is a no-op.
    view.update(cx, |view, cx| {
        assert!(view.remove(4, 2, cx));
        assert!(!view.remove(4, 2, cx));
    });
    cx.read(|cx| {
        let ids: Vec<RowId> = view.read(cx).rows(cx).iter().map(|row| row.id).collect();
        assert_eq!(ids, vec![1, 3]);
    });
}

/// A window host: the transcript above, the todo panel below, as the tab page
/// arranges them.
struct TranscriptHost {
    transcript: Entity<TranscriptView>,
}

impl TranscriptHost {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
        }
    }
}

impl Render for TranscriptHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("transcript-host")
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(TodoPanel::new(self.transcript.read(cx).todos()))
            .test_support()
    }
}

#[gpui_kit::test]
fn an_assistant_document_is_created_when_the_row_is_shown_and_then_retained(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    view.update(cx, |view, cx| {
        view.replace(1, vec![assistant(1, 1, "# Head")], cx);
    });

    // Rendering it parses the markdown: the heading marker is gone.
    cx.update(|window, cx| window.render_frame(cx));
    let first = cx.read(|cx| document(&view, cx, 1));
    cx.read(|cx| {
        assert_eq!(first.read(cx).rendered_text().as_str().trim(), "Head");
    });

    // A delta extends the same document instead of creating a new one.
    view.update(cx, |view, cx| {
        assert!(view.upsert(1, assistant(1, 2, "# Head\n\nSome **bo"), cx));
    });
    cx.update(|window, cx| window.render_frame(cx));
    let second = cx.read(|cx| document(&view, cx, 1));
    assert_eq!(
        first.entity_id(),
        second.entity_id(),
        "the delta must extend the retained document, not replace it"
    );
    cx.read(|cx| {
        let rendered = second.read(cx).rendered_text();
        assert!(
            rendered.as_str().contains("bo"),
            "the delta is in the document"
        );
        assert!(
            !rendered.as_str().contains('#'),
            "the heading is rendered, not shown as source, while the message still grows: {:?}",
            rendered.as_str()
        );
    });

    // The construct completes inside the same document.
    view.update(cx, |view, cx| {
        assert!(view.upsert(1, assistant(1, 3, "# Head\n\nSome **bold** words"), cx));
    });
    cx.update(|window, cx| window.render_frame(cx));
    let third = cx.read(|cx| document(&view, cx, 1));
    assert_eq!(first.entity_id(), third.entity_id());
    cx.read(|cx| {
        let rendered = third.read(cx).rendered_text();
        assert!(
            rendered.as_str().contains("bold"),
            "{:?}",
            rendered.as_str()
        );
        assert!(!rendered.as_str().contains("**"), "{:?}", rendered.as_str());
    });

    // A rebuild without the row drops its document.
    view.update(cx, |view, cx| {
        view.replace(1, vec![user(2, 1, "gone")], cx);
    });
    cx.read(|cx| {
        assert!(
            view.read(cx).data.read(cx).documents.is_empty(),
            "a row that is gone takes its document with it"
        );
    });
}

#[gpui_kit::test]
fn the_todo_panel_is_hidden_while_the_agent_has_no_todos(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("todo-panel").is_none(),
            "no todos renders no panel"
        );
    });

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.set_todos(
                vec![
                    Todo {
                        text: "read the spec".into(),
                        status: TodoStatus::Done,
                    },
                    Todo {
                        text: "render the rows".into(),
                        status: TodoStatus::InProgress,
                    },
                    Todo {
                        text: "write the tests".into(),
                        status: TodoStatus::Pending,
                    },
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("todo-panel").is_some(),
            "todos render a panel"
        );
        assert!(window.try_find(("todo-item", 0usize)).is_some());
        assert!(window.try_find(("todo-item", 2usize)).is_some());
    });
}

#[gpui_kit::test]
fn thinking_is_hidden_until_the_view_reveals_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![assistant_with_thinking(
                    1,
                    "Answer.",
                    "Weighing the options…",
                )],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("transcript-row", 1u64)).is_some());
    });

    host.update(cx, |host, cx| {
        assert!(!host.transcript.read(cx).is_showing_thinking(cx));
        host.transcript
            .update(cx, |view, cx| view.toggle_thinking(cx));
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(host.read(cx).transcript.read(cx).is_showing_thinking(cx));
        assert!(
            window.try_find(("transcript-thinking", 1u64)).is_some(),
            "the toggle reveals the thinking text"
        );
    });
}

#[gpui_kit::test]
fn a_tool_row_opens_on_click(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(1, vec![tool(1, 1)], cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("transcript-tool-arguments", 1u64))
                .is_none(),
            "a tool row is a one-liner until it is opened"
        );
        window.click(("transcript-tool", 1u64), cx);
        window.render_frame(cx);
        assert!(
            window
                .try_find(("transcript-tool-arguments", 1u64))
                .is_some(),
            "clicking the header opens the row"
        );
    });
}

/// The swarm talking to the coordinator is not the coordinator's turn: one quiet line
/// per thing it said, with the lane named in front of it and the whole line a hover
/// away.
#[gpui_kit::test]
fn a_lane_notice_is_one_quiet_line(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let long = format!("run ended (stop) — task: {}", long_path());
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    lane_notice(1, 1, &long, DimStyle::Notice),
                    lane_notice(
                        2,
                        3,
                        "failed to start — see /tmp/evo/lanes/3/lane.log",
                        DimStyle::Error,
                    ),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        // What the row says is the swarm's own words, with the lane they are about.
        let notice = window.find(("transcript-lane-notice", 1u64));
        assert_eq!(notice.label(), Some(format!("Lane 1 · {long}").as_str()));
        assert_eq!(
            window.find(("transcript-lane-notice", 2u64)).label(),
            Some("Lane 3 · failed to start — see /tmp/evo/lanes/3/lane.log")
        );

        // One line, whatever the swarm's task path is, and never wider than the
        // reading measure: a notice does not push the column out.
        assert!(
            notice.bounds().size.height <= px(20.),
            "the notice wrapped: {:?}",
            notice.bounds()
        );
        assert!(
            notice.bounds().size.width <= px(MEASURE),
            "the notice runs past the measure: {:?}",
            notice.bounds()
        );
        for id in [1u64, 2] {
            let measure = window.find(("transcript-measure", id)).bounds();
            assert_eq!(
                measure.size.width,
                px(MEASURE),
                "row {id} keeps the measure"
            );
        }
    });
}

/// A report that came from the swarm is headed with the lane it is about; the lane's own
/// transcript keeps the heading it has always had.
#[gpui_kit::test]
fn a_lane_report_is_headed_with_its_lane(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    lane_report(1, 2, "Created hello.txt and verified it"),
                    Row {
                        id: 2,
                        version: 1,
                        kind: RowKind::Report {
                            done: "the lane's own row".into(),
                            evidence: String::new(),
                            next: String::new(),
                            blocked: String::new(),
                            requests: String::new(),
                            goal: None,
                            lane: None,
                        },
                    },
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("transcript-report-heading", 1u64)).label(),
            Some("Lane 2 report")
        );
        assert_eq!(
            window.find(("transcript-report-heading", 2u64)).label(),
            Some("report")
        );
    });
}

/// Neither the swarm's notices nor its reports are turns: the reader's own message is
/// still turn 1, and everything the swarm said stays above it.
#[gpui_kit::test]
fn the_swarms_own_words_open_no_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    lane_report(1, 2, "Created hello.txt"),
                    lane_notice(
                        2,
                        2,
                        "run ended (stop) — task: create hello.txt",
                        DimStyle::Notice,
                    ),
                    user(3, 1, "Did it work?"),
                    assistant(4, 1, "Yes."),
                    user(5, 1, "Thanks."),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let first = window
            .try_find(("transcript-turn", 1usize))
            .expect("the reader's first turn is turn 1");
        assert!(
            first.bounds().origin.y
                > window
                    .find(("transcript-lane-notice", 2u64))
                    .bounds()
                    .origin
                    .y,
            "no turn opens above the swarm's own words"
        );
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_some(),
            "the reader's second message is turn 2"
        );
        assert!(
            window.try_find(("transcript-turn", 3usize)).is_none(),
            "two reports from the swarm are not three turns"
        );
    });
}

/// A message an extension injected is context, not a turn of the reader's: one quiet
/// line naming where it came from, closed until it is asked for.
#[gpui_kit::test]
fn injected_context_is_one_quiet_line_that_opens_on_click(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    context(1, "global-memory", "<global-memory>\nthe lanes are busy"),
                    context(
                        2,
                        "project-memory",
                        "<project-memory>\nno commits, only reports",
                    ),
                    user(3, 1, "hello"),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let header = window.find(("transcript-context", 1u64));
        assert_eq!(header.label(), Some("Context · global memory"));
        assert_eq!(header.expanded(), Some(false));
        assert_eq!(
            window.find(("transcript-context", 2u64)).label(),
            Some("Context · project memory")
        );
        assert!(
            window.try_find((CONTEXT_TEXT, 1u64)).is_none(),
            "a context row is one line until it is opened"
        );

        window.click(("transcript-context", 1u64), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(("transcript-context", 1u64)).expanded(),
            Some(true)
        );
        let content = window.find((CONTEXT_CONTENT, 1u64));
        assert!(
            content.visible(),
            "the text it injected is on screen once it is opened"
        );
        assert!(
            window.try_find((CONTEXT_TEXT, 2u64)).is_none(),
            "opening one context leaves its neighbours closed"
        );

        // The text is the block's own child, so it is there to be read and selected
        // rather than flattened into the line above it.
        let block = window.find((CONTEXT_TEXT, 1u64)).bounds();
        assert!(
            content.bounds().origin.y >= block.origin.y
                && content.bounds().origin.y < block.origin.y + block.size.height,
            "the text sits inside the block"
        );

        window.click(("transcript-context", 1u64), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(("transcript-context", 1u64)).expanded(),
            Some(false)
        );
        assert!(window.try_find((CONTEXT_TEXT, 1u64)).is_none());
    });
}

/// A key is not a label: the extension's own spelling is translated into the words a
/// reader reads, and a key nobody has translated is shown as the extension named it.
#[test]
fn a_context_key_is_named_in_words_where_there_are_words() {
    assert_eq!(crate::rows::context_label("global-memory"), "global memory");
    assert_eq!(
        crate::rows::context_label("project-memory"),
        "project memory"
    );
    assert_eq!(crate::rows::context_label("recovery"), "recovery");
    assert_eq!(crate::rows::context_label("some-new-key"), "some-new-key");
}

/// A memory snapshot is kilobytes of the reader's own context: opened, it takes twelve
/// lines of the transcript and the rest is a scroll inside the block.
#[gpui_kit::test]
fn an_opened_context_is_capped_and_scrolls_on_its_own(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let text: String = (1..=40)
        .map(|line| format!("memory line {line}\n"))
        .collect();
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(1, vec![context(1, "global-memory", &text)], cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(("transcript-context", 1u64), cx);
        window.render_frame(cx);

        // Twelve lines of the payload face, plus the block's own padding; the border
        // is drawn outside the cap.
        let cap = Palette::from_app(cx).payload_size
            * (PAYLOAD_LINE_HEIGHT * CONTEXT_BLOCK_LINES as f32)
            + px(20.);
        let block = window.find((CONTEXT_TEXT, 1u64)).bounds();
        let content = window.find((CONTEXT_CONTENT, 1u64)).bounds();
        assert!(
            content.size.height > block.size.height,
            "the text runs past the block: {} in {}",
            content.size.height,
            block.size.height
        );
        assert!(
            block.size.height <= cap,
            "a forty line snapshot takes twelve lines, not {}",
            block.size.height
        );

        // The wheel over the block moves the text inside it, and the transcript
        // underneath stays where it is.
        window.scroll(
            (CONTEXT_TEXT, 1u64),
            ScrollDelta::Pixels(point(px(0.), px(-40.))),
            cx,
        );
        window.render_frame(cx);
        let scrolled = window.find((CONTEXT_CONTENT, 1u64)).bounds();
        let moved = content.origin.y - scrolled.origin.y;
        assert!(moved > px(0.), "the block scrolled with the wheel");
        assert!(moved <= px(40.), "and only by the wheel's delta: {moved}");
    });
}

/// `/transcript` rebuilds an injected message as a `user`-role message, but it did not
/// open a turn and does not count as one.
#[gpui_kit::test]
fn injected_context_does_not_open_a_turn(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    context(1, "global-memory", "<global-memory>"),
                    context(2, "project-memory", "<project-memory>"),
                    user(3, 1, "first"),
                    assistant(4, 1, "ok"),
                    user(5, 1, "second"),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        // The injected rows are above the first turn's separator, not turns of their own.
        let first = window
            .try_find(("transcript-turn", 1usize))
            .expect("the reader's first turn is turn 1");
        assert!(
            first.bounds().origin.y > window.find(("transcript-context", 2u64)).bounds().origin.y,
            "no turn opens above the context it was injected with"
        );
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_some(),
            "the second thing the reader said is turn 2"
        );
        assert!(
            window.try_find(("transcript-turn", 3usize)).is_none(),
            "two injected messages are not three turns"
        );
    });
}

/// The reader's own words while evo has them queued (§9.1): the card a turn is
/// drawn on, held back, with a caption saying where they are.
///
/// It is deliberately not a turn yet — nothing has opened — and the model replaces
/// the row, in the place evo inserted the turn, when the event carrying the words
/// arrives.
#[gpui_kit::test]
fn a_queued_turn_is_drawn_as_a_turn_that_has_not_opened_yet(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    context(1, "global-memory", "<global-memory>"),
                    user(2, 1, "first"),
                    assistant(3, 1, "working on it"),
                    pending_user(4, "stop, read §9.1 first"),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        // The card is a turn's, with room for the words and the line under them.
        let queued = window.find(("transcript-user", 4u64)).bounds();
        assert!(queued.size.height > px(20.), "the queued card: {queued:?}");
        // The caption says where the words are...
        let caption: gpui_kit::ElementId = ("transcript-pending", 4u64).into();
        assert_eq!(
            window.find((caption, "caption")).label(),
            Some("queued · sent at the next step")
        );
        // ...and no boundary is drawn: the reader's one turn is still turn 1.
        assert!(window.try_find(("transcript-turn", 1usize)).is_some());
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_none(),
            "queued words have not opened a turn"
        );

        // evo takes them: the model drops the queued row and appends the turn. What is
        // left is the same card, a turn boundary above it and no caption.
        host.update(cx, |host, cx| {
            host.transcript.update(cx, |view, cx| {
                view.remove(1, 4, cx);
                view.upsert(1, user(5, 1, "stop, read §9.1 first"), cx);
            });
        });
        window.render_frame(cx);
        assert!(window.try_find(("transcript-user", 4u64)).is_none());
        let caption: gpui_kit::ElementId = ("transcript-pending", 5u64).into();
        assert!(
            window.try_find((caption, "caption")).is_none(),
            "a turn evo has taken says nothing about being queued"
        );
        assert_eq!(
            window.find(("transcript-user", 5u64)).bounds().size.height,
            window.find(("transcript-user", 2u64)).bounds().size.height,
            "the promoted card is the reader's plain turn"
        );
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_some(),
            "the turn is open now"
        );
    });
}

/// One field of the list a tool's JSON draws as.
fn field(key: &str, value: &str) -> Field {
    Field {
        key: key.into(),
        value: FieldValue::Text {
            text: value.into(),
            full: None,
        },
    }
}

/// A field whose value is a container the panel draws out.
fn nested(key: &str, children: Vec<Field>) -> Field {
    Field {
        key: key.into(),
        value: FieldValue::Nested(children),
    }
}

/// A field whose value is a container the panel only counts.
fn collapsed(key: &str, summary: &str, full: &str) -> Field {
    Field {
        key: key.into(),
        value: FieldValue::Collapsed {
            summary: summary.into(),
            full: full.into(),
        },
    }
}

#[test]
fn tool_arguments_read_as_a_key_value_list() {
    let fields = json_fields(
        r#"{"path":"crates/transcript/src/rows.rs","timeout":120,"dry_run":false,"note":null,"env":{"RUST_LOG":"debug","RUST_BACKTRACE":"1"},"args":["--lib","--nocapture"],"matrix":[[1,2]]}"#,
    )
    .expect("a JSON object is a key/value list");

    assert_eq!(
        fields,
        vec![
            // The call's own key order, not a sorted one: a `write_file` reads
            // `path` before `content`, the way the call was written.
            field("path", "crates/transcript/src/rows.rs"),
            field("timeout", "120"),
            field("dry_run", "false"),
            field("note", "null"),
            field("env.RUST_LOG", "debug"),
            field("env.RUST_BACKTRACE", "1"),
            field("args.0", "--lib"),
            field("args.1", "--nocapture"),
            // One level flattens; what is inside it keeps its own keys and is
            // drawn as rows indented under it.
            nested("matrix.0", vec![field("0", "1"), field("1", "2")]),
        ]
    );

    // Anything that is not JSON — or a bare number or string, which is not a
    // structure at all — is left to the caller to show as it came.
    assert_eq!(json_fields("crates/transcript/src/rows.rs"), None);
    assert_eq!(json_fields("42"), None);
    assert_eq!(json_fields("\"a string\""), None);
}

#[test]
fn a_json_array_reads_as_a_list_keyed_by_index() {
    // A result that is a list rather than an object is still JSON: it reads as
    // one row per entry, not as the brackets it arrived in.
    assert_eq!(
        json_fields(r#"[{"path":"a.rs","ok":true},{"path":"b.rs","ok":false}]"#)
            .expect("a JSON array is a key/value list"),
        vec![
            field("0.path", "a.rs"),
            field("0.ok", "true"),
            field("1.path", "b.rs"),
            field("1.ok", "false"),
        ]
    );

    // A list of plain values is keyed by position.
    assert_eq!(
        json_fields(r#"["--lib","--nocapture"]"#).expect("a JSON array"),
        vec![field("0", "--lib"), field("1", "--nocapture")]
    );
}

#[test]
fn a_long_value_is_elided_with_the_whole_text_kept_for_the_tooltip() {
    let path = "crates/transcript/src/rows.rs ".to_string() + &"x".repeat(80);
    let fields = json_fields(&format!(r#"{{"path":"{path}"}}"#)).expect("an object");

    let FieldValue::Text { text, full } = &fields[0].value else {
        panic!("a one-line string is one line of the list: {:?}", fields[0]);
    };
    assert!(!text.contains('"'), "the value carries no quotes: {text:?}");
    assert!(
        text.ends_with('…'),
        "a long value ends in an ellipsis: {text:?}"
    );
    assert_eq!(
        text.chars().count(),
        VALUE_LIMIT + 1,
        "and it is cut at the limit"
    );
    assert_eq!(
        full.as_deref(),
        Some(path.as_str()),
        "the whole text is on hover"
    );

    // A value that fits is shown whole and carries no tooltip.
    let fields = json_fields(r#"{"timeout":120}"#).expect("an object");
    assert_eq!(fields, vec![field("timeout", "120")]);
}

#[test]
fn a_multi_line_string_becomes_a_block() {
    let content: String = (0..14).map(|line| format!("line {line}\n")).collect();
    let arguments = format!(r#"{{"content":"{}"}}"#, content.replace('\n', "\\n"));
    let fields = json_fields(&arguments).expect("an object");

    let FieldValue::Block(text) = &fields[0].value else {
        panic!("a string with line breaks is a block: {:?}", fields[0]);
    };
    assert_eq!(fields[0].key, "content");
    assert_eq!(text, &content, "and it carries the whole string");

    // A command is a block too, however it is written: it is what the call
    // does, and a line of the grid is not what a reader reads it on.
    let command = "cargo test -p transcript --lib -- --nocapture ".to_string() + &"x".repeat(80);
    let fields = json_fields(&format!(r#"{{"command":"{command}"}}"#)).expect("an object");

    assert_eq!(
        fields[0].value,
        FieldValue::Block(command.clone()),
        "a one-line command is drawn as a block"
    );
}

#[test]
fn a_panels_text_is_capped_at_its_own_limit_and_says_what_it_left_out() {
    // A command is one of the call's arguments: the arguments panel's cap is
    // the command's cap, and it is measured on the text the panel draws.
    let command = "echo ".to_string() + &"x".repeat(ARGUMENTS_LIMIT);
    let fields = json_fields(&format!(r#"{{"command":"{command}"}}"#)).expect("an object");
    let mut cap = Cap::new(ARGUMENTS_LIMIT);
    let fitted = cap_fields(&fields, &mut cap);

    let FieldValue::Block(shown) = &fitted[0].value else {
        panic!("a command is a block: {:?}", fitted[0]);
    };
    assert_eq!(
        shown.chars().count(),
        ARGUMENTS_LIMIT,
        "exactly the arguments limit is shown"
    );
    assert_eq!(command.len() - ARGUMENTS_LIMIT, 5);
    assert_eq!(cap.hidden(), 5, "and the note counts the rest");
    assert!(
        command.starts_with(shown.as_str()),
        "a prefix of the command"
    );

    // A result that is not JSON is capped at the result limit.
    let output = "y".repeat(RESULT_LIMIT + 512);
    let (body, hidden) = cap_text(&output, None, RESULT_LIMIT);
    assert_eq!(body.chars().count(), RESULT_LIMIT);
    assert_eq!(hidden, 512);
    assert_eq!(cap_note_text(hidden), "… (512 more chars)");

    // Where the swarm said the result was longer than the copy that arrived,
    // the note counts from what it said it sent.
    let (body, hidden) = cap_text(&output, Some(50_000), RESULT_LIMIT);
    assert_eq!(hidden, 50_000 - RESULT_LIMIT);
    assert_eq!(body.chars().count(), RESULT_LIMIT);

    // Text the panel can show whole is shown whole, and says nothing: there is
    // nothing left out.
    let (body, hidden) = cap_text("cargo test\ntest result: ok\n", None, RESULT_LIMIT);
    assert_eq!(body, "cargo test\ntest result: ok\n");
    assert_eq!(hidden, 0);

    // What the budget runs out of is counted: the tail of the value it cut
    // into, and the whole of the two fields it never reached.
    let fields = json_fields(r#"{"path":"a.rs","offset":10,"limit":20}"#).expect("an object");
    let mut cap = Cap::new(3);
    let fitted = cap_fields(&fields, &mut cap);
    assert_eq!(
        fitted,
        vec![field("path", "a.r")],
        "the budget stops mid-value"
    );
    assert_eq!(cap.hidden(), 1 + 2 + 2, "a.rs cut, and two fields dropped");

    // One character left out reads as one character.
    assert_eq!(cap_note_text(1), "… (1 more char)");
}

#[test]
fn a_cut_lands_between_characters_whatever_the_text_is_written_in() {
    for unit in ["é", "→", "漢", "🙂"] {
        let text: String = unit.repeat(ARGUMENTS_LIMIT + 5);

        assert_eq!(
            take_chars(&text, ARGUMENTS_LIMIT),
            unit.repeat(ARGUMENTS_LIMIT)
        );

        let (body, hidden) = cap_text(&text, None, ARGUMENTS_LIMIT);
        assert_eq!(body.chars().count(), ARGUMENTS_LIMIT);
        assert_eq!(hidden, 5);
        assert_eq!(body, unit.repeat(ARGUMENTS_LIMIT), "cut between characters");
        assert!(text.starts_with(&body));
        assert!(body.is_char_boundary(body.len()), "and never torn");

        // The same for a value inside a key/value panel.
        let fields = json_fields(&format!(r#"{{"command":"{text}"}}"#)).expect("an object");
        let mut cap = Cap::new(ARGUMENTS_LIMIT);
        let fitted = cap_fields(&fields, &mut cap);
        let FieldValue::Block(shown) = &fitted[0].value else {
            panic!("a command is a block");
        };
        assert_eq!(shown, &unit.repeat(ARGUMENTS_LIMIT));
        assert_eq!(cap.hidden(), 5);
    }
}

#[test]
fn values_are_set_in_mono_only_when_they_read_as_code() {
    // Paths, commands, flags and identifiers read as code…
    for code in [
        "crates/transcript/src/rows.rs",
        "~/coding/evo-gui",
        "RowKind::Tool",
        "tool_row(id, name)",
        "timeout=120",
    ] {
        assert!(looks_like_code(code), "{code:?} reads as code");
    }

    // …and prose does not, however long it is.
    for prose in [
        "the provider returned 429",
        "Stood up the transcript view",
        "128",
        "true",
        "transcript",
    ] {
        assert!(!looks_like_code(prose), "{prose:?} does not read as code");
    }
}

#[gpui_kit::test]
fn a_plain_result_starts_at_the_panels_own_edge(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![tool_with(
                    1,
                    "bash",
                    r#"{"command":"cargo test -p transcript --lib","timeout":120}"#,
                    // A result that is not JSON: plain lines of output.
                    Some(ToolResult {
                        is_error: false,
                        content: "running 14 tests\ntest result: ok".into(),
                        content_chars: None,
                    }),
                )],
                cx,
            );
            view.set_expanded(1, true, cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        // A body with no keys of its own is the whole width of its panel: it
        // starts at the panel's padding, not indented into a value column, so a
        // long line of output is not wrapped for the sake of a grid.
        let arguments: gpui_kit::ElementId = (ARGUMENTS, 1u64).into();
        let result: gpui_kit::ElementId = (RESULT, 1u64).into();
        let result_note = result.clone();
        let panel = window.find(result.clone()).bounds();
        let text = window.find((result, "text")).bounds();
        assert_eq!(
            text.origin.x,
            panel.origin.x + px(9.),
            "the result's text starts at the panel's own edge (its 1px border and 8px padding)"
        );

        // The keyed panels keep their grid: a key column, then its values.
        let key = window.find(row_id(ARGUMENTS, 1, &[0])).bounds();
        assert!(text.origin.x < key.origin.x + KEY_WIDTH);
        assert!(window.find(row_id(ARGUMENTS, 1, &[1])).bounds().origin.x == key.origin.x);

        // And nothing the panel can show whole gets a note under it.
        assert!(window.try_find((arguments, "note")).is_none());
        assert!(window.try_find((result_note, "note")).is_none());
    });
}

/// A call whose command and whose output are both longer than a panel shows:
/// each panel is captioned in lower case, stops at its own limit, and ends in a
/// muted note saying how much of the call is not on screen.
#[gpui_kit::test]
fn a_long_call_is_capped_at_the_panel_limits_and_says_what_it_left_out(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    // A command of 1029 characters, and two results — one twice the result
    // limit, one ten times it.
    let command = "echo ".to_string() + &"x".repeat(ARGUMENTS_LIMIT);
    let output = "y".repeat(RESULT_LIMIT + 512);
    let huge = "z".repeat((RESULT_LIMIT + 512) * 10);
    let call = || format!(r#"{{"command":"{command}"}}"#);

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    tool_with(
                        1,
                        "bash",
                        &call(),
                        Some(ToolResult {
                            is_error: false,
                            content: output.clone(),
                            content_chars: None,
                        }),
                    ),
                    tool_with(
                        2,
                        "bash",
                        &call(),
                        Some(ToolResult {
                            is_error: false,
                            content: huge.clone(),
                            content_chars: None,
                        }),
                    ),
                ],
                cx,
            );
            view.set_expanded(1, true, cx);
            view.set_expanded(2, true, cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let arguments: gpui_kit::ElementId = (ARGUMENTS, 1u64).into();
        let result: gpui_kit::ElementId = (RESULT, 1u64).into();
        let longer: gpui_kit::ElementId = (RESULT, 2u64).into();

        // The captions name their sections in lower case: a label, not a shout.
        assert_eq!(
            window.find((arguments.clone(), "caption")).label(),
            Some("arguments")
        );
        assert_eq!(
            window.find((result.clone(), "caption")).label(),
            Some("result")
        );

        // The command is drawn as a block, and the panel shows
        // `ARGUMENTS_LIMIT` of its 1029 characters.
        assert_eq!(
            window.find((arguments.clone(), "note")).label(),
            Some("… (5 more chars)")
        );

        assert_eq!(
            window.find((result.clone(), "note")).label(),
            Some("… (512 more chars)")
        );
        assert_eq!(
            window.find((longer.clone(), "note")).label(),
            Some(format!("… ({} more chars)", huge.chars().count() - RESULT_LIMIT).as_str())
        );

        // A result ten times longer draws the same panel: what the limit cuts
        // is what the panel draws, not what the tool sent.
        assert_eq!(
            window.find(longer).bounds().size.height,
            window.find(result).bounds().size.height,
            "and the panel it draws is the same height"
        );
    });
}

#[gpui_kit::test]
fn an_open_tool_row_renders_its_arguments_as_key_value_rows(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    tool_with(
                        1,
                        "bash",
                        r#"{"command":"cargo test -p transcript --lib","timeout":120}"#,
                        Some(ToolResult {
                            is_error: false,
                            content: r#"{"shell":"zsh","exit":0}"#.into(),
                            content_chars: None,
                        }),
                    ),
                    // Arguments that are not JSON at all: they are shown as the
                    // text that came, with no field rows to find.
                    tool_with(2, "read_file", "crates/transcript/src/rows.rs", None),
                ],
                cx,
            );
            view.set_expanded(1, true, cx);
            view.set_expanded(2, true, cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        // One row per argument, in key order — no field beyond the two.
        assert!(window.try_find(row_id(ARGUMENTS, 1u64, &[0])).is_some());
        assert!(window.try_find(row_id(ARGUMENTS, 1, &[1])).is_some());
        assert!(window.try_find(row_id(ARGUMENTS, 1, &[2])).is_none());

        // A JSON result opens onto the same list, keyed by its own block.
        assert!(
            window.try_find(row_id(RESULT, 1, &[0])).is_some(),
            "a JSON result reads as a key/value list too"
        );

        // The plain-text arguments still render, as one block.
        assert!(
            window
                .try_find(("transcript-tool-arguments", 2u64))
                .is_some(),
            "arguments that are not JSON are shown as they came"
        );
        assert!(window.try_find(row_id(ARGUMENTS, 2, &[0])).is_none());
    });
}

#[gpui_kit::test]
fn a_run_that_ended_badly_renders_as_its_own_notice_row(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let outcome = |id: RowId, outcome: &str, text: &str| Row {
        id,
        version: 1,
        kind: RowKind::RunOutcome {
            outcome: outcome.into(),
            text: text.into(),
        },
    };

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    user(1, 1, "hello"),
                    outcome(2, "aborted", "Run aborted"),
                    outcome(3, "length", "Run stopped at the length limit"),
                    outcome(4, "error", "Run failed: model not registered"),
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        for id in [2u64, 3, 4] {
            assert!(
                window.try_find(("transcript-run-outcome", id)).is_some(),
                "the run's own line is a row of its own: {id}"
            );
        }
    });

    // The colour follows the outcome: a run that failed is an error, one that
    // was stopped or ran out of room is a notice, and anything the swarm adds
    // later reads as a notice rather than falling off the screen.
    {
        assert_eq!(run_outcome_style("error"), DimStyle::Error);
        assert_eq!(run_outcome_style("aborted"), DimStyle::Notice);
        assert_eq!(run_outcome_style("length"), DimStyle::Notice);
        assert_eq!(run_outcome_style("something-new"), DimStyle::Notice);
    }
}

#[gpui_kit::test]
fn long_content_wraps_inside_a_centred_reading_measure(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    assistant(1, 1, &format!("One unbroken token:\n\n{}", long_token())),
                    dim(2, DimStyle::Dim, &format!("output · {}", long_path())),
                    Row {
                        id: 3,
                        version: 1,
                        kind: RowKind::Report {
                            done: long_token(),
                            evidence: String::new(),
                            next: String::new(),
                            blocked: String::new(),
                            requests: String::new(),
                            goal: None,
                            lane: None,
                        },
                    },
                ],
                cx,
            );
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        for id in [1u64, 2, 3] {
            let measure = window.find(("transcript-measure", id)).bounds();
            assert_eq!(
                measure.size.width,
                gpui_kit::px(MEASURE),
                "row {id} keeps the reading measure whatever it holds"
            );

            // The measure sits in the middle of the column, not against one edge.
            let row = window.find(("transcript-row", id)).bounds();
            let left = measure.origin.x - row.origin.x;
            let right = row.origin.x + row.size.width - (measure.origin.x + measure.size.width);
            assert!(
                (left - right).abs() <= gpui_kit::px(1.),
                "row {id} is centred in its column: {left:?} of space on the left, {right:?} on the right"
            );
        }

        // A 300-character path with nothing to break on is wrapped onto several
        // lines rather than painted outside the box.
        let path = window.find(("transcript-dim", 2u64)).bounds();
        assert!(
            path.size.height > gpui_kit::px(18.),
            "the long path wraps onto more than one line instead of running out of the box: {:?}",
            path.size.height
        );
    });
}

#[gpui_kit::test]
fn the_todo_panel_counts_its_items_caps_its_height_and_aligns_its_glyphs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let todos: Vec<Todo> = (0..12)
        .map(|index| Todo {
            text: format!("todo number {index}"),
            status: match index {
                0..=2 => TodoStatus::Done,
                3 => TodoStatus::InProgress,
                _ => TodoStatus::Pending,
            },
        })
        .collect();

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(1, vec![user(1, 1, "what is left?")], cx);
            view.set_todos(todos.clone(), cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        // Three done of twelve, and the list capped: a long list cannot push
        // the transcript away.
        let panel = window.find("todo-panel").bounds();
        assert!(
            panel.size.height < gpui_kit::px(200.),
            "twelve todos stay in a bounded panel: {:?}",
            panel.size.height
        );

        // One glyph column: every glyph sits on the same axis, whatever it is.
        let first = window.find(("todo-glyph", 0usize)).bounds();
        for index in 1..12usize {
            let glyph = window.find(("todo-glyph", index)).bounds();
            assert_eq!(
                glyph.origin.x, first.origin.x,
                "glyph {index} is in the same column as the first"
            );
        }

        // And the text of every item starts on the same axis too.
        let first_item = window.find(("todo-item", 0usize)).bounds();
        for index in 1..12usize {
            let item = window.find(("todo-item", index)).bounds();
            assert_eq!(item.origin.x, first_item.origin.x);
        }

        // The panel sits in the same measure as the rows above it.
        let row = window.find(("transcript-measure", 1u64)).bounds();
        let header = window.find("todo-header").bounds();
        assert_eq!(
            header.origin.x, row.origin.x,
            "the panel starts on the transcript's left edge"
        );
        assert_eq!(header.size.width, row.size.width, "and shares its measure");
    });
}

#[gpui_kit::test]
fn a_long_todo_list_scrolls_inside_the_panel(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    let todos: Vec<Todo> = (0..12)
        .map(|index| Todo {
            text: format!("todo number {index}"),
            status: TodoStatus::Pending,
        })
        .collect();
    host.update(cx, |host, cx| {
        host.transcript
            .update(cx, |view, cx| view.set_todos(todos, cx));
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let panel = window.find("todo-panel").bounds();
        let first = window.find(("todo-item", 0usize)).bounds().origin.y;

        // The list carries a scrollbar: an overlay on the list's own box, so
        // the thumb has the list's own height to travel down.
        let list = window.find("todo-list").bounds();
        let bar = window.find("todo-scrollbar").bounds();
        assert_eq!(bar.origin, list.origin);
        assert_eq!(bar.size, list.size);
        assert!(
            list.size.height < MAX_LIST_HEIGHT + px(1.),
            "the scrollbar's viewport is the capped list: {:?}",
            list.size
        );

        window.scroll(
            "todo-panel",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-120.))),
            cx,
        );
        window.render_frame(cx);

        let scrolled = window.find(("todo-item", 0usize)).bounds().origin.y;
        assert!(
            scrolled < first,
            "the wheel over the panel moves the list: {first:?} -> {scrolled:?}"
        );
        assert_eq!(
            window.find("todo-panel").bounds().size.height,
            panel.size.height,
            "scrolling the list does not resize the panel"
        );
    });
}

#[gpui_kit::test]
fn markdown_tables_use_the_sideways_scrolling_layout(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    cx.update(|cx| {
        let style = crate::style::text_style(cx);
        // Columns keep the width their content needs and the table moves
        // sideways once it is wider than the measure, instead of squeezing
        // cells (and breaking words inside them) to fit.
        assert_eq!(
            style.table.overflow.x,
            Some(gpui_kit::Overflow::Scroll),
            "the table scrolls rather than squeezing its columns"
        );
        // A cell carries a little padding of its own — less than the 8px a side
        // gpui-base has already counted into every column's floor. Asking for
        // all 16px would leave each text box exactly as wide as its text, which
        // is where a word gets broken in half; asking for none leaves two
        // columns touching ("91%crates/transcript/src/rows.rs"). What is left of
        // the floor is the air between them.
        let padding = style.table_cell.padding;
        let base = gpui_kit::AbsoluteLength::Pixels(gpui_kit::px(0.));
        let rem = gpui_kit::px(16.);
        let side = |side: Option<gpui_kit::DefiniteLength>| {
            side.map(|side| side.to_pixels(base, rem))
                .unwrap_or(gpui_kit::px(0.))
        };
        let (left, right) = (side(padding.left), side(padding.right));
        assert!(
            left > gpui_kit::px(0.)
                && left < gpui_kit::px(8.)
                && right > gpui_kit::px(0.)
                && right < gpui_kit::px(8.),
            "a cell keeps a little air and the measured floor keeps its slack: {padding:?}"
        );
    });
}

/// What an empty transcript says, and how lit a pip of the waiting row is.
#[test]
fn an_empty_transcript_invites_the_coordinator() {
    let (note, detail) = crate::empty_note(session::AgentKey::Coordinator);
    assert_eq!(note, "Ask the coordinator to get started");
    assert_eq!(
        detail,
        Some("It plans the work and hands tasks to its lanes.")
    );

    let (note, detail) = crate::empty_note(session::AgentKey::Lane(2));
    assert_eq!(note, "Lane 2 hasn't been given work yet.");
    assert_eq!(detail, None);

    // Every pip is visible at every point of the cycle, and the three are out of
    // step with each other: the row pulses rather than blinking.
    let at = |delta: f32| crate::rows::dot_ink(delta, 0.);
    assert_eq!(
        at(0.),
        crate::rows::DOT_INK_FLOOR + (1. - crate::rows::DOT_INK_FLOOR) * 0.
    );
    assert!((at(0.5) - 1.).abs() < f32::EPSILON, "a pip peaks mid-turn");
    assert!(at(0.) < at(0.25) && at(0.25) < at(0.5));
    for phase in crate::rows::DOT_PHASES {
        for step in 0..12 {
            let ink = crate::rows::dot_ink(step as f32 / 12., phase);
            assert!(
                (crate::rows::DOT_INK_FLOOR..=1.).contains(&ink),
                "a pip is never invisible: {ink} at step {step}, phase {phase}"
            );
        }
    }
}

#[gpui_kit::test]
fn a_fresh_tab_shows_the_invitation_and_a_lane_shows_its_own_note(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("transcript-empty").is_some(),
            "a transcript with no rows shows the invitation"
        );
        assert!(
            window.try_find(("transcript-empty-lane", 3u64)).is_none(),
            "and not a lane's note"
        );

        // Centred in the column, and quiet: it is not a row of the transcript.
        let bounds = window.find("transcript-empty").bounds();
        let window_bounds = window.bounds();
        let centre = bounds.center().y;
        let area = window_bounds.center().y;
        assert!(
            (centre - area).abs() < px(40.),
            "the invitation is centred in the transcript area: {bounds:?} in {window_bounds:?}"
        );
    });

    // A lane's view says so, and names the lane.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.set_agent(session::AgentKey::Lane(3), cx)
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        let lane = window.find(("transcript-empty-lane", 3u64)).bounds();
        assert!(
            lane.size.width > px(300.),
            "the note spans the column: {lane:?}"
        );
        assert!(
            window.try_find("transcript-empty").is_none(),
            "a lane's empty transcript is not the coordinator's"
        );
    });

    // The first row replaces it.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.upsert(1, user(1, 1, "hello"), cx);
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("transcript-empty").is_none()
                && window.try_find(("transcript-empty-lane", 3u64)).is_none(),
            "the empty state goes away when the first row arrives"
        );
        assert!(window.try_find(("transcript-user", 1u64)).is_some());
    });
}

#[gpui_kit::test]
fn a_waiting_message_shows_pips_until_its_first_delta(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));

    // `message-start`, and nothing else yet: the row holds its place with pips.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.upsert(1, assistant(1, 1, ""), cx);
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        let pips = window.find(("transcript-waiting", 1u64));
        assert!(
            pips.bounds().size.height > px(15.),
            "the pips sit at about a line's height, so nothing jumps when the text lands: {:?}",
            pips.bounds()
        );
        assert!(
            window.try_find("transcript-empty").is_none(),
            "a message that has started is not an empty transcript"
        );
    });

    // The first delta: the text takes over from the pips, in the same row.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.upsert(1, assistant(1, 2, "Hello"), cx);
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(("transcript-waiting", 1u64)).is_none(),
            "the first delta retires the pips"
        );
        assert!(window.try_find(("transcript-assistant", 1u64)).is_some());
    });

    // Somebody who asked for less motion still gets the row: `with_animation`
    // stops the pulse, it does not take the pips away.
    cx.update(|_, cx: &mut App| cx.set_reduce_motion(true));
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(2, vec![assistant(1, 3, "")], cx);
        });
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(("transcript-waiting", 1u64)).is_some(),
            "a row that is waiting is drawn with or without motion"
        );
    });
}

#[gpui_kit::test]
fn the_thinking_toggle_has_nothing_to_show_until_a_message_has_thinking(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let view = cx.new(TranscriptView::new);

    // Nothing to reveal: no rows, and rows that carry no thinking.
    cx.read(|cx| assert!(!view.read(cx).has_thinking(cx)));
    view.update(cx, |view, cx| {
        view.replace(1, vec![user(1, 1, "hello"), assistant(2, 1, "# Head")], cx);
    });
    cx.read(|cx| assert!(!view.read(cx).has_thinking(cx)));

    // An assistant row with thinking text is what the header waits for.
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![
                user(1, 1, "hello"),
                assistant_with_thinking(2, "# Head", "weighing the options"),
            ],
            cx,
        );
    });
    cx.read(|cx| {
        assert!(view.read(cx).has_thinking(cx));
        // And the state the toggle flips is the view's own, hidden by default.
        assert!(!view.read(cx).is_showing_thinking(cx));
    });

    view.update(cx, |view, cx| {
        view.toggle_thinking(cx);
    });
    cx.read(|cx| assert!(view.read(cx).is_showing_thinking(cx)));

    // A resync that drops the thinking text takes the toggle's reason with it.
    view.update(cx, |view, cx| {
        view.replace(2, vec![user(1, 1, "hello")], cx);
    });
    cx.read(|cx| assert!(!view.read(cx).has_thinking(cx)));
}

#[gpui_kit::test]
fn a_message_is_parsed_when_its_row_is_shown_and_not_before(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    // A view nobody has mounted holds the rows and no documents at all: the
    // parse is what a message costs, and it waits for a frame that shows it.
    let unwatched = cx.new(TranscriptView::new);
    unwatched.update(cx, |view, cx| {
        view.replace(1, many_messages(200), cx);
    });
    cx.read(|cx| {
        assert!(
            unwatched.read(cx).data.read(cx).documents.is_empty(),
            "no frame, no parse"
        );
    });

    // Mounted, it parses what the frame shows and nothing else.
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(1, many_messages(200), cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    let parsed = cx.read(|cx| view.read(cx).data.read(cx).documents.len());
    assert!(
        parsed > 0 && parsed < 60,
        "a window's worth of rows is parsed, not the whole transcript: {parsed} of 150 messages"
    );
}

#[gpui_kit::test]
fn documents_stay_bounded_as_the_reader_scrolls_away(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(1, many_messages(400), cx);
    });

    // Scroll away from the tail, a screenful at a time, remembering every row
    // whose message was parsed on the way.
    let mut seen: std::collections::HashSet<RowId> = std::collections::HashSet::new();
    let mut worst = 0;
    cx.update(|window, cx| {
        window.render_frame(cx);
        for _ in 0..40 {
            seen.extend(view.read(cx).data.read(cx).documents.keys().copied());
            worst = worst.max(view.read(cx).data.read(cx).documents.len());
            assert!(
                view.read(cx).data.read(cx).documents.len() <= KEPT_DOCUMENTS,
                "a transcript holds the documents of the rows the reader is near"
            );
            // The wheel goes to the window's centre, which is over the
            // transcript: the list scrolls, and the rows it shows are parsed.
            window.scroll(
                "transcript-host",
                gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(320.))),
                cx,
            );
            window.render_frame(cx);
        }
    });

    assert!(
        seen.len() > KEPT_DOCUMENTS,
        "the scroll passed more rows than are kept, so the map is doing something: {}",
        seen.len()
    );
    assert!(
        worst <= KEPT_DOCUMENTS,
        "the documents never passed the cap: {worst}"
    );
}

/// `count` short assistant messages, one after another.
fn many_messages(count: usize) -> Vec<Row> {
    (0..count)
        .map(|index| assistant(index as RowId + 1, 1, "## Step\n\nA line of the answer."))
        .collect()
}

#[gpui_kit::test]
fn a_resync_of_the_same_rows_changes_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    // A transcript taller than the window, with a tool row the reader opened, and
    // a window's worth of markdown already parsed.
    let mut rows = many_messages(120);
    rows.push(tool_with(
        999,
        "read_file",
        "{\"path\":\"src/lib.rs\"}",
        Some(ToolResult {
            is_error: false,
            content: "fn main() {}".into(),
            content_chars: Some(12),
        }),
    ));
    view.update(cx, |view, cx| {
        view.replace(1, rows.clone(), cx);
        view.set_expanded(999, true, cx);
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        // The reader wheels up from the tail: the place they left is what a
        // resync has to keep.
        window.scroll(
            "transcript-host",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(300.))),
            cx,
        );
        window.render_frame(cx);
    });

    let anchored = cx.read(|cx| {
        let data = view.read(cx).data.read(cx);
        let documents: Vec<(RowId, gpui_kit::EntityId)> = data
            .documents
            .iter()
            .map(|(id, document)| (*id, document.entity_id()))
            .collect();
        (documents, view.read(cx).is_following_tail(cx))
    });
    let visible = cx.update(|window, cx| {
        window.render_frame(cx);
        let row = (0..140u64)
            .filter_map(|id| window.try_find(("transcript-row", id)))
            .map(|snapshot| snapshot.bounds().origin.y)
            .next()
            .expect("a row is on screen");
        let arguments = window
            .try_find(("transcript-tool-arguments", 999u64))
            .map(|snapshot| snapshot.bounds());
        (row, arguments)
    });

    // The same rows again, as `/transcript` hands a rebuilt transcript over.
    view.update(cx, |view, cx| {
        view.replace(2, rows.clone(), cx);
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let row = (0..140u64)
            .filter_map(|id| window.try_find(("transcript-row", id)))
            .map(|snapshot| snapshot.bounds().origin.y)
            .next()
            .expect("a row is on screen");
        assert_eq!(row, visible.0, "a resync does not move the reader's place");
        assert_eq!(
            window
                .try_find(("transcript-tool-arguments", 999u64))
                .map(|snapshot| snapshot.bounds()),
            visible.1,
            "an opened tool row stays open, where it was"
        );
    });

    cx.read(|cx| {
        let data = view.read(cx).data.read(cx);
        let documents: Vec<(RowId, gpui_kit::EntityId)> = data
            .documents
            .iter()
            .map(|(id, document)| (*id, document.entity_id()))
            .collect();
        assert_eq!(
            documents, anchored.0,
            "the markdown documents are reused, not re-created"
        );
        assert_eq!(
            view.read(cx).is_following_tail(cx),
            anchored.1,
            "and following the tail is where it was"
        );
    });
}

/// Mount the transcript the way the application does: inside the base `Root`.
///
/// That is what puts a `TextSelectionLayer` over the content, and selection
/// with it: a dragged selection needs the layer's window-level mouse handling,
/// so a bare view in a test window never selects anything.
fn add_root_window(cx: &mut TestAppContext) -> (Entity<TranscriptHost>, &mut VisualTestContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(TranscriptHost::new);
        Root::new(host, window, cx)
    });
    let host = root.read_with(cx, |root, _| {
        root.view()
            .clone()
            .downcast::<TranscriptHost>()
            .expect("the root hosts the transcript")
    });
    (host, cx)
}

/// What the test platform's clipboard holds, as text.
fn clipboard(cx: &VisualTestContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

/// Empty the clipboard, so an assertion can only pass on what the drag and ⌘C
/// under test put there.
fn clear_clipboard(cx: &VisualTestContext) {
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(String::new()));
}

/// The button that copies a message's own markdown source.
fn message_copy(row: RowId) -> gpui_kit::ElementId {
    ("transcript-copy-message", row).into()
}

/// The button that copies one of a message's code blocks.
///
/// A block's button is scoped by the block's own element path, so the id is the
/// same in every block of a message.
fn block_copy(row: RowId) -> gpui_kit::ElementId {
    ("transcript-copy-block", row).into()
}

/// A row's text is a reading surface: it can be dragged over, and ⌘C takes the
/// selection.
#[gpui_kit::test]
fn dragging_over_a_row_selects_it_and_command_c_copies_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![
                user(1, 1, "rename the foo helper"),
                assistant(2, 1, "The quick brown fox jumps over the lazy dog."),
                dim(3, DimStyle::Status, "step 4 - 12s"),
            ],
            cx,
        );
    });

    let handle = cx.update(|window, cx| {
        window.render_frame(cx);
        window.window_handle()
    });

    // The message: a drag across its first line, then ⌘C.
    clear_clipboard(cx);
    let message = cx.read(|cx| document(&view, cx, 2).read(cx).bounds());
    cx.update(|window, cx| {
        window.drag(
            message.origin + point(px(1.), px(6.)),
            message.origin + point(message.size.width - px(1.), px(6.)),
            cx,
        );
    });
    cx.dispatch_keystroke(handle, Keystroke::parse("cmd-c").expect("a keystroke"));
    let copied = clipboard(cx).unwrap_or_default();
    assert!(
        copied.contains("lazy dog"),
        "the message's own text is copied, not its markup: {copied:?}"
    );

    // A user row is plain text, and is selectable all the same.
    clear_clipboard(cx);
    let row = cx.update(|window, cx| {
        window.render_frame(cx);
        window.find(("transcript-user", 1u64)).bounds()
    });
    cx.update(|window, cx| {
        window.drag(
            row.origin + point(px(14.), px(14.)),
            row.origin + point(px(120.), px(14.)),
            cx,
        );
    });
    cx.dispatch_keystroke(handle, Keystroke::parse("cmd-c").expect("a keystroke"));
    let copied = clipboard(cx).unwrap_or_default();
    assert!(
        copied.contains("rename"),
        "a user row is selectable: {copied:?}"
    );

    // A dim line is a row like any other.
    clear_clipboard(cx);
    let row = cx.update(|window, cx| {
        window.render_frame(cx);
        window.find(("transcript-dim", 3u64)).bounds()
    });
    cx.update(|window, cx| {
        window.drag(
            row.origin + point(px(1.), px(8.)),
            row.origin + point(px(70.), px(8.)),
            cx,
        );
    });
    cx.dispatch_keystroke(handle, Keystroke::parse("cmd-c").expect("a keystroke"));
    let copied = clipboard(cx).unwrap_or_default();
    assert!(
        copied.contains("step 4"),
        "a dim line is selectable: {copied:?}"
    );
}

/// A memory snapshot is text the reader may want back: an opened context row's
/// block can be selected and copied like the prose of a message.
#[gpui_kit::test]
fn an_opened_context_is_selectable(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![context(
                    1,
                    "global-memory",
                    "<global-memory>\nthis is a persisted global user memory snapshot",
                )],
                cx,
            );
            view.set_expanded(1, true, cx);
        });
    });

    let handle = cx.update(|window, cx| {
        window.render_frame(cx);
        window.window_handle()
    });
    clear_clipboard(cx);
    let text = cx.update(|window, _| window.find((CONTEXT_TEXT, 1u64)).bounds());
    cx.update(|window, cx| {
        window.drag(
            text.origin + point(px(1.), px(10.)),
            text.origin + point(px(400.), px(46.)),
            cx,
        );
    });

    cx.dispatch_keystroke(handle, Keystroke::parse("cmd-c").expect("a keystroke"));
    let copied = clipboard(cx).unwrap_or_default();
    assert!(
        copied.contains("persisted global user memory snapshot"),
        "an opened context is selectable: {copied:?}"
    );
}

/// What a tool call came back with is payload, not a picture of one: an open
/// row's text can be selected and copied like the prose of a message.
#[gpui_kit::test]
fn a_tool_payload_is_selectable(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![tool_with(
                    1,
                    "bash",
                    r#"{"command":"cargo test -p transcript"}"#,
                    Some(ToolResult {
                        is_error: false,
                        content: "running 24 tests\ntest result: ok".into(),
                        content_chars: None,
                    }),
                )],
                cx,
            );
            view.set_expanded(1, true, cx);
        });
    });

    let handle = cx.update(|window, cx| {
        window.render_frame(cx);
        window.window_handle()
    });
    let result: gpui_kit::ElementId = ("transcript-tool-result", 1u64).into();
    clear_clipboard(cx);
    let text = cx.update(|window, _| window.find((result, "text")).bounds());
    cx.update(|window, cx| {
        window.drag(
            text.origin + point(px(1.), px(6.)),
            text.origin + point(px(200.), px(6.)),
            cx,
        );
    });

    cx.dispatch_keystroke(handle, Keystroke::parse("cmd-c").expect("a keystroke"));
    let copied = clipboard(cx).unwrap_or_default();
    assert!(
        copied.contains("running 24 tests"),
        "an open tool row's payload is selectable: {copied:?}"
    );
}

/// A fenced block carries its own Copy, out of the way until the reader is over
/// the message, and acknowledges the click.
#[gpui_kit::test]
fn a_code_block_copies_its_own_text(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![assistant(
                    1,
                    1,
                    "Run it:\n\n```sh\ncargo test -p transcript\n```\n\nThat is all.",
                )],
                cx,
            );
        });
    });

    let button = block_copy(1);
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            !window.find(button.clone()).visible(),
            "the block's Copy is out of the way until the message is hovered"
        );

        window.hover(("transcript-assistant", 1u64), cx);
        assert!(
            window.find(button.clone()).visible(),
            "hovering the message shows the block's Copy"
        );

        window.click(button.clone(), cx);
    });

    assert_eq!(
        clipboard(cx).as_deref(),
        Some("cargo test -p transcript"),
        "the block copies its raw code, not the prose around it"
    );

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find((button.clone(), "copied")).is_some(),
            "the button acknowledges the click"
        );
    });
}

/// A message's own action copies its markdown source — markup and all — which
/// is what a reader wants to quote or paste elsewhere.
#[gpui_kit::test]
fn a_message_copies_its_markdown_source(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let source = "A **bold** claim and a `code` run.";

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(1, vec![assistant(1, 1, source)], cx);
        });
    });

    let button = message_copy(1);
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            !window.find(button.clone()).visible(),
            "the message's Copy is out of the way until it is hovered"
        );
        window.hover(("transcript-assistant", 1u64), cx);
        window.click(button.clone(), cx);
    });

    assert_eq!(
        clipboard(cx).as_deref(),
        Some(source),
        "the message copies its source, so the markup survives"
    );

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find((button.clone(), "copied")).is_some(),
            "the button acknowledges the click"
        );
    });
}

/// The point a click lands on when a test means "the start of this message's
/// first line": a couple of pixels in from the measure's top-left corner.
fn first_line_start(window: &mut Window, row: RowId) -> gpui_kit::Point<gpui_kit::Pixels> {
    let bounds = window.find(("transcript-measure", row)).bounds();
    point(bounds.left() + px(4.), bounds.top() + px(8.))
}

/// An HTTP client that records what a test's app asks it to fetch, so a test
/// can prove that something was *not* fetched. Nothing is sent: the point is
/// the request, not the reply.
struct RecordingClient(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl gpui_kit::http_client::HttpClient for RecordingClient {
    fn user_agent(&self) -> Option<&gpui_kit::http_client::http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&gpui_kit::http_client::Url> {
        None
    }

    fn send(
        &self,
        req: gpui_kit::http_client::Request<gpui_kit::http_client::AsyncBody>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = gpui_kit::http_client::Result<
                        gpui_kit::http_client::Response<gpui_kit::http_client::AsyncBody>,
                    >,
                > + Send
                + 'static,
        >,
    > {
        self.0.lock().unwrap().push(req.uri().to_string());
        Box::pin(async move { Err(gpui_kit::http_client::anyhow!("no network in a test")) })
    }
}

/// Install a recording client and hand back what it has been asked for.
fn record_http(cx: &mut TestAppContext) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    cx.update(|cx| {
        cx.set_http_client(std::sync::Arc::new(RecordingClient(requests.clone())));
    });
    requests
}

/// A window holding one bare image: the control that proves the test platform
/// really does fetch an image, so an empty request list means something.
struct BareImage;

impl Render for BareImage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(120.))
            .h(px(120.))
            .child(gpui_kit::img("https://example.com/control.png"))
    }
}

/// An image reference is drawn as its alt text and nothing is fetched: a
/// message must not make this app issue a request to an address a model chose.
#[gpui_kit::test]
fn an_image_reference_is_drawn_as_its_alt_text_and_never_fetched(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let requests = record_http(cx);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(
                1,
                1,
                "A diagram ![architecture diagram](https://example.com/diagram.png) sits here.",
            )],
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));

    let document = cx.read(|cx| document(&view, cx, 1));
    cx.read(|cx| {
        let rendered = document.read(cx).rendered_text();
        assert!(
            rendered.as_str().contains("[image: architecture diagram]"),
            "the reference is drawn as its alt text: {:?}",
            rendered.as_str()
        );
    });
    assert_eq!(
        requests.lock().unwrap().len(),
        0,
        "an image reference must not fetch anything"
    );

    // The control: the same platform, given an image element, asks for it. So
    // the empty list above is the claim being held, not the loader being idle.
    let (_root, cx) = cx.add_window_view(|_window, _cx| BareImage);
    cx.update(|window, cx| window.render_frame(cx));
    cx.run_until_parked();
    assert_eq!(
        requests.lock().unwrap().clone(),
        vec!["https://example.com/control.png".to_string()],
        "the platform does load images, so the transcript holding none is the point"
    );
}

/// Raw HTML is shown as the source it is: the kit interprets the tags it knows
/// and drops a node it cannot parse, which loses text the reader was meant to
/// see — and turns an `<img>` into something this app would fetch.
#[gpui_kit::test]
fn raw_html_is_shown_as_the_source_it_is(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let requests = record_http(cx);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![
                assistant(
                    1,
                    1,
                    "Press <kbd>K</kbd>, keep <br> and <img src=\"https://example.com/pixel.png\" alt=\"beacon\"> as tags.",
                ),
                assistant(
                    2,
                    1,
                    "<div align=\"center\">\n  <p>a block of markup</p>\n</div>",
                ),
            ],
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));

    let rendered = cx.read(|cx| document(&view, cx, 1).read(cx).rendered_text());
    let rendered = rendered.as_str().to_string();
    for piece in [
        "<kbd>K</kbd>",
        "<br>",
        "<img src=\"https://example.com/pixel.png\" alt=\"beacon\">",
    ] {
        assert!(
            rendered.contains(piece),
            "{piece} is shown as it was written: {rendered:?}"
        );
    }

    let block = cx.read(|cx| document(&view, cx, 2).read(cx).rendered_text());
    assert!(
        block.as_str().contains("<div align=\"center\">")
            && block.as_str().contains("a block of markup"),
        "a block of raw HTML is shown too: {:?}",
        block.as_str()
    );

    assert_eq!(
        requests.lock().unwrap().len(),
        0,
        "raw HTML is never handed to the renderer, so it cannot fetch either"
    );
}

/// The schemes the app opens, and the ones it refuses to hand to the OS.
#[test]
fn the_app_opens_web_and_mail_links_and_nothing_else() {
    for url in [
        "https://gpui.rs/docs",
        "http://example.com",
        "HTTPS://Example.com/Upper",
        "mailto:dev@example.com",
        "MAILTO:dev@example.com",
    ] {
        assert!(crate::link::openable(url), "{url} opens");
    }

    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "ftp://example.com/file",
        "ssh://git@github.com/x",
        "./crates/transcript/src/rows.rs",
        "#a-fragment",
        "crates/transcript/src/rows.rs:412",
        "https:/one-slash",
        "",
    ] {
        assert!(!crate::link::openable(url), "{url} does not open");
    }
}

/// A left click on a link opens it in the browser.
#[gpui_kit::test]
fn a_left_click_on_a_link_opens_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(1, 1, "[the GPUI docs](https://gpui.rs/docs)")],
            cx,
        );
    });

    let point = cx.update(|window, cx| {
        window.render_frame(cx);
        first_line_start(window, 1)
    });
    cx.simulate_click(point, gpui_kit::Modifiers::default());

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://gpui.rs/docs"),
        "a click on the link opens it"
    );
}

/// A click on plain text — the same place in the same kind of row — opens
/// nothing: the handler is the link's, not the row's.
#[gpui_kit::test]
fn a_click_on_plain_text_opens_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(1, 1, "no link in this message at all")],
            cx,
        );
    });

    let point = cx.update(|window, cx| {
        window.render_frame(cx);
        first_line_start(window, 1)
    });
    cx.simulate_click(point, gpui_kit::Modifiers::default());

    assert_eq!(cx.opened_url(), None, "there is nothing to open");
}

/// A right click on a link opens nothing, as it does in the kit's own links:
/// a right click is how a reader asks for a menu, not for a page.
#[gpui_kit::test]
fn a_right_click_on_a_link_opens_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(1, 1, "[the GPUI docs](https://gpui.rs/docs)")],
            cx,
        );
    });

    let point = cx.update(|window, cx| {
        window.render_frame(cx);
        first_line_start(window, 1)
    });
    let button = gpui_kit::MouseButton::Right;
    cx.simulate_mouse_down(point, button, gpui_kit::Modifiers::default());
    cx.simulate_mouse_up(point, button, gpui_kit::Modifiers::default());

    assert_eq!(cx.opened_url(), None, "a right click is not an activation");
}

/// A scheme the app does not open is ignored rather than handed to the OS:
/// `file:` would reach the filesystem and `javascript:` the browser's own
/// address bar.
#[gpui_kit::test]
fn a_link_the_app_does_not_open_is_ignored(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(1, 1, "[a local file](file:///etc/passwd)")],
            cx,
        );
    });

    let point = cx.update(|window, cx| {
        window.render_frame(cx);
        first_line_start(window, 1)
    });
    cx.simulate_click(point, gpui_kit::Modifiers::default());

    assert_eq!(
        cx.opened_url(),
        None,
        "a file: link is not opened by a click"
    );
}

/// A `mailto:` link opens the reader's mail composer.
#[gpui_kit::test]
fn a_mailto_link_opens_a_mail_composer(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![assistant(1, 1, "[write to us](mailto:dev@example.com)")],
            cx,
        );
    });

    let point = cx.update(|window, cx| {
        window.render_frame(cx);
        first_line_start(window, 1)
    });
    cx.simulate_click(point, gpui_kit::Modifiers::default());

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("mailto:dev@example.com"),
        "the mail composer is opened with the address"
    );
}

/// A payload nests as deep as the panel draws it out — four levels under the
/// one that flattens — and summarises what is left.
#[test]
fn a_payload_nests_four_levels_and_summarises_the_rest() {
    let arguments = r#"{"one":{"two":{"three":{"four":{"five":{"six":{"seven":"deep"}}}}}}}"#;
    let fields = json_fields(arguments).expect("an object");

    assert_eq!(
        fields,
        vec![nested(
            "one.two",
            vec![nested(
                "three",
                vec![nested(
                    "four",
                    vec![nested(
                        "five",
                        vec![collapsed("six", "{…1 key}", r#"{"seven":"deep"}"#)],
                    )],
                )],
            )],
        )],
    );
    assert_eq!(MAX_DEPTH, 4, "the chain above is drawn four levels deep");

    // An array under the limit is drawn, item by item, however deep it sits.
    let a_list = r#"{"a":{"b":{"tags":["one","two"]}}}"#;
    let fields = json_fields(a_list).expect("an object");
    let FieldValue::Nested(children) = &fields[0].value else {
        panic!("a container nests: {fields:?}");
    };
    assert_eq!(
        children,
        &vec![nested("tags", vec![field("0", "one"), field("1", "two")])],
        "a list inside a container keeps its items as rows of their own"
    );
}

/// A long array is counted rather than drawn row by row — the whole of it stays
/// one hover away — while one at the bound is drawn.
#[test]
fn a_long_array_is_summarised_rather_than_drawn_row_by_row() {
    let long: Vec<String> = (0..MAX_ARRAY + 22)
        .map(|index| format!("\"row-{index}\""))
        .collect();
    let arguments = format!(r#"{{"rows":[{}],"count":{}}}"#, long.join(","), long.len());
    let fields = json_fields(&arguments).expect("an object");

    assert_eq!(fields[0].key, "rows");
    let FieldValue::Collapsed { summary, full } = &fields[0].value else {
        panic!("a long array is summarised: {:?}", fields[0]);
    };
    assert_eq!(summary, &format!("[…{} items]", long.len()));
    assert!(
        full.starts_with("[\"row-0\",\"row-1\""),
        "the whole array is what the tooltip carries: {full:?}"
    );
    assert_eq!(
        fields[1],
        field("count", &long.len().to_string()),
        "the fields after it are untouched"
    );

    // An array at the bound is still drawn item by item, keyed by index.
    let at_the_bound: Vec<String> = (0..MAX_ARRAY).map(|index| format!("\"{index}\"")).collect();
    let fields =
        json_fields(&format!(r#"{{"rows":[{}]}}"#, at_the_bound.join(","))).expect("an object");
    assert_eq!(fields.len(), MAX_ARRAY);
    assert_eq!(fields[0], field("rows.0", "0"));
    assert_eq!(
        fields[MAX_ARRAY - 1],
        field(
            &format!("rows.{}", MAX_ARRAY - 1),
            &format!("{}", MAX_ARRAY - 1)
        )
    );

    // A container inside one is summarised too, not just one at the top.
    let deep = format!(r#"{{"a":{{"rows":[{}]}}}}"#, long.join(","));
    let fields = json_fields(&deep).expect("an object");
    assert_eq!(fields[0].key, "a.rows");
    assert!(
        matches!(fields[0].value, FieldValue::Collapsed { .. }),
        "a long array nested one level in is summarised as well: {:?}",
        fields[0]
    );
}

/// A nested payload is drawn as rows under their key, indented a step per
/// level; a container the panel will not draw out reads as the count it is.
#[gpui_kit::test]
fn a_nested_payload_renders_as_rows_indented_under_their_key(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = add_root_window(cx);
    // A chain deeper than the panel draws, and a field beside it that must stay
    // at the panel's own column.
    let arguments = r#"{"outer":{"inner":{"deep":{"deeper":{"deepest":{"deeper-still":{"too-deep":1}}}}}},"flat":"x"}"#;

    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(1, vec![tool_with(1, "bash", arguments, None)], cx);
            view.set_expanded(1, true, cx);
        });
    });

    cx.update(|window, cx| {
        window.render_frame(cx);

        let at = |path: &[usize]| window.find(key_cell_id(ARGUMENTS, 1, path)).bounds();

        // `outer.inner` is what the top level flattened; the levels under it keep
        // their own keys, each a step further in than the one above it.
        let mut previous = at(&[0]);
        let levels: [&[usize]; 3] = [&[0, 0], &[0, 0, 0], &[0, 0, 0, 0]];
        for (level, path) in levels.iter().enumerate() {
            let here = at(path);
            assert_eq!(
                here.origin.x - previous.origin.x,
                NEST_INDENT,
                "level {} is one indent in: {:?} -> {:?}",
                level + 1,
                previous,
                here
            );
            previous = here;
        }

        assert_eq!(
            at(&[1]).origin.x,
            at(&[0]).origin.x,
            "a field at the top level stays at the panel's own column"
        );

        // The container that would sit past the bound is one row: its key, and
        // the count of what it holds beside it.
        let summary = window
            .find(row_value_id(ARGUMENTS, 1, &[0, 0, 0, 0, 0]))
            .bounds();
        let key = at(&[0, 0, 0, 0, 0]);
        assert_eq!(
            summary.origin.x - (key.origin.x + key.size.width),
            COLUMN_GAP,
            "the summary sits in the value column of its own row: {summary:?} {key:?}"
        );
        assert!(
            key.origin.x - at(&[0]).origin.x == NEST_INDENT * 4.,
            "four levels of indentation, and the fifth row is the summary"
        );
    });
}

/// The value cell of the row at `path`: a container the panel only counts puts
/// its summary there.
fn row_value_id(block: &'static str, id: RowId, path: &[usize]) -> gpui_kit::ElementId {
    (row_id(block, id, path), "value").into()
}

/// The key cell of the row at `path` — one index per nesting level.
fn key_cell_id(block: &'static str, id: RowId, path: &[usize]) -> gpui_kit::ElementId {
    (row_id(block, id, path), "key").into()
}

/// The id of the row at `path`, one index per nesting level.
fn row_id(block: &'static str, id: RowId, path: &[usize]) -> gpui_kit::ElementId {
    let mut row: gpui_kit::ElementId = (block, id).into();
    for index in path {
        row = (row, index.to_string()).into();
    }
    row
}

/// A fenced block is highlighted: the kit's tree-sitter highlighter is installed
/// for every `TextView`, the tags a model writes resolve to a grammar, and a tag
/// nobody has a grammar for stays plain rather than failing.
///
/// The kit ships a stub behind the same names when its `tree-sitter` feature is
/// off, so this test compiles either way and fails on the stub — where every
/// language, known or not, comes back as one unstyled range.
#[gpui_kit::test]
fn code_fences_are_highlighted_and_an_unknown_language_stays_plain(cx: &mut TestAppContext) {
    use gpui_kit::component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};
    use gpui_kit::component::Rope;

    cx.update(gpui_kit::init);
    cx.update(|cx| {
        assert!(
            gpui_kit::base::TextViewDefaults::global(cx).has_code_block_highlighter(),
            "the theme installs a code block highlighter for the app's text views"
        );
    });

    let registry = LanguageRegistry::singleton();
    for tag in [
        "rust",
        "rs",
        "python",
        "py",
        "javascript",
        "js",
        "typescript",
        "ts",
        "json",
        "bash",
        "sh",
        "toml",
    ] {
        assert!(
            registry
                .language(tag)
                .is_some_and(|config| config.has_grammar()),
            "{tag} resolves to a grammar"
        );
    }

    // What the highlighter makes of a fence: the ranges it styles, and whether
    // any of them carries a color at all.
    let styled = |tag: &str, code: &str| {
        let mut highlighter = SyntaxHighlighter::new(tag);
        highlighter.update(None, &Rope::from_str(code), None);
        let styles = highlighter.styles(&(0..code.len()), &*HighlightTheme::default_light());
        styles
            .iter()
            .filter(|(_, style)| style.color.is_some())
            .count()
    };

    let rust = "fn main() {\n    // a comment\n    println!(\"hi\");\n}\n";
    assert!(
        styled("rust", rust) >= 4,
        "a Rust fence is colored: keywords, a string and a comment"
    );
    assert!(styled("rs", rust) >= 4, "and so is the `rs` alias");
    assert!(styled("python", "def f(x):\n    return x + 1\n") >= 3);
    assert!(styled("json", "{\"a\": [1, true, null]}\n") >= 2);
    assert!(styled("bash", "for f in *.rs; do echo \"$f\"; done\n") >= 2);
    assert!(
        styled("sh", "cargo test -p transcript\n") >= 1,
        "the `sh` alias"
    );
    assert!(styled("typescript", "const n: number = 1;\n") >= 2);

    // A tag with no grammar, and no tag at all, are both left as one plain run.
    for (tag, code) in [
        ("not-a-language", rust),
        ("", rust),
        ("lisp", "(defun f (x) x)\n"),
        ("console", "$ cargo test\n"),
    ] {
        assert_eq!(
            styled(tag, code),
            0,
            "{tag:?} has no grammar, so nothing is colored and nothing fails"
        );
    }
}
