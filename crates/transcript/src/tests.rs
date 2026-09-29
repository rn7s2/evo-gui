//! `TestAppContext` tests: row building, the retained markdown documents, the
//! interactive rows, and the todo panel.

use gpui_kit::base::TextViewState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window,
};
use session::{DimStyle, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

use gpui_kit::px;

use crate::rows::{
    block_text, json_fields, looks_like_code, run_outcome_style, Field, FieldValue, BLOCK_LINES,
    KEY_WIDTH, TOOL_TEXT_LIMIT, VALUE_LIMIT,
};
use crate::style::MEASURE;
use crate::todo::MAX_LIST_HEIGHT;
use crate::{TodoPanel, TranscriptView};

/// The element ids of a tool row's two blocks: its arguments and its result.
const ARGUMENTS: &str = "transcript-tool-arguments";
const RESULT: &str = "transcript-tool-result";

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
    let view = cx.new(|cx| TranscriptView::new(cx));

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
}

#[gpui_kit::test]
fn an_assistant_document_is_retained_across_deltas(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let view = cx.new(|cx| TranscriptView::new(cx));

    view.update(cx, |view, cx| {
        view.replace(1, vec![assistant(1, 1, "# Head")], cx);
    });

    // Markdown is rendered, not shown as source: the heading marker is gone.
    let first = cx.read(|cx| document(&view, cx, 1));
    cx.read(|cx| {
        assert_eq!(first.read(cx).rendered_text().as_str().trim(), "Head");
    });

    // A delta extends the same document instead of creating a new one.
    view.update(cx, |view, cx| {
        assert!(view.upsert(1, assistant(1, 2, "# Head\n\nSome **bo"), cx));
    });
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
        assert!(view.read(cx).data.read(cx).documents.is_empty());
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
            transcript: cx.new(|cx| TranscriptView::new(cx)),
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

/// The id of the `index`th key/value row of a tool row's block.
fn field_row_id(block: &'static str, id: RowId, index: usize) -> (gpui_kit::ElementId, String) {
    ((block, id).into(), index.to_string())
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
            // Only one level flattens: what is left of a deeper structure is one
            // line of compact JSON, not a wall of braces.
            field("matrix.0", "[1,2]"),
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
    let command = "cargo test -p transcript --lib -- --nocapture ".to_string() + &"x".repeat(80);
    let fields = json_fields(&format!(r#"{{"command":"{command}"}}"#)).expect("an object");

    let FieldValue::Text { text, full } = &fields[0].value else {
        panic!("a one-line string is one line of the list: {:?}", fields[0]);
    };
    assert!(!text.contains('"'), "the value carries no quotes: {text:?}");
    assert!(text.ends_with('…'), "a long value ends in an ellipsis: {text:?}");
    assert_eq!(
        text.chars().count(),
        VALUE_LIMIT + 1,
        "and it is cut at the limit"
    );
    assert_eq!(
        full.as_deref(),
        Some(command.as_str()),
        "the whole text is on hover"
    );

    // A value that fits is shown whole and carries no tooltip.
    let fields = json_fields(r#"{"timeout":120}"#).expect("an object");
    assert_eq!(fields, vec![field("timeout", "120")]);
}

#[test]
fn a_multi_line_string_becomes_a_capped_block() {
    let content: String = (0..14).map(|line| format!("line {line}\n")).collect();
    let arguments = format!(r#"{{"content":"{}"}}"#, content.replace('\n', "\\n"));
    let fields = json_fields(&arguments).expect("an object");

    let FieldValue::Block(text) = &fields[0].value else {
        panic!("a string with line breaks is a block: {:?}", fields[0]);
    };
    assert_eq!(fields[0].key, "content");

    let shown = block_text(text, None);
    assert_eq!(
        shown.lines().count(),
        BLOCK_LINES + 1,
        "the block shows its lines and the note: {shown:?}"
    );
    assert!(
        shown.ends_with("… 6 more lines"),
        "and says what it left out: {shown:?}"
    );

    // Text with few lines but far too many characters is capped too.
    let long_line = "x".repeat(TOOL_TEXT_LIMIT + 10);
    let shown = block_text(&long_line, None);
    assert!(
        shown.ends_with(&format!("… truncated ({} characters)", TOOL_TEXT_LIMIT + 10)),
        "{shown:?}"
    );
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
        let result: gpui_kit::ElementId = ("transcript-tool-result", 1u64).into();
        let panel = window.find(result.clone()).bounds();
        let text = window.find((result, "text")).bounds();
        assert_eq!(
            text.origin.x,
            panel.origin.x + px(9.),
            "the result's text starts at the panel's own edge (its 1px border and 8px padding)"
        );

        // The keyed panels keep their grid: a key column, then its values.
        let key = window.find(field_row_id(ARGUMENTS, 1, 0)).bounds();
        assert!(text.origin.x < key.origin.x + KEY_WIDTH);
        assert!(window.find(field_row_id(ARGUMENTS, 1, 1)).bounds().origin.x == key.origin.x);
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
        assert!(window.try_find(field_row_id(ARGUMENTS, 1u64, 0)).is_some());
        assert!(window.try_find(field_row_id(ARGUMENTS, 1, 1)).is_some());
        assert!(window.try_find(field_row_id(ARGUMENTS, 1, 2)).is_none());

        // A JSON result opens onto the same list, keyed by its own block.
        assert!(
            window
                .try_find(field_row_id(RESULT, 1, 0))
                .is_some(),
            "a JSON result reads as a key/value list too"
        );

        // The plain-text arguments still render, as one block.
        assert!(
            window
                .try_find(("transcript-tool-arguments", 2u64))
                .is_some(),
            "arguments that are not JSON are shown as they came"
        );
        assert!(window.try_find(field_row_id(ARGUMENTS, 2, 0)).is_none());
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
        // The cell's own padding stays zero on purpose: gpui-base has already
        // counted its `CELL_PAD_PX` into every column's floor, so asking for
        // that padding again would leave each text box exactly as wide as its
        // text, which is where a word gets broken in half.
        let padding = style.table_cell.padding;
        assert!(
            padding
                .left
                .is_none_or(|left| left == gpui_kit::px(0.).into())
                && padding
                    .right
                    .is_none_or(|right| right == gpui_kit::px(0.).into()),
            "no cell padding of our own, so the measured floor keeps its slack: {padding:?}"
        );
    });
}
