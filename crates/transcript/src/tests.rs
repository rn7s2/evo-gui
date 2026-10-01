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
    RESULT_LIMIT, TURN_LABEL_OVERHANG, VALUE_LIMIT,
};
use crate::TranscriptView;

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
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
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
fn a_running_agent_shows_pips_from_the_requests_start(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![user("e_1", "go")]);
    let pending = |cx: &mut gpui_kit::VisualTestContext| {
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|window, _| window.try_find("transcript-pending").is_some())
    };
    assert!(!pending(cx), "an idle agent shows no pips");

    // The request is in flight: nothing of the turn has arrived yet.
    view.update(cx, |view, cx| view.set_running(true, cx));
    assert!(pending(cx), "pips from the moment the request starts");

    // A message streaming carries its own pips, then its words.
    view.update(cx, |view, cx| {
        view.upsert(assistant("e_2", "", "streaming"), cx)
    });
    assert!(!pending(cx), "the streaming message shows the work itself");

    // The message is done and its tool has answered: the next request is on its way.
    view.update(cx, |view, cx| {
        view.upsert(assistant("e_2", "Running the tests.", "final"), cx);
        view.upsert(tool("t_1", "c_1", "bash", "ok", false), cx);
    });
    assert!(pending(cx), "pips while the next request is in flight");

    // A queued input below the turn does not end it.
    view.update(cx, |view, cx| view.upsert(queued("e_3", "later"), cx));
    assert!(pending(cx));

    view.update(cx, |view, cx| view.set_running(false, cx));
    assert!(!pending(cx), "the run is over");
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

/// The tool head's sentence is read out of the call's own arguments: where it
/// went, and what it was asked to do. Nothing is invented — a call that names
/// neither is drawn as its name and its status.
#[test]
fn a_tool_calls_sentence_comes_from_its_arguments() {
    // A delegation: the lane it went to, and the task it was given.
    let (target, summary) = crate::rows::tool_sentence(&json!({
        "lane": 5,
        "task": "Lower-case the collapsible captions"
    }));
    assert_eq!(target, "lane 5");
    assert_eq!(summary, "Lower-case the collapsible captions");

    // A command: the command is what it was aimed at, and there is nothing else
    // worth saying in the head.
    let (target, summary) = crate::rows::tool_sentence(&json!({"command": "make test"}));
    assert_eq!(target, "make test");
    assert_eq!(summary, "");

    // A file and a patch: the path leads, the change describes.
    let (target, summary) = crate::rows::tool_sentence(&json!({
        "path": "crates/transcript/src/rows.rs",
        "content": "the new body"
    }));
    assert_eq!(target, "crates/transcript/src/rows.rs");
    assert_eq!(summary, "the new body");

    // Nothing to say: no sentence, no invented subject.
    let (target, summary) = crate::rows::tool_sentence(&json!({}));
    assert_eq!((target.as_str(), summary.as_str()), ("", ""));
    let (target, summary) = crate::rows::tool_sentence(&Value::Null);
    assert_eq!((target.as_str(), summary.as_str()), ("", ""));

    // A long task is cut to the head's own limit rather than measured whole.
    let long = "x".repeat(400);
    let (_, summary) = crate::rows::tool_sentence(&json!({"path": "a", "task": long}));
    assert!(summary.chars().count() <= 121, "{}", summary.len());
    assert!(summary.ends_with('…'));
}

/// A tool head is one line: a multi-line command shows its first line and `…`,
/// and a line wider than the head gives way to the status, which stays in the card.
#[test]
fn a_tool_heads_text_is_one_line() {
    use crate::rows::one_line;
    assert_eq!(one_line("make test"), "make test");
    assert_eq!(
        one_line("\n  cd repo && python3 - <<'EOF'\nprint(1)\nEOF\n"),
        "cd repo && python3 - <<'EOF' …"
    );
    assert_eq!(one_line("  \n "), "");
}

#[gpui_kit::test]
fn a_long_tool_head_keeps_its_status_in_the_card(cx: &mut TestAppContext) {
    let long = format!(
        "cd ~/coding/evo-gui && {}\nsecond line\n}}",
        "grep -n pattern file; ".repeat(40)
    );
    let call = item(json!({
        "id": "t_1", "ts": 1, "kind": "tool", "call_id": "c_1", "name": "bash",
        "args": { "command": long }, "status": "ok",
    }));
    let (_view, cx) = open!(cx, vec![call]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        let head = window.find(row_id("transcript-tool", "t_1")).bounds();
        let status = window
            .find(row_id("transcript-tool-status", "t_1"))
            .bounds();
        assert_eq!(head.size.height, px(34.), "the head stays one line tall");
        assert!(
            status.right() <= head.right() && status.left() > head.left(),
            "the status stays inside the head: {status:?} in {head:?}"
        );
    });
}

/// Switching agent starts the new transcript at its latest item, whatever the old
/// one's reader was doing.
#[gpui_kit::test]
fn switching_agent_follows_the_tail_again(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![user("u_1", "hello"), assistant("e_1", "hi", "final")]
    );
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));

    // The reader scrolls up, well past the pill's threshold.
    view.update(cx, |view, cx| {
        view.pin.touched();
        view.pin.on_scroll(900.);
        assert!(!view.is_following_tail(cx));
        assert!(view.is_away_from_latest(cx));
    });

    view.update(cx, |view, cx| {
        view.set_agent(session::AgentKey::Lane(1), cx)
    });
    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "another agent's transcript opens at its latest"
    );
    assert!(cx.read(|cx| !view.read(cx).is_away_from_latest(cx)));
}

/// The list opens at its latest item: with more rows than the pane can show, the
/// last one is on screen — which is the whole point of the pinning, and the one
/// thing a scroll position can be held to from a test.
#[gpui_kit::test]
fn a_long_transcript_opens_at_its_latest_row(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..40)
        .map(|i| user(&format!("u_{i:02}"), "a turn of its own"))
        .collect();
    let (_view, cx) = open!(cx, items);
    // The first frame lays the list out; the second applies the scroll the first
    // frame asked for (the rule runs in prepaint, so it is one frame behind).
    for _ in 0..4 {
        cx.update(|window, cx| window.render_frame(cx));
    }

    let transcript = cx.update(|window, _| window.find("transcript").bounds());
    let last = cx.update(|window, _| window.find(row_id("transcript-row", "u_39")).bounds());
    assert!(
        last.bottom() <= transcript.bottom() + px(1.),
        "the latest row is inside the pane: {last:?} vs {transcript:?}"
    );
    assert!(
        last.bottom() > transcript.bottom() - px(60.),
        "and at the bottom of it, not floating above: {last:?} vs {transcript:?}"
    );
}

/// The turn rule, to the pixel the design asks for: a full-width hairline whose
/// floor the *next* row starts below, with `turn N` sitting on the line — the
/// label's vertical centre within a pixel of it.
#[gpui_kit::test]
fn a_turn_rule_puts_its_label_on_the_line(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            user("u_1", "the first turn"),
            user("u_2", "the second turn"),
        ]
    );
    // Two frames: one to lay the rule out, one for the row that follows it.
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    });
    cx.update(|window, _| {
        let line = window.find(("transcript-turn-line", 2usize)).bounds();
        let label = window.find(("transcript-turn-label", 2usize)).bounds();
        // The two turns' *cards*: the row wrapper carries the rule above it, so the
        // card is what says where a turn actually starts.
        let row = window.find(row_id("transcript-user", "u_2")).bounds();
        let first = window.find(row_id("transcript-user", "u_1")).bounds();

        // The line is the row's own width: a hairline across the measure, not a
        // stub beside the label.
        assert!(
            line.size.width >= row.size.width - px(1.),
            "the line spans the row: {line:?} vs {row:?}"
        );
        assert_eq!(line.size.height, px(1.));

        // The label is *on* the line: its box crosses it — the design's
        // `bottom: -7px` — and its vertical centre is within a couple of pixels of
        // it (a 12px label on a 1.5 line box is 2px above the line by the design's
        // own arithmetic).
        assert!(
            label.origin.y < line.origin.y && label.bottom() > line.bottom(),
            "the label straddles the line: {label:?} vs {line:?}"
        );
        let label_centre = label.origin.y + label.size.height / 2.;
        let line_centre = line.origin.y + line.size.height / 2.;
        assert!(
            (label_centre - line_centre).abs() <= px(2.),
            "the label sits on the line: {label:?} vs {line:?}"
        );
        assert_eq!(
            label.bottom() - line.bottom(),
            px(TURN_LABEL_OVERHANG),
            "and hangs the design's 7px below the line"
        );

        // The rule has the design's own 26px above it, and the turn below it
        // starts *below the line* rather than over it — which is what keeps the
        // label readable: in the design the label is painted over the next row and
        // the row's 8px of padding is what it overlaps.
        assert!(
            row.origin.y >= line.bottom(),
            "the next turn starts below the line: {row:?} vs {line:?}"
        );
        assert!(
            line.origin.y >= first.bottom() + px(26.),
            "26px of air above the rule: {line:?} vs {first:?}"
        );
    });
}

/// An ephemeral system line is not part of the conversation: `session ready` is
/// said at every boot and dropped; a durable notice — the kind the journal keeps —
/// is the record and stays.
#[gpui_kit::test]
fn a_notice_the_server_does_not_keep_is_not_in_the_transcript(cx: &mut TestAppContext) {
    let mut ephemeral = notice("e_1", "info", "session ready");
    if let ItemKind::Notice(notice) = &mut ephemeral.kind {
        notice.durable = false;
        notice.source = session::NoticeSource::Serve;
    }
    let mut kept = notice("e_2", "warn", "the provider is unreachable");
    if let ItemKind::Notice(notice) = &mut kept.kind {
        notice.durable = true;
    }

    let (view, cx) = open!(cx, vec![ephemeral, kept]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(
            window
                .try_find(row_id("transcript-notice", "e_1"))
                .is_none(),
            "the ephemeral line is not drawn"
        );
        assert!(
            window
                .try_find(row_id("transcript-notice", "e_2"))
                .is_some(),
            "a durable one is"
        );
    });
    // And it is not held either: an item that arrives later is dropped the same
    // way, so the transcript never holds a line it will not draw.
    let later = {
        let mut item = notice("e_3", "info", "session ready");
        if let ItemKind::Notice(notice) = &mut item.kind {
            notice.durable = false;
        }
        item
    };
    view.update(cx, |view, cx| {
        assert!(!view.upsert(later, cx), "an ephemeral line changes nothing");
        assert_eq!(view.items(cx).len(), 1, "only the durable one is held");
    });
}

/// A system line is drawn in the ink its severity earns: info is chrome, warn and
/// error are the design's two colours — and none of them is the accent a link
/// would be drawn in.
#[gpui_kit::test]
fn a_system_line_is_not_drawn_in_the_accent(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let _ = host;
    cx.update(|_window, cx| {
        let palette = crate::style::Palette::from_app(cx);
        use session::NoticeSeverity::*;
        assert_eq!(
            crate::rows::severity_color(Info, &palette),
            palette.muted_foreground,
            "an info line is chrome"
        );
        assert_eq!(crate::rows::severity_color(Warn, &palette), palette.warning);
        assert_eq!(
            crate::rows::severity_color(Error, &palette),
            palette.destructive
        );
        assert_ne!(crate::rows::severity_color(Info, &palette), palette.primary);
        assert_ne!(crate::rows::severity_color(Info, &palette), palette.info);
    });
}

// --- the design's own numbers (`Rows.css`, `styles.css`, `Workspace.css`) -------------

/// A row's box, by the id its wrapper carries.
fn box_of(window: &Window, id: gpui_kit::ElementId) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    window.find(id).bounds()
}

/// The room between two rows, as the list paints them: the next row's top less
/// the one before it's bottom.
fn room_between(
    window: &Window,
    above: gpui_kit::ElementId,
    below: gpui_kit::ElementId,
) -> gpui_kit::Pixels {
    let above = box_of(window, above);
    let below = box_of(window, below);
    below.origin.y - above.bottom()
}

/// The room the design puts between two rows is the larger of the two margins it
/// gives them (`.tc{margin:10px 0}`, `.rp{margin:14px 0}`, `p{margin:10px 0}`),
/// because a block flow collapses the two it finds side by side: 10px between one
/// tool call and the next, 14px wherever a report is one of the two.
#[gpui_kit::test]
fn the_room_between_rows_is_the_designs(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            user("u_1", "a turn"),
            assistant("a_1", "an answer", "final"),
            tool("t_1", "c1", "bash", "first", false),
            tool("t_2", "c2", "bash", "second", false),
            report("r_1", 3),
            report("r_2", 4),
            tool("t_3", "c3", "bash", "third", false),
        ]
    );
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let measure = |id: &str| row_id("transcript-measure", id);
        let room = |above: &str, below: &str| room_between(window, measure(above), measure(below));

        assert_eq!(room("u_1", "a_1"), px(10.), "a user row then a message");
        assert_eq!(room("a_1", "t_1"), px(10.), "a message then a tool call");
        assert_eq!(
            room("t_1", "t_2"),
            px(10.),
            "two tool calls, .tc's own margin"
        );
        assert_eq!(room("t_2", "r_1"), px(14.), "a tool call then a report");
        assert_eq!(room("r_1", "r_2"), px(14.), "two reports, .rp's own margin");
        assert_eq!(room("r_2", "t_3"), px(14.), "a report then a tool call");

        // The list opens at its own 16px (`.transcript-scroll{padding:var(--inset)
        // 0}`), and the first row is there.
        assert_eq!(box_of(window, measure("u_1")).origin.y, px(16.));
    });
}

/// The tool card, to the numbers `Rows.css` gives it: a 34px head with the
/// caret's own 16px column, a body inset 42px from the card's left and 10px from
/// its top, a caption on a 17.25px line box 4px above a `72px 1fr` key/value
/// grid whose rows sit 3px apart on a 19px line.
#[gpui_kit::test]
fn the_tool_card_is_the_designs(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![tool("t_1", "c1", "bash", "all tests passed", false)]
    );
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let card = box_of(window, row_id("transcript-tool-row", "t_1"));
        let head = box_of(window, row_id("transcript-tool", "t_1"));
        // `.tc-head{height:34px}`, inside the card's own 1px border.
        assert_eq!(head.size.height, px(34.));
        assert_eq!(card.size.height, px(36.), "1px + the 34px head + 1px");
    });

    cx.update(|window, cx| window.click(row_id("transcript-tool", "t_1"), cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let head = box_of(window, row_id("transcript-tool", "t_1"));

        let args = row_id("transcript-tool-arguments", "t_1");
        let args_box = box_of(window, args.clone());
        // `.tc-body{padding:10px 12px 12px 42px}` — from the card's own edge,
        // which is the head's box (the 1px border is outside it) — under the
        // body's own 1px top rule (`border-top:1px solid var(--rule-soft)`).
        assert_eq!(args_box.origin.x - head.origin.x, px(42.));
        assert_eq!(
            args_box.origin.y - head.bottom(),
            px(11.),
            "1px rule + 10px"
        );

        // `.tc-caption{font-size:11.5px}` on the page's 1.5 line box, 4px above
        // the grid (`.tc-caption{margin-bottom:4px}`).
        let caption_id: gpui_kit::ElementId = (args.clone(), "caption").into();
        let caption = box_of(window, caption_id);
        assert!(
            (caption.size.height - px(17.25)).abs() <= px(0.5),
            "an 11.5px caption on the page's 1.5 line box: {caption:?}"
        );
        let first: gpui_kit::ElementId = (args.clone(), "0").into();
        let first_box = box_of(window, first.clone());
        assert_eq!(first_box.origin.y - caption.bottom(), px(4.));

        // `.tc-kv{grid-template-columns:72px 1fr;gap:3px 12px;font-size:13px;
        // line-height:19px}` — the key's column is 72px, the value starts 12px
        // past it, and the rows are 19px tall.
        let key: gpui_kit::ElementId = (first.clone(), "key").into();
        let value: gpui_kit::ElementId = (first.clone(), "value").into();
        let key_box = box_of(window, key);
        let value_box = box_of(window, value);
        assert_eq!(key_box.size.width, px(72.));
        assert_eq!(value_box.origin.x - key_box.right(), px(12.));
        assert_eq!(first_box.size.height, px(19.));
        assert_eq!(key_box.size.height, px(19.));

        // `.tc-result{font-size:13px;line-height:19px}`, under its own caption.
        let result = row_id("transcript-tool-result", "t_1");
        let result_box = box_of(window, result.clone());
        let result_caption: gpui_kit::ElementId = (result.clone(), "caption").into();
        let result_caption = box_of(window, result_caption);
        let body: gpui_kit::ElementId = (result.clone(), "text").into();
        let body = box_of(window, body);
        assert_eq!(body.size.height, px(19.));
        assert_eq!(result_box.origin.y - args_box.bottom(), px(10.)); // .tc-body{gap:10px}
        assert_eq!(body.origin.y - result_caption.bottom(), px(4.));
    });
}

/// Two fields of one payload sit `.tc-kv`'s own 3px apart.
#[gpui_kit::test]
fn a_tool_payloads_rows_sit_three_pixels_apart(cx: &mut TestAppContext) {
    let args = json!({ "command": "cargo test", "timeout": 30 });
    let item = item(json!({
        "id": "t_1", "ts": 1, "kind": "tool", "call_id": "c1", "name": "bash",
        "args": args, "status": "ok",
        "result": { "text": "ok", "chars": 2, "truncated": false },
    }));
    let (_view, cx) = open!(cx, vec![item]);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| window.click(row_id("transcript-tool", "t_1"), cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let args = row_id("transcript-tool-arguments", "t_1");
        let first: gpui_kit::ElementId = (args.clone(), "0").into();
        let second: gpui_kit::ElementId = (args.clone(), "1").into();
        assert_eq!(room_between(window, first, second), px(3.));
    });
}

/// The report card, to `Rows.css`'s `.rp`: a 36px head over one row per section,
/// each `9px 12px` of padding on a 20px line with an 80px label column 12px from
/// the value.
#[gpui_kit::test]
fn the_report_card_is_the_designs(cx: &mut TestAppContext) {
    let (_view, cx) = open!(cx, vec![report("r_1", 3)]);
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let card = box_of(window, row_id("transcript-report", "r_1"));
        let head = box_of(window, row_id("transcript-report-heading", "r_1"));
        assert_eq!(head.size.height, px(36.));
        assert_eq!(
            head.origin.y - card.origin.y,
            px(1.),
            "inside the card's border"
        );

        let row_id_ = row_id("transcript-report-done", "r_1");
        let report_row = box_of(window, row_id_.clone());
        // `.rp-row{padding:9px 12px;font-size:13.5px;line-height:20px}` and the
        // 1px rule `+ .rp-row` is drawn with, at the card's bottom.
        assert_eq!(report_row.size.height, px(39.));
        assert_eq!(
            report_row.origin.y - head.bottom(),
            px(0.),
            "the leading rule belongs to the row above it"
        );
        assert_eq!(
            report_row.origin.x - card.origin.x,
            px(1.),
            "inside the border"
        );

        let label: gpui_kit::ElementId = (row_id_.clone(), "label").into();
        let label = box_of(window, label);
        // `.rp-row{grid-template-columns:80px 1fr;gap:12px;padding:… 12px}`.
        assert_eq!(label.origin.x - report_row.origin.x, px(12.));
        assert_eq!(label.size.width, px(80.));
        let value: gpui_kit::ElementId = (row_id_.clone(), "value").into();
        assert_eq!(box_of(window, value).origin.x - label.right(), px(12.));
    });
}

/// The user row: `.user-row`'s `8px 12px` of padding around a 14px line on the
/// page's 1.5 line box.
#[gpui_kit::test]
fn the_user_row_is_the_designs(cx: &mut TestAppContext) {
    let (_view, cx) = open!(cx, vec![user("u_1", "a turn")]);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let row = box_of(window, row_id("transcript-user", "u_1"));
        assert_eq!(row.size.height, px(37.), "8 + 21 + 8");
    });
}

/// The thinking a message carries is drawn *above* it — the model's own aside
/// before the message it wrote (`Transcript.tsx`'s `Thinking`, before its
/// `Markdown`) — on the design's sizes: a 12px label, 14px italic text, and the
/// 10px `.thinking{margin:10px 0}` leaves between it and the message.
#[gpui_kit::test]
fn thinking_is_drawn_above_its_message(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![assistant_with_thinking("a_1", "an answer", "a thought")]
    );
    view.update(cx, |view, cx| view.set_show_thinking(true, cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let thinking = box_of(window, row_id("transcript-thinking", "a_1"));
        let message = box_of(window, row_id("transcript-message", "a_1"));
        assert!(
            thinking.bottom() < message.origin.y,
            "the aside comes first: {thinking:?} vs {message:?}"
        );
        // `.thinking{padding-left:8px;border-left:2px solid var(--border)}`, and
        // `.thinking{margin:10px 0}` leaving the message 10px below it.
        let text = box_of(window, row_id("transcript-thinking-text", "a_1"));
        // The 2px rule is outside the 8px padding, as it is in the design.
        assert_eq!(text.origin.x - thinking.origin.x, px(10.));
        assert_eq!(message.origin.y - thinking.bottom(), px(10.));
        // `.thinking-label{font-size:12px}` then `.thinking-text{font-size:14px}`,
        // each on the page's 1.5 line box.
        assert_eq!(thinking.size.height, px(18. + 21.));
    });
}

/// A report's own fields are markdown, as the design draws them: `.rp-row`'s body
/// goes through the renderer, so an evidence line is a list and a path is a code
/// span, not the source with its `-` and backticks.
#[gpui_kit::test]
fn a_reports_field_is_rendered_as_markdown(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![item(json!({
            "id": "r_1", "ts": 1, "kind": "lane_report", "lane": 3,
            "done": "the captions read `arguments`", "evidence": "- `transcript::tests`\n- both themes",
            "next": "", "blocked": "", "requests": ""
        }))]
    );
    cx.update(|window, cx| window.render_frame(cx));

    let document = move |view: &Entity<TranscriptView>, cx: &App, label: &'static str| {
        view.read(cx)
            .data
            .read(cx)
            .field_documents
            .get(&("r_1".to_string(), label))
            .cloned()
            .unwrap_or_else(|| panic!("the {label} field keeps a document"))
    };
    cx.read(|cx| {
        let evidence = document(&view, cx, "evidence").read(cx).rendered_text();
        assert!(
            evidence.as_str().contains("transcript::tests")
                && evidence.as_str().contains("both themes"),
            "{:?}",
            evidence.as_str()
        );
        assert!(
            !evidence.as_str().contains('`'),
            "the backticks are a code span, not text: {:?}",
            evidence.as_str()
        );
        let done = document(&view, cx, "done").read(cx).rendered_text();
        assert_eq!(done.as_str().trim(), "the captions read arguments");
    });

    // A report that goes takes its fields' documents with it.
    view.update(cx, |view, cx| view.replace(vec![user("u_2", "gone")], cx));
    cx.read(|cx| assert!(view.read(cx).data.read(cx).field_documents.is_empty()));
}

/// A reader's own wheel takes the list off its tail: the offset moves, the pin lets
/// go, and the design's "↓ Jump to latest" is drawn — and scrolling back to the tail
/// puts it away again.
///
/// The wheel is the one scroll the design counts as the reader's (`USER_WINDOW`),
/// and the one thing that drives it is the box's own wheel handler — a positive dy
/// is a wheel up, away from the tail.
#[gpui_kit::test]
fn a_wheel_takes_the_list_off_its_tail_and_the_pill_appears(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..40)
        .map(|i| user(&format!("u_{i:02}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    for _ in 0..4 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    let tail = cx.read(|cx| view.read(cx).scroll.offset());
    // The design's own distance from the bottom: `max + offset`, 0 at the tail (the
    // offset grows negative as the list scrolls down, to `-max`).
    let gap = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let view = view.read(cx);
            view.scroll.max_offset().y + view.scroll.offset().y
        })
    };
    assert_eq!(gap(cx), px(0.), "the list opens at its tail");
    assert!(
        cx.read(|cx| view.read(cx).pin.is_pinned()),
        "the list opens following its tail"
    );
    assert!(
        !cx.update(|window, _| window.find("transcript-jump").visible()),
        "and there is nothing to jump back to"
    );

    // A wheel up, over the list.
    cx.update(|window, cx| {
        window.scroll(
            "transcript-scroll",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(600.))),
            cx,
        );
    });
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    assert_eq!(
        gap(cx),
        px(600.),
        "the wheel moved the list 600px off its tail (from {tail:?})"
    );
    assert!(
        cx.read(|cx| view.read(cx).pin.is_away()),
        "600px up is past the design's 240px, so the list is not following"
    );
    assert!(
        cx.update(|window, _| window.find("transcript-jump").visible()),
        "and the pill is drawn"
    );

    // Back down to the tail: following again, nothing to jump to.
    cx.update(|window, cx| {
        window.scroll(
            "transcript-scroll",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-900.))),
            cx,
        );
    });
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    assert_eq!(
        cx.read(|cx| view.read(cx).scroll.offset()),
        tail,
        "the wheel down stops at the tail"
    );
    assert_eq!(gap(cx), px(0.));
    assert!(cx.read(|cx| view.read(cx).pin.is_pinned()));
    assert!(
        !cx.update(|window, _| window.find("transcript-jump").visible()),
        "with the reader back at the latest, the pill is away..."
    );
}

/// "↓ Jump to latest" goes to the tail — the latest row on screen — not to the
/// head. gpui's offset runs *negative* downwards, so an ease aimed at
/// `+max_offset` clamps to 0 and the reader never leaves the head.
#[gpui_kit::test]
fn jump_to_latest_lands_on_the_tail(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..40)
        .map(|i| user(&format!("u_{i:02}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    for _ in 0..4 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    // The reader goes to the very top.
    view.update(cx, |view, _| {
        view.pin.touched();
        view.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
    });
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    let transcript = cx.update(|window, _| window.find("transcript").bounds());
    let first = cx.update(|window, _| window.find(row_id("transcript-row", "u_00")).bounds());
    assert!(
        first.top() >= transcript.top() - px(1.),
        "at the head first: {first:?}"
    );

    // The reader's own way back: a press on the pill, not a call.
    view.update(cx, |view, cx| {
        view.pin.on_scroll(3000.);
        cx.notify();
    });
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    assert!(
        cx.update(|window, _| window.find("transcript-jump").visible()),
        "the pill is drawn before the press"
    );
    cx.update(|window, cx| window.click("transcript-jump", cx));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    // The scroller itself is at the tail: gpui's offset is `-max_offset` down
    // there, and an ease aimed at `+max_offset` clamps to 0 — the head.
    let (offset, max_offset) = cx.read(|cx| {
        let view = view.read(cx);
        (
            f32::from(view.scroll.offset().y),
            f32::from(view.scroll.max_offset().y),
        )
    });
    assert_eq!(
        offset, -max_offset,
        "the scroller sits at `-max_offset`, the tail, not at the head"
    );
    let last = cx.update(|window, _| window.find(row_id("transcript-row", "u_39")).bounds());
    assert!(
        last.bottom() <= transcript.bottom() + px(1.)
            && last.bottom() > transcript.bottom() - px(60.),
        "the latest row is at the pane's foot after the jump: {last:?} vs {transcript:?}"
    );
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(
        !cx.update(|window, _| window.find("transcript-jump").visible()),
        "and the pill is away again"
    );
}

/// The rich-text style is built on the Base seam so that the ink a table's *row*
/// rules are drawn in is the design's `--rule` — `color-mix(in srgb, var(--fg)
/// 17%, var(--bg))` — rather than the widget border the component fold always
/// ended at (`--border`, a step lighter), in both themes.
///
/// The colours the fold took from the theme are passed explicitly on this seam
/// (`TextViewStyle::default()` is the neutral light palette), so this holds them
/// to the theme's own values and the heading steps to the app's, as well.
#[gpui_kit::test]
fn the_text_style_draws_rules_in_the_designs_ink_in_both_themes(cx: &mut TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
    use gpui_kit::{rems, StyleRefinement};

    cx.update(gpui_kit::init);
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update(|cx| ComponentTheme::change(mode, None, cx));
        let (style, palette, theme) = cx.update(|cx| {
            (
                crate::style::text_style(cx),
                crate::style::Palette::from_app(cx),
                cx.theme().clone(),
            )
        });

        assert_eq!(
            style.border(),
            crate::style::mix(style.foreground(), 17., palette.background),
            "{mode:?}: a row rule is `--rule`, 17% of the foreground into the page"
        );
        assert_ne!(
            style.border(),
            theme.border,
            "{mode:?}: and not the widget border the component style fell back to"
        );

        // Nothing else rides on the seam change: the fold's colours, the
        // appearance and the paragraph gap are the theme's own.
        assert_eq!(style.foreground(), theme.foreground);
        assert_eq!(style.muted_foreground(), theme.muted_foreground);
        assert_eq!(style.link(), theme.link);
        assert_eq!(style.selection(), theme.selection);
        assert_eq!(style.code_background(), theme.muted);
        assert_eq!(style.is_dark(), theme.is_dark());
        assert_eq!(
            style.paragraph_gap(),
            rems(0.625),
            "`p {{ margin: 10px 0 }}`"
        );

        // Headings keep the app's own step up from the body size.
        for (level, scale) in [(1u8, 1.25), (2, 1.1), (3, 1.05), (4, 1.), (5, 1.), (6, 1.)] {
            assert_eq!(
                style.heading(level),
                StyleRefinement::default().text_size(px(f32::from(theme.font_size) * scale)),
                "{mode:?}: heading {level} steps by {scale}"
            );
        }
    }
}
