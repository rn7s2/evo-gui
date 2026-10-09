//! `TestAppContext` tests: the items the list is fed, the retained markdown documents, the
//! rows a reader can act on, and the panel helpers that do not need a window.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::base::TextViewState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, px, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, Styled as _, TestAppContext, TestSupportExt as _, Window,
};
use serde_json::{json, Value};
use session::{Item, ItemKind};

use crate::rows::{
    cap_fields, cap_text, json_fields, row_id, take_chars, thinking_tail, Cap, FieldValue,
    CONTEXT_BLOCK_LINES, RESULT_LIMIT, TC_BODY_INDENT, THINKING_TAIL_CHARS, TOOL_ROW_HEIGHT,
    TURN_LABEL_OVERHANG, USER_LINE, VALUE_LIMIT,
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

/// A message still being written, whose thinking has started to arrive.
fn streaming_with_thinking(id: &str, text: &str, thinking: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "assistant", "text": text,
        "thinking": thinking, "status": "streaming"
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

/// The tab page's own arrangement: the transcript in a cached view, and a neighbour that
/// notifies on its own — a lane's breathing dot, which asks for a frame every frame.
struct CachedHost {
    transcript: Entity<TranscriptView>,
    neighbour: Entity<Neighbour>,
}

struct Neighbour;

impl Render for Neighbour {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        cx.notify();
        div().h(px(24.)).id("neighbour").test_support()
    }
}

impl CachedHost {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
            neighbour: cx.new(|_| Neighbour),
        }
    }
}

impl Render for CachedHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div().flex_1().min_h_0().children(Some(
                    gpui_kit::AnyView::from(self.transcript.clone()).cached(
                        gpui_kit::StyleRefinement::default()
                            .flex_1()
                            .min_h_0()
                            .min_w_0(),
                    ),
                )),
            )
            .child(self.neighbour.clone())
            .id("cached-host")
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
    // The page is drawn in the record's own order: the older item is spliced in front
    // of what is held, above the rows the reader was reading.
    cx.update(|window, cx| {
        window.render_frame(cx);
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

/// With the thinking hidden, the pips a streaming message waits behind carry the
/// model's own last words past beside them, the way the TUI's activity line does:
/// one line, the newest text at the right edge, the older cut off at the left.
#[gpui_kit::test]
fn a_hidden_thinking_streams_past_beside_the_waiting_pips(cx: &mut TestAppContext) {
    let kept = "alpha  beta\n\tgamma\tdelta\n\nepsilon";
    let filler = "z".repeat(200);
    let thinking = format!("{kept}\n{filler}");
    let expected = format!(
        "{} {filler}",
        kept.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    let (view, cx) = open!(cx, vec![streaming_with_thinking("e_1", "", &thinking)]);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let ticker = window.find(row_id("transcript-thinking-ticker", "e_1"));
        assert_eq!(
            ticker.label(),
            Some(expected.as_str()),
            "the collapsed tail"
        );
        assert_eq!(ticker.bounds().size.height, px(18.), "one line");

        // The line is longer than the box it is drawn in: its right end is the
        // box's right end, and its left end is past the box's left one, where it
        // is clipped — the newest words are the ones in sight.
        let text = window
            .find(row_id("transcript-thinking-ticker-text", "e_1"))
            .bounds();
        let ticker = ticker.bounds();
        assert!(
            text.size.width > ticker.size.width,
            "the tail is longer than the row: {text:?} in {ticker:?}"
        );
        assert!(
            text.origin.x < ticker.origin.x,
            "and cut off at its left: {text:?} in {ticker:?}"
        );
        assert!(
            (text.right() - ticker.right()).abs() <= px(0.5),
            "with the newest words at the right edge: {text:?} in {ticker:?}"
        );

        // The pips are still what holds the place, and the thinking block is not
        // drawn beside them.
        assert!(window
            .try_find(row_id("transcript-waiting", "e_1"))
            .is_some());
        assert!(window
            .try_find(row_id("transcript-thinking", "e_1"))
            .is_none());
    });

    // A message that has started writing is its own words, not a ticker.
    view.update(cx, |view, cx| {
        view.upsert(streaming_with_thinking("e_1", "Here we go.", &thinking), cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-thinking-ticker", "e_1"))
            .is_none());
    });
}

/// The ticker is the hidden thinking's double, so it steps aside when the thinking
/// is shown and when there is nothing to show but whitespace.
#[gpui_kit::test]
fn the_ticker_steps_aside_for_the_shown_thinking(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![streaming_with_thinking("e_1", "", "a thought")]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-thinking-ticker", "e_1"))
            .is_some());
    });

    view.update(cx, |view, cx| view.set_show_thinking(true, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(
            window
                .try_find(row_id("transcript-thinking-ticker", "e_1"))
                .is_none(),
            "the thinking block carries it instead"
        );
        assert!(window
            .try_find(row_id("transcript-thinking", "e_1"))
            .is_some());
        assert!(window
            .try_find(row_id("transcript-waiting", "e_1"))
            .is_some());
    });

    view.update(cx, |view, cx| view.set_show_thinking(false, cx));
    view.update(cx, |view, cx| {
        view.upsert(streaming_with_thinking("e_1", "", "  \n\t "), cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(
            window
                .try_find(row_id("transcript-thinking-ticker", "e_1"))
                .is_none(),
            "whitespace is nothing to run past"
        );
        assert!(window
            .try_find(row_id("transcript-waiting", "e_1"))
            .is_some());
    });
}

/// The ticker's line: whitespace folds to single spaces, and only a bounded tail of
/// a long think is kept, cut on a character boundary.
#[test]
fn the_ticker_keeps_a_collapsed_tail_of_the_thinking() {
    assert_eq!(thinking_tail(""), None);
    assert_eq!(thinking_tail("  \n\t "), None);
    assert_eq!(
        thinking_tail("one\ntwo").as_deref(),
        Some("one two"),
        "a newline is a space"
    );
    assert_eq!(
        thinking_tail(" a\t\tb   c\n\nd ").as_deref(),
        Some("a b c d"),
        "runs of whitespace are one space, and the ends are trimmed"
    );

    let long = format!("start {}", "é".repeat(1_000));
    let tail = thinking_tail(&long).expect("a tail");
    assert_eq!(tail.chars().count(), THINKING_TAIL_CHARS);
    assert!(tail.ends_with(&"é".repeat(20)), "the newest words are kept");
    assert!(!tail.contains("start"), "and the oldest are not");
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

/// A queued turn says when it goes, and reads as a sent turn once evo takes it.
#[gpui_kit::test]
fn a_queued_turn_becomes_a_sent_turn(cx: &mut TestAppContext) {
    let after_run = item(json!({
        "id": "e_4", "ts": 1, "kind": "user", "text": "once it ends",
        "status": "queued", "queue": "after_run"
    }));
    let now = item(json!({
        "id": "e_3", "ts": 1, "kind": "user", "text": "and then",
        "status": "queued", "queue": "now"
    }));
    let (view, cx) = open!(cx, vec![user("e_1", "go"), now, after_run]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find(row_id("transcript-queued", "e_3")).label(),
            Some("queued · sent at the next step")
        );
        assert_eq!(
            window.find(row_id("transcript-queued", "e_4")).label(),
            Some("queued · sent when the run ends")
        );
    });

    // evo drained it: `item.patch {status: "sent"}`.
    view.update(cx, |view, cx| {
        view.upsert(user("e_3", "and then"), cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert!(window
            .try_find(row_id("transcript-queued", "e_3"))
            .is_none());
        assert!(window
            .try_find(row_id("transcript-cancel", "e_3"))
            .is_none());
        assert!(window
            .try_find(row_id("transcript-queued", "e_4"))
            .is_some());
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

/// The scrollback walks itself back: the reader presses nothing. For as long as the
/// topic says there is more behind the oldest item held, the view asks for the next
/// page — one in flight at a time, each asked from the oldest item it holds — and the
/// quiet line at the head says a page is on its way.
#[gpui_kit::test]
fn the_scrollback_is_walked_back_without_being_pressed_for(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![user("e_1", "the tail")]);
    let asked = Rc::new(RefCell::new(Vec::new()));
    let oldest_seen = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(
            move |oldest, _window, _cx| oldest_seen.borrow_mut().push(oldest.to_string()),
            cx,
        );
    });

    // Nothing behind the only item held: nothing is asked for, and nothing is said.
    frames(cx, 2);
    assert!(asked.borrow().is_empty());
    assert!(
        cx.update(|window, _| window.try_find("transcript-loading-older").is_none()),
        "no line while there is nothing behind the record"
    );

    // The topic says there is more: the page is asked for at once, from the oldest item
    // held, and the head says it is on its way.
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);
    assert_eq!(asked.borrow().as_slice(), ["e_1"]);
    assert!(
        cx.update(|window, _| window.try_find("transcript-loading-older").is_some()),
        "and the head says a page is on its way"
    );
    // One page at a time: the next frame asks for nothing while that one is in flight.
    frames(cx, 3);
    assert_eq!(asked.borrow().len(), 1, "one ask per page");

    // The page lands: it is spliced in front of what is held, and the next one is asked
    // for from its own oldest item.
    view.update(cx, |view, cx| {
        view.prepend(vec![user("e_0", "earlier")], cx);
        view.set_history(true, false, cx);
    });
    frames(cx, 2);
    assert_eq!(asked.borrow().as_slice(), ["e_1", "e_0"]);
    assert!(
        cx.update(|window, _| window.try_find(row_id("transcript-row", "e_0")).is_some()),
        "the page that landed is in the list"
    );

    // The last page of all: the topic says there is nothing behind it, and the walk ends.
    view.update(cx, |view, cx| {
        view.prepend(vec![user("e_-1", "older still")], cx);
        view.set_history(false, false, cx);
    });
    frames(cx, 3);
    assert_eq!(
        asked.borrow().len(),
        2,
        "nothing is asked for past the record"
    );
    assert!(
        cx.update(|window, _| window.try_find("transcript-loading-older").is_none()),
        "and the head is quiet again"
    );
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
            item(json!({
                "id": "e_6", "ts": 1, "kind": "human_action",
                "action": "stop", "lanes": [2]
            })),
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
        // A person stopping a lane names it as a name: `Stopped Lane 2`.
        assert_eq!(
            window.find(row_id("transcript-action", "e_6")).label(),
            Some("Stopped Lane 2")
        );
        // A structured line is not a turn and opens none.
        assert!(window.try_find(("transcript-turn", 1u64)).is_none());
    });
}

/// A goal row's block is the objective, never the goal's id: the id is evo's own handle
/// on the goal (`b1` in the fixture's status line), and a reader reads what the goal is,
/// not what evo calls it. A transition with no objective on it says what happened.
#[test]
fn a_goal_row_reads_the_objective_and_never_the_goal_id() {
    let ItemKind::Goal(created) = goal("e_1", "created", "ship the redesign").kind else {
        panic!("the fixture is a goal transition");
    };
    let text = crate::rows::goal_text(&created);
    assert_eq!(text, "ship the redesign");
    assert!(
        !text.contains("b1"),
        "the goal's id is nowhere in what the row opens onto: {text:?}"
    );

    let paused = item(json!({
        "id": "e_2", "ts": 1, "kind": "goal", "event": "paused", "goal_id": "b1"
    }));
    let ItemKind::Goal(paused) = paused.kind else {
        panic!("the fixture is a goal transition");
    };
    assert_eq!(
        crate::rows::goal_text(&paused),
        "paused",
        "a transition with no objective says what happened to the goal"
    );
}

/// What evo journals for one `/goal <objective>` (a real session's entries, in order):
/// the goal itself, a `◆ goal created: …` notice with `source: goal`, and the message the
/// goal steers the agent with, whose origin is the same transition — a second `goal`
/// item, `created`, same id and objective. The reader is told once.
fn goal_created_as_evo_journals_it(objective: &str) -> Vec<Item> {
    vec![
        user("u_0", "before"),
        goal("e_goal", "created", objective),
        item(json!({
            "id": "e_notice", "ts": 1, "kind": "notice", "severity": "info",
            "text": format!("◆ goal created: {objective}"), "source": "goal", "durable": true
        })),
        item(json!({
            "id": "e_steer", "ts": 1, "kind": "goal", "event": "created", "goal_id": "b1",
            "objective": objective
        })),
    ]
}

fn held_ids(view: &Entity<TranscriptView>, cx: &App) -> Vec<String> {
    view.read(cx)
        .items(cx)
        .iter()
        .map(|item| item.id.clone())
        .collect()
}

#[gpui_kit::test]
fn a_goal_set_with_goal_is_one_row_however_the_record_arrives(cx: &mut TestAppContext) {
    let objective = "fix evo-gui bugs:\n- zooming\n- \"/goal\" shows two texts";

    // As a snapshot.
    let (view, cx) = open!(cx, goal_created_as_evo_journals_it(objective));
    cx.read(|cx| {
        assert_eq!(held_ids(&view, cx), ["u_0", "e_goal"]);
        assert_eq!(view.read(cx).declined(cx), 2, "the notice and the echo");
    });

    // Live, one item at a time.
    view.update(cx, |view, cx| view.clear(cx));
    for item in goal_created_as_evo_journals_it(objective) {
        view.update(cx, |view, cx| {
            view.upsert(item, cx);
        });
    }
    cx.read(|cx| assert_eq!(held_ids(&view, cx), ["u_0", "e_goal"]));

    // Paged back, a page at a time, with the boundary between the two goal items.
    view.update(cx, |view, cx| view.clear(cx));
    let mut record = goal_created_as_evo_journals_it(objective);
    let newer = record.split_off(2);
    view.update(cx, |view, cx| {
        view.replace(newer, cx);
        view.prepend(record, cx);
    });
    cx.read(|cx| {
        assert_eq!(
            held_ids(&view, cx),
            ["u_0", "e_steer"],
            "the page gives its copy up; the row already drawn stays"
        );
    });

    // Its header is one line of the row's height, not the objective's three.
    view.update(cx, |view, cx| {
        view.replace(goal_created_as_evo_journals_it(objective), cx)
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        let header = window.find(row_id("transcript-goal", "e_goal"));
        assert_eq!(
            header.label(),
            Some("Goal · created — fix evo-gui bugs: … · 12k/50k")
        );
        assert_eq!(header.bounds().size.height, px(TOOL_ROW_HEIGHT));
    });
}

/// A transition that is not one evo tells twice is never folded: a nudge after a nudge
/// is a nudge, and a second resume is a resume after a pause.
#[gpui_kit::test]
fn goal_transitions_that_repeat_on_purpose_all_stay(cx: &mut TestAppContext) {
    let (view, cx) = open!(
        cx,
        vec![
            goal("e_1", "created", "ship it"),
            goal("e_2", "continue", "ship it"),
            goal("e_3", "continue", "ship it"),
            goal("e_4", "paused", "ship it"),
            goal("e_5", "resumed", "ship it"),
            goal("e_6", "paused", "ship it"),
            goal("e_7", "resumed", "ship it"),
            goal("e_8", "objective_updated", "ship it now"),
            goal("e_9", "objective_updated", "ship it today"),
        ]
    );
    cx.read(|cx| {
        assert_eq!(
            held_ids(&view, cx),
            ["e_1", "e_2", "e_3", "e_4", "e_5", "e_6", "e_7", "e_8", "e_9"]
        );
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

/// The invitation an empty transcript draws, and the detail under it: a swarm's
/// coordinator has lanes to hand the work to, and the one agent of a single-agent
/// session has nobody to coordinate. The tab says which program it is, and a cached
/// view is a subtree that only redraws when it is notified.
#[gpui_kit::test]
fn an_empty_transcript_invites_the_agent_the_tab_is_showing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| TranscriptHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());

    // A view nobody has told is a swarm's: the copy the app drew before it had a
    // program to ask.
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            empty_note_line(window),
            "Ask the coordinator to get started"
        );
        assert_eq!(
            empty_detail_line(window),
            "It plans the work and hands tasks to its lanes."
        );
    });

    // One agent: no lanes, and no coordinator to plan the work for it.
    view.update(cx, |view, cx| view.set_swarm(false, cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(empty_note_line(window), "Ask the agent to get started");
        assert_eq!(
            empty_detail_line(window),
            "It does the work itself — there are no lanes to hand it to."
        );
    });

    // A lane's own line is its own either way, and a lane has no detail under it.
    view.update(cx, |view, cx| {
        view.set_agent(session::AgentKey::Lane(3), cx)
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            empty_note_line(window),
            "Lane 3 hasn't been given work yet."
        );
        assert!(
            window.try_find("transcript-empty-detail").is_none(),
            "one line is all a lane's empty transcript says"
        );
    });

    // And a swarm's view is a swarm's again once it is told so: the tab says which
    // program it is, both ways.
    view.update(cx, |view, cx| {
        view.set_agent(session::AgentKey::Coordinator, cx);
        view.set_swarm(true, cx);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            empty_note_line(window),
            "Ask the coordinator to get started"
        );
        assert_eq!(
            empty_detail_line(window),
            "It plans the work and hands tasks to its lanes."
        );
    });
}

/// The program a tab is is a cached transcript's own news: a view told it is one agent's
/// draws again, and being told the same thing twice is not news at all.
///
/// The app embeds the transcript as a cached subtree (§7.3), whose own elements are not
/// observable in this harness, so what this holds is the frame: how many times the view
/// built itself.
#[gpui_kit::test]
fn the_program_a_tab_is_renders_the_cached_transcript_again(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| CachedHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let rendered = cx.read(|cx| view.read(cx).renders());
    assert!(rendered > 0, "the cached view rendered when it was shown");

    view.update(cx, |view, cx| view.set_swarm(false, cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.read(|cx| view.read(cx).renders()) > rendered,
        "one agent's program is the empty transcript's own news, so the cached view is drawn again"
    );

    let settled = cx.read(|cx| view.read(cx).renders());
    view.update(cx, |view, cx| view.set_swarm(false, cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        cx.read(|cx| view.read(cx).renders()),
        settled,
        "the same program again changes nothing"
    );
}

/// The invitation's own line, as the empty transcript is drawing it.
fn empty_note_line(window: &Window) -> String {
    window
        .find("transcript-empty-note")
        .label()
        .unwrap_or_default()
        .to_string()
}

/// The detail under it.
fn empty_detail_line(window: &Window) -> String {
    window
        .find("transcript-empty-detail")
        .label()
        .unwrap_or_default()
        .to_string()
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

/// A picture that arrives is measured into its row: the row grows by exactly what the
/// picture added, the row below it moves down by the same amount and is never drawn over,
/// and the thumbnail is the design's own box at the reader's zoom.
///
/// The bytes arrive on a thread of their own, long after the frame that asked for them, so
/// this is the list's half of "nothing jumps".
#[gpui_kit::test]
fn an_arriving_image_re_measures_the_row_it_lands_in(cx: &mut TestAppContext) {
    let (view, cx) = open_in(
        cx,
        1200.,
        vec![
            user_with_image("u_1", 1),
            user("u_2", "the row after the picture"),
        ],
    );
    view.update(cx, |view, cx| view.on_fetch_image(|_, _, _, _| {}, cx));
    frames(cx, 2);

    let loading = row_box(cx, row_id("transcript-user", "u_1")).size.height;
    let below = row_box(cx, row_id("transcript-user", "u_2")).origin.y;
    assert!(
        row_box(cx, row_id("transcript-image-note-0", "u_1"))
            .size
            .height
            < px(32.),
        "until the bytes are here the row says so in one line"
    );

    let picture = crate::decode_image(&transcript_png_sized(1400, 1000)).expect("a decodable PNG");
    view.update(cx, |view, cx| view.set_image("u_1", 0, picture, cx));
    frames(cx, 2);

    let grown = row_box(cx, row_id("transcript-user", "u_1"));
    let after = row_box(cx, row_id("transcript-user", "u_2"));
    assert!(
        grown.size.height > loading,
        "the picture's box is taller than the line it replaced: {loading:?} -> {:?}",
        grown.size.height
    );
    assert_eq!(
        after.origin.y - below,
        grown.size.height - loading,
        "the row below moves down by exactly what the picture added"
    );
    assert!(
        after.origin.y >= grown.origin.y + grown.size.height,
        "and is never drawn over: {grown:?} then {after:?}"
    );

    // The thumbnail is the design's own box at the reader's zoom — a screenshot is read, not
    // framed, so it grows with the rest of the row (§7.2) — and its frame is the rounded one,
    // the border's 2px around what the picture was fitted to (168×120 of the 1400×1000 it is).
    let thumbnail = |cx: &mut gpui_kit::VisualTestContext| {
        row_box(cx, row_id("transcript-image-0", "u_1")).size
    };
    assert_eq!(
        thumbnail(cx),
        gpui_kit::size(px(170.), px(122.)),
        "the fitted 168×120 of a 1400×1000, inside the frame's 1px border"
    );
    set_zoom(cx, 1.5);
    assert_eq!(
        thumbnail(cx),
        gpui_kit::size(px(254.), px(182.)),
        "half again as large at 150% — a screenshot is read, not framed (§7.2)"
    );
    set_zoom(cx, 0.75);
    assert_eq!(
        thumbnail(cx),
        gpui_kit::size(px(128.), px(92.)),
        "and smaller at 75%"
    );
}

/// The same picture arriving while its row is off screen: the reader is somewhere else in
/// the record when the bytes land, and the row is not built again until it is scrolled back
/// to. The pane holds still — the reader's place is the reader's — and the row is measured
/// when it comes back.
#[gpui_kit::test]
fn an_image_that_arrives_off_screen_is_measured_when_it_comes_back(cx: &mut TestAppContext) {
    // The list follows the tail, so the picture is put one row from the end: on screen
    // at first, and out of it after one wheel up.
    let mut items: Vec<Item> = (1..38).map(|n| user(&format!("u_{n}"), "filler")).collect();
    items.push(user_with_image("u_38", 1));
    items.push(user("u_39", "the row after the picture"));
    let (view, cx) = open_in(cx, 1200., items);
    view.update(cx, |view, cx| view.on_fetch_image(|_, _, _, _| {}, cx));
    frames(cx, 2);

    wheel(cx, 2000.);
    assert!(
        cx.update(|window, _| window.try_find(row_id("transcript-user", "u_38")).is_none()),
        "the picture's row is off screen now"
    );
    let (shown, was) = (1..38)
        .find_map(|n| {
            let id = format!("u_{n}");
            cx.update(|window, _| {
                window
                    .try_find(row_id("transcript-user", &id))
                    .map(|element| (id, element.bounds()))
            })
        })
        .expect("a row the reader is on");

    let picture = crate::decode_image(&transcript_png_sized(1400, 1000)).expect("a decodable PNG");
    view.update(cx, |view, cx| view.set_image("u_38", 0, picture, cx));
    frames(cx, 2);
    assert_eq!(
        row_box(cx, row_id("transcript-user", &shown)).origin.y,
        was.origin.y,
        "the row the reader is on does not move while the picture lands behind them"
    );

    // Back at the tail, the row is the picture's own height, and the row after it is
    // where that height puts it — never drawn over it.
    wheel(cx, -20000.);
    let grown = row_box(cx, row_id("transcript-user", "u_38"));
    let after = row_box(cx, row_id("transcript-user", "u_39"));
    assert_eq!(
        row_box(cx, row_id("transcript-image-0", "u_38"))
            .size
            .height,
        px(122.),
        "the picture is measured when the row is built again"
    );
    assert!(
        after.origin.y >= grown.origin.y + grown.size.height,
        "and the row after it sits under it, never over it: {grown:?} then {after:?}"
    );
}

/// A picture too small for the box it is drawn in is a box, not a dot: `decode_image` bakes
/// it up by whole pixels, and a frame that has not the picture's own size to hug is held open
/// at `MIN_PICTURE` (scaled by the reader's zoom like the rest of the row).
///
/// A picture that is large enough to be drawn in its own shape hugs its frame instead
/// (`a_frame_hugs_the_picture_it_draws`), so the floor here is for the two cases that would
/// otherwise be a dot or a line: a 1×1 and an 8×8, baked to 48×48, and a 20×10, whose bake
/// stops at 40×20.
#[gpui_kit::test]
fn a_picture_smaller_than_its_frame_is_a_box_not_a_dot(cx: &mut TestAppContext) {
    let (view, cx) = open_in(cx, 1200., vec![user_with_image("u_1", 3)]);
    view.update(cx, |view, cx| view.on_fetch_image(|_, _, _, _| {}, cx));
    frames(cx, 2);
    for (n, (w, h)) in [(0u32, (1u32, 1u32)), (1, (8, 8)), (2, (20, 10))] {
        let picture = crate::decode_image(&transcript_png_sized(w, h)).expect("a decodable PNG");
        view.update(cx, |view, cx| view.set_image("u_1", n, picture, cx));
    }
    frames(cx, 2);

    let frame = |cx: &mut gpui_kit::VisualTestContext, n: u32| {
        row_box(cx, row_id(format!("transcript-image-{n}"), "u_1"))
    };
    let drawn = |cx: &mut gpui_kit::VisualTestContext, n: u32| {
        row_box(cx, row_id(format!("transcript-image-picture-{n}"), "u_1"))
    };
    let centred = |cx: &mut gpui_kit::VisualTestContext, n: u32| {
        let (frame, picture) = (frame(cx, n), drawn(cx, n));
        let slack = |outer: f32, inner: f32| (outer - inner) / 2.;
        for (axis, gap, want) in [
            (
                "x",
                f32::from(picture.origin.x - frame.origin.x),
                slack(f32::from(frame.size.width), f32::from(picture.size.width)),
            ),
            (
                "y",
                f32::from(picture.origin.y - frame.origin.y),
                slack(f32::from(frame.size.height), f32::from(picture.size.height)),
            ),
        ] {
            assert!(
                (gap - want).abs() <= 0.5,
                "{axis}: picture {n} sits at {gap}, not centred ({want})"
            );
        }
    };

    // Baked up to the box: a 1×1 and an 8×8 are both a 48×48 block of whole pixels, which
    // holds its own frame open — 48 and the 1px border.
    for n in 0..2 {
        assert_eq!(
            drawn(cx, n).size,
            gpui_kit::size(px(48.), px(48.)),
            "a picture too small for the box is baked up to fill it"
        );
        assert_eq!(
            frame(cx, n).size,
            gpui_kit::size(px(50.), px(50.)),
            "which is the frame's 48 plus the border it carries"
        );
        centred(cx, n);
    }

    // A 20×10 bakes to 40×20 — a whole number of pixels, not a stretch to the box — which
    // is a dot in one direction, so the frame is the 48px floor and the picture is centred
    // on it rather than hugging it.
    assert_eq!(drawn(cx, 2).size, gpui_kit::size(px(40.), px(20.)));
    assert_eq!(
        frame(cx, 2).size,
        gpui_kit::size(px(48.), px(48.)),
        "the floor"
    );
    centred(cx, 2);

    // The floor follows the reader's zoom like everything else in the row (§7.2), and what
    // is baked is baked: a 20×10's floor is 72 at 150%, while a 1×1's 48px block holds its
    // own frame — half again as large around it would be a band on every side, which is the
    // whole of what a frame must not be.
    set_zoom(cx, 1.5);
    assert_eq!(
        frame(cx, 0).size,
        gpui_kit::size(px(50.), px(50.)),
        "the picture's own 48 and its border, at any zoom"
    );
    assert_eq!(drawn(cx, 0).size, gpui_kit::size(px(48.), px(48.)));
    assert_eq!(
        frame(cx, 2).size,
        gpui_kit::size(px(72.), px(72.)),
        "the floor is half again as large at 150%"
    );
    centred(cx, 0);
    centred(cx, 2);
    set_zoom(cx, 1.0);

    // Opened, a tiny picture is a box as well: the full cap does not change what a 48px
    // block is, and the frame around it is still the block and its border.
    cx.update(|window, cx| window.click(row_id("transcript-image-0", "u_1"), cx));
    frames(cx, 2);
    assert_eq!(
        frame(cx, 0).size,
        gpui_kit::size(px(50.), px(50.)),
        "a 1×1 opens into the same box"
    );
    assert_eq!(drawn(cx, 0).size, gpui_kit::size(px(48.), px(48.)));
    centred(cx, 0);
}

/// A frame hugs the picture it draws: its size is the picture's fitted size — the picture's
/// own shape, inside the thumbnail's caps — plus the 1px border, so the picture fills it and
/// no side is left as an empty band.
///
/// GPUI fits a picture inside the element's box and leaves the rest of the box empty, so an
/// element sized 520×120 would draw a 1000×600 shot 200×120 with a 320px band beside it, and
/// a 300×900 one 40×120 with a band each side of its 40. Hence `fitted_size`, which is why
/// the element is sized rather than the box.
#[gpui_kit::test]
fn a_frame_hugs_the_picture_it_draws(cx: &mut TestAppContext) {
    let (view, cx) = open_in(cx, 1200., vec![user_with_image("u_1", 4)]);
    view.update(cx, |view, cx| view.on_fetch_image(|_, _, _, _| {}, cx));
    frames(cx, 2);
    let pictures = [
        (0u32, (1000u32, 600u32)),
        (1, (300, 900)),
        (2, (1400, 1000)),
        (3, (100, 100)),
    ];
    for (n, (w, h)) in pictures {
        let picture = crate::decode_image(&transcript_png_sized(w, h)).expect("a decodable PNG");
        view.update(cx, |view, cx| view.set_image("u_1", n, picture, cx));
    }
    frames(cx, 2);

    let frame = |cx: &mut gpui_kit::VisualTestContext, n: u32| {
        row_box(cx, row_id(format!("transcript-image-{n}"), "u_1"))
    };
    let drawn = |cx: &mut gpui_kit::VisualTestContext, n: u32| {
        row_box(cx, row_id(format!("transcript-image-picture-{n}"), "u_1"))
    };
    // The whole of "hugs": the frame is the picture and its border, and nothing else — the
    // picture's box starts at the frame's own 1px, and ends at its 1px.
    let hugs = |cx: &mut gpui_kit::VisualTestContext, n: u32, source: (u32, u32)| {
        let (frame, picture) = (frame(cx, n), drawn(cx, n));
        assert_eq!(
            (
                f32::from(frame.size.width) - f32::from(picture.size.width),
                f32::from(frame.size.height) - f32::from(picture.size.height),
            ),
            (2., 2.),
            "picture {n}: no band on any side of the {}×{} it was made from",
            source.0,
            source.1
        );
        assert_eq!(
            (
                f32::from(picture.origin.x - frame.origin.x),
                f32::from(picture.origin.y - frame.origin.y),
            ),
            (1., 1.),
            "picture {n}: and the picture sits on the frame's own border"
        );
        let kept = f32::from(picture.size.width) / f32::from(picture.size.height);
        let own = source.0 as f32 / source.1 as f32;
        assert!(
            (kept - own).abs() < 0.005,
            "picture {n}: drawn {kept} to one, its own shape being {own}"
        );
    };

    // 1000×600 is wider than the thumbnail's 520×120, so the height is what fits: 200×120.
    assert_eq!(
        drawn(cx, 0).size,
        gpui_kit::size(px(200.), px(120.)),
        "a 1000×600 fits `THUMBNAIL` 120 and brings its own width"
    );
    assert_eq!(frame(cx, 0).size, gpui_kit::size(px(202.), px(122.)));
    // 300×900 is taller: the width is what fits, 40.
    assert_eq!(
        drawn(cx, 1).size,
        gpui_kit::size(px(40.), px(120.)),
        "a 300×900 fits the same 120 and brings its own width"
    );
    assert_eq!(frame(cx, 1).size, gpui_kit::size(px(42.), px(122.)));
    // 1400×1000 fit both ways.
    assert_eq!(drawn(cx, 2).size, gpui_kit::size(px(168.), px(120.)));
    assert_eq!(frame(cx, 2).size, gpui_kit::size(px(170.), px(122.)));
    // A picture under both caps is its own size — the caps are caps, and nothing is
    // enlarged to meet them.
    assert_eq!(drawn(cx, 3).size, gpui_kit::size(px(100.), px(100.)));
    assert_eq!(frame(cx, 3).size, gpui_kit::size(px(102.), px(102.)));
    for (n, source) in pictures {
        hugs(cx, n, source);
    }

    // At 150% the caps are half again as large, and what fits them follows.
    set_zoom(cx, 1.5);
    assert_eq!(drawn(cx, 0).size, gpui_kit::size(px(300.), px(180.)));
    assert_eq!(frame(cx, 0).size, gpui_kit::size(px(302.), px(182.)));
    assert_eq!(drawn(cx, 1).size, gpui_kit::size(px(60.), px(180.)));
    assert_eq!(frame(cx, 1).size, gpui_kit::size(px(62.), px(182.)));
    assert_eq!(drawn(cx, 2).size, gpui_kit::size(px(252.), px(180.)));
    assert_eq!(
        drawn(cx, 3).size,
        gpui_kit::size(px(100.), px(100.)),
        "a picture under the caps is still its own size (§7.2)"
    );
    for (n, source) in pictures {
        hugs(cx, n, source);
    }
    set_zoom(cx, 1.0);

    // Opened, the cap is `FULL_IMAGE` 340: the same fits, and the same hug.
    for (n, source) in pictures {
        cx.update(|window, cx| window.click(row_id(format!("transcript-image-{n}"), "u_1"), cx));
        frames(cx, 2);
        hugs(cx, n, source);
    }
    assert_eq!(
        drawn(cx, 0).size,
        gpui_kit::size(px(520.), px(312.)),
        "a 1000×600 opens to 520 wide"
    );
    assert_eq!(frame(cx, 0).size, gpui_kit::size(px(522.), px(314.)));
    assert_eq!(
        drawn(cx, 2).size,
        gpui_kit::size(px(476.), px(340.)),
        "a 1400×1000 opens to 340 tall"
    );
    assert_eq!(frame(cx, 2).size, gpui_kit::size(px(478.), px(342.)));
    assert_eq!(
        drawn(cx, 3).size,
        gpui_kit::size(px(100.), px(100.)),
        "and a 100×100 is still its own size"
    );
    assert_eq!(frame(cx, 3).size, gpui_kit::size(px(102.), px(102.)));
}

/// A picture too small for its box is blown up **by whole pixels**, and nothing else is
/// touched: no interpolation between them (which is what a scaled draw would give), and no
/// enlargement at all for a picture that already reaches the box.
#[test]
fn a_picture_too_small_for_its_box_is_baked_up_by_whole_pixels() {
    for (width, height, want) in [
        (1u32, 1u32, (48u32, 48u32)),
        (8, 8, (48, 48)),
        (20, 10, (40, 20)),
        (24, 24, (48, 48)),
        // Already the box's size, or longer than it: its own pixels, one for one.
        (48, 48, (48, 48)),
        (100, 8, (100, 8)),
        (1000, 600, (1000, 600)),
    ] {
        let bytes = transcript_png_sized(width, height);
        let decoded = crate::decode_image(&bytes).expect("a decodable PNG");
        let size = decoded.size(0);
        assert_eq!(
            (size.width.0, size.height.0),
            (want.0 as i32, want.1 as i32),
            "a {width}×{height} picture bakes to {want:?}"
        );

        // Every pixel of the bake is a whole copy of the one it came from: the block an
        // 8×8 makes is 6×6 of the same ink, with no blend at its edge. The copy is in the
        // order GPUI carries a picture in — BGRA, its own decoders swap the red and blue —
        // so the expectation is the file's pixel with its outer two bytes exchanged.
        let factor = (want.0 / width).max(1);
        let source = image::load_from_memory(&bytes)
            .expect("the same PNG")
            .into_rgba8();
        let baked = decoded.as_bytes(0).expect("one frame");
        let stride = want.0 as usize * 4;
        let carried = |rgba: [u8; 4]| [rgba[2], rgba[1], rgba[0], rgba[3]];
        for y in 0..want.1 {
            for x in 0..want.0 {
                let at = y as usize * stride + x as usize * 4;
                assert_eq!(
                    &baked[at..at + 4],
                    carried(source.get_pixel(x / factor, y / factor).0),
                    "({x}, {y}) of the bake is ({}, {}) of the picture, pixel for pixel",
                    x / factor,
                    y / factor
                );
            }
        }
        // And the order itself, said once where a reader can see it: the fixture's ink is
        // `Rgba([10, 20, 30, 255])` and the frame carries it blue-first, which is the whole
        // of "a `RenderImage` is BGRA".
        assert_eq!(
            &baked[0..4],
            &[30, 20, 10, 255],
            "a `RenderImage` is BGRA, not the RGBA the file holds"
        );
    }
}

/// A one-pixel PNG, made here rather than embedded: the test is about the path from
/// bytes to a drawn frame, and this is the smallest thing that goes through it.
fn transcript_png() -> Vec<u8> {
    transcript_png_sized(2, 2)
}

/// The same, at a size a case can choose: an image larger than the row's own box is what
/// a screenshot is, and the one a cap can be seen on.
fn transcript_png_sized(width: u32, height: u32) -> Vec<u8> {
    use image::{ImageFormat, Rgba, RgbaImage};
    let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
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
    assert_eq!(target, "Lane 5");
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

/// The serve's own ephemeral system line is not part of the conversation: `session
/// ready` is said at every boot and dropped; a durable notice — the kind the journal
/// keeps — is the record and stays.
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
            notice.source = session::NoticeSource::Serve;
        }
        item
    };
    view.update(cx, |view, cx| {
        assert!(!view.upsert(later, cx), "an ephemeral line changes nothing");
        assert_eq!(view.items(cx).len(), 1, "only the durable one is held");
    });
}

/// A command's own output is a `notice` item the serve does **not** journal (`durable:
/// false`) whose source is `command`: `/lore` with no lore to show, `/model`'s list,
/// `/eval`'s answer, a command's refusal. It is what the reader asked to see, so it is
/// the record — a row, drawn and kept, and not counted as declined.
#[gpui_kit::test]
fn a_commands_output_is_drawn_even_though_the_server_does_not_keep_it(cx: &mut TestAppContext) {
    let saying = item(json!({
        "id": "n_cmd", "ts": 2, "kind": "notice", "severity": "info",
        "text": "no lore — /lore <text> adds durable guidance",
        "source": "command", "durable": false
    }));
    let refusing = item(json!({
        "id": "n_warn", "ts": 3, "kind": "notice", "severity": "warn",
        "text": "✗ /eval: boom", "source": "command", "durable": false
    }));
    // The serve's own boot line rides in the same snapshot, and is still dropped.
    let (view, cx) = open!(
        cx,
        vec![
            user("e_1", "hi"),
            ephemeral_notice("n_ready"),
            saying,
            refusing
        ]
    );
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find(row_id("transcript-notice", "n_cmd")).label(),
            Some("command · no lore — /lore <text> adds durable guidance"),
            "the command's line is drawn, under the source that said it"
        );
        assert_eq!(
            window.find(row_id("transcript-notice", "n_warn")).label(),
            Some("command · ✗ /eval: boom"),
            "a refused command's line is drawn the same way"
        );
        assert!(
            window
                .try_find(row_id("transcript-notice", "n_ready"))
                .is_none(),
            "the serve's boot line is still not drawn"
        );
    });
    // Held, and not declined: the record plus the declined is the topic's own list.
    cx.read(|cx| {
        assert_eq!(view.read(cx).items(cx).len(), 3, "turn + two command lines");
        assert_eq!(view.read(cx).declined(cx), 1, "only the serve's line");
    });

    // The stream's own shape: the `item.add` for such a notice, arriving after the
    // snapshot, is a row the moment it lands — the ephemeral is not what is dropped.
    let streamed = item(json!({
        "id": "n_cmd2", "ts": 4, "kind": "notice", "severity": "error",
        "text": "✗ /lore: boom", "source": "command", "durable": false
    }));
    view.update(cx, |view, cx| {
        assert!(view.upsert(streamed, cx), "the stream's line is the record");
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find(row_id("transcript-notice", "n_cmd2")).label(),
            Some("command · ✗ /lore: boom")
        );
    });
    cx.read(|cx| {
        assert_eq!(view.read(cx).declined(cx), 1, "and is not declined");
    });
}

/// A command's output that ends on a line break (`/model`'s list) is drawn without it:
/// the text view would draw a trailing break as a literal hard-break `\` after the last
/// line.
#[gpui_kit::test]
fn a_notice_is_drawn_without_its_trailing_line_break(cx: &mut TestAppContext) {
    let list = item(json!({
        "id": "n_list", "ts": 2, "kind": "notice", "severity": "info",
        "text": "model:\n  stub  stub-a  200k ctx · current\n  stub  stub-b  100k ctx\n",
        "source": "command", "durable": false
    }));
    let (_view, cx) = open!(cx, vec![list]);
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _| {
        assert_eq!(
            window.find(row_id("transcript-notice", "n_list")).label(),
            Some("command · model:\n  stub  stub-a  200k ctx · current\n  stub  stub-b  100k ctx"),
        );
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

/// A user row that opens no turn — queued, or cancelled before evo took it —
/// gets no turn rule above it, so it must carry the ordinary block air itself:
/// without that it is drawn flush against the row above it.
#[gpui_kit::test]
fn a_row_that_opens_no_turn_keeps_the_air_between_blocks(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![
            user("u_1", "a turn"),
            assistant("a_1", "an answer", "final"),
            queued("e_1", "and then"),
            tool("t_1", "c1", "bash", "all tests passed", false),
            queued("e_2", "one more"),
            queued("e_3", "and another"),
            assistant("a_2", "an answer to the queued words", "final"),
            user("u_2", "a second turn"),
        ]
    );
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, _| {
        let room = |above: gpui_kit::ElementId, below: gpui_kit::ElementId| {
            room_between(window, above, below)
        };
        let card = |name: &str, id: &str| row_id(name, id);

        // The cards themselves, not their row wrappers: a card's top edge is what
        // the reader sees against the row above it.
        assert_eq!(
            room(
                card("transcript-assistant", "a_1"),
                card("transcript-user", "e_1")
            ),
            px(10.),
            "a queued row under a message: p's own margin"
        );
        assert_eq!(
            room(
                card("transcript-user", "e_1"),
                card("transcript-tool-row", "t_1")
            ),
            px(10.),
            "and a tool call under a queued row keeps its own"
        );
        let above_tool = room(
            card("transcript-tool-row", "t_1"),
            card("transcript-user", "e_2"),
        );
        assert_eq!(
            above_tool,
            px(10.),
            "a queued row under a tool card: .tc's own margin"
        );
        assert!(
            above_tool >= px(10.),
            "and never a bare edge: {above_tool:?}"
        );
        assert_eq!(
            room(
                card("transcript-user", "e_2"),
                card("transcript-user", "e_3")
            ),
            px(10.),
            "two queued rows are two blocks"
        );
        assert_eq!(
            room(
                card("transcript-user", "e_3"),
                card("transcript-assistant", "a_2")
            ),
            px(10.),
            "and what follows a queued row keeps its own spacing"
        );

        // A sent turn is untouched: the rule above the card is still what carries
        // its space, so the card's own row adds none of its own and the card sits
        // exactly the design's 7px overhang below the line.
        let line = box_of(window, ("transcript-turn-line", 2usize).into());
        let row = box_of(window, row_id("transcript-row", "u_2"));
        let second = box_of(window, card("transcript-user", "u_2"));
        let first = box_of(window, card("transcript-assistant", "a_2"));
        assert_eq!(
            row.origin.y - first.bottom(),
            px(0.),
            "a sent turn takes no air of its own — the rule above it does"
        );
        assert!(
            line.origin.y >= first.bottom() + px(26.),
            "the rule keeps its own 26px above it: {line:?} vs {first:?}"
        );
        assert_eq!(
            second.origin.y - line.bottom(),
            px(TURN_LABEL_OVERHANG),
            "and the card still starts at the line, not below extra air"
        );
    });
}

/// The tool card, to the numbers `Rows.css` gives it: a 34px head with the
/// caret's own 16px column, a body inset to the column the head's text starts in
/// (the head's padding, the caret and the gap after it) and 10px from its top, a
/// caption on a 17.25px line box 4px above a `72px 1fr` key/value grid whose rows
/// sit 3px apart on a 19px line.
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
        // `.tc-body{padding:10px 12px 12px …}` — from the card's own edge, which is
        // the head's box (the 1px border is outside it) — under the body's own 1px
        // top rule (`border-top:1px solid var(--rule-soft)`). Its left inset is the
        // head's own text column, so the arguments and the result begin under the
        // tool's name.
        let name = box_of(window, row_id("transcript-tool-name", "t_1"));
        assert_eq!(
            TC_BODY_INDENT, 32.,
            "the head's own 8px of padding + its 16px caret column + 8px of gap"
        );
        assert_eq!(
            name.origin.x - head.origin.x,
            px(TC_BODY_INDENT),
            "the head's text starts at the column the body is inset to"
        );
        assert_eq!(
            args_box.origin.x, name.origin.x,
            "the body starts under the tool's name: {args_box:?} vs {name:?}"
        );
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
    cx.read(|cx| {
        assert!(
            !view
                .read(cx)
                .data
                .read(cx)
                .field_documents
                .keys()
                .any(|(id, _)| id == "r_1"),
            "the report's fields go with the report"
        )
    });
}

/// A reader's own wheel takes the list off its tail: the list scrolls, the pin lets go,
/// and the design's "↓ Jump to latest" is drawn — and scrolling back to the tail puts it
/// away again.
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
    assert_eq!(gap(&view, cx), 0., "the list opens at its tail");
    assert!(
        cx.read(|cx| view.read(cx).pin.is_pinned()),
        "the list opens following its tail"
    );
    assert!(
        !cx.update(|window, _| window.find("transcript-jump").visible()),
        "and there is nothing to jump back to"
    );

    // A wheel up, over the list.
    wheel(cx, 600.);
    let moved = gap(&view, cx);
    assert!(
        (moved - 600.).abs() < 1.,
        "the wheel moved the list 600px off its tail ({moved}px)"
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
    wheel(cx, -900.);
    assert_eq!(gap(&view, cx), 0., "the wheel down stops at the tail");
    assert!(cx.read(|cx| view.read(cx).pin.is_pinned()));
    assert!(
        !cx.update(|window, _| window.find("transcript-jump").visible()),
        "with the reader back at the latest, the pill is away..."
    );
}

#[gpui_kit::test]
fn an_unmeasured_tail_is_not_mistaken_for_a_complete_transcript(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, vec![user("u_0", "the last user message")]);
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    view.update(cx, |view, cx| {
        for i in 1..40 {
            view.upsert(assistant(&format!("a_{i}"), "newer answer", "final"), cx);
        }
        view.list.scroll_to(gpui_kit::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        assert_eq!(view.list.max_offset_for_scrollbar().y, px(0.));
        assert!(view.list.bounds_for_item(39).is_none());
        assert!(
            view.gap().is_infinite(),
            "unmeasured newer rows are not the bottom of the transcript"
        );
    });
}

/// "↓ Jump to latest" goes to the tail — the latest row on screen — not to the head.
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
        view.list.scroll_to(gpui_kit::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        // The gesture that brought them there is the reader's own, whichever way they
        // scrolled: a drag of the scrollbar to the head.
        view.touched();
    });
    for _ in 0..3 {
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
        view.pin.touched();
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
    let last = cx.update(|window, _| window.find(row_id("transcript-row", "u_39")).bounds());
    assert!(
        last.bottom() <= transcript.bottom() + px(1.)
            && last.bottom() > transcript.bottom() - px(60.),
        "the latest row is at the pane's foot after the jump: {last:?} vs {transcript:?}"
    );
    assert_eq!(
        gap(&view, cx),
        0.,
        "the list lands on its tail, not its head"
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
// --- the list: the whole record, and only a pane of rows built ----------------------

/// How many rows the list holds: every item of the record, and nothing else.
fn held(view: &Entity<TranscriptView>, cx: &gpui_kit::VisualTestContext) -> usize {
    cx.read(|cx| view.read(cx).slots.rows)
}

/// How far the reader is from the bottom of the list, in pixels.
fn gap(view: &Entity<TranscriptView>, cx: &gpui_kit::VisualTestContext) -> f32 {
    cx.read(|cx| view.read(cx).gap())
}

/// Whether the row of `id` is *built*: the list builds the rows the pane can reach, so
/// a row that is not on screen is not built either.
fn drawn_row(window: &mut gpui_kit::VisualTestContext, id: &str) -> bool {
    window.update(|window, _| window.try_find(row_id("transcript-row", id)).is_some())
}

/// Where a row is, in window coordinates.
fn row_bounds(
    cx: &mut gpui_kit::VisualTestContext,
    id: &str,
) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    cx.update(|window, _| window.find(row_id("transcript-row", id)).bounds())
}

/// The pane the reader can see: the transcript's own box.
fn pane(cx: &mut gpui_kit::VisualTestContext) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    cx.update(|window, _| window.find("transcript-scroll").bounds())
}

/// The row at the top of the pane, of `ids`: what the reader's eye lands on first.
fn top_row_in_pane(cx: &mut gpui_kit::VisualTestContext, ids: &[String]) -> Option<String> {
    let pane = pane(cx);
    ids.iter()
        .find(|id| {
            drawn_row(cx, id)
                && row_bounds(cx, id).top() >= pane.top()
                && row_bounds(cx, id).top() < pane.bottom()
        })
        .cloned()
}

/// Render `count` frames.
fn frames(cx: &mut gpui_kit::VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.update(|window, cx| window.render_frame(cx));
    }
}

/// One wheel over the list: a positive `dy` is a wheel up, away from the tail, and it
/// is the reader's own however the list answers it.
fn wheel(cx: &mut gpui_kit::VisualTestContext, dy: f32) {
    cx.update(|window, cx| {
        window.scroll(
            "transcript-scroll",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(dy))),
            cx,
        );
    });
    for _ in 0..3 {
        cx.update(|window, cx| window.render_frame(cx));
    }
}

/// The whole record is in the list — every item of the topic — and only the rows the
/// pane can reach are built: a journal of hundreds of items costs a pane of rows a
/// frame, not a parse and a layout per item.
#[gpui_kit::test]
fn the_whole_record_is_held_and_only_the_rows_on_screen_are_built(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..700)
        .map(|i| {
            user(
                &format!("u_{i:04}"),
                "a turn of its own, long enough that its row is a few lines tall and the \
                 whole record is far taller than the pane it is read in",
            )
        })
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 2);

    assert_eq!(
        held(&view, cx),
        700,
        "every item of the record is in the list"
    );
    assert_eq!(cx.read(|cx| view.read(cx).items(cx).len()), 700);

    // The list opens at its latest row, and builds a pane of rows: the head of the
    // record is in the list and is not a row yet.
    assert!(drawn_row(cx, "u_0699"), "the latest row is on screen");
    assert!(
        !drawn_row(cx, "u_0000"),
        "the head of the record is not built"
    );

    // How many rows a frame builds, for a record of 700.
    let built = {
        crate::counted::reset();
        cx.update(|window, cx| window.render_frame(cx));
        crate::counted::rows()
    };
    assert!(built > 0, "a frame builds the rows on screen");
    assert!(
        built < 64,
        "and only those: {built} rows built of a 700-row record"
    );

    // A second frame builds the same handful: the record behind the pane is not walked.
    crate::counted::reset();
    cx.update(|window, cx| window.render_frame(cx));
    assert!(crate::counted::rows() < 64);

    // The built rows are the pane's, and they follow the reader: taken to the head of
    // the record — a drag of the scrollbar to the top, which is the reader's own scroll
    // — the head is a row and the tail no longer is.
    view.update(cx, |view, _| {
        view.list.scroll_to(gpui_kit::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        // The gesture that brought them there is the reader's own, whichever way they
        // scrolled: a drag of the scrollbar to the head.
        view.touched();
    });
    frames(cx, 3);
    assert!(
        drawn_row(cx, "u_0000"),
        "the head of the record is on screen"
    );
    assert!(!drawn_row(cx, "u_0699"), "and the tail is not");
}

/// The transcript is embedded as a *cached* view (the tab page's own arrangement): a
/// notification somewhere else in the window — a lane's breathing dot asks for a frame
/// every frame — must not render the list again, while the transcript's own news does.
#[gpui_kit::test]
fn a_notify_elsewhere_does_not_re_render_the_cached_transcript(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| CachedHost::new(cx));
    let (view, neighbour) = cx.read(|cx| {
        let host = host.read(cx);
        (host.transcript.clone(), host.neighbour.clone())
    });
    let items: Vec<Item> = (0..300)
        .map(|i| user(&format!("u_{i:04}"), "a turn of its own"))
        .collect();
    view.update(cx, |view, cx| view.replace(items, cx));
    frames(cx, 3);
    let rendered = cx.read(|cx| view.read(cx).renders());
    assert!(rendered > 0, "the transcript rendered when it was shown");

    // The neighbour notifies on its own, frame after frame: the transcript is reused.
    // (`Window::refresh` turns caching off for a frame — it is for a window that has to
    // be laid out from scratch — so these frames are drawn rather than refreshed.)
    for _ in 0..3 {
        neighbour.update(cx, |_, cx| cx.notify());
        for _ in 0..2 {
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }
    assert_eq!(
        cx.read(|cx| view.read(cx).renders()),
        rendered,
        "a notify elsewhere in the window does not render the transcript again"
    );

    // The transcript's own news is its own: it renders again.
    view.update(cx, |_, cx| cx.notify());
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.read(|cx| view.read(cx).renders()) > rendered,
        "a notify on the transcript itself renders it"
    );
}

/// Following the tail, the list is at the newest row however many arrive. A reader who
/// has scrolled away is not moved: what arrives is appended below them, and offered by
/// the pill.
#[gpui_kit::test]
fn output_that_arrives_moves_a_follower_and_leaves_a_reader_alone(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..300)
        .map(|i| user(&format!("u_{i:04}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 3);
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(drawn_row(cx, "u_0299"), "the list opens at its latest row");

    // An item arrives while the reader is following: the tail follows it.
    view.update(cx, |view, cx| {
        view.upsert(user("u_0300", "the newest"), cx);
    });
    frames(cx, 2);
    assert!(drawn_row(cx, "u_0300"), "the newest row is on screen");
    assert_eq!(gap(&view, cx), 0., "and the list is still on its tail");

    // The reader wheels up, well past the pill's threshold.
    wheel(cx, 900.);
    assert!(!cx.read(|cx| view.read(cx).is_following_tail(cx)));
    let ids: Vec<String> = (0..301).map(|i| format!("u_{i:04}")).collect();
    let reading = top_row_in_pane(cx, &ids).expect("a row at the top of the pane");
    let before = row_bounds(cx, &reading);

    // Output arrives while they are reading: nothing on screen moves, and the pill
    // offers the way back to it.
    view.update(cx, |view, cx| {
        view.upsert(user("u_0301", "arrived while reading"), cx);
    });
    frames(cx, 3);
    assert_eq!(
        row_bounds(cx, &reading),
        before,
        "the row they were reading has not moved"
    );
    assert!(
        cx.update(|window, _| window.find("transcript-jump").visible()),
        "and the new output is offered by the pill"
    );

    // Back to the bottom: the list is at the newest row, which is the one that arrived.
    wheel(cx, -100_000.);
    assert!(drawn_row(cx, "u_0301"));
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(!cx.read(|cx| view.read(cx).is_away_from_latest(cx)));
}

/// A page of scrollback that lands while the reader is at the latest is spliced in
/// above them: the rows they are looking at do not move, and the list stays on its foot.
#[gpui_kit::test]
fn a_page_that_lands_leaves_a_reader_at_the_latest_where_they_were(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..256)
        .map(|i| user(&format!("u_{i:03}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    let asked = Rc::new(RefCell::new(0));
    let recorded = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(
            move |_, _, _| {
                *recorded.borrow_mut() += 1;
            },
            cx,
        );
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 3);
    assert_eq!(held(&view, cx), 256, "the list holds the whole record");
    assert_eq!(*asked.borrow(), 1, "and the page behind it was asked for");

    // The page lands while the reader is at the latest.
    let before = row_bounds(cx, "u_255");
    let page: Vec<Item> = (0..256)
        .map(|i| user(&format!("p_{i:03}"), "an older turn"))
        .collect();
    view.update(cx, |view, cx| {
        view.prepend(page, cx);
        view.set_history(false, false, cx);
    });
    frames(cx, 3);
    assert_eq!(held(&view, cx), 512, "the page is in the list, all of it");
    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the reader is still at the latest"
    );
    assert_eq!(gap(&view, cx), 0., "and at the very bottom of it");
    assert_eq!(
        row_bounds(cx, "u_255"),
        before,
        "the row they were at has not moved"
    );
    assert!(
        !drawn_row(cx, "p_000"),
        "and the page above them is not built under their pane"
    );
}

/// A page that lands after the reader has read on is spliced in above their place
/// without moving it: the row they were looking at stays where it was, and so does their
/// distance from the bottom.
#[gpui_kit::test]
fn a_page_that_lands_after_the_reader_read_on_leaves_their_place_alone(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..256)
        .map(|i| user(&format!("u_{i:03}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    view.update(cx, |view, cx| {
        view.on_load_older(|_, _, _| {}, cx);
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 3);

    // The reader reads on while the page is in flight.
    wheel(cx, 600.);
    assert!(!cx.read(|cx| view.read(cx).is_following_tail(cx)));
    let ids: Vec<String> = (0..256).map(|i| format!("u_{i:03}")).collect();
    let reading = top_row_in_pane(cx, &ids).expect("a row at the top of the pane");
    let anchor = row_bounds(cx, &reading);
    let away = gap(&view, cx);

    let page: Vec<Item> = (0..256)
        .map(|i| user(&format!("p_{i:03}"), "an older turn"))
        .collect();
    view.update(cx, |view, cx| {
        view.prepend(page, cx);
        view.set_history(false, false, cx);
    });
    frames(cx, 3);
    assert_eq!(held(&view, cx), 512, "the page is in the list");
    assert!(!drawn_row(cx, "p_000"), "and not opened under their pane");
    assert_eq!(
        row_bounds(cx, &reading),
        anchor,
        "the row they were reading has not moved"
    );
    let drifted = (gap(&view, cx) - away).abs();
    assert!(
        drifted < 1.,
        "nor has their distance from the bottom ({away}px -> {}px)",
        gap(&view, cx)
    );
}

/// The turn rule counts the record's turns, not the rows the list happens to have built:
/// the rule at the tail of a long record carries the number it would have had if every
/// row were drawn.
#[gpui_kit::test]
fn the_turn_rule_counts_the_records_turns_not_the_rows_on_screen(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..600)
        .map(|i| {
            if i % 2 == 0 {
                user(&format!("u_{i:03}"), "a turn of its own")
            } else {
                assistant(&format!("a_{i:03}"), "an answer", "final")
            }
        })
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 2);

    // 300 turns in the record, and the list is at its tail: the rule above the last
    // turn is numbered 300 — counted from the head of the record, through every row the
    // list has not built.
    assert!(drawn_row(cx, "a_599"), "the newest row is on screen");
    cx.update(|window, _| {
        assert!(
            window.try_find(("transcript-turn", 300usize)).is_some(),
            "the rule is numbered from the head of the record"
        );
        assert!(
            window.try_find(("transcript-turn", 1usize)).is_none(),
            "and the first turn's rule is far above the pane, not renamed"
        );
    });

    // The head of the record numbers from the same count: its own first rule is 1.
    view.update(cx, |view, _| {
        view.list.scroll_to(gpui_kit::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        // The gesture that brought them there is the reader's own, whichever way they
        // scrolled: a drag of the scrollbar to the head.
        view.touched();
    });
    frames(cx, 3);
    cx.update(|window, _| {
        assert!(
            window.try_find(("transcript-turn", 2usize)).is_some(),
            "the record opens with its own first rule"
        );
    });
}

/// Only the rows the list builds keep a parsed document — markdown is parsed for the
/// rows a reader is looking at, not for the whole record.
#[gpui_kit::test]
fn only_the_rows_the_list_builds_keep_a_document(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..600)
        .map(|i| assistant(&format!("a_{i:04}"), "# Head", "final"))
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 2);

    let documents = |cx: &gpui_kit::VisualTestContext| {
        cx.read(|cx| view.read(cx).data.read(cx).documents.len())
    };
    let built = documents(cx);
    assert!(
        built > 0 && built < 64,
        "one per built row, and no more: {built} of 600"
    );
    assert!(drawn_row(cx, "a_0599"), "the newest row is on screen");
    assert!(cx.read(|cx| view.read(cx).data.read(cx).documents.contains_key("a_0599")));
    assert!(
        !cx.read(|cx| view.read(cx).data.read(cx).documents.contains_key("a_0000")),
        "the head of the record has no document until it is read"
    );

    // Taken to the head, its rows are built and parsed.
    view.update(cx, |view, _| {
        view.list.scroll_to(gpui_kit::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        // The gesture that brought them there is the reader's own, whichever way they
        // scrolled: a drag of the scrollbar to the head.
        view.touched();
    });
    frames(cx, 3);
    assert!(
        cx.read(|cx| view.read(cx).data.read(cx).documents.contains_key("a_0000")),
        "the head is parsed once the reader is looking at it"
    );

    // A record that is replaced outright leaves the last one's documents behind.
    view.update(cx, |view, cx| {
        view.replace(vec![assistant("a_9999", "# Fresh", "final")], cx)
    });
    frames(cx, 2);
    assert_eq!(documents(cx), 1);
}

/// A page that settles without bringing a row back — an empty page, the last page, or
/// one that failed — ends the walk: the same question is not asked again, frame after
/// frame, and the next page that turns up later is not taken for an answer to it.
#[gpui_kit::test]
fn a_page_that_settles_with_nothing_ends_the_walk(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..256)
        .map(|i| user(&format!("u_{i:03}"), "a turn of its own"))
        .collect();
    let (view, cx) = open!(cx, items);
    let asked = Rc::new(RefCell::new(0));
    let counted = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(move |_, _, _| *counted.borrow_mut() += 1, cx);
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 3);
    assert_eq!(
        *asked.borrow(),
        1,
        "the page behind the record is asked for"
    );

    // It settles — a page that overlaps what is held, or the last page of all — and the
    // record did not grow.
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 4);
    assert_eq!(
        *asked.borrow(),
        1,
        "a page that brought nothing back is not asked for again"
    );

    // A snapshot that says there is scrollback where there was none starts the walk
    // over.
    view.update(cx, |view, cx| view.set_history(false, false, cx));
    frames(cx, 2);
    assert_eq!(*asked.borrow(), 1);
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);
    assert_eq!(
        *asked.borrow(),
        2,
        "a topic with scrollback again is walked again"
    );

    // And nothing is opened under the reader by a page that lands: they stay at the
    // tail they were at.
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(drawn_row(cx, "u_255"), "the reader is where they were");
}

/// A row leaving the record while the reader is reading is the *list's* own layout
/// shrinking under them: it must not be taken for coming back to the latest, and what
/// arrived while they read must stay below the pane.
#[gpui_kit::test]
fn a_row_leaving_in_front_of_the_reader_does_not_put_them_back_on_the_tail(
    cx: &mut TestAppContext,
) {
    let items: Vec<Item> = (0..300)
        .map(|i| {
            user(
                &format!("u_{i:04}"),
                "a turn of its own, long enough that its row is a few lines tall",
            )
        })
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 3);
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));

    // The reader goes up, off the tail, and the agent answers while they read: the
    // answers are appended below the pane they are looking at.
    wheel(cx, 400.);
    for i in 300..320 {
        view.update(cx, |view, cx| {
            assert!(
                view.upsert(user(&format!("u_{i:04}"), "an answer"), cx),
                "the agent answers"
            );
        });
    }
    frames(cx, 3);
    assert!(
        !drawn_row(cx, "u_0319"),
        "the answers arrived below the pane the reader is looking at"
    );

    // What the reader is reading: the row at the foot of the pane, and the height of a
    // row above it, so that one leaving is a real movement under them.
    let ids: Vec<String> = (0..320).map(|i| format!("u_{i:04}")).collect();
    let reading = ids
        .iter()
        .rev()
        .find(|id| {
            drawn_row(cx, id)
                && row_bounds(cx, id).bottom() <= pane(cx).bottom()
                && row_bounds(cx, id).bottom() > pane(cx).bottom() - px(120.)
        })
        .cloned()
        .expect("a row near the foot of the pane");
    let before = row_bounds(cx, &reading);
    let pitch = {
        let before_it: Vec<String> = ids.clone();
        let above = before_it
            .iter()
            .take_while(|id| **id != reading)
            .last()
            .cloned()
            .expect("a row above");
        f32::from(before.top()) - f32::from(row_bounds(cx, &above).top())
    };
    assert!(pitch > 8., "a row is tall enough for one leaving to matter");

    // A row in front of them — the one at the top of the pane, where the list has
    // measured it — goes away.
    view.update(cx, |view, cx| {
        assert!(view.remove("u_0150", cx), "the row leaves the record");
    });
    frames(cx, 3);

    assert!(
        !drawn_row(cx, "u_0319"),
        "the answers that arrived are still below the pane"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "a row leaving is not the reader coming back to the latest"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "and the way back to those answers is still offered"
    );
    // They are still where they were, looking at the row they were reading: the list
    // splices the hole out rather than rebuilding around it.
    let after = row_bounds(cx, &reading);
    let drift = f32::from(after.top()) - f32::from(before.top());
    assert!(
        drift.abs() <= pitch + 1.,
        "the reader's place did not run away: {drift}px, of a {pitch}px row"
    );
    assert!(
        after.bottom() <= pane(cx).bottom(),
        "and the row they were reading is still on screen: {after:?}"
    );
}

/// A row that goes away far above the pane is one the reader cannot see: the rows they
/// are looking at do not move a pixel.
#[gpui_kit::test]
fn a_row_leaving_above_the_pane_does_not_move_the_reader(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..300)
        .map(|i| user(&format!("u_{i:04}"), "a turn of its own, a few lines tall"))
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 3);
    wheel(cx, 600.);

    let ids: Vec<String> = (0..300).map(|i| format!("u_{i:04}")).collect();
    let reading = top_row_in_pane(cx, &ids).expect("a row at the top of the pane");
    let before = row_bounds(cx, &reading);
    view.update(cx, |view, cx| {
        assert!(view.remove("u_0010", cx), "a row far above leaves");
    });
    frames(cx, 3);

    assert_eq!(held(&view, cx), 299, "the record is one row shorter");
    assert_eq!(
        row_bounds(cx, &reading),
        before,
        "and the row the reader is looking at has not moved"
    );
}

/// The thinking a reader can reveal is counted as the record changes, so the control in
/// the header costs no walk of the record — and a row far above the pane still counts,
/// since what the control reveals is the transcript, not the rows on screen.
#[gpui_kit::test]
fn thinking_behind_the_window_still_counts(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..600)
        .map(|i| {
            if i == 100 {
                assistant_with_thinking("u_0100", "an answer", "a thought")
            } else {
                user(&format!("u_{i:04}"), "a turn of its own")
            }
        })
        .collect();
    let (view, cx) = open!(cx, items);
    frames(cx, 2);

    assert!(
        !drawn_row(cx, "u_0100"),
        "the thinking is far above the pane the list has built"
    );
    assert!(
        cx.read(|cx| view.read(cx).has_thinking(cx)),
        "a row behind the window still offers the control"
    );

    // The answer is replaced by one that carried no thinking: nothing left to reveal.
    view.update(cx, |view, cx| {
        view.upsert(assistant("u_0100", "an answer", "final"), cx);
    });
    assert!(
        !cx.read(|cx| view.read(cx).has_thinking(cx)),
        "and the control goes with the thinking"
    );

    // A new answer that carries some, far behind the window again.
    view.update(cx, |view, cx| {
        view.upsert(
            assistant_with_thinking("u_0100", "an answer", "another thought"),
            cx,
        );
    });
    assert!(cx.read(|cx| view.read(cx).has_thinking(cx)));

    // A snapshot that carries none leaves nothing behind.
    view.update(cx, |view, cx| {
        view.replace(vec![user("u_9999", "a fresh session")], cx)
    });
    assert!(!cx.read(|cx| view.read(cx).has_thinking(cx)));
}

// --- links (`linkify`, `link`) -------------------------------------------------------

/// A real folder with a real file in it, for the rows whose paths have to be there to be
/// pressed. It is thrown away when the test that made it ends.
///
/// The name has to be the test's own: two tests run at once, and `Paths` remembers what
/// the disk said for three seconds.
struct Disk {
    root: PathBuf,
}

impl Disk {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "evo-transcript-links-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("notes")).expect("a folder to point at");
        std::fs::write(root.join("notes/report.md"), "the report").expect("a file to point at");
        Disk { root }
    }

    /// The folder's path, as a row's words would carry it.
    fn folder(&self) -> String {
        self.root.join("notes").display().to_string()
    }

    /// The file's path, as a row's words would carry it.
    fn file(&self) -> String {
        self.root.join("notes/report.md").display().to_string()
    }

    /// A path under the folder that is not there.
    fn gone(&self) -> String {
        self.root.join("gone.md").display().to_string()
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What the reader pressed, without opening anything: the app hands every link to the
/// handler its owner put on the view, so a test can hold them.
fn presses(view: &Entity<TranscriptView>, cx: &mut TestAppContext) -> Arc<Mutex<Vec<String>>> {
    let pressed = Arc::new(Mutex::new(Vec::new()));
    let held = pressed.clone();
    view.update(cx, |view, cx| {
        view.on_open_link(
            move |href, _window, _cx| held.lock().unwrap().push(href.to_string()),
            cx,
        )
    });
    pressed
}

/// Press the words of a row, at their first glyph: a link that starts a line is under the
/// pointer there, and nothing else in the line is a link.
fn press(window: &mut Window, id: gpui_kit::ElementId, cx: &mut App) {
    window.click_at(id, gpui_kit::point(px(4.), px(8.)), cx);
}

/// A row's own words are where a reader's links are: the address of a page, a file that
/// is there, and a folder that is there all open where they point. What is opened is the
/// absolute path the words named, not the words themselves.
#[gpui_kit::test]
fn a_link_in_a_rows_own_words_opens_where_it_points(cx: &mut TestAppContext) {
    let disk = Disk::new("opens");
    let (view, cx) = open!(
        cx,
        vec![
            user("u_1", "https://evo.dev/state.json is the shape"),
            user("u_2", &format!("{} is where it went", disk.file())),
            user("u_3", &format!("{} holds it", disk.folder())),
        ]
    );
    let pressed = presses(&view, cx);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| {
        press(window, row_id("transcript-user-text", "u_1"), cx);
        press(window, row_id("transcript-user-text", "u_2"), cx);
        press(window, row_id("transcript-user-text", "u_3"), cx);
    });
    assert_eq!(
        *pressed.lock().unwrap(),
        [
            "https://evo.dev/state.json".to_string(),
            disk.file(),
            disk.folder(),
        ],
        "an address opens as it was written, and a path opens as the file it names"
    );
}

/// An answer's prose is links too — a bare path on its own line, and a path written as
/// code — and a report's field is the same kind of prose.
#[gpui_kit::test]
fn a_link_in_prose_and_in_a_report_field_opens_where_it_points(cx: &mut TestAppContext) {
    let disk = Disk::new("prose");
    let (view, cx) = open!(
        cx,
        vec![
            assistant("a_1", &disk.file(), "final"),
            item(json!({
                "id": "r_1", "ts": 2, "kind": "lane_report", "lane": 3,
                "done": format!("`{}`", disk.file()), "evidence": "none",
                "next": "", "blocked": "", "requests": ""
            })),
        ]
    );
    let pressed = presses(&view, cx);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| {
        // The message is the path: its own first glyph is inside the link.
        press(window, row_id("transcript-message", "a_1"), cx);
    });
    cx.update(|window, cx| {
        // The field's words are in its value column; the label beside it is not prose.
        let value: gpui_kit::ElementId = (row_id("transcript-report-done", "r_1"), "value").into();
        press(window, value, cx);
    });
    assert_eq!(
        *pressed.lock().unwrap(),
        [disk.file(), disk.file()],
        "the message and the field both open the file they name"
    );
}

/// A path that is not there is not a link, and neither is anything that is not a path or
/// an address: pressing what is left of the row opens nothing, and the words are still
/// exactly what was said.
#[gpui_kit::test]
fn words_that_name_nothing_to_open_stay_words(cx: &mut TestAppContext) {
    let disk = Disk::new("nothing");
    let text = format!("{} was never written", disk.gone());
    let (view, cx) = open!(cx, vec![user("u_1", &text)]);
    let pressed = presses(&view, cx);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    // The words are drawn exactly as they came: what a path that is not there gets is
    // its own characters back, with nothing under the pointer.
    cx.read(|cx| {
        let drawn = view
            .read(cx)
            .data
            .read(cx)
            .plain_document(&"u_1".to_string(), crate::rows::USER_TEXT)
            .expect("the row's words are read from a document")
            .read(cx)
            .rendered_text();
        assert_eq!(
            drawn.as_str().trim_end(),
            text,
            "a path that is not there is drawn as it was written"
        );
    });
    cx.update(|window, cx| press(window, row_id("transcript-user-text", "u_1"), cx));
    assert!(
        pressed.lock().unwrap().is_empty(),
        "a path that is not there opens nothing"
    );
}

/// A tool call's arguments and its result are data, not prose: the addresses and paths in
/// them are drawn exactly as they came, and pressing them opens nothing.
#[gpui_kit::test]
fn a_tool_calls_arguments_and_result_are_not_links(cx: &mut TestAppContext) {
    let disk = Disk::new("tool");
    let url = "https://evo.dev/state.json";
    let spec = json!({
        "id": "t_1", "ts": 1, "kind": "tool", "call_id": "c1", "name": "bash",
        "args": { "url": url },
        "status": "ok",
        "result": { "text": format!("{} was written", disk.file()), "chars": 40, "truncated": false },
    });
    // Nothing of a call is read as prose, so there is nothing in it to press.
    assert!(
        crate::rows::plain_texts(&item(spec.clone()), true).is_empty(),
        "a call's words are data, not prose"
    );
    let (view, cx) = open!(cx, vec![item(spec)]);
    let pressed = presses(&view, cx);
    cx.update(|window, cx| window.render_frame(cx));
    // The call's body is behind the head, and is where a path in a call would show.
    cx.update(|window, cx| window.click(row_id("transcript-tool", "t_1"), cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| {
        let args = row_id("transcript-tool-arguments", "t_1");
        let first: gpui_kit::ElementId = (args, "0").into();
        // Both lines begin with what would be a link, were a call prose.
        let command: gpui_kit::ElementId = (first, "value").into();
        let result: gpui_kit::ElementId = (row_id("transcript-tool-result", "t_1"), "text").into();
        // Both are drawn — the call's body is open, and the words are its own.
        assert!(window.try_find(command.clone()).is_some());
        assert!(window.try_find(result.clone()).is_some());
        press(window, command, cx);
        press(window, result, cx);
    });
    assert!(
        pressed.lock().unwrap().is_empty(),
        "a call's arguments and its result open nothing"
    );
    cx.read(|cx| {
        assert!(
            !view
                .read(cx)
                .data
                .read(cx)
                .field_documents
                .keys()
                .any(|(id, _)| id == "t_1"),
            "a call's words are read as text, never as prose with links in it"
        )
    });
}

/// What a message's own text is made of: the words the reader wrote, and the files
/// `session` names under them. The reading is exact — a heading, then absolute paths,
/// and nothing else — because everything else is the reader's own prose.
#[test]
fn the_files_a_message_names_are_read_off_the_end_of_it() {
    let block = |text: &str| {
        session::attachment_turn(text, &[session::Attached::File("/tmp/one.csv".into())]).0
    };
    let read = |text: &str| {
        crate::rows::sent(text).map(|sent| {
            (
                sent.words.to_string(),
                sent.files
                    .iter()
                    .map(|file| file.to_string())
                    .collect::<Vec<_>>(),
            )
        })
    };

    // What session writes, the row reads back.
    assert_eq!(
        read(&block("summarise these")),
        Some((
            "summarise these".to_string(),
            vec!["/tmp/one.csv".to_string()]
        ))
    );
    // A message that is only the files it carries has no words.
    assert_eq!(
        read(&block("   \n")),
        Some(("".to_string(), vec!["/tmp/one.csv".to_string()]))
    );
    // One path per line, spaces and all, and a trailing blank line is not a line.
    assert_eq!(
        read("here\n\nAttached files:\n- /tmp/a b.pdf\n- /opt/x.tar.gz\n"),
        Some((
            "here".to_string(),
            vec!["/tmp/a b.pdf".to_string(), "/opt/x.tar.gz".to_string()]
        ))
    );

    // Nothing else is a block.
    assert_eq!(read("here are the words alone"), None);
    assert_eq!(read("no files were attached"), None);
    // A heading inside the prose is the reader's own line, and so is a list that runs
    // on past it.
    assert_eq!(
        read("Attached files:\n- /tmp/one.csv\nand that is that"),
        None
    );
    assert_eq!(
        read("the words\n\nAttached files:\n- /tmp/one.csv\nthe words carry on"),
        None
    );
    assert_eq!(read("the words\nAttached files:\n- /tmp/one.csv"), None);
    // A path that is not absolute is prose, and so is a line that is not a path.
    assert_eq!(read("the words\n\nAttached files:\n- notes.txt"), None);
    assert_eq!(read("the words\n\nAttached files:\n- ~/notes.txt"), None);
    assert_eq!(read("the words\n\nAttached files:\nnothing at all"), None);
    // A heading with no file under it names nothing.
    assert_eq!(read("the words\n\nAttached files:"), None);
    // And a heading that is not exactly the one session writes.
    assert_eq!(read("the words\n\nAttached file:\n- /tmp/one.csv"), None);
    assert_eq!(
        read("the words\n\nAttached files: and more\n- /tmp/one.csv"),
        None
    );
}

/// The files a message carries are chips under its words: the file's name, the whole
/// path a hover away, and a press that opens the file itself — the same opening a link
/// in the words gets. A file that is gone is drawn, dimmed, and presses nothing.
#[gpui_kit::test]
fn a_messages_files_are_chips_under_its_words(cx: &mut TestAppContext) {
    let disk = Disk::new("files");
    // A message names the file where the file system says it is, not where the draft
    // that picked it said: `session` writes the canonical path (`attachments::absolute`).
    let real = std::fs::canonicalize(disk.file())
        .expect("the file's own path")
        .display()
        .to_string();
    let text = session::attachment_turn(
        "summarise these",
        &[
            session::Attached::File(disk.file().into()),
            session::Attached::File(disk.gone().into()),
        ],
    )
    .0;
    let (view, cx) = open!(cx, vec![user("u_1", &text)]);
    let pressed = presses(&view, cx);
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.read(|cx| {
        let data = view.read(cx).data.read(cx);
        let drawn = |key| {
            data.plain_document(&"u_1".to_string(), key)
                .expect("the row's text is read from a document")
                .read(cx)
                .rendered_text()
        };
        assert_eq!(
            drawn(crate::rows::USER_WORDS).as_str().trim_end(),
            "summarise these",
            "the row draws the reader's words, without the line meant for the agent"
        );
        assert_eq!(
            drawn(crate::rows::USER_TEXT).as_str().trim_end(),
            text.trim_end(),
            "and what a copy of the row takes is the message as it was written"
        );
    });
    cx.update(|window, cx| {
        let there = row_id("transcript-file-0", "u_1");
        let gone = row_id("transcript-file-1", "u_1");
        let chip = window.find(there.clone());
        assert!(chip.visible(), "the file that is there has a chip");
        assert!(
            chip.bounds().size.width > px(0.),
            "and the chip is drawn, not a hole"
        );
        assert_eq!(chip.label(), Some(real.as_str()), "the path is its own");
        assert!(
            window.find(gone.clone()).visible(),
            "and so does the file that is gone"
        );
        window.click_at(there, gpui_kit::point(px(4.), px(10.)), cx);
        window.click_at(gone, gpui_kit::point(px(4.), px(10.)), cx);
    });
    assert_eq!(
        *pressed.lock().unwrap(),
        [real],
        "the chip opens the file it names, and a file that is gone opens nothing"
    );
}

/// The app embeds a transcript as a *cached* view (§7.3), so a folder that arrives has
/// to be the transcript's own news: the view renders again and reads its rows against the
/// new folder. (That they are links then is
/// `a_relative_path_is_measured_from_the_tabs_folder`'s business — the elements inside a
/// cached subtree are not observable in this harness, so this test holds the frame.)
#[gpui_kit::test]
fn a_folder_that_arrives_renders_the_cached_transcript_again(cx: &mut TestAppContext) {
    let disk = Disk::new("cached");
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| CachedHost::new(cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace(vec![user("u_1", "notes/report.md is where it went")], cx)
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let rendered = cx.read(|cx| view.read(cx).renders());
    assert!(rendered > 0, "the cached view rendered when it was shown");

    view.update(cx, |view, cx| view.set_folder(Some(disk.root.clone()), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        cx.read(|cx| view.read(cx).renders()) > rendered,
        "a folder that arrives is the transcript's own news, so the cached view is drawn again"
    );

    // The same folder a second time is not news: a tab that reports its state over and
    // over does not re-read the rows.
    let settled = cx.read(|cx| view.read(cx).renders());
    view.update(cx, |view, cx| view.set_folder(Some(disk.root.clone()), cx));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        cx.read(|cx| view.read(cx).renders()),
        settled,
        "the same folder again changes nothing"
    );
}

/// A relative path is measured from the folder the tab works in, and a tab that moves
/// reads its rows again: the same words name a file in one folder and nothing in none.
#[gpui_kit::test]
fn a_relative_path_is_measured_from_the_tabs_folder(cx: &mut TestAppContext) {
    let disk = Disk::new("folder");
    let (view, cx) = open!(cx, vec![user("u_1", "notes/report.md is where it went")]);
    let pressed = presses(&view, cx);
    view.update(cx, |view, cx| view.set_folder(Some(disk.root.clone()), cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| press(window, row_id("transcript-user-text", "u_1"), cx));
    assert_eq!(*pressed.lock().unwrap(), [disk.file()]);

    // The tab moves to a folder where the same words name nothing.
    view.update(cx, |view, cx| view.set_folder(None, cx));
    for _ in 0..2 {
        cx.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|window, cx| press(window, row_id("transcript-user-text", "u_1"), cx));
    assert_eq!(
        *pressed.lock().unwrap(),
        [disk.file()],
        "a relative path with no folder to measure it from opens nothing"
    );
}

// --- the reading measure and the reader's zoom (§7.2) ---------------------------------

/// The transcript in a pane of its own width: a headless test's window is maximised, and a
/// fixed box is how a narrow pane — or one narrower than the reader's measure — is arranged.
struct PaneHost {
    transcript: Entity<TranscriptView>,
    width: f32,
}

impl PaneHost {
    fn new(width: f32, cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
            width,
        }
    }
}

impl Render for PaneHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().size_full().child(
            div()
                .id("pane-host")
                .test_support()
                .w(px(self.width))
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().child(self.transcript.clone())),
        )
    }
}

/// Open a transcript in a pane `width` pixels wide.
fn open_in(
    cx: &mut TestAppContext,
    width: f32,
    items: Vec<Item>,
) -> (Entity<TranscriptView>, &mut gpui_kit::VisualTestContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| PaneHost::new(width, cx));
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| view.replace(items, cx));
    frames(cx, 2);
    (view, cx)
}

/// The reader's zoom, for the whole app: one global, so every transcript on screen redraws
/// at the new size.
fn set_zoom(cx: &mut gpui_kit::VisualTestContext, scale: f32) {
    cx.update(|_, cx| crate::TranscriptZoom(scale).set(cx));
    frames(cx, 2);
}

/// The reading column's own box: the padded one the list centres, whose id says which slot
/// of the record it belongs to.
fn column_width(cx: &mut gpui_kit::VisualTestContext) -> gpui_kit::Pixels {
    cx.update(|window, _| {
        window
            .find(("transcript-column", 0usize))
            .bounds()
            .size
            .width
    })
}

/// The whole pane the transcript is drawn in.
fn pane_width(cx: &mut gpui_kit::VisualTestContext) -> gpui_kit::Pixels {
    pane(cx).size.width
}

/// A row's box, by the id its wrapper carries, in the window the reader is looking at.
fn row_box(
    cx: &mut gpui_kit::VisualTestContext,
    id: gpui_kit::ElementId,
) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    cx.update(|window, _| window.find(id).bounds())
}

/// One row's measure box: the width a line of it may use.
fn measure_width(cx: &mut gpui_kit::VisualTestContext, id: &str) -> gpui_kit::Pixels {
    row_box(cx, row_id("transcript-measure", id)).size.width
}

/// The reading measure follows the reader's zoom: the design's 800px column at 100%, and
/// half again at 150%, so a line holds the same number of characters at every zoom. It is a
/// **maximum** — a pane narrower than the measure fills the pane, as it always has — and
/// the page's own inset stays the design's 16px inside it, so a line gets the column less
/// 32.
#[gpui_kit::test]
fn the_reading_measure_follows_the_zoom(cx: &mut TestAppContext) {
    let (_view, cx) = open_in(cx, 1600., vec![user("u_1", "a turn of its own")]);
    assert_eq!(pane_width(cx), px(1600.), "the fixture pane");
    for (scale, column) in [(1.0, 800.), (1.5, 1200.), (0.75, 600.), (2.0, 1600.)] {
        set_zoom(cx, scale);
        assert_eq!(column_width(cx), px(column), "the column at {scale}×");
        assert_eq!(
            measure_width(cx, "u_1"),
            px(column - 32.),
            "and what a line of it may use, at {scale}×"
        );
    }

    // The design's own size is exactly what it was before the measure scaled, and the
    // column is centred in its pane rather than stretched by it.
    set_zoom(cx, 1.0);
    assert_eq!(column_width(cx), px(800.));
    assert_eq!(measure_width(cx, "u_1"), px(768.));
}

/// A pane narrower than the reader's measure is the pane: 150% of 800px is 1200px and a
/// 1000px window has 1000 of them, so the column is the pane and a line is the pane less
/// its insets.
#[gpui_kit::test]
fn a_pane_narrower_than_the_measure_is_the_pane(cx: &mut TestAppContext) {
    let (_view, cx) = open_in(cx, 1000., vec![user("u_1", "a turn of its own")]);
    assert_eq!(pane_width(cx), px(1000.), "the fixture pane");
    for (scale, column) in [(1.0, 800.), (1.5, 1000.), (2.0, 1000.), (0.75, 600.)] {
        set_zoom(cx, scale);
        assert_eq!(column_width(cx), px(column), "the column at {scale}×");
        assert_eq!(
            measure_width(cx, "u_1"),
            px(column - 32.),
            "and what a line of it may use, at {scale}×"
        );
    }
}

/// What the scaled measure is *for*: the same paragraph is the same number of lines at 150%
/// as at 100%, in a pane wide enough to be the measure's own. Unscaled, the line would hold
/// two thirds of the words at 150% and the paragraph would be half again as many lines.
#[gpui_kit::test]
fn a_line_holds_the_same_words_at_every_zoom(cx: &mut TestAppContext) {
    let sentence = "The measure is what a reader's line is: the design gives it to a window, \
                    and the reader's zoom gives it a size. ";
    let paragraph = sentence.repeat(8);
    let (_view, cx) = open_in(cx, 1600., vec![user("u_1", &paragraph)]);

    // The lines a plain row's own text is drawn in: its box over the line box it draws on,
    // which is the design's 21px at the reader's zoom.
    let lines = |cx: &mut gpui_kit::VisualTestContext, scale: f32| -> f32 {
        set_zoom(cx, scale);
        let text = row_box(cx, row_id("transcript-user-text", "u_1"));
        let line =
            cx.update(|_, cx| f32::from(crate::style::Palette::from_app(cx).scaled(USER_LINE)));
        f32::from(text.size.height) / line
    };

    let at_100 = lines(cx, 1.0);
    assert!(
        at_100 > 4.,
        "the fixture is a paragraph, not a line: {at_100}"
    );
    let at_150 = lines(cx, 1.5);
    assert!(
        (at_100 - at_150).abs() <= 1.,
        "{at_100} lines at 100%, {at_150} at 150%: the column did not follow the type"
    );
    let at_75 = lines(cx, 0.75);
    assert!(
        (at_100 - at_75).abs() <= 1.,
        "{at_100} lines at 100%, {at_75} at 75%"
    );
}

/// A zoom is a change of measure, not of place (§7.2): the rows are re-measured, and the
/// reader is where they were — at the tail one stays at the tail, and one reading back keeps
/// the row they were on.
#[gpui_kit::test]
fn a_zoom_keeps_the_readers_place(cx: &mut TestAppContext) {
    let items: Vec<Item> = (0..300)
        .map(|i| {
            user(
                &format!("u_{i:04}"),
                "a turn of its own, long enough that its row is a few lines tall in the pane",
            )
        })
        .collect();
    let ids: Vec<String> = (0..300).map(|i| format!("u_{i:04}")).collect();
    let (view, cx) = open!(cx, items);

    // Following the tail: every zoom still shows the newest row, and the pin holds.
    for scale in [1.5, 0.75, 2.0, 1.0] {
        set_zoom(cx, scale);
        assert!(drawn_row(cx, "u_0299"), "the newest row at {scale}×");
        assert!(
            cx.read(|cx| view.read(cx).is_following_tail(cx)),
            "a follower is still a follower at {scale}×"
        );
    }

    // Reading back: the row at the top of the pane is the one the reader was on.
    wheel(cx, 600.);
    wheel(cx, 600.);
    let before = top_row_in_pane(cx, &ids).expect("a row at the top of the pane");
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "the reader is reading back"
    );
    set_zoom(cx, 1.5);
    let after = top_row_in_pane(cx, &ids).expect("a row at the top of the pane");
    assert_eq!(
        before, after,
        "the reader keeps the row they were on when the measure changes"
    );
    assert!(cx.read(|cx| view.read(cx).is_away_from_latest(cx)));

    // And a second zoom leaves them on it again.
    set_zoom(cx, 0.75);
    assert_eq!(top_row_in_pane(cx, &ids).as_deref(), Some(after.as_str()));
}

// --- the record under a storm of the ops a live session makes --------------------------

/// The storm's own randomness: a deterministic walk, so a failing interleaving is the same
/// one on every run — the ops are what matters here, not luck.
struct Storm(u64);

impl Storm {
    fn step(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.step().is_multiple_of(n)
    }
}

/// The record as the storm believes it is: the view's own copy is held to it.
#[derive(Default)]
struct Record {
    items: Vec<Item>,
}

impl Record {
    fn ids(&self) -> Vec<String> {
        self.items.iter().map(|item| item.id.clone()).collect()
    }

    /// The view's `upsert`: by id, in place, or at the end when it is new.
    fn add(&mut self, item: Item) {
        match self.items.iter().position(|held| held.id == item.id) {
            Some(index) => self.items[index] = item,
            None => self.items.push(item),
        }
    }

    /// The view's `prepend`: older items in front of what is held, each held once.
    fn prepend(&mut self, older: Vec<Item>) {
        let mut fresh: Vec<Item> = older
            .into_iter()
            .filter(|item| !self.items.iter().any(|held| held.id == item.id))
            .collect();
        if fresh.is_empty() {
            return;
        }
        fresh.append(&mut self.items);
        self.items = fresh;
    }

    fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != before
    }
}

/// The list's own shape, the slots it was last told to hold, and the record the view
/// holds: three numbers that have to be one row's worth of each other.
fn shape(
    view: &Entity<TranscriptView>,
    cx: &gpui_kit::VisualTestContext,
) -> (usize, crate::Slots, usize) {
    cx.read(|cx| {
        let view = view.read(cx);
        (
            view.list.item_count(),
            view.slots,
            view.data.read(cx).items.len(),
        )
    })
}

/// Everything a live session's ops have to leave true, after the frames they caused.
fn in_step(
    view: &Entity<TranscriptView>,
    record: &Record,
    when: &str,
    cx: &mut gpui_kit::VisualTestContext,
) {
    let (held, slots, rows) = shape(view, cx);
    let ids: Vec<String> = cx.read(|cx| {
        view.read(cx)
            .data
            .read(cx)
            .items
            .iter()
            .map(|i| i.id.clone())
            .collect()
    });
    assert_eq!(
        ids,
        record.ids(),
        "{when}: the view holds the ops' record, in order"
    );
    let (slots_now, pinned, newest) = cx.read(|cx| {
        let view = view.read(cx);
        (
            view.slots_now(cx),
            view.is_following_tail(cx),
            view.data.read(cx).items.last().map(|item| item.id.clone()),
        )
    });
    assert_eq!(
        slots, slots_now,
        "{when}: the list was told what the record is now"
    );
    assert_eq!(
        slots.len(),
        held,
        "{when}: the list holds what it was told ({held} in the list, {slots:?} in the view)"
    );
    assert_eq!(
        rows,
        record.items.len(),
        "{when}: the record is the ops' own"
    );
    if !pinned {
        return;
    }
    let Some(newest) = newest else { return };
    let built = cx.update(|window, _| {
        window
            .try_find(row_id("transcript-measure", &newest))
            .is_some()
    });
    assert!(
        built,
        "{when}: following the tail, so the newest row ({newest}) is built"
    );
    let bottom = row_box(cx, row_id("transcript-measure", &newest)).bottom();
    let pane = pane(cx).bottom();
    // The design's own slack: within 24px of the foot counts as being at the latest.
    assert!(
        bottom <= pane + px(crate::pin::SLACK),
        "{when}: following the tail, so the newest row is not below the fold ({bottom:?} against {pane:?})"
    );
}

fn turn(id: &str) -> Item {
    user(
        id,
        "a turn of its own, long enough that its row is a few lines tall and the whole \
         list is taller than the pane it is read in",
    )
}

/// A long session, driven the way a live one is: snapshots and `topic.reset`s, items
/// streaming in and being rewritten, older pages spliced in above the reader, rows leaving
/// the record, a queued turn being sent, a run ending in an abort — with the reader at the
/// tail and, now and then, nudging the wheel the way a reader does.
///
/// Whatever the interleaving, the list holds the record, and a reader who is following the
/// tail has the newest row on screen: a transcript that stops a row short of the record is
/// a reader looking at a conversation that ended, while the session goes on.
#[gpui_kit::test]
fn the_list_is_the_record_under_a_storm_of_live_ops(cx: &mut TestAppContext) {
    let mut storm = Storm(0x5eed_0001);
    let opening: Vec<Item> = (0..30).map(|i| turn(&format!("e_{i:04}"))).collect();
    let (view, cx) = open_in(cx, 1000., opening.clone());
    frames(cx, 3);
    let mut record = Record { items: opening };
    let mut fresh = 30usize;
    let mut waiting: Option<String> = None;

    // A reader who is following the tail, past the first pane of the record.
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(gap(&view, cx) <= 0.5, "the fixture opens at its tail");

    for round in 0..240 {
        let when = format!("round {round}");
        match storm.pick(12) {
            // A new item arrives: the run's next assistant message, a tool row, a notice,
            // a compaction, a goal.
            0..=2 => {
                fresh += 1;
                let item = match storm.pick(5) {
                    0 => turn(&format!("e_{fresh:04}")),
                    1 => assistant(
                        &format!("e_{fresh:04}"),
                        "writing it out as it goes",
                        "streaming",
                    ),
                    2 => tool(
                        &format!("e_{fresh:04}"),
                        "call_1",
                        "bash",
                        "the output of it",
                        false,
                    ),
                    3 => notice(&format!("e_{fresh:04}"), "info", "session ready"),
                    _ => compaction(&format!("e_{fresh:04}")),
                };
                view.update(cx, |view, cx| {
                    view.upsert(item.clone(), cx);
                });
                record.add(item);
            }
            // A message already in the record is rewritten: text arriving token by token,
            // a status flip, a tool row gaining its result.
            3 | 4 => {
                let index = storm.pick(record.items.len().max(1));
                let id = record.items[index].id.clone();
                let item = assistant(&id, "the rest of the answer, as it streams in", "streaming");
                view.update(cx, |view, cx| {
                    view.upsert(item.clone(), cx);
                });
                record.add(item);
            }
            // The turn the reader typed while the run was going: it is queued, then sent.
            5 => {
                fresh += 1;
                let id = format!("q_{fresh:04}");
                view.update(cx, |view, cx| {
                    view.upsert(queued(&id, "sent while the run was going"), cx);
                });
                record.add(queued(&id, "sent while the run was going"));
                waiting = Some(id);
            }
            6 => {
                if let Some(id) = waiting.take() {
                    let item = user(&id, "sent while the run was going");
                    view.update(cx, |view, cx| {
                        view.upsert(item.clone(), cx);
                    });
                    record.add(item);
                }
            }
            // A row leaves the record.
            7 => {
                if record.items.len() > 4 {
                    let index = storm.pick(record.items.len());
                    let id = record.items[index].id.clone();
                    view.update(cx, |view, cx| {
                        view.remove(&id, cx);
                    });
                    record.remove(&id);
                }
            }
            // An older page lands above the reader, as the scrollback walks back.
            8 => {
                let oldest = record.items.len();
                let older: Vec<Item> = (oldest..oldest + 10)
                    .map(|i| turn(&format!("old_{i:04}")))
                    .collect();
                view.update(cx, |view, cx| {
                    view.set_history(true, true, cx);
                    view.prepend(older.clone(), cx);
                    view.set_history(true, false, cx);
                });
                record.prepend(older);
            }
            // The topic says what it always says about the history behind the record.
            9 => {
                let (has_older, loading) = (storm.one_in(2), storm.one_in(2));
                view.update(cx, |view, cx| view.set_history(has_older, loading, cx));
            }
            // A `topic.reset`, or a fresh snapshot: the record is replaced outright.
            10 => {
                if storm.one_in(8) {
                    let items: Vec<Item> = (0..20)
                        .map(|i| turn(&format!("r_{fresh}_{i:03}")))
                        .collect();
                    view.update(cx, |view, cx| view.replace(items.clone(), cx));
                    record = Record { items };
                    waiting = None;
                } else {
                    view.update(cx, |view, cx| view.set_running(storm.one_in(2), cx));
                }
            }
            // The reader nudges the wheel: still at the tail, and the window in which a
            // scroll counts as theirs is open.
            _ => {
                wheel(cx, if storm.one_in(2) { 6. } else { -6. });
            }
        }
        frames(cx, 1 + storm.pick(3));
        in_step(&view, &record, &when, cx);
    }
}

// --- what the view declined, and why it is counted ------------------------------------

/// An ephemeral notice the *serve* itself says (`durable: false`, `source: serve`): a
/// line about the machine, said again at every boot, and not part of the conversation.
/// A command's own ephemeral notice is not this — see
/// `a_commands_output_is_drawn_even_though_the_server_does_not_keep_it`.
fn ephemeral_notice(id: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "notice", "severity": "info",
        "text": "session ready", "source": "serve", "durable": false
    }))
}

/// The record *plus* the items the view declined is the topic's own list — that is the
/// invariant the tab holds the two lists to (`TabContent::hold_the_record_to_the_topic`),
/// and it only holds if a declined item is counted rather than forgotten.
#[gpui_kit::test]
fn a_notice_the_server_does_not_keep_is_counted_not_just_dropped(cx: &mut TestAppContext) {
    let (view, cx) = open_in(cx, 1000., vec![user("e_1", "hi")]);
    let declined = |cx: &gpui_kit::VisualTestContext| cx.read(|cx| view.read(cx).declined(cx));
    let held = |cx: &gpui_kit::VisualTestContext| cx.read(|cx| view.read(cx).items(cx).len());
    assert_eq!((held(cx), declined(cx)), (1, 0), "nothing declined yet");

    // A durable notice *is* the record: it is a row, and it is not counted as declined.
    view.update(cx, |view, cx| {
        view.upsert(notice("n_1", "info", "the swarm started"), cx);
    });
    assert_eq!((held(cx), declined(cx)), (2, 0));

    // An ephemeral one is not a row — and is counted, which is what makes the two
    // lists comparable at all.
    view.update(cx, |view, cx| {
        view.upsert(ephemeral_notice("n_2"), cx);
    });
    assert_eq!((held(cx), declined(cx)), (2, 1), "declined, and said so");

    // Offered again — a patch for the same notice, which the stream does send — it is
    // still the same one item.
    view.update(cx, |view, cx| {
        view.upsert(ephemeral_notice("n_2"), cx);
    });
    assert_eq!((held(cx), declined(cx)), (2, 1), "counted once");

    // A record replaced outright re-counts from what it was given, and an item a page
    // brings in front of the reader is counted the same way.
    view.update(cx, |view, cx| {
        view.replace(vec![user("e_1", "hi"), ephemeral_notice("n_3")], cx);
    });
    assert_eq!(
        (held(cx), declined(cx)),
        (1, 1),
        "re-counted from the topic"
    );
    view.update(cx, |view, cx| {
        view.prepend(vec![ephemeral_notice("n_4")], cx);
    });
    assert_eq!((held(cx), declined(cx)), (1, 2), "a page is counted too");

    // And an item the topic no longer holds leaves the count with it.
    view.update(cx, |view, cx| {
        view.remove("n_3", cx);
        view.remove("n_4", cx);
    });
    assert_eq!((held(cx), declined(cx)), (1, 0), "gone is gone");

    // A view nobody has told holds nothing and declined nothing: the two lists agree
    // at the start as well as after every op.
    view.update(cx, |view, cx| view.clear(cx));
    assert_eq!((held(cx), declined(cx)), (0, 0));
}

/// A record far taller than the pane, ending on the reader's own long turn — one row
/// longer than the pane, so a reader scrolled up into it is reading it.
fn long_record() -> Vec<Item> {
    let long = "the reader's own turn, long enough that this one row is taller than the \
                pane it is read in, many times over. "
        .repeat(60);
    let mut items: Vec<Item> = (0..200)
        .map(|i| {
            user(
                &format!("u_{i:04}"),
                "a turn of its own, long enough that its row is a few lines tall",
            )
        })
        .collect();
    items.push(user("u_long", &long));
    items
}

/// The rows of the record the list has measured, and the height it holds for each — the
/// heights its scroll range is the sum of, and the ones a reader's bar is drawn from.
fn measured_heights(
    view: &Entity<TranscriptView>,
    cx: &gpui_kit::VisualTestContext,
) -> Vec<(usize, Pixels)> {
    cx.read(|cx| {
        let view = view.read(cx);
        (0..view.slots.rows)
            .filter_map(|index| {
                view.list
                    .bounds_for_item(view.slots.at(index))
                    .map(|bounds| (index, bounds.size.height))
            })
            .collect()
    })
}

/// The range the list's scrollbar draws: the sum of the heights it has measured, less the
/// pane it is read in.
fn scroll_range(view: &Entity<TranscriptView>, cx: &gpui_kit::VisualTestContext) -> Pixels {
    cx.read(|cx| view.read(cx).list.max_offset_for_scrollbar().y)
}

/// The switch: the answer arrives while the transcript is not drawn, and the column
/// re-reads the topic into the view — the same items, the answer last (`replace`).
fn switch_back_with_an_answer(view: &Entity<TranscriptView>, cx: &mut gpui_kit::VisualTestContext) {
    view.update(cx, |view, cx| {
        view.upsert(
            assistant("u_new", "the answer to the reader's own turn", "final"),
            cx,
        )
    });
    let held: Vec<Item> = cx.read(|cx| view.read(cx).items(cx).to_vec());
    view.update(cx, |view, cx| view.replace(held, cx));
    frames(cx, 3);
}

/// A drag of the scrollbar's thumb to the bottom of its track, released before the frame
/// that would have painted the move: what the kit's own drag does on its last mouse move
/// (`set_offset_from_scrollbar` at the end of the range the bar draws), with the thumb
/// already let go by the time the frame runs.
fn drag_the_bar_to_the_bottom(view: &Entity<TranscriptView>, cx: &mut gpui_kit::VisualTestContext) {
    view.update(cx, |view, _| {
        view.list.scrollbar_drag_started();
        let end = view.list.max_offset_for_scrollbar().y;
        view.list
            .set_offset_from_scrollbar(gpui_kit::point(px(0.), -end));
        view.list.scrollbar_drag_ended();
    });
    frames(cx, 3);
}

/// The same drag held to the end of the track: a real drag's moves paint while the thumb
/// is down, which is where the list's own drag flag is set.
fn drag_the_bar_to_the_bottom_while_held(
    view: &Entity<TranscriptView>,
    cx: &mut gpui_kit::VisualTestContext,
) {
    view.update(cx, |view, _| {
        view.list.scrollbar_drag_started();
        let end = view.list.max_offset_for_scrollbar().y;
        view.list
            .set_offset_from_scrollbar(gpui_kit::point(px(0.), -end));
    });
    frames(cx, 3);
    view.update(cx, |view, _| view.list.scrollbar_drag_ended());
    frames(cx, 2);
}

/// A reader who drags the scrollbar's thumb to the bottom of its track has asked for the
/// end of the record. The list's range is the sum of the rows it has *measured*, though,
/// so the bottom of the track is only the end of the record while the rows behind it have
/// been measured. A re-read that starts the list again would take those measurements with
/// it, and a drag to the bottom would then leave the newest rows out of reach.
///
/// The switch this stands in for, as the app does it: `TabModel::select` marks the new
/// agent's topic stale (`crates/session/src/tab.rs:364`), `TabContent::push` turns that
/// into `plan.reset` (`crates/workspace/src/tab.rs:1565`) and calls `replace`
/// (`:1637`) — and the selection also refetches the topic (`:1922`), whose snapshot is
/// another reset. It is the same rows in the same order with the answer behind them, so
/// the list keeps every height it has measured.
///
/// A wheel is no comparison in the bar's favour — it *does* reach the end, because a
/// scroll the list clamps at its own end is read as the reader asking for the foot
/// (`reaches_the_foot` → `scroll_to_end`), and a scrollbar drag never reports one.
#[gpui_kit::test]
fn a_scrollbar_drag_to_the_bottom_reaches_the_end_of_the_record(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, long_record());
    frames(cx, 2);

    // The reader wheels up into their own turn: unpinned, and the tail is off screen.
    for _ in 0..4 {
        wheel(cx, 1080.);
    }
    assert!(!cx.read(|cx| view.read(cx).is_following_tail(cx)));

    switch_back_with_an_answer(&view, cx);
    assert!(
        !drawn_row(cx, "u_new"),
        "the switch keeps the reader's place: the answer is below them"
    );

    let before = cx.read(|cx| view.read(cx).list.logical_scroll_top());
    drag_the_bar_to_the_bottom(&view, cx);
    let after = cx.read(|cx| view.read(cx).list.logical_scroll_top());
    let range = scroll_range(&view, cx);
    assert!(
        drawn_row(cx, "u_new"),
        "the bottom of the track is the end of the record: the drag moved {before:?} -> \
         {after:?} within a range of {range:?}, and the newest row is still not built"
    );
}

/// The same ask with a wheel: one hard wheel to the foot does reach the end — the list
/// clamps it at the end of what it has measured, and the view reads that as the reader
/// asking for the bottom, which is what the scrollbar's drag has no way to say.
#[gpui_kit::test]
fn a_wheel_to_the_foot_reaches_the_end_of_the_record(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, long_record());
    frames(cx, 2);

    for _ in 0..4 {
        wheel(cx, 1080.);
    }
    switch_back_with_an_answer(&view, cx);
    assert!(!drawn_row(cx, "u_new"), "the answer is below the reader");

    wheel(cx, -100_000.);
    assert!(
        drawn_row(cx, "u_new"),
        "the wheel reaches the end of the record"
    );
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
}

/// A re-read that only extends the record keeps the list's own measurements — every row
/// the reader has scrolled past holds the height it was measured at, so the bar still
/// covers what they have read — while a row whose item changed under its id is measured
/// again, since its height is the one thing the list cannot be trusted with.
#[gpui_kit::test]
fn an_extending_re_read_keeps_the_measured_heights_and_measures_a_changed_row(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open!(cx, long_record());
    frames(cx, 2);

    // The reader wheels up the record: the rows they pass are measured, and the bar's
    // range grows to cover them.
    for _ in 0..2 {
        wheel(cx, 1080.);
    }
    let before = measured_heights(&view, cx);
    let range_before = scroll_range(&view, cx);
    assert!(
        before.len() > 8,
        "a window of rows is measured: {} of them",
        before.len()
    );
    assert!(
        range_before > px(1080.),
        "and the bar covers more than a pane: {range_before:?}"
    );

    // The re-read: the same rows in the same order, one of them rewritten with far more
    // words, and the answer arriving behind them.
    let mut next: Vec<Item> = cx.read(|cx| view.read(cx).items(cx).to_vec());
    let changed = before[before.len() / 2].0;
    let rewritten = "the same turn, rewritten with a great many more words than it had \
                     the first time — enough that its row is taller than the one the list \
                     measured for it. "
        .repeat(8);
    next[changed] = user(&format!("u_{changed:04}"), &rewritten);
    next.push(assistant(
        "u_new",
        "the answer to the reader's own turn",
        "final",
    ));
    view.update(cx, |view, cx| view.replace(next, cx));
    frames(cx, 3);

    // Every row that was measured is still measured, at the height it was measured at...
    let after = measured_heights(&view, cx);
    for (index, height) in &before {
        if *index == changed {
            continue;
        }
        assert_eq!(
            after
                .iter()
                .find(|(row, _)| row == index)
                .map(|(_, height)| *height),
            Some(*height),
            "row {index} kept the height the list measured for it"
        );
    }
    // ...the rewritten row is not: it is measured again, and it is taller...
    let held_height = before
        .iter()
        .find(|(row, _)| *row == changed)
        .expect("the rewritten row was measured before")
        .1;
    let measured = after
        .iter()
        .find(|(row, _)| *row == changed)
        .expect("the rewritten row is measured again")
        .1;
    assert!(
        measured > held_height,
        "the rewritten row is measured again: {held_height:?} -> {measured:?}"
    );
    // ...and the bar still covers what it did before the re-read.
    let range_after = scroll_range(&view, cx);
    assert!(
        range_after >= range_before,
        "the bar kept its range: {range_before:?} -> {range_after:?}"
    );
}

/// A drag of the bar to its own end reaches the record's end even when the list has just
/// been started again — a re-read that is not the same rows in the same order (a
/// compaction, a restart, a shorter record), where the rows behind the reader are
/// unmeasured once more and the end of the bar is short of the end of the record. The
/// thumb held at the bottom of the track is the reader's own ask, and the view takes them
/// to the bottom of the record.
#[gpui_kit::test]
fn a_drag_to_the_bottom_reaches_the_end_after_the_list_starts_again(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, long_record());
    frames(cx, 2);
    for _ in 0..4 {
        wheel(cx, 1080.);
    }
    assert!(!cx.read(|cx| view.read(cx).is_following_tail(cx)));

    // A record that is not the one held: the older half has been compacted away, so the
    // row the reader was at is still there and the ids in front of it are not.
    let held: Vec<Item> = cx.read(|cx| view.read(cx).items(cx).to_vec());
    let mut next: Vec<Item> = held[100..].to_vec();
    next.push(assistant(
        "u_new",
        "the answer to the reader's own turn",
        "final",
    ));
    view.update(cx, |view, cx| view.replace(next, cx));
    frames(cx, 3);
    assert!(!drawn_row(cx, "u_new"), "the answer is below the reader");

    drag_the_bar_to_the_bottom_while_held(&view, cx);
    assert!(
        drawn_row(cx, "u_new"),
        "the thumb held at the bottom of the track asks for the record's end"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "and the tail is followed again from there"
    );
}
