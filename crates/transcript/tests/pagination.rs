//! The transcript's record, as a reader and a window see it.
//!
//! These are the review lane's own tests, in a binary of their own so they assert the
//! *published* behaviour of the list through a real window, a real wheel and painted
//! geometry — never through the view's private fields. What each one holds:
//!
//! * the whole journal is one list: every item is in it, and only the rows the pane can
//!   reach are built;
//! * the scrollback walks itself back — no button, one page in flight at a time, for as
//!   long as the topic says there is more behind the oldest item held — and a page whose
//!   answer brings nothing ends the walk;
//! * a page that lands is spliced in above the reader: what they are looking at does not
//!   move a pixel, and a reader at the latest stays at the latest;
//! * the reader's own wheel takes the list off its tail, and back to it;
//! * the working pips are the list's foot.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    ParentElement as _, Pixels, Render, ScrollDelta, Styled as _, TestAppContext,
    VisualTestContext, Window,
};
use serde_json::json;
use session::Item;
use transcript::TranscriptView;

/// The page the topic hands over, and the size of the record these tests build: the
/// session's own page size.
const PAGE: usize = session::PAGE_ITEMS as usize;

/// A window over one transcript, as the tab page arranges it: the transcript takes the
/// pane and nothing else.
struct Host {
    transcript: Entity<TranscriptView>,
}

impl Host {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
        }
    }
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
    }
}

/// Open a window over a transcript holding `items`, render it, and hand back the view
/// and the window's own context.
macro_rules! open {
    ($cx:expr, $items:expr) => {{
        $cx.update(gpui_kit::init);
        let (host, cx) = $cx.add_window_view(|_window, cx| Host::new(cx));
        let view = cx.read(|cx| host.read(cx).transcript.clone());
        view.update(cx, |view, cx| view.replace($items, cx));
        frames(cx, 3);
        (view, cx)
    }};
}

// --- fixtures -------------------------------------------------------------

fn item(value: serde_json::Value) -> Item {
    Item::from_json(&value).expect("a fixture item has an id")
}

/// One turn of the reader's that evo has taken: one row, a turn rule above it, and enough
/// words that a page of them is far taller than the pane.
fn turn(id: &str) -> Item {
    item(json!({
        "id": id, "ts": 1, "kind": "user", "status": "sent",
        "text": "a turn of its own, long enough that its row is a few lines tall and the \
                 whole list is taller than the pane it is read in"
    }))
}

/// A record of `count` turns, oldest first, named `e_0000` upwards.
fn record(count: usize) -> Vec<Item> {
    (0..count).map(|i| turn(&id_at(i))).collect()
}

fn id_at(index: usize) -> String {
    format!("e_{index:04}")
}

// --- the list, as a reader and a test can see it --------------------------

/// The element a row's measure cell is named by (the row's own, not the column's).
fn row(id: &str) -> ElementId {
    (ElementId::from("transcript-measure"), id.to_string()).into()
}

/// Whether the row for `id` is *built* — the list builds the rows the pane can reach, so
/// a row that is not built is a row nobody is looking at.
fn built(cx: &mut VisualTestContext, id: &str) -> bool {
    cx.update(|window, _| window.try_find(row(id)).is_some())
}

/// Where a row is, in window coordinates: positive down, and beyond the pane when it is
/// scrolled out of it.
fn row_bounds(cx: &mut VisualTestContext, id: &str) -> Bounds<Pixels> {
    cx.update(|window, _| window.find(row(id)).bounds())
}

/// Where a named element is, in window coordinates.
fn at(cx: &mut VisualTestContext, name: &'static str) -> Bounds<Pixels> {
    cx.update(|window, _| window.find(name).bounds())
}

fn shown(cx: &mut VisualTestContext, name: &'static str) -> bool {
    cx.update(|window, _| window.find(name).visible())
}

fn drawn(cx: &mut VisualTestContext, name: &'static str) -> bool {
    cx.update(|window, _| window.try_find(name).is_some())
}

/// The transcript's own pane: what the reader can see.
fn pane(cx: &mut VisualTestContext) -> Bounds<Pixels> {
    at(cx, "transcript-scroll")
}

/// How many of `ids` are built.
fn built_of(cx: &mut VisualTestContext, ids: &[String]) -> usize {
    ids.iter().filter(|id| built(cx, id)).count()
}

/// The row at the top of the pane, of the rows in `ids`: what a reader's eye lands on
/// first.
fn top_row_in_pane(cx: &mut VisualTestContext, ids: &[String]) -> Option<String> {
    let pane = pane(cx);
    for id in ids {
        if !built(cx, id) {
            continue;
        }
        let top = row_bounds(cx, id).top();
        if top >= pane.top() && top < pane.bottom() {
            return Some(id.clone());
        }
    }
    None
}

fn frames(cx: &mut VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.update(|window, cx| window.render_frame(cx));
    }
}

/// A wheel over the list: positive is up, away from the tail, exactly as the design's
/// handler reads it (and as a reader's own wheel does).
fn wheel(cx: &mut VisualTestContext, dy: f32) {
    cx.update(|window, cx| {
        window.scroll(
            "transcript-scroll",
            ScrollDelta::Pixels(point(px(0.), px(dy))),
            cx,
        )
    });
    frames(cx, 3);
}

fn ids(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(id_at).collect()
}

// --- the list -------------------------------------------------------------

/// The whole journal is one list: every item of the record is in it, and only the rows
/// the pane can reach are built — a record of hundreds of turns costs a pane of rows.
#[gpui_kit::test]
fn the_whole_record_is_one_list_and_only_a_pane_of_it_is_built(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(3 * PAGE));

    assert_eq!(
        cx.read(|cx| view.read(cx).items(cx).len()),
        3 * PAGE,
        "the whole record is held"
    );

    // The list opens at its latest row, and builds what the pane reaches: the head of a
    // 768-row record is not a row at all.
    let pane = pane(cx);
    assert!(
        built(cx, &id_at(3 * PAGE - 1)),
        "the newest row is on screen"
    );
    assert!(
        !built(cx, &id_at(0)),
        "and the first row of the record is not built"
    );
    let drawn = built_of(cx, &ids(0..3 * PAGE));
    assert!(
        drawn > 0 && drawn < PAGE,
        "a pane of rows is built, not a page of the record: {drawn} of {}",
        3 * PAGE
    );

    // The newest row's foot is the pane's side of the pane's foot: the list sits on its
    // tail, not somewhere above it.
    let last = row_bounds(cx, &id_at(3 * PAGE - 1));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() > pane.bottom() - px(40.),
        "the newest row is at the foot of the pane: {last:?} vs {pane:?}"
    );

    // Every row of the record is reachable: a wheel up from the tail walks back through
    // it, and what is built follows the reader.
    wheel(cx, 4_000.);
    let reading = top_row_in_pane(cx, &ids(0..3 * PAGE)).expect("a row at the top of the pane");
    assert!(
        !built(cx, &id_at(3 * PAGE - 1)),
        "the tail is no longer built, and {reading} is what the reader is looking at"
    );
    assert!(cx.read(|cx| view.read(cx).is_away_from_latest(cx)));
}

// --- the scrollback -------------------------------------------------------

/// The scrollback walks itself back: for as long as the topic says there is more behind
/// the oldest item held, the view asks for the next page — one in flight at a time, each
/// from the oldest item it holds — and stops when the answer says there is nothing
/// behind. The reader presses nothing.
#[gpui_kit::test]
fn the_scrollback_arrives_without_being_asked_for(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(PAGE));

    let asked: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let record_ask = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(
            move |oldest, _window, _cx| record_ask.borrow_mut().push(oldest.to_string()),
            cx,
        )
    });

    // The topic says there is more: the page is asked for at once, and the head of the
    // list says a page is on its way.
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);
    assert_eq!(
        asked.borrow().as_slice(),
        [id_at(0)],
        "the page is asked for from the oldest item held"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_loading_older()),
        "and the view is waiting for it"
    );
    // The line that says so is the list's head, and the reader is at its foot: it is
    // built when they are looking at the head (the crate's own tests hold it there).
    assert!(
        !drawn(cx, "transcript-load-older"),
        "there is no button to press, in the head or anywhere else"
    );
    assert!(!drawn(cx, "transcript-loading-older"));

    // One page at a time: nothing more is asked for while that one is in flight.
    frames(cx, 3);
    assert_eq!(asked.borrow().len(), 1, "one ask per page");

    // The answer lands with nothing behind it: the walk is over, and the page it brought
    // is in the list, above the rows the reader was at.
    let page: Vec<Item> = (0..PAGE).map(|i| turn(&format!("p{i:04}"))).collect();
    view.update(cx, |view, cx| {
        view.prepend(page, cx);
        view.set_history(false, false, cx);
    });
    frames(cx, 3);
    assert_eq!(
        asked.borrow().len(),
        1,
        "nothing is asked for past the record"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_loading_older()),
        "and the view is not waiting for anything"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_loading_older()),
        "with nothing left in flight"
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).items(cx).len()),
        2 * PAGE,
        "and the page is held with the rest of the record"
    );
    let last = row_bounds(cx, &id_at(PAGE - 1));
    let pane = pane(cx);
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() > pane.bottom() - px(40.),
        "the reader is still at the latest, at the foot of the pane: {last:?} vs {pane:?}"
    );
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
}

/// A page whose answer brings nothing — the last page of all, one that overlaps what is
/// held, or one that failed — ends the walk where it is: the same question is not asked
/// again, frame after frame.
#[gpui_kit::test]
fn a_page_that_brings_nothing_ends_the_walk(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(PAGE));
    let asked = Rc::new(RefCell::new(0usize));
    let counted = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(move |_, _, _| *counted.borrow_mut() += 1, cx)
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 3);
    assert_eq!(
        *asked.borrow(),
        1,
        "the page behind the record is asked for"
    );

    // It settles without bringing a row back.
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 4);
    assert_eq!(
        *asked.borrow(),
        1,
        "a page that brought nothing is not asked for again"
    );

    // A topic that has scrollback again — a fresh snapshot — is walked again.
    view.update(cx, |view, cx| view.set_history(false, false, cx));
    frames(cx, 2);
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);
    assert_eq!(*asked.borrow(), 2, "and a new walk is a new ask");
}

// --- the reader -----------------------------------------------------------

/// The reader's own wheel takes the list off its tail and the pill offers the way back;
/// a wheel to the foot puts it on the tail again, with the newest row on screen.
#[gpui_kit::test]
fn a_wheel_leaves_the_tail_and_the_wheel_back_returns_to_it(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(3 * PAGE));
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(
        !shown(cx, "transcript-jump"),
        "nothing to jump to at the tail"
    );

    wheel(cx, 5_000.);
    assert!(
        !cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the reader is off the tail"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "5000px up is past the design's 240px"
    );
    assert!(shown(cx, "transcript-jump"), "so the pill is drawn");

    wheel(cx, -100_000.);
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(!cx.read(|cx| view.read(cx).is_away_from_latest(cx)));
    assert!(!shown(cx, "transcript-jump"), "and the pill is away again");
    let pane = pane(cx);
    let last = row_bounds(cx, &id_at(3 * PAGE - 1));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() > pane.bottom() - px(40.),
        "the list lands on the record's own foot: {last:?} vs {pane:?}"
    );
}

/// Output that arrives while the reader is reading does not move what they are reading:
/// it is appended below them, and the pill is the way to it.
#[gpui_kit::test]
fn output_that_arrives_while_the_reader_reads_does_not_move_them(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(400));

    wheel(cx, 900.);
    let reading = top_row_in_pane(cx, &ids(0..400)).expect("a row at the top of the pane");
    let before = row_bounds(cx, &reading);

    // The agent answers.
    view.update(cx, |view, cx| {
        assert!(view.upsert(turn(&id_at(400)), cx), "the new turn lands");
    });
    frames(cx, 3);

    let after = row_bounds(cx, &reading);
    let moved = f32::from(after.top()) - f32::from(before.top());
    assert!(
        moved.abs() < 1.,
        "the row they were reading has not moved ({moved}px): {after:?} vs {before:?}"
    );
    assert!(
        !built(cx, &id_at(400)),
        "the answer is below the pane they are looking at"
    );
    assert!(
        shown(cx, "transcript-jump"),
        "and the pill says there is more below"
    );

    // Back at the foot: the answer is what the reader is looking at.
    wheel(cx, -100_000.);
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(built(cx, &id_at(400)), "the newest row is on screen");
    let pane = pane(cx);
    let last = row_bounds(cx, &id_at(400));
    assert!(
        last.bottom() <= pane.bottom() + px(1.),
        "at the pane's foot"
    );
}

/// A page that lands while the reader is reading above it is spliced in above them: the
/// rows they are looking at do not move.
#[gpui_kit::test]
fn a_page_that_lands_above_the_reader_does_not_move_what_they_are_reading(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(320));
    frames(cx, 2);
    wheel(cx, 600.);
    let reading = top_row_in_pane(cx, &ids(0..320)).expect("a row at the top of the pane");
    let before = row_bounds(cx, &reading);

    let page: Vec<Item> = (0..PAGE).map(|i| turn(&format!("p{i:04}"))).collect();
    view.update(cx, |view, cx| view.prepend(page, cx));
    frames(cx, 3);

    let after = row_bounds(cx, &reading);
    let moved = f32::from(after.top()) - f32::from(before.top());
    assert!(
        moved.abs() < 1.,
        "the row they were reading settles where it was ({moved}px): {after:?} vs {before:?}"
    );
    assert!(
        !built(cx, &format!("p{:04}", PAGE - 1)),
        "and the page is above their pane, not on screen"
    );
}

// --- the pips -------------------------------------------------------------

/// The working pips mark the foot of the list, which is the foot of the record: they are
/// there while the agent works and nothing on screen says so.
#[gpui_kit::test]
fn the_working_pips_are_the_lists_foot(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(400));
    frames(cx, 2);
    assert!(
        !drawn(cx, "transcript-pending-row"),
        "an idle agent has none"
    );

    view.update(cx, |view, cx| {
        view.set_running(true, cx);
        assert!(view.upsert(turn(&id_at(400)), cx), "the new turn lands");
    });
    frames(cx, 3);
    assert!(
        drawn(cx, "transcript-pending-row"),
        "the pips are at the foot of the record"
    );

    // The reader wheels away: the foot of the record is not what they are looking at,
    // and the pips are not the foot of what they can see.
    wheel(cx, 5_000.);
    assert!(!built(cx, &id_at(400)), "the newest row is off the pane");
    assert!(
        !drawn(cx, "transcript-pending-row"),
        "and the pips are not drawn at a foot that is not the list's"
    );

    // Back on the tail they say what they are for.
    wheel(cx, -100_000.);
    assert!(built(cx, &id_at(400)), "the turn is on screen at the tail");
    assert!(
        drawn(cx, "transcript-pending-row"),
        "and the pips are at the foot of the list"
    );
}
