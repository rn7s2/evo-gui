//! `TestAppContext` tests: the items the list is fed, the retained markdown documents, the
//! rows a reader can act on, and the panel helpers that do not need a window.

use gpui_kit::base::TextViewState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, px, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestAppContext, TestSupportExt as _, Window,
};
use serde_json::{json, Value};
use session::{Item, ItemKind};

use crate::rows::{
    cap_fields, cap_text, json_fields, row_id, take_chars, Cap, FieldValue, CONTEXT_BLOCK_LINES,
    RESULT_LIMIT, VALUE_LIMIT,
};
use crate::{TodoPanel, TranscriptView};

/// One item of a fixture, by id.
fn item(value: Value) -> Item {
    Item::from_json(&value).expect("a fixture item has an id")
}

fn user(id: &str, text: &str) -> Item {
    item(json!({ "id": id, "ts": 1, "kind": "user", "text": text, "status": "sent" }))
}

fn queued(id: &str, text: &str) -> Item {
    item(
        json!({ "id": id, "ts": 1, "kind": "user", "text": text, "status": "queued", "queue": "after_run" }),
    )
}

fn assistant(id: &str, text: &str, status: &str) -> Item {
    item(json!({ "id": id, "ts": 1, "kind": "assistant", "text": text, "status": status }))
}

fn assistant_with_thinking(id: &str, text: &str, thinking: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "assistant", "text": text,
        "thinking": thinking, "status": "final"
    }))
}

fn tool(id: &str, call_id: &str, name: &str, text: &str, truncated: bool) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "tool", "call_id": call_id, "name": name,
        "args": { "command": "cargo test" }, "status": "ok",
        "result": { "text": text, "chars": text.chars().count(), "truncated": truncated },
    }))
}

fn notice(id: &str, severity: &str, text: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "notice", "severity": severity,
        "text": text, "source": "swarm", "durable": true
    }))
}

fn lane_event(id: &str, lane: u64, event: &str, severity: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "lane_event", "lane": lane, "event": event,
        "detail": "its process exited", "severity": severity
    }))
}

fn goal(id: &str, event: &str, objective: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "goal", "event": event, "goal_id": "b1",
        "objective": objective, "budget": 50000, "tokens": 12000
    }))
}

fn compaction(id: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "compaction", "summary": "The plan was settled.",
        "tokens_before": 180000, "tokens_after": 9000, "manual": false
    }))
}

fn report(id: &str, lane: u64) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "lane_report", "lane": lane, "done": "ported the items",
        "evidence": "cargo test -p session", "next": "", "blocked": "", "requests": ""
    }))
}

fn context(id: &str, key: &str, text: &str) -> Item {
    item(json!({ "id": id, "ts": 1, "kind": "context", "key": key, "text": text }))
}

fn user_with_image(id: &str, n: usize) -> Item {
    let images: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "name": format!("shot-{i}.png"), "media_type": "image/png",
                "bytes": 68, "href": format!("/media/{id}/{i}")
            })
        })
        .collect();
    item(json!({
        "id": id, "ts": 1, "kind": "user", "text": "look at this",
        "images": images, "status": "sent"
    }))
}

fn run_outcome(id: &str, outcome: &str) -> Item {
    item(json!({ "id": id, "ts": 1, "kind": "run_outcome", "outcome": outcome }))
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
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(TodoPanel::new(self.transcript.read(cx).todos()))
            .id("transcript-host")
            .test_support()
    }
}

fn document(view: &Entity<TranscriptView>, cx: &App, id: &str) -> Entity<TextViewState> {
    view.read(cx)
        .data
        .read(cx)
        .documents
        .get(id)
        .expect("every assistant item keeps a document")
        .clone()
}

/// Open a window over a transcript holding `items`, and hand back the view and the
/// window's own context — which is what the row assertions need.
macro_rules! open {
    ($cx:expr, $items:expr) => {{
        $cx.update(gpui_kit::init);
        let (host, cx) = $cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
        let view = cx.read(|cx| host.read(cx).transcript.clone());
        view.update(cx, |view, cx| view.replace($items, cx));
        (view, cx)
    }};
}

#[gpui_kit::test]
fn items_move_by_id_and_keep_the_order_they_arrived_in(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![user("e_1", "first"), assistant("e_2", "hi", "final")]
    );
    cx.read(|cx| {
        let ids: Vec<&str> = view
            .read(cx)
            .items(cx)
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["e_1", "e_2"]);
    });

    // A patch is the same item, in the same place.
    view.update(cx, |view, cx| {
        assert!(view.upsert(assistant("e_2", "hi there", "final"), cx));
    });
    cx.read(|cx| {
        let ItemKind::Assistant(assistant) = &view.read(cx).items(cx)[1].kind else {
            panic!("e_2 is an assistant item")
        };
        assert_eq!(assistant.text, "hi there");
    });

    // Older items go in front of what is held, and an item already held is not doubled.
    view.update(cx, |view, cx| {
        view.prepend(vec![user("e_0", "earlier"), user("e_1", "first")], cx);
    });
    cx.read(|cx| {
        let ids: Vec<&str> = view
            .read(cx)
            .items(cx)
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["e_0", "e_1", "e_2"]);
    });
    // The list draws them, in that order, without the reader losing their place: the
    // prepend is an insert at the head, not a rebuild.
    cx.update(|window, cx| {
        window.render_frame(cx);
        let older = window.find(row_id("transcript-measure", "e_0")).bounds();
        let first = window.find(row_id("transcript-measure", "e_1")).bounds();
        let answer = window.find(row_id("transcript-measure", "e_2")).bounds();
        assert!(
            older.origin.y < first.origin.y && first.origin.y < answer.origin.y,
            "the older item is above the ones already held"
        );
    });

    // An item the topic dropped goes, and goes only once.
    view.update(cx, |view, cx| {
        assert!(view.remove("e_1", cx));
        assert!(!view.remove("e_1", cx));
    });
    cx.read(|cx| {
        let ids: Vec<&str> = view
            .read(cx)
            .items(cx)
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["e_0", "e_2"]);
    });

    // A snapshot replaces the lot.
    view.update(cx, |view, cx| {
        view.replace(vec![user("e_9", "a fresh session")], cx)
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).items(cx).len(), 1);
    });
}

#[gpui_kit::test]
fn an_assistant_document_is_created_when_the_row_is_shown_and_then_retained(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open!(cx, vec![assistant("e_1", "# Head", "streaming")]);
    cx.update(|window, cx| window.render_frame(cx));
    let first = cx.read(|cx| document(&view, cx, "e_1"));
    cx.read(|cx| {
        assert_eq!(first.read(cx).rendered_text().as_str().trim(), "Head");
    });

    // A delta extends the same document instead of creating a new one.
    view.update(cx, |view, cx| {
        view.upsert(
            assistant("e_1", "# Head\n\nSome **bold** words", "streaming"),
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));
    let second = cx.read(|cx| document(&view, cx, "e_1"));
    assert_eq!(first.entity_id(), second.entity_id());
    cx.read(|cx| {
        let rendered = second.read(cx).rendered_text();
        assert!(
            rendered.as_str().contains("bold"),
            "{:?}",
            rendered.as_str()
        );
        assert!(!rendered.as_str().contains("**"), "{:?}", rendered.as_str());
    });

    // An item that goes takes its document with it.
    view.update(cx, |view, cx| view.replace(vec![user("e_2", "gone")], cx));
    cx.read(|cx| {
        assert!(view.read(cx).data.read(cx).documents.is_empty());
    });
}

#[gpui_kit::test]
fn a_streaming_message_shows_waiting_pips_until_its_first_word(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![assistant("e_1", "", "streaming")]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(
            window
                .try_find(row_id("transcript-waiting", "e_1"))
                .is_some(),
            "a message that has said nothing holds its place with pips"
        );
    });

    view.update(cx, |view, cx| {
        view.upsert(assistant("e_1", "Here we go.", "streaming"), cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-waiting", "e_1"))
            .is_none());
    });
}

#[gpui_kit::test]
fn a_turn_opens_a_boundary_and_a_queued_turn_does_not(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            user("e_1", "first"),
            assistant("e_2", "answer", "final"),
            queued("e_3", "and then"),
            user("e_4", "second"),
        ]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        // The first turn has no boundary above it (nothing precedes it), the second
        // one does — and the queued row between them opens none of its own.
        assert!(window.try_find(("transcript-turn", 1u64)).is_none());
        assert!(window.try_find(("transcript-turn", 2u64)).is_some());
        assert!(
            window.try_find(("transcript-turn", 3u64)).is_none(),
            "a queued turn has not opened one"
        );
        // The queued row says so, and offers the one thing to do about it.
        assert!(window
            .try_find(row_id("transcript-queued", "e_3"))
            .is_some());
        assert!(window
            .try_find(row_id("transcript-cancel", "e_3"))
            .is_some());
        assert!(window
            .try_find(row_id("transcript-cancel", "e_1"))
            .is_none());
    });
}

#[gpui_kit::test]
fn the_cancel_button_asks_the_owner_to_take_the_words_back(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![queued("e_3", "and then")]);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = asked.clone();
    view.update(cx, |view, cx| {
        view.on_cancel_input(
            move |id, _window, _cx| recorded.borrow_mut().push(id.to_string()),
            cx,
        );
    });

    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        window.click(row_id("transcript-cancel", "e_3"), cx);
    });
    assert_eq!(asked.borrow().as_slice(), ["e_3"]);
}

#[gpui_kit::test]
fn a_tool_row_opens_onto_its_arguments_and_its_result(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![tool(
            "t_call_1",
            "call_1",
            "bash",
            "all tests passed",
            false,
        )]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(window
            .try_find(row_id("transcript-tool-arguments", "t_call_1"))
            .is_none());
        window.click(row_id("transcript-tool", "t_call_1"), cx);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        let arguments = window.find(row_id("transcript-tool-arguments", "t_call_1"));
        assert!(arguments.bounds().size.height > px(0.));
        // The result is there, and it is the whole of it: nothing was cut.
        assert!(window
            .try_find(row_id("transcript-tool-result", "t_call_1"))
            .is_some());
        assert!(window
            .try_find(row_id("transcript-load-more", "t_call_1"))
            .is_none());
    });
}

#[gpui_kit::test]
fn a_result_the_server_shortened_offers_the_whole_of_it(cx: &mut TestAppContext) {
    let long = "x".repeat(RESULT_LIMIT * 2);
    let (view, cx) = open!(cx, vec![tool("t_call_1", "call_1", "bash", &long, true)]);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = asked.clone();
    view.update(cx, |view, cx| {
        view.on_fetch_item(
            move |id, _window, _cx| recorded.borrow_mut().push(id.to_string()),
            cx,
        );
    });

    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(row_id("transcript-tool", "t_call_1"), cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click(row_id("transcript-load-more", "t_call_1"), cx);
    });
    assert_eq!(asked.borrow().as_slice(), ["t_call_1"]);

    // The owner answers with the untruncated result, and the offer goes.
    view.update(cx, |view, cx| {
        view.set_full_result("t_call_1", "y".repeat(3), cx)
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-load-more", "t_call_1"))
            .is_none());
    });
}

#[gpui_kit::test]
fn a_compaction_is_drawn_as_a_divider(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            user("e_1", "before"),
            compaction("e_2"),
            user("e_3", "after"),
            // The second turn is the one after the compaction, so the divider sits
            // between them.
        ]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        let divider = window.find(row_id("transcript-compaction", "e_2")).bounds();
        let before = window.find(row_id("transcript-measure", "e_1")).bounds();
        let after = window.find(row_id("transcript-measure", "e_3")).bounds();
        assert!(before.origin.y < divider.origin.y);
        assert!(divider.origin.y < after.origin.y);
        assert_eq!(
            window
                .find(row_id("transcript-compaction-label", "e_2"))
                .label(),
            Some("context compacted, 180k → 9k")
        );
    });
}

#[gpui_kit::test]
fn the_history_header_asks_for_older_items_only_when_there_are_some(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![user("e_1", "the tail")]);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(0));
    let oldest_seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let oldest_handle = oldest_seen.clone();
    let recorded = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(
            move |oldest, _window, _cx| {
                oldest_handle.borrow_mut().push(oldest.to_string());
                *recorded.borrow_mut() += 1;
            },
            cx,
        );
    });

    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(
            window.try_find("transcript-load-older").is_none(),
            "nothing is behind the only item"
        );
    });

    view.update(cx, |view, cx| view.set_history(true, false, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(window.try_find("transcript-load-older").is_some());
        window.click("transcript-load-older", cx);
    });
    assert_eq!(*asked.borrow(), 1);
    assert_eq!(
        oldest_seen.borrow().as_slice(),
        ["e_1"],
        "the page is asked for from the oldest item on screen"
    );

    // While a page is in flight the header says so.
    view.update(cx, |view, cx| view.set_history(true, true, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find("transcript-load-older").label(),
            Some("Loading earlier items…")
        );
    });
}

#[gpui_kit::test]
fn structured_lines_read_as_one_line_each(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            notice("e_1", "error", "the provider is unreachable"),
            lane_event("e_2", 2, "crashed", "error"),
            goal("e_3", "created", "ship the redesign"),
            run_outcome("e_4", "aborted"),
            report("e_5", 3),
        ]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find(row_id("transcript-notice", "e_1")).label(),
            Some("swarm · the provider is unreachable")
        );
        assert_eq!(
            window.find(row_id("transcript-lane-event", "e_2")).label(),
            Some("Lane 2 · crashed — its process exited")
        );
        assert_eq!(
            window.find(row_id("transcript-goal", "e_3")).label(),
            Some("Goal · created — ship the redesign · 12k/50k")
        );
        assert_eq!(
            window.find(row_id("transcript-run-outcome", "e_4")).label(),
            Some("Run aborted")
        );
        assert_eq!(
            window
                .find(row_id("transcript-report-heading", "e_5"))
                .label(),
            Some("Lane 3 report")
        );
        // A structured line is not a turn and opens none.
        assert!(window.try_find(("transcript-turn", 1u64)).is_none());
    });
}

#[gpui_kit::test]
fn a_quiet_line_opens_onto_the_whole_of_it(cx: &mut TestAppContext) {
    let text = "a memory snapshot the reader never typed";
    let (_view, cx) = open!(cx, vec![context("e_1", "global-memory", text)]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert_eq!(
            window.find(row_id("transcript-context", "e_1")).label(),
            Some("Context · global memory")
        );
        assert!(window
            .try_find(row_id("transcript-context-text", "e_1"))
            .is_none());
        window.click(row_id("transcript-context", "e_1"), cx);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window
            .try_find(row_id("transcript-context-text", "e_1"))
            .is_some());
        // The block the text is drawn in scrolls, so a long injection cannot take the
        // whole column.
        let block = window
            .find(row_id("transcript-context-text", "e_1"))
            .bounds();
        assert!(block.size.height <= px(CONTEXT_BLOCK_LINES as f32 * 40.));
    });
}

#[gpui_kit::test]
fn thinking_is_hidden_until_the_view_reveals_it(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![assistant_with_thinking("e_1", "answer", "a thought")]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-thinking", "e_1"))
            .is_none());
    });
    assert!(cx.read(|cx| view.read(cx).has_thinking(cx)));

    view.update(cx, |view, cx| view.set_show_thinking(true, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-thinking", "e_1"))
            .is_some());
    });
}

#[gpui_kit::test]
fn an_empty_transcript_names_the_agent(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("transcript-empty").is_some());
    });

    view.update(cx, |view, cx| {
        view.set_agent(session::AgentKey::Lane(3), cx)
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("transcript-empty").is_none());
        assert!(window.try_find(("transcript-empty-lane", 3u64)).is_some());
    });
}

/// An image is fetched once, when its row is on screen, and drawn from the bytes: the
/// row says it is loading until they arrive, and says so calmly when they cannot be
/// decoded.
#[gpui_kit::test]
fn an_image_is_fetched_once_and_then_drawn(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![user_with_image("e_1", 2)]);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = asked.clone();
    view.update(cx, |view, cx| {
        view.on_fetch_image(
            move |id, n, _window, _cx| recorded.borrow_mut().push((id.to_string(), n)),
            cx,
        );
    });

    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(
        asked.borrow().as_slice(),
        [("e_1".to_string(), 0), ("e_1".to_string(), 1)],
        "each image of the row is asked for exactly once"
    );
    cx.update(|window, _| {
        assert!(
            window
                .try_find(row_id("transcript-image-0", "e_1"))
                .is_none(),
            "nothing is drawn until the bytes are here"
        );
    });

    // The bytes arrive: one PNG, decoded by the owner, handed to the row.
    let frame = transcript_png();
    let decoded = crate::decode_image(&frame).expect("a decodable PNG");
    view.update(cx, |view, cx| view.set_image("e_1", 0, decoded, cx));
    view.update(cx, |view, cx| view.set_image_failed("e_1", 1, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(
            window
                .try_find(row_id("transcript-image-0", "e_1"))
                .is_some(),
            "the decoded image is drawn"
        );
        // A frame that could not be read says so instead of leaving a hole.
        let note = window.find(row_id("transcript-image-note-1", "e_1"));
        assert_eq!(note.label(), Some("shot-1.png — could not be shown"));
        // And nothing is asked for twice.
        window.render_frame(cx);
    });
    assert_eq!(asked.borrow().len(), 2);
}

/// A one-pixel PNG, made here rather than embedded: the test is about the path from
/// bytes to a drawn frame, and this is the smallest thing that goes through it.
fn transcript_png() -> Vec<u8> {
    use image::{ImageFormat, Rgba, RgbaImage};
    let image = RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .expect("a PNG in memory");
    bytes.into_inner()
}

#[gpui_kit::test]
fn a_todo_panel_hides_while_there_is_nothing_to_check_off(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("todo-panel").is_none());
    });

    view.update(cx, |view, cx| {
        view.set_todos(
            vec![session::Todo {
                text: "port the items".to_string(),
                status: session::TodoStatus::InProgress,
            }],
            cx,
        );
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("todo-panel").is_some());
    });
}

// --- the panel helpers, which need no window ------------------------------------------

#[test]
fn a_json_payload_becomes_fields_and_a_non_json_one_does_not() {
    let fields = json_fields(r#"{"timeout": 30, "command": "ls -l", "env": {"A": "1"}}"#)
        .expect("an object is fields");
    let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    assert_eq!(keys, ["timeout", "command", "env.A"]);
    assert!(json_fields("plain text").is_none());
}

#[test]
fn a_panel_caps_what_it_draws_and_counts_what_it_cut() {
    let text = "y".repeat(VALUE_LIMIT * 2);
    let (body, hidden) = cap_text(&text, Some(9_000), RESULT_LIMIT);
    assert_eq!(body.chars().count(), RESULT_LIMIT.min(text.chars().count()));
    assert_eq!(hidden, 9_000 - body.chars().count());

    let fields = json_fields(&format!(r#"{{"a": "{text}"}}"#)).unwrap();
    let mut cap = Cap::new(10);
    let capped = cap_fields(&fields, &mut cap);
    assert_eq!(capped.len(), 1);
    assert!(
        matches!(&capped[0].value, FieldValue::Text { text, .. } if text.chars().count() <= 10)
    );
    // What the limit cut is counted after the value's own one-line elision, which is
    // what the panel would have drawn.
    assert_eq!(cap.hidden(), 97 - 10);

    assert_eq!(take_chars("abc", 2), "ab");
    assert_eq!(take_chars("abc", 9), "abc");
}
