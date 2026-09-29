//! `TestAppContext` tests: row building, the retained markdown documents, the
//! interactive rows, and the todo panel.

use gpui_kit::base::TextViewState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window, div,
};
use session::{Row, RowId, RowKind, Todo, TodoStatus};

use crate::{TodoPanel, TranscriptView};

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
        assert!(rendered.as_str().contains("bo"), "the delta is in the document");
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
        assert!(rendered.as_str().contains("bold"), "{:?}", rendered.as_str());
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
        assert!(window.try_find("todo-panel").is_some(), "todos render a panel");
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
                vec![assistant_with_thinking(1, "Answer.", "Weighing the options…")],
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
            window.try_find(("transcript-tool-arguments", 1u64)).is_none(),
            "a tool row is a one-liner until it is opened"
        );
        window.click(("transcript-tool", 1u64), cx);
        window.render_frame(cx);
        assert!(
            window.try_find(("transcript-tool-arguments", 1u64)).is_some(),
            "clicking the header opens the row"
        );
    });
}
