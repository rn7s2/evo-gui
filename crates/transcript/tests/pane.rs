//! The transcript in the app's own embedding — a *cached* view filling the box the
//! conversation column gives it — while that box changes height under it, and while the
//! reader is looking at another lane.
//!
//! A cached view is laid out from the style it is handed and is never measured from
//! its rows, and it is only rendered again when it is notified or its bounds change.
//! The composer growing as a message is typed, a drawer opening and the window being
//! resized all change the box and nothing else: a reader who is following the latest
//! must still see it, and a reader who scrolled away must keep their place.
//!
//! The box holds **one** agent's transcript at a time, so a lane's own rows arrive
//! while it is not drawn at all; the switch back is the re-read `TabModel::select`
//! asks for — the topic's items, replaced outright. Those tests are at the foot of
//! this file.

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
    /// Whether the transcript is the one being read: the box holds it when it is, and
    /// nothing when it is not — the tab shows one agent's transcript at a time.
    showing: bool,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(div().flex_1().min_h_0().children(self.showing.then(|| {
                AnyView::from(self.transcript.clone())
                    .cached(StyleRefinement::default().size_full())
            })))
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

/// The answer to [`turn`]: the row a lane writes back, and the one that arrives after the
/// reader's own.
fn answer(index: usize, text: &str) -> Item {
    Item::from_json(&json!({
        "id": format!("e_{index:04}"), "ts": 2, "kind": "assistant", "status": "final",
        "text": text
    }))
    .expect("a fixture item has an id")
}

/// The brief a lane is given: a `user` row of its own, which opens its turn.
fn brief(index: usize) -> Item {
    Item::from_json(&json!({
        "id": format!("e_{index:04}"), "ts": 1, "kind": "user", "status": "sent",
        "text": "the task this lane was given, long enough to be a few lines of its own"
    }))
    .expect("a fixture item has an id")
}

/// One `tool` row, as the wire publishes it: a call running, then running no longer.
fn call(id: &str, status: &str, result: Option<&str>) -> Item {
    let mut value = json!({
        "id": id, "ts": 2, "kind": "tool", "call_id": id, "name": "bash",
        "args": {"command": "cargo test -p transcript"}, "status": status, "parent": null
    });
    if let Some(text) = result {
        value["result"] = json!({"text": text, "chars": text.len() as u64, "truncated": false});
    }
    Item::from_json(&value).expect("a fixture item has an id")
}

/// One `assistant` row: streaming with no words yet, the same row with some, then the
/// same row final — the three shapes a lane's answer takes on the wire.
fn message(id: &str, status: &str, text: &str) -> Item {
    Item::from_json(&json!({
        "id": id, "ts": 3, "kind": "assistant", "status": status, "text": text
    }))
    .expect("a fixture item has an id")
}

/// One user turn whose row is taller than the pane it is read in.
fn long_turn(index: usize) -> Item {
    let text = "the reader's own turn, long enough that this one row is taller than \
                the pane it is read in, many times over. "
        .repeat(40);
    Item::from_json(&json!({
        "id": format!("e_{index:04}"), "ts": 4, "kind": "user", "status": "sent",
        "text": text
    }))
    .expect("a fixture item has an id")
}

/// Scroll the transcript's own scroller, as a wheel does: down is a negative delta.
fn wheel(cx: &mut VisualTestContext, delta: f32) {
    cx.update(|window, cx| {
        window.scroll(
            "transcript-scroll",
            ScrollDelta::Pixels(point(px(0.), px(delta))),
            cx,
        )
    });
    frames(cx, 3);
}

fn row(index: usize) -> ElementId {
    (
        ElementId::from("transcript-measure"),
        format!("e_{index:04}"),
    )
        .into()
}

/// The row wrapper of the item with this id, whatever its kind.
fn row_of(id: &str) -> ElementId {
    (ElementId::from("transcript-measure"), id.to_string()).into()
}

/// The element an assistant row draws its document through — what a row with a parsed
/// markdown document has and a row drawn as its raw text does not.
fn message_of(id: &str) -> ElementId {
    (ElementId::from("transcript-message"), id.to_string()).into()
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

/// The bounds of an element with this id, if the frame built it.
fn bounds_of(cx: &mut VisualTestContext, id: ElementId) -> Option<Bounds<Pixels>> {
    cx.update(|window, _| window.try_find(id).map(|element| element.bounds()))
}

const ITEMS: usize = 200;

fn open(cx: &mut TestAppContext) -> (Entity<Host>, Entity<TranscriptView>, &mut VisualTestContext) {
    open_with(cx, (0..ITEMS).map(turn).collect())
}

/// The same host, over a record the test builds itself.
fn open_with(
    cx: &mut TestAppContext,
    items: Vec<Item>,
) -> (Entity<Host>, Entity<TranscriptView>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let (host, cx) = cx.add_window_view(|_window, cx| Host {
        transcript: cx.new(TranscriptView::new),
        foot: px(80.),
        showing: true,
    });
    let view = cx.read(|cx| host.read(cx).transcript.clone());
    view.update(cx, |view, cx| view.replace(items, cx));
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

/// Show the transcript, or take it out of the box the conversation column gives it (the
/// reader picked another lane).
fn set_showing(host: &Entity<Host>, showing: bool, cx: &mut VisualTestContext) {
    host.update(cx, |host, cx| {
        host.showing = showing;
        cx.notify();
    });
    frames(cx, 3);
}

/// The latest row sits inside the pane, at its foot: what a reader following the tail
/// sees.
fn assert_row_at_foot(cx: &mut VisualTestContext, index: usize, when: &str) {
    let pane = pane(cx);
    let last = row_bounds(cx, index).unwrap_or_else(|| panic!("{when}: the row is built"));
    assert!(
        last.bottom() <= pane.bottom() + px(1.) && last.bottom() >= pane.bottom() - px(40.),
        "{when}: the row stands at the pane's foot — {last:?} in {pane:?}"
    );
}

/// The record's last row is the one at the foot: the follower's own check, for the
/// record's own length.
fn assert_at_latest(cx: &mut VisualTestContext, when: &str) {
    assert_row_at_foot(cx, ITEMS - 1, when);
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

/// §9.3, §14.4, and §9.1's re-read: the lane the reader is not looking at.
///
/// The conversation column holds **one** agent's transcript, embedded as a *cached*
/// view; while another lane is selected this one is not rendered at all, and its own
/// rows keep arriving. Switching back is `select`: `TabModel::select` says the center
/// column's rows are stale, and the tab replaces the view with the items the topic
/// holds — the very ones the view already has, in the same order
/// ([`TranscriptView::replace`]). What the reader must land on is the row that arrived
/// last, not the one that was there when they left.
#[gpui_kit::test]
fn a_hidden_transcript_replaced_on_the_switch_lands_on_its_latest_row(cx: &mut TestAppContext) {
    let (host, view, cx) = open(cx);
    assert_at_latest(cx, "opened");
    assert!(cx.read(|cx| view.read(cx).is_following_tail(cx)));
    assert!(!cx.read(|cx| view.read(cx).is_away_from_latest(cx)));

    // The reader picks another lane: this transcript stays where it is, and none of
    // it is drawn while the box holds the other one.
    set_showing(&host, false, cx);
    assert!(
        row_bounds(cx, ITEMS - 1).is_none(),
        "hidden: the pane draws none of this transcript"
    );

    // Its own rows arrive while it is hidden: the turn the coordinator gave it, and
    // the answer it wrote back.
    view.update(cx, |view, cx| {
        view.upsert(turn(ITEMS), cx);
        view.upsert(
            answer(
                ITEMS + 1,
                "the lane's answer, written while nobody was looking at this transcript",
            ),
            cx,
        );
    });

    // The switch back. The tab re-reads the topic into the view — the same items, in
    // the same order, replaced outright — before it is drawn again.
    let items = cx.read(|cx| view.read(cx).items(cx).to_vec());
    assert_eq!(items.len(), ITEMS + 2, "the two rows arrived while hidden");
    view.update(cx, |view, cx| view.replace(items, cx));
    set_showing(&host, true, cx);

    // The reader lands on the row that arrived last: the answer the lane wrote while
    // they were away, at the pane's foot, with nothing below it.
    assert_row_at_foot(cx, ITEMS + 1, "switched back");
    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the switch opens the transcript at its latest item"
    );
    assert!(!cx.read(|cx| view.read(cx).is_away_from_latest(cx)));
}

/// The shape a lane's own turn actually has, arriving while the reader is looking at
/// another lane: a brief, a `bash` call that is running, an answer row that is there
/// before a word of it is, the call finishing, the answer's deltas, the answer final,
/// and the lane already on its next step (the foot of the list is the pips). The switch
/// back is the re-read a selection does — the same items, replaced outright — and what
/// the reader must see is the answer's own text, at the foot of a list that reaches its
/// own end, with the pips under it; and the row that arrives *after* the switch must
/// still land there.
#[gpui_kit::test]
fn a_lanes_streaming_turn_arriving_hidden_is_visible_after_the_switch(cx: &mut TestAppContext) {
    let (host, view, cx) = open(cx);
    assert_at_latest(cx, "opened");

    // The reader picks another lane: this transcript is not drawn.
    set_showing(&host, false, cx);
    assert!(row_bounds(cx, ITEMS - 1).is_none(), "hidden");

    // Its own turn, in the order the wire gives it — none of it drawn.
    view.update(cx, |view, cx| {
        view.set_running(true, cx);
        view.upsert(brief(ITEMS), cx);
        view.upsert(call("t_0201", "running", None), cx);
        // The answer row is on the record before a word of it is: the row holds its
        // own pips.
        view.upsert(message("a_0202", "streaming", ""), cx);
        // The call finishes, and the answer comes in a delta at a time.
        view.upsert(call("t_0201", "ok", Some("test result: ok. 3 passed")), cx);
        view.upsert(message("a_0202", "streaming", "the answer, as it is"), cx);
        view.upsert(
            message(
                "a_0202",
                "streaming",
                "the answer, as it is written, a delta at a time",
            ),
            cx,
        );
        // Final, and the lane is on its next step: nothing in the turn shows the work,
        // so the foot of the list is the pips.
        view.upsert(
            message(
                "a_0202",
                "final",
                "the answer, as it is written, a delta at a time",
            ),
            cx,
        );
    });

    // The switch back: the topic re-read into the view, the same items in the same
    // order, replaced outright.
    let items = cx.read(|cx| view.read(cx).items(cx).to_vec());
    assert_eq!(items.len(), ITEMS + 3);
    view.update(cx, |view, cx| view.replace(items, cx));
    set_showing(&host, true, cx);

    // The answer that arrived while the reader was away is the newest row of the
    // record, drawn through its own document, inside the pane.
    let pane = pane(cx);
    let answer_row = bounds_of(cx, row_of("a_0202")).expect("the answer row is built");
    assert!(
        answer_row.bottom() <= pane.bottom() + px(1.),
        "the answer row is inside the pane — {answer_row:?} in {pane:?}"
    );
    let drawn = bounds_of(cx, message_of("a_0202"))
        .expect("the answer is drawn through its markdown document, not as raw text");
    assert!(
        drawn.size.width > px(1.) && drawn.size.height > px(1.) && drawn.bottom() <= pane.bottom(),
        "the answer's own text is on screen, not just its box — {drawn:?} in {pane:?}"
    );
    // And the foot of the list is the pips: the list reached its own end.
    let pips = bounds_of(cx, ElementId::from("transcript-pending-row"))
        .expect("the pips are the foot of the list while the lane is on its next step");
    assert!(
        pips.bottom() <= pane.bottom() + px(1.) && pips.bottom() >= pane.bottom() - px(40.),
        "the pips stand at the pane's foot — {pips:?} in {pane:?}"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "the switch opens the transcript at its latest item"
    );

    // A row that arrives *after* the switch lands at the foot, on the transcript that
    // is now being drawn: the re-read did not freeze it.
    view.update(cx, |view, cx| {
        view.upsert(
            message(
                "a_0203",
                "streaming",
                "and the next step's answer, arriving while shown",
            ),
            cx,
        );
    });
    frames(cx, 3);
    let next = bounds_of(cx, row_of("a_0203")).expect("the row that arrived while shown is built");
    assert!(
        next.bottom() <= pane.bottom() + px(1.) && next.bottom() >= pane.bottom() - px(40.),
        "a row arriving after the switch is drawn at the foot — {next:?} in {pane:?}"
    );
    assert!(
        bounds_of(cx, message_of("a_0203")).is_some(),
        "and drawn through its own document"
    );
}

/// The same switch for a reader who had scrolled away in that lane before leaving it.
///
/// A `replace` is a record of the same items, and the list under it is reset outright:
/// what holds the reader's place is the view — the row they were reading is found again
/// by id and the list is anchored there. Without that, a reset leaves a `TOP`-aligned
/// list with no anchor at all, and the pane lays itself out from its *first* item: the
/// rows they were reading are not built, and what it shows is the beginning of a
/// transcript they had walked away from.
#[gpui_kit::test]
fn a_replace_does_not_throw_a_reader_who_scrolled_away_to_the_head(cx: &mut TestAppContext) {
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
    assert!(!cx.read(|cx| view.read(cx).is_following_tail(cx)));
    // The first row wholly on screen: where the reader's eye is.
    let pane = pane(cx);
    let reading = (0..ITEMS)
        .find(|&i| {
            row_bounds(cx, i).is_some_and(|b| b.top() >= pane.top() && b.bottom() <= pane.bottom())
        })
        .expect("a row wholly on screen");
    assert!(reading > 0, "the reader scrolled away from the head too");

    // Another lane is shown; this transcript's rows arrive while it is hidden.
    set_showing(&host, false, cx);
    view.update(cx, |view, cx| {
        view.upsert(turn(ITEMS), cx);
        view.upsert(
            answer(
                ITEMS + 1,
                "the answer that arrived while this transcript was hidden",
            ),
            cx,
        );
    });

    // Back, through a `replace` of the items the topic holds.
    let items = cx.read(|cx| view.read(cx).items(cx).to_vec());
    view.update(cx, |view, cx| view.replace(items, cx));
    set_showing(&host, true, cx);

    // The reader is not at the head of the record: the rows before the one they were
    // reading are still above them, and that row is still on screen.
    let lowest = (0..ITEMS).find(|&i| row_bounds(cx, i).is_some());
    assert!(
        lowest != Some(0),
        "the reader was thrown back to the head of the record: the pane builds from \
         row {lowest:?}"
    );
    assert!(
        row_bounds(cx, reading).is_some(),
        "the row the reader was reading is no longer built at all"
    );
    assert!(
        cx.read(|cx| view.read(cx).is_away_from_latest(cx)),
        "and they are still away from the latest, as they left it"
    );
}

/// The same switch for a reader who had scrolled a little above the foot of a lane's
/// transcript — the shape the reported cutoff had: the record ends on one long user turn,
/// and the answer to it arrives while another lane is shown. Switching back is the
/// re-read a selection does — the same items, replaced outright — and the reader must
/// keep the rows they were reading, with the newer ones still there to be scrolled to.
#[gpui_kit::test]
fn switching_back_keeps_newer_rows_reachable(cx: &mut TestAppContext) {
    // A record that ends on one long user turn — taller than the pane it is read in —
    // with the reader a wheel above its foot.
    let mut items: Vec<Item> = (0..ITEMS).map(turn).collect();
    items.push(long_turn(ITEMS));
    let (host, view, cx) = open_with(cx, items);
    wheel(cx, 120.);
    assert!(
        !cx.read(|cx| view.read(cx).is_following_tail(cx)),
        "a wheel unpins the reader"
    );

    // The rows on screen, as the record indices they are drawn from.
    let drawn = |cx: &mut VisualTestContext| -> Vec<usize> {
        (0..=ITEMS + 1)
            .filter(|&i| row_bounds(cx, i).is_some())
            .collect()
    };
    let reading = drawn(cx);
    assert!(
        !reading.contains(&0),
        "the reader is not at the head of the record to begin with"
    );

    // Another lane is shown; the answer to the reader's own turn arrives while this
    // transcript is hidden.
    set_showing(&host, false, cx);
    view.update(cx, |view, cx| {
        view.upsert(answer(ITEMS + 1, "the answer to the reader's own turn"), cx)
    });

    // Back, through the re-read a selection does: the same items, the answer last.
    let held = cx.read(|cx| view.read(cx).items(cx).to_vec());
    assert_eq!(held.len(), ITEMS + 2, "the answer arrived while hidden");
    view.update(cx, |view, cx| view.replace(held, cx));
    set_showing(&host, true, cx);

    // The reader is where they were, not at the head of the record.
    let after = drawn(cx);
    assert!(
        !after.contains(&0),
        "the switch threw the reader back to the head of the record: {after:?}"
    );
    assert!(
        after.iter().any(|row| reading.contains(row)),
        "the rows the reader was reading are gone: {reading:?} -> {after:?}"
    );

    // And the answer is reachable: one scroll to the foot draws it there.
    wheel(cx, -100_000.);
    assert_row_at_foot(cx, ITEMS + 1, "one scroll to the foot after the switch");
}
