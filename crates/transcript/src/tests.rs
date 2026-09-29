//! `TestAppContext` tests: row building, the retained markdown documents, the
//! interactive rows, and the todo panel.

use gpui_kit::base::TextViewState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window,
};
use session::{DimStyle, Row, RowId, RowKind, Todo, TodoStatus};

use gpui_kit::px;

use crate::style::MEASURE;
use crate::{TodoPanel, TranscriptView};

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

#[gpui_kit::test]
fn run_markers_are_not_rows_and_a_failed_run_is_a_notice(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let view = cx.new(|cx| TranscriptView::new(cx));

    view.update(cx, |view, cx| {
        view.replace(
            1,
            vec![
                dim(1, DimStyle::Status, "run-start · turn 1"),
                user(2, 1, "hello"),
                dim(3, DimStyle::Status, "run-end · outcome ok"),
                dim(4, DimStyle::Status, "run-end · outcome aborted"),
                dim(5, DimStyle::Status, "compacting..."),
            ],
            cx,
        );
    });

    cx.read(|cx| {
        let rows = view.read(cx).rows(cx);
        assert_eq!(
            rows.len(),
            3,
            "only the turn and the lines worth reading stay"
        );
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![2, 4, 5]
        );

        let RowKind::Dim { style, text } = &rows[1].kind else {
            panic!("the failed run stays a dim row");
        };
        assert_eq!(
            *style,
            DimStyle::Notice,
            "a run that did not end ok reads as a notice, not as bookkeeping"
        );
        assert!(text.contains("aborted"));
    });

    // A run marker that arrives on its own (a live `run-start`) is not a row.
    view.update(cx, |view, cx| {
        assert!(!view.upsert(1, dim(6, DimStyle::Status, "run-start · turn 2"), cx));
        assert!(!view.upsert(1, dim(7, DimStyle::Status, "run-end · outcome ok"), cx));
    });
    cx.read(|cx| assert_eq!(view.read(cx).rows(cx).len(), 3));
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
        host.transcript
            .update(cx, |view, cx| view.set_todos(todos.clone(), cx));
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
