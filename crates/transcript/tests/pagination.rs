//! The transcript's paging window, driven the way a reader drives it.
//!
//! These are the review lane's own tests, in a binary of their own so they assert the
//! *published* behaviour of the window through a real window, a real wheel and painted
//! geometry — never through the view's private fields. What each one holds:
//!
//! * the list opens on the newest page and "Earlier items" opens the page in front of it
//!   out of what is already in hand, landing at the top of the block it opened;
//! * output that arrives while the reader is reading is held behind the window: what they
//!   are reading does not move, and the way back is the pill;
//! * the reader's own wheel to the foot of what they can see is what brings the window
//!   back to the newest page, with the output that arrived on screen;
//! * a page that lands after the reader went back to the latest — mid-jump or after it —
//!   is held behind the tail, and the tail stays where it is;
//! * the working pips are the *list's* foot, not the window's: they are not drawn under a
//!   window whose foot is not the record's.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    ParentElement as _, Pixels, Render, ScrollDelta, Styled as _, TestAppContext,
    VisualTestContext, Window,
};
use serde_json::json;
use session::Item;
use transcript::TranscriptView;

/// The page the list opens on and one press of "Earlier items" opens: the session's own
/// page size, which these tests hold the view to.
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

// --- the window, as a reader and a test can see it ------------------------

/// The element a row's measure cell is named by (the row's own, not the column's).
fn row(id: &str) -> ElementId {
    (ElementId::from("transcript-measure"), id.to_string()).into()
}

/// Whether the row for `id` is *built* — in the window — whatever its place on screen.
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

fn label(cx: &mut VisualTestContext, name: &'static str) -> Option<String> {
    cx.update(|window, _| window.find(name).label().map(str::to_string))
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

// --- the window -----------------------------------------------------------

/// The list opens on the newest whole page; the rows in front of it are held and not
/// built; and "Earlier items" opens the next page out of what is in hand — the server is
/// asked for nothing until that is used up — landing at the top of the block it opened.
#[gpui_kit::test]
fn the_list_opens_on_the_newest_page_and_the_header_opens_the_one_in_front(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open!(cx, record(3 * PAGE));

    let asked = Rc::new(RefCell::new(Vec::new()));
    let record_ask = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(
            move |oldest, _window, _cx| record_ask.borrow_mut().push(oldest.to_string()),
            cx,
        )
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);

    // The newest page is built, and only the newest page.
    assert!(
        built(cx, &id_at(2 * PAGE)),
        "the page's oldest row is built"
    );
    assert!(built(cx, &id_at(3 * PAGE - 1)), "and its newest");
    assert!(
        !built(cx, &id_at(2 * PAGE - 1)),
        "the row in front of the page is held, not built"
    );
    assert_eq!(
        built_of(cx, &ids(0..3 * PAGE)),
        PAGE,
        "a window is one page of the record"
    );

    // To the head of the list, where the header is, and press it.
    wheel(cx, 100_000.);
    assert!(
        shown(cx, "transcript-load-older"),
        "the header is on screen at the head"
    );
    cx.update(|window, cx| window.click("transcript-load-older", cx));
    frames(cx, 2);

    assert!(
        asked.borrow().is_empty(),
        "a page out of what is in hand asks the server for nothing: {:?}",
        asked.borrow()
    );
    assert_eq!(
        built_of(cx, &ids(0..3 * PAGE)),
        2 * PAGE,
        "the window grew by exactly one page"
    );
    assert!(
        built(cx, &id_at(PAGE)) && !built(cx, &id_at(PAGE - 1)),
        "the page that opened is the one in front of the window"
    );

    // The press lands at the top of the block it opened: what was the head of the list
    // has been pushed a page down, and the new block starts inside the pane.
    let pane = pane(cx);
    let header = at(cx, "transcript-load-older");
    assert!(
        header.top() >= pane.top() - px(1.) && header.top() < pane.bottom(),
        "the header sits at the top of the pane: {header:?} vs {pane:?}"
    );
    let first = row_bounds(cx, &id_at(PAGE));
    assert!(
        first.top() >= pane.top() - px(1.) && first.top() < pane.bottom(),
        "the first row of the block is on screen: {first:?} vs {pane:?}"
    );
    let was_head = row_bounds(cx, &id_at(2 * PAGE));
    assert!(
        was_head.top() > pane.bottom(),
        "what was the head is a page below the pane: {was_head:?} vs {pane:?}"
    );
}

// --- reading --------------------------------------------------------------

/// Output that arrives while the reader is reading is held behind the window: the row
/// they are reading does not move a pixel, the new output is not built under them, and
/// the pill is the way to it.
#[gpui_kit::test]
fn output_that_arrives_while_the_reader_reads_does_not_move_what_they_are_reading(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open!(cx, record(400));
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);

    wheel(cx, 900.);
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "the reader is away from the tail"
    );

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
        "the output that arrived is behind the window, not under them"
    );
    assert!(
        shown(cx, "transcript-jump"),
        "and the pill says there is more below"
    );
}

/// The reader's own wheel to the foot of what they can see is what brings the window
/// back to the newest page: the output that arrived is on screen, the window is one
/// page again, and the newest row is at the foot of the pane.
#[gpui_kit::test]
fn a_wheel_to_the_foot_shows_what_arrived_and_collapses_the_window(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(400));
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);

    wheel(cx, 900.);
    view.update(cx, |view, cx| {
        assert!(view.upsert(turn(&id_at(400)), cx), "the new turn lands");
    });
    frames(cx, 3);
    assert!(
        !built(cx, &id_at(400)),
        "held behind the window while they read"
    );

    wheel(cx, -100_000.);

    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the reader is back on the tail"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "with nothing to jump to"
    );
    assert!(!shown(cx, "transcript-jump"), "so no pill");
    assert!(
        built(cx, &id_at(400)),
        "the output that arrived is on screen"
    );
    assert_eq!(
        built_of(cx, &ids(0..401)),
        PAGE,
        "the window is one page again"
    );
    assert!(
        built(cx, &id_at(401 - PAGE)) && !built(cx, &id_at(400 - PAGE)),
        "the newest page of the record"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_loading_older()),
        "nothing is left in flight"
    );

    let pane = pane(cx);
    let last = row_bounds(cx, &id_at(400));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() > pane.bottom() - px(160.),
        "the newest row is at the foot of the pane: {last:?} vs {pane:?}"
    );
}

// --- the way back, with a page still on its way ---------------------------

/// A page the reader asked for, landing after they have already gone back to the latest
/// — the jump still running, or finished — is held behind the tail: the list stays on
/// its newest page, on its foot, with no pill to press.
fn a_page_lands_after_the_jump(cx: &mut TestAppContext, mid_jump: bool) {
    let (view, cx) = open!(cx, record(PAGE));

    let asked = Rc::new(RefCell::new(0usize));
    let counted = asked.clone();
    view.update(cx, |view, cx| {
        view.on_load_older(move |_, _, _| *counted.borrow_mut() += 1, cx)
    });
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);

    // The reader goes to the head of the list, where the header is, presses it, and then
    // goes back to the latest before the page lands.
    wheel(cx, 100_000.);
    assert!(
        shown(cx, "transcript-load-older"),
        "the header is on screen at the head"
    );
    cx.update(|window, cx| window.click("transcript-load-older", cx));
    frames(cx, 2);
    assert_eq!(
        *asked.borrow(),
        1,
        "the header asks the server for the page behind"
    );
    assert_eq!(
        label(cx, "transcript-load-older").as_deref(),
        Some("Loading earlier items…"),
        "and says it is on its way"
    );

    assert!(
        shown(cx, "transcript-jump"),
        "the pill is drawn before the press"
    );
    cx.update(|window, cx| window.click("transcript-jump", cx));
    let at = Duration::from_millis(if mid_jump { 40 } else { 500 });
    cx.executor().advance_clock(at);
    cx.run_until_parked();

    // The page lands: a whole page older than everything held.
    let page: Vec<Item> = (0..PAGE).map(|i| turn(&format!("p{i:04}"))).collect();
    view.update(cx, |view, cx| {
        view.prepend(page, cx);
        view.set_history(true, false, cx);
    });
    frames(cx, 3);
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    frames(cx, 3);

    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the list is still on the tail"
    );
    assert!(
        !cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "with nothing to jump to"
    );
    assert!(!shown(cx, "transcript-jump"), "so no pill");
    assert_eq!(
        built_of(
            cx,
            &(0..PAGE).map(|i| format!("p{i:04}")).collect::<Vec<_>>()
        ),
        0,
        "the page that landed is held behind the window"
    );
    assert_eq!(
        built_of(cx, &ids(0..PAGE)),
        PAGE,
        "and the window is the newest page"
    );
    assert_eq!(
        label(cx, "transcript-load-older").as_deref(),
        Some("Earlier items"),
        "the header is offered again, not still loading"
    );

    let pane = pane(cx);
    let last = row_bounds(cx, &id_at(PAGE - 1));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() > pane.bottom() - px(160.),
        "the newest row is at the foot of the pane: {last:?} vs {pane:?}"
    );
}

#[gpui_kit::test]
fn a_page_that_lands_while_the_jump_runs_stays_behind_the_tail(cx: &mut TestAppContext) {
    a_page_lands_after_the_jump(cx, true);
}

#[gpui_kit::test]
fn a_page_that_lands_after_the_jump_stays_behind_the_tail(cx: &mut TestAppContext) {
    a_page_lands_after_the_jump(cx, false);
}

/// A page the reader never asked for — the model's own scrollback arriving late — is
/// held in front of their window without moving what they are reading. The anchor puts
/// their place back once the new rows are laid out.
#[gpui_kit::test]
fn a_page_the_reader_did_not_ask_for_does_not_move_what_they_are_reading(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(320));
    frames(cx, 2);
    wheel(cx, 600.);
    let reading = top_row_in_pane(cx, &ids(0..320)).expect("a row at the top of the pane");
    let before = row_bounds(cx, &reading);

    let page: Vec<Item> = (0..256).map(|i| turn(&format!("p{i:04}"))).collect();
    view.update(cx, |view, cx| view.prepend(page, cx));
    cx.update(|window, cx| window.render_frame(cx));
    let landing = row_bounds(cx, &reading);
    frames(cx, 3);
    let after = row_bounds(cx, &reading);

    let moved = f32::from(after.top()) - f32::from(before.top());
    eprintln!(
        "the page lands: the row is {:.0}px off, and settles {:.0}px off",
        f32::from(landing.top()) - f32::from(before.top()),
        moved
    );
    assert!(
        moved.abs() < 1.,
        "the row they were reading settles where it was ({moved}px): {after:?} vs {before:?}"
    );
    assert!(
        !built(cx, &format!("p{:04}", 255)),
        "and the page is in front of the window, not on screen"
    );
}

// --- the layout, not the reader ------------------------------------------

/// Rows going away in front of the reader — a queued input taken back — is not the
/// reader asking for the tail either. One row leaving brings the foot of what they can
/// see up to them, and the output that arrived while they read must stay behind their
/// window: the reader is reading, not catching up.
#[gpui_kit::test]
fn a_row_going_away_in_front_of_the_reader_does_not_pull_them_to_the_tail(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(320));
    view.update(cx, |view, cx| view.set_history(true, false, cx));
    frames(cx, 2);

    // The reader is away, and the agent answers while they read.
    wheel(cx, 10_000.);
    for i in 320..380 {
        view.update(cx, |view, cx| {
            assert!(view.upsert(turn(&id_at(i)), cx), "the agent answers");
        });
    }
    frames(cx, 2);
    assert!(
        !built(cx, &id_at(379)),
        "the answers are behind the frozen window"
    );
    assert!(shown(cx, "transcript-jump"), "and the pill says so");

    // The reader stands a row's height above the foot of what they can see.
    let pitch =
        f32::from(row_bounds(cx, &id_at(319)).top()) - f32::from(row_bounds(cx, &id_at(318)).top());
    assert!(
        pitch > 100.,
        "a row of the fixture is tall enough that one leaving matters: {pitch}px"
    );
    let last = row_bounds(cx, &id_at(319));
    let gap = f32::from(last.bottom()) - f32::from(pane(cx).bottom());
    wheel(cx, -(gap - pitch * 0.8));
    assert!(
        !cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the reader is not on the tail"
    );

    // A row in front of them goes away.
    assert!(
        view.update(cx, |view, cx| view.remove(&id_at(150), cx)),
        "the row leaves the record"
    );
    frames(cx, 3);

    assert!(
        !built(cx, &id_at(379)),
        "the output that arrived is still behind their window"
    );
    assert!(shown(cx, "transcript-jump"), "and the pill still says so");
}

// --- the pips -------------------------------------------------------------

/// The working pips mark the foot of the *list*. They are not drawn under a window whose
/// foot is not the record's — where they would say the list ends while the output that
/// arrived sits hidden behind it — and they are there once the reader is back on the
/// tail.
#[gpui_kit::test]
fn the_working_pips_are_not_drawn_at_a_foot_that_is_not_the_lists(cx: &mut TestAppContext) {
    let (view, cx) = open!(cx, record(400));
    frames(cx, 2);

    wheel(cx, 900.);
    // The agent is working: its newest turn is one evo has taken, with nothing said yet.
    view.update(cx, |view, cx| {
        view.set_running(true, cx);
        assert!(view.upsert(turn(&id_at(400)), cx), "the new turn lands");
    });
    frames(cx, 3);
    assert!(!built(cx, &id_at(400)), "the new turn is behind the window");
    assert!(
        !drawn(cx, "transcript-pending-row"),
        "so the pips are not drawn at a foot that is not the list's"
    );

    // Back on the tail they say what they are for.
    wheel(cx, -100_000.);
    assert!(built(cx, &id_at(400)), "the turn is on screen at the tail");
    assert!(
        drawn(cx, "transcript-pending-row"),
        "and the pips are at the foot of the list"
    );
}
