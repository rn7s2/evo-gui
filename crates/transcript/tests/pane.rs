//! The transcript in the app's own embedding — a *cached* view filling the box the
//! conversation column gives it — while that box changes height under it.
//!
//! A cached view is laid out from the style it is handed and is never measured from
//! its rows, and it is only rendered again when it is notified or its bounds change.
//! The composer growing as a message is typed, a drawer opening and the window being
//! resized all change the box and nothing else: a reader who is following the latest
//! must still see it, and a reader who scrolled away must keep their place.

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, AnyView, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    ParentElement as _, Pixels, Render, ScrollDelta, StyleRefinement, Styled as _, TestAppContext,
    VisualTestContext, Window,
};
use serde_json::json;
use session::Item;
use transcript::TranscriptView;

/// A window shaped like the conversation column: the transcript's box over a foot whose
/// height the test sets — the composer, as it grows and shrinks.
struct Host {
    transcript: Entity<TranscriptView>,
    foot: Pixels,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div().flex_1().min_h_0().child(
                    AnyView::from(self.transcript.clone())
                        .cached(StyleRefinement::default().size_full()),
                ),
            )
            .child(div().flex_none().h(self.foot))
    }
}

fn turn(index: usize) -> Item {
    Item::from_json(&json!({
        "id": format!("e_{index:04}"), "ts": 1, "kind": "user", "status": "sent",
        "text": "a turn of its own, long enough that its row is a few lines tall and the \
                 whole list is taller than the pane it is read in"
    }))
    .expect("a fixture item has an id")
}

fn row(index: usize) -> ElementId {
    (
        ElementId::from("transcript-measure"),
        format!("e_{index:04}"),
    )
        .into()
}

fn frames(cx: &mut VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.update(|window, cx| window.render_frame(cx));
    }
}

fn pane(cx: &mut VisualTestContext) -> Bounds<Pixels> {
    cx.update(|window, _| window.find("transcript-scroll").bounds())
}

fn row_bounds(cx: &mut VisualTestContext, index: usize) -> Option<Bounds<Pixels>> {
    cx.update(|window, _| window.try_find(row(index)).map(|row| row.bounds()))
}

const ITEMS: usize = 200;

fn open(cx: &mut TestAppContext) -> (Entity<Host>, Entity<TranscriptView>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| Host {
        transcript: cx.new(TranscriptView::new),
        foot: px(80.),
    });
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| {
        view.replace((0..ITEMS).map(turn).collect(), cx)
    });
    frames(cx, 3);
    (host, view, cx)
}

fn set_foot(host: &Entity<Host>, foot: f32, cx: &mut VisualTestContext) {
    host.update(cx, |host, cx| {
        host.foot = px(foot);
        cx.notify();
    });
    frames(cx, 3);
}

/// The latest row sits inside the pane, at its foot: what a reader following the tail
/// sees.
fn assert_at_latest(cx: &mut VisualTestContext, when: &str) {
    let pane = pane(cx);
    let last =
        row_bounds(cx, ITEMS - 1).unwrap_or_else(|| panic!("{when}: the latest row is built"));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() >= pane.bottom() - px(40.),
        "{when}: the latest row stands at the pane's foot — {last:?} in {pane:?}"
    );
}

/// A reader following the latest stays on it while the box the transcript lives in
/// shrinks (the composer grows) and grows back (the message is sent).
#[gpui_kit::test]
fn a_follower_stays_at_the_latest_while_the_pane_changes_height(cx: &mut TestAppContext) {
    let (host, view, cx) = open(cx);
    assert_at_latest(cx, "opened");
    let before = pane(cx);

    set_foot(&host, 320., cx);
    let shrunk = pane(cx);
    assert!(
        shrunk.size.height < before.size.height - px(200.),
        "the pane really shrank: {before:?} then {shrunk:?}"
    );
    assert_at_latest(cx, "the composer grew");
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));

    set_foot(&host, 40., cx);
    assert_at_latest(cx, "the composer shrank");
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
}

/// A reader who scrolled away keeps the row they were reading on screen when the box
/// changes height under them, and is not pulled back to the latest.
#[gpui_kit::test]
fn a_reader_keeps_their_place_while_the_pane_changes_height(cx: &mut TestAppContext) {
    let (host, view, cx) = open(cx);
    for _ in 0..4 {
        cx.update(|window, cx| {
            window.scroll(
                "transcript-scroll",
                ScrollDelta::Pixels(point(px(0.), px(400.))),
                cx,
            )
        });
        frames(cx, 3);
    }
    assert!(cx.read(|cx| view.read(cx).is_away_from_latest(cx)));
    let pane_before = pane(cx);
    // The first row wholly on screen: where the reader's eye is, and what a box
    // shrinking from its foot must leave where it was.
    let reading = (0..ITEMS)
        .find(|&i| {
            row_bounds(cx, i)
                .is_some_and(|b| b.top() >= pane_before.top() && b.bottom() <= pane_before.bottom())
        })
        .expect("a row wholly on screen");
    let top_before = row_bounds(cx, reading).unwrap().top();

    set_foot(&host, 320., cx);
    let after = row_bounds(cx, reading).expect("the row being read is still built");
    assert!(
        (after.top() - top_before).abs() <= px(1.),
        "the row being read did not move: {top_before:?} then {:?}",
        after.top()
    );
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "a change of height does not put the reader back on the tail"
    );
}
