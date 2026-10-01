//! transcript_states — the transcript's own states, as pictures.
//!
//! The app's screens (`docs/screens.md`) show a tab, a lane at work and a report
//! arriving. They never show what the transcript itself does, which is where the two
//! newest pieces live: the journal is *one virtual list* — `gpui::list` over the whole
//! record, paging older items back by itself — and the reader's font zoom
//! ([`TranscriptZoom`], the View menu) redraws every row at another size.
//!
//! ```sh
//! cargo run -p transcript --example transcript_states --features test-support -- \
//!     --capture /tmp/lane5-captures
//! ```
//!
//! One state can be taken on its own with `--only <state>`, in an app of its own. That
//! matters for the formula in the record: the app lays a formula out *off the frame*
//! (`math::picture` starts a job and draws the source until it lands), and in this
//! headless harness that job settles for the first window's keys only — so the sweep
//! below, which opens a window per state, captures every state's first formulas as the
//! source they are drawn as before the job lands. Run a state with `--only` when the
//! picture of a formula is what matters.
//!
//! The record is [`ITEMS`] items — turns, markdown with headings, lists, a table, a code
//! fence and TeX, tool calls, lane reports, notices, goal rows, a compaction divider and
//! thinking text — and every state below is reached the way a reader reaches it: a wheel
//! over the list, a press on the pill, a message growing under a pinned reader, a card
//! folded open under the pointer, the zoom the menu sets, a window that changes shape.
//! Nothing pokes the view's fields.
//!
//! Hosting is the app's own: the transcript sits in a `flex_1().min_h_0()` box, as
//! `tab_page.rs`'s conversation column embeds it, in a *cached* view whose style is
//! `size_full()` — so what these pictures show is what the app shows.
//!
//! Every state prints: the pane's bounds, which rows of the record the frame built
//! (probed by the row ids the crate registers), the distance of the first and last
//! painted row from the pane's edges, the pill's presence, the pin's own flags, and —
//! with `test-support` — how many rows the last frame built and how many times the view
//! rendered. The view exposes no numeric scroll offset (only `is_following_tail` and
//! `is_away_from_latest`), so the reader's place is those numbers plus the painted rows.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, point, px, size, AnyView, AnyWindowHandle, AppContext as _, Bounds, Context, ElementId,
    Entity, HeadlessAppContext, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, ScrollDelta, SharedString, Styled as _, Window, WindowBounds, WindowOptions,
};
use serde_json::{json, Value};
use session::Item;

#[cfg(feature = "test-support")]
use transcript::counted;
use transcript::TranscriptView;
use transcript::TranscriptZoom;

/// The window the pictures are taken in: the conversation column's width, with the lane
/// column the app docks beside it left out.
const WINDOW_SIZE: (f32, f32) = (1000., 800.);

/// How many items the record holds.
const ITEMS: usize = 800;

/// One screen of wheel: the pane is as tall as the window.
const SCREEN: f32 = WINDOW_SIZE.1;

// ---------------------------------------------------------------------------
// The record: what a long session's journal holds.
// ---------------------------------------------------------------------------

fn item(value: Value) -> Item {
    Item::from_json(&value).expect("a fixture item has an id")
}

/// The record under construction. An item's id is where it lands (`e_0000`…), so a
/// state can say which row of the record the pane is showing.
struct Record {
    items: Vec<Item>,
    n: usize,
}

impl Record {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            n: 0,
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn add(&mut self, build: impl FnOnce(&str) -> Value) {
        let id = format!("e_{:04}", self.items.len());
        self.items.push(item(build(&id)));
    }
}

/// A user turn: the reader's own words.
fn turn(record: &mut Record, text: &str) {
    record.add(|id| json!({ "id": id, "ts": 1, "kind": "user", "text": text, "status": "sent" }));
}

/// An assistant reply, with the thinking that produced it where a state wants one.
fn reply(record: &mut Record, text: &str, thinking: Option<&str>) {
    record.add(|id| {
        let mut value = json!({
            "id": id, "ts": 1, "kind": "assistant", "text": text, "status": "final",
            "model": "stub-a", "provider": "stub",
            "usage": { "input": 1200, "output": 300, "cache_read": 4000, "cache_write": 0 },
        });
        if let Some(thinking) = thinking {
            value["thinking"] = json!(thinking);
        }
        value
    });
}

/// A tool call: the arguments the name implies, and a result of its own.
fn call(record: &mut Record, name: &str, args: Value, result: String, truncated: bool) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "tool", "call_id": format!("call_{id}"), "name": name,
            "args": args, "status": "ok",
            "result": { "text": result, "chars": result.chars().count(), "truncated": truncated },
            "parent": id,
        })
    });
}

fn report(record: &mut Record, lane: u64) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_report", "lane": lane,
            "done": "ported the item model and its rows",
            "evidence": "cargo test -p session -p transcript: 186 passed",
            "next": "the view crates, then the screens",
            "blocked": "", "requests": "", "goal": "active",
        })
    });
}

fn notice(record: &mut Record, severity: &str, text: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "notice", "severity": severity, "text": text,
            "source": "swarm", "durable": true,
        })
    });
}

fn lane_event(record: &mut Record, lane: u64, event: &str, severity: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_event", "lane": lane, "event": event,
            "detail": "its process exited", "severity": severity,
        })
    });
}

fn goal(record: &mut Record, event: &str, objective: &str, tokens: u64) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "goal", "event": event, "goal_id": "a1b2c3d4",
            "objective": objective, "budget": 50000, "tokens": tokens,
        })
    });
}

fn compaction(record: &mut Record, summary: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "compaction", "summary": summary,
            "tokens_before": 180000, "tokens_after": 9000, "manual": false,
        })
    });
}

fn outcome(record: &mut Record, outcome: &str) {
    record.add(|id| json!({ "id": id, "ts": 1, "kind": "run_outcome", "outcome": outcome }));
}

/// A long message: a heading, lists, a table, a code fence and TeX — inline and
/// displayed — so one row of it is several hundred pixels tall.
fn long_message(topic: &str, n: usize) -> String {
    format!(
        "# {topic}\n\
         \n\
         The measure is $800\\text{{px}}$ wide and the pane is the window less the column \
         beside it, so a row of prose is about \
         $\\frac{{800 - 32}}{{7.5}} \\approx 102$ characters, and the list builds \
         only what it needs:\n\
         \n\
         $$\n\
         \\text{{rows built}} = \\frac{{\\text{{pane height}}}}{{\\text{{row \
         height}}}} + \\text{{overdraw}}\n\
         $$\n\
         \n\
         Pass {n} over the same ground, and what changed: the item index is a map, \
         so a patch is a splice rather than a rescan; each message retains one document, \
         extended with `set_text`; and the journal is one virtual list, so a wheel costs a \
         pane of rows rather than a parse of the whole record.\n\
         \n\
         ## What the pane holds\n\
         \n\
         - the rows the pane reaches, plus a few rows of overdraw\n\
         - one measured height per row, kept across a splice\n\
         - the reader's place, which the pin decides\n\
         \n\
         | step | owner | state |\n\
         | --- | --- | --- |\n\
         | items | session | done |\n\
         | rows | transcript | in progress |\n\
         | screens | app | waiting |\n\
         \n\
         ```rust\n\
         let view = cx.new(TranscriptView::new);\n\
         view.update(cx, |view, cx| view.replace(items, cx));\n\
         ```\n\
         \n\
         The rest is the same work at another {n} rows: read it, move it, and let the list \
         keep the heights it measured.\n"
    )
}

/// The last message of the record: one paragraph and one display formula, and short
/// enough that the whole of it fits the pane — which is what lets the three zoom states
/// line their pictures up on it.
fn math_close() -> String {
    "The last word, in one formula: the pane is the window less the column beside it, so \
     a row of prose is about $\\frac{800 - 32}{7.5} \\approx 102$ characters — and the \
     list builds only what it needs:\n\
     \n\
     $$\n\
     \\text{rows built} = \\frac{\\text{pane height}}{\\text{row height}} + \\
     \\text{overdraw}\n\
     $$\n"
        .to_string()
}

/// A short reply, the kind that lands between two tool calls.
fn short_reply(n: usize) -> String {
    format!(
        "Reading the {n}th file now. The type is already public, so this is a rename and a \
         call site rather than a new module."
    )
}

/// The record: [`ITEMS`] items, in the order a long session produced them.
fn record() -> Vec<Item> {
    let mut record = Record::new();
    while record.len() < ITEMS {
        record.n += 1;
        let n = record.n;
        turn(
            &mut record,
            if n == 1 {
                "port the view model, then take the screens again"
            } else {
                "carry on — the next unit, and keep the record long enough that the list is \
                 what draws it"
            },
        );
        reply(
            &mut record,
            &long_message("Porting the view model", n),
            if n.is_multiple_of(3) {
                Some(
                    "The list measures a row only when it is built, so the heights arrive \
                     as the reader walks. That is the whole design: nothing above the pane \
                     is measured until someone looks at it.",
                )
            } else {
                None
            },
        );
        call(
            &mut record,
            "bash",
            json!({ "command": "cargo test -p session -p transcript", "timeout": 120 }),
            (0..24)
                .map(|i| format!("test the_{i}th_row_is_built ... ok"))
                .collect::<Vec<_>>()
                .join("\n"),
            n.is_multiple_of(2),
        );
        call(
            &mut record,
            "read",
            json!({ "path": format!("/Users/you/coding/evo-gui/crates/transcript/src/rows.rs:{n}") }),
            "//! One row of the journal, and the air between two of them.\n".repeat(6),
            false,
        );
        call(
            &mut record,
            "edit",
            json!({
                "path": format!("/Users/you/coding/evo-gui/crates/transcript/src/rows.rs:{n}"),
                "old_string": "let heights = measure_all(&items);",
                "new_string": "let heights = measure_as_the_pane_reaches(&items);",
            }),
            "the edit landed on line 512".to_string(),
            false,
        );
        reply(&mut record, &short_reply(n), None);
        report(&mut record, (n % 6 + 1) as u64);
        notice(
            &mut record,
            if n.is_multiple_of(4) { "warn" } else { "info" },
            "lane 3 was restarted by its supervisor; the transcript kept its place",
        );
        lane_event(&mut record, (n % 6 + 1) as u64, "restarted", "warn");
        goal(
            &mut record,
            "updated",
            "Ship the redesign: every screen from the design, every state bound to the real \
             view model, and the gate green.",
            (n as u64) * 4000,
        );
        if n.is_multiple_of(5) {
            compaction(
                &mut record,
                "The redesign was planned; the item model, the rows and the list are in.",
            );
        }
        if n.is_multiple_of(7) {
            outcome(&mut record, "completed");
        }
        call(
            &mut record,
            "grep",
            json!({ "pattern": "logical_scroll_top", "path": "crates/transcript/src" }),
            "crates/transcript/src/lib.rs:1261:        self.list.logical_scroll_top()\n".repeat(3),
            false,
        );
        turn(&mut record, "and the pill — does it come back on its own?");
        reply(
            &mut record,
            "It does: `Pin::jumped` puts the reader back on the tail, and the pill goes away \
             on the same frame the list reaches the foot.",
            None,
        );
    }
    record.items.truncate(ITEMS - 1);
    // The record ends on the math message, so every state can put the same formula under
    // the reader's eye and the zoom states are pictures of the same text.
    reply(&mut record, &math_close(), None);
    record.items
}

// ---------------------------------------------------------------------------
// The host: the app's own embedding, one cached view in a `flex_1` box.
// ---------------------------------------------------------------------------

struct Page {
    transcript: Entity<TranscriptView>,
}

impl Page {
    fn new(items: &[Item], cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(TranscriptView::new);
        transcript.update(cx, |view, cx| view.replace(items.to_vec(), cx));
        Self { transcript }
    }
}

impl Render for Page {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // The conversation column's own box (§7.3): the transcript fills it, embedded as
        // a cached view whose style is `size_full()` — a cached view is laid out from its
        // style alone, so it must fill the box outright.
        div()
            .id("page")
            .test_support()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .id("transcript-box")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .children(Some(
                        AnyView::from(self.transcript.clone())
                            .cached(gpui_kit::StyleRefinement::default().size_full()),
                    )),
            )
    }
}

fn context() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx
}

/// Open the capture window with the record in it, at its tail.
fn open(cx: &mut HeadlessAppContext, items: &[Item]) -> (AnyWindowHandle, Entity<Page>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    gpui_kit::point(px(0.), px(0.)),
                    size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |_window, cx| cx.new(|cx| Page::new(items, cx)),
        )
    })
    .expect("open the capture window")
}

// ---------------------------------------------------------------------------
// The things a reader does.
// ---------------------------------------------------------------------------

/// A frame, or three: a state is whatever the last of them left behind.
fn frames(cx: &mut HeadlessAppContext, window: AnyWindowHandle, count: usize) {
    cx.update_window(window, |_, window, cx| {
        for _ in 0..count {
            window.render_frame(cx);
        }
    })
    .expect("the capture window is open");
}

/// A wheel over the list, at its centre: a real `ScrollWheelEvent`, so it lands in the
/// box's own handler the way a reader's does — the one scroll that unpins the list.
fn wheel(cx: &mut HeadlessAppContext, window: AnyWindowHandle, dy: f32) {
    cx.update_window(window, |_, window, cx| {
        window.scroll(
            "transcript-scroll",
            ScrollDelta::Pixels(point(px(0.), px(dy))),
            cx,
        );
    })
    .expect("the capture window is open");
    frames(cx, window, 3);
}

/// The id of a row of the record, built the way `rows.rs` builds it.
fn row_id(id: &str) -> ElementId {
    (
        ElementId::from(SharedString::from("transcript-row")),
        id.to_string(),
    )
        .into()
}

/// The id the record's `n`th item was given.
fn item_id(n: usize) -> String {
    format!("e_{n:04}")
}

/// Which rows of the record the last frame built: the list builds the rows the pane
/// reaches, so this is the pane's own window onto the journal.
fn painted(window: &Window) -> (Option<usize>, Option<usize>) {
    let mut first = None;
    let mut last = None;
    for n in 0..ITEMS {
        if window.try_find(row_id(&item_id(n))).is_some() {
            first.get_or_insert(n);
            last = Some(n);
        }
    }
    (first, last)
}

/// Wheel up until the head of the record is what the pane shows, and say how many wheels
/// it took — the list builds and measures a row when the pane reaches it, so walking back
/// through a long record is a walk rather than one jump.
fn wheel_to_the_head(cx: &mut HeadlessAppContext, window: AnyWindowHandle) -> Option<usize> {
    for taken in 1..=400 {
        wheel(cx, window, SCREEN * 2.);
        if painted_in(cx, window).0 == Some(0) {
            return Some(taken);
        }
    }
    None
}

fn painted_in(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> (Option<usize>, Option<usize>) {
    cx.update_window(window, |_, window, _| painted(window))
        .expect("the capture window is open")
}

/// Let the simulated clock move on, so a timer-driven animation — the jump's own steps,
/// the pill's 160ms fade — runs to its end.
fn run_animation(cx: &mut HeadlessAppContext, window: AnyWindowHandle, budget: Duration) {
    let step = Duration::from_millis(20);
    let mut spent = Duration::ZERO;
    while spent < budget {
        cx.advance_clock(step);
        cx.run_until_parked();
        frames(cx, window, 1);
        spent += step;
    }
}

// ---------------------------------------------------------------------------
// What a state says about itself.
// ---------------------------------------------------------------------------

struct Report {
    size: (f32, f32),
    page: (f32, f32),
    pane: Bounds<Pixels>,
    built: (Option<usize>, Option<usize>),
    head: Option<f32>,
    foot: Option<f32>,
    pill: bool,
    loading_line: bool,
    following: bool,
    away: bool,
    older: bool,
    loading: bool,
    rows_built: Option<u64>,
    renders: Option<u64>,
}

/// One frame of its own, with the row counter reset, and the state read off it.
fn measure(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) -> Report {
    #[cfg(feature = "test-support")]
    counted::reset();
    frames(cx, window, 1);
    #[cfg(feature = "test-support")]
    let rows_built = Some(counted::rows());
    #[cfg(not(feature = "test-support"))]
    let rows_built = None;

    let page = page.clone();
    cx.update_window(window, |_, window, cx| {
        let transcript = page.read(cx).transcript.clone();
        let view = transcript.read(cx);
        // The box the app gives the transcript, not the cached view inside it: a
        // cached subtree is re-laid out without being re-rendered, so its own
        // registration can be a frame older than the window around it.
        let page = window.find("page").bounds();
        let pane = window.find("transcript-box").bounds();
        let built = painted(window);
        let top = built
            .0
            .and_then(|n| window.try_find(row_id(&item_id(n))))
            .map(|row| f32::from(row.bounds().top() - pane.top()));
        let foot = built
            .1
            .and_then(|n| window.try_find(row_id(&item_id(n))))
            .map(|row| f32::from(pane.bottom() - row.bounds().bottom()));
        Report {
            size: (
                f32::from(window.bounds().size.width),
                f32::from(window.bounds().size.height),
            ),
            page: (f32::from(page.size.width), f32::from(page.size.height)),
            pane,
            built,
            head: top,
            foot,
            pill: window.find("transcript-jump").visible(),
            loading_line: window.try_find("transcript-loading-older").is_some(),
            following: view.is_following_tail(cx),
            away: view.is_away_from_latest(cx),
            older: view.has_older(),
            loading: view.is_loading_older(),
            rows_built,
            renders: renders(view),
        }
    })
    .expect("the capture window is open")
}

#[cfg(feature = "test-support")]
fn renders(view: &TranscriptView) -> Option<u64> {
    Some(view.renders())
}

#[cfg(not(feature = "test-support"))]
fn renders(_view: &TranscriptView) -> Option<u64> {
    None
}

fn say(name: &str, report: &Report) {
    let rows = match report.rows_built {
        Some(rows) => rows.to_string(),
        None => "n/a (build with --features test-support)".to_string(),
    };
    let renders = match report.renders {
        Some(renders) => renders.to_string(),
        None => "n/a".to_string(),
    };
    let place = match report.built {
        (Some(first), Some(last)) => format!(
            "{}..={last} of {ITEMS}",
            item_id(first).trim_start_matches("e_")
        ),
        _ => "none".to_string(),
    };
    println!(
        "[states] {name:<16} window {}x{} | page {:.0}x{:.0} | pane {:.0}x{:.0} | painted {place} \
         | head {:+.1}px foot {:+.1}px | pill {} | loading line {} | pin following={} away={} \
         older={} loading={} | rows built {rows} | renders {renders}",
        report.size.0,
        report.size.1,
        report.page.0,
        report.page.1,
        f32::from(report.pane.size.width),
        f32::from(report.pane.size.height),
        report.head.unwrap_or(f32::NAN),
        report.foot.unwrap_or(f32::NAN),
        if report.pill { "shown" } else { "hidden" },
        if report.loading_line {
            "shown"
        } else {
            "hidden"
        },
        report.following,
        report.away,
        report.older,
        report.loading,
    );
}

// ---------------------------------------------------------------------------
// The states.
// ---------------------------------------------------------------------------

/// The zoom a state draws at, for the two zoom states.
fn set_zoom(cx: &mut HeadlessAppContext, scale: f32) {
    cx.update(|cx| TranscriptZoom(scale).set(cx));
}

fn click(cx: &mut HeadlessAppContext, window: AnyWindowHandle, id: &'static str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .expect("the capture window is open");
}

/// The view the page hosts.
fn transcript_of(cx: &mut HeadlessAppContext, page: &Entity<Page>) -> Entity<TranscriptView> {
    let page = page.clone();
    cx.update(|cx| page.read(cx).transcript.clone())
}

/// Grow the last message the way a stream of `item.append` ops does, and say whether the
/// reader is still on the tail after each one.
fn stream(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) {
    let view = transcript_of(cx, page);
    for step in 1..=6 {
        cx.update(|cx| {
            let last = view
                .read(cx)
                .items(cx)
                .last()
                .expect("the record ends on a message")
                .clone();
            let mut raw = last.raw().clone();
            let text = raw.get("text").and_then(Value::as_str).unwrap_or("");
            raw["text"] = json!(format!(
                "{text}\n\nThe {step}th paragraph of the same message, still arriving: the \
                 document is extended, never parsed again.\n"
            ));
            raw["status"] = json!("streaming");
            view.update(cx, |view, cx| {
                view.upsert(item(raw), cx);
            });
        });
        frames(cx, window, 2);
        let following = cx.update(|cx| view.read(cx).is_following_tail(cx));
        println!("[states] stream step {step}: following={following}");
    }
}

// ---------------------------------------------------------------------------
// The two corners of a tool card's own ink.
// ---------------------------------------------------------------------------

/// The heads of the tool rows the pane has built, with where each was painted: the
/// list builds a row when the pane reaches it, so this is the pane's own window onto
/// the cards again.
fn tool_heads(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Vec<(ElementId, Bounds<Pixels>)> {
    let mut heads = Vec::new();
    for n in 0..ITEMS {
        let head: ElementId = (
            ElementId::from(SharedString::from("transcript-tool")),
            item_id(n),
        )
            .into();
        let found = cx
            .update_window(window, |_, window, _| {
                window.try_find(head.clone()).map(|fact| fact.bounds())
            })
            .expect("the capture window is open");
        if let Some(bounds) = found {
            heads.push((head, bounds));
        }
    }
    heads
}

/// A pane that has stopped moving: opening a card changes the height of a row above
/// the one a reader is looking at, so a head probed before the list has settled is a
/// head in the wrong place.
fn settle(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    for _ in 0..6 {
        cx.run_until_parked();
        frames(cx, window, 2);
    }
}

/// Put the pointer on a row, as a reader's does.
fn hover(cx: &mut HeadlessAppContext, window: AnyWindowHandle, head: ElementId) {
    cx.update_window(window, |_, window, cx| window.hover(head, cx))
        .expect("the capture window is open");
    settle(cx, window);
}

fn main() {
    let dir: PathBuf = std::env::args()
        .skip_while(|arg| arg != "--capture")
        .nth(1)
        .unwrap_or_else(|| "/tmp/transcript-states".to_string())
        .into();
    std::fs::create_dir_all(&dir).expect("the capture directory");

    let only: Option<String> = std::env::args().skip_while(|arg| arg != "--only").nth(1);
    let items = record();
    println!("[states] the record: {} items", items.len());

    // The states, in the order they are read: the list's own places, a tool card's own
    // two corners, then the pill and the walk back, then a message growing, then the
    // scrollback's quiet line, then the reader's zoom, then a window that changes shape.
    type Setup = fn(&mut HeadlessAppContext, AnyWindowHandle, &Entity<Page>);
    let states: [(&str, f32, Setup); 15] = [
        ("bottom", 1., |_, _, _| {}),
        ("tool-hover", 1., |cx, window, _| {
            // A folded card under the pointer: the head's hover ink is the whole of
            // the card's inside, so all four of its corners are the card's own.
            settle(cx, window);
            let shown = tool_heads(cx, window).into_iter().find(|(_, bounds)| {
                bounds.top() >= px(60.) && bounds.bottom() <= px(WINDOW_SIZE.1)
            });
            if let Some((head, _)) = shown {
                hover(cx, window, head);
            }
        }),
        ("tool-open", 1., |cx, window, _| {
            // An open card: the head's own ink reaches the top corners, and the body's
            // fill the bottom ones, with the head's underside square between them.
            settle(cx, window);
            let last = tool_heads(cx, window).pop();
            if let Some((head, _)) = last {
                cx.update_window(window, |_, window, cx| window.click(head.clone(), cx))
                    .expect("the capture window is open");
                settle(cx, window);
                hover(cx, window, head);
            }
        }),
        ("scrolled-up", 1., |cx, window, _| {
            for _ in 0..3 {
                wheel(cx, window, SCREEN);
            }
        }),
        ("top", 1., |cx, window, _| {
            match wheel_to_the_head(cx, window) {
                Some(wheels) => println!(
                    "[states] top: {wheels} wheels of {}px reached the head",
                    SCREEN * 2.
                ),
                None => println!("[states] top: the head was never reached"),
            }
        }),
        ("jump-back", 1., |cx, window, page| {
            for _ in 0..3 {
                wheel(cx, window, SCREEN);
            }
            let view = transcript_of(cx, page);
            let away = cx.update(|cx| view.read(cx).is_away_from_latest(cx));
            println!("[states] jump-back: away before the press={away}");
            click(cx, window, "transcript-jump");
            run_animation(cx, window, Duration::from_millis(240));
        }),
        ("streaming", 1., |cx, window, page| {
            stream(cx, window, page);
        }),
        ("loading-older", 1., |cx, window, page| {
            let view = transcript_of(cx, page);
            cx.update(|cx| view.update(cx, |view, cx| view.set_history(true, true, cx)));
            match wheel_to_the_head(cx, window) {
                Some(wheels) => println!(
                    "[states] loading-older: {wheels} wheels of {}px reached the head",
                    SCREEN * 2.
                ),
                None => println!("[states] loading-older: the head was never reached"),
            }
        }),
        ("zoom-150-bottom", 1.5, |cx, window, _| {
            frames(cx, window, 3);
        }),
        ("zoom-75-bottom", 0.75, |cx, window, _| {
            frames(cx, window, 3);
        }),
        ("mid", 1., |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
        }),
        ("zoom-150-mid", 1.5, |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
        }),
        ("zoom-75-mid", 0.75, |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
        }),
        ("resize", 1., |cx, window, _| {
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1 - 240.)));
            })
            .expect("the capture window is open");
            frames(cx, window, 3);
        }),
        ("resize-back", 1., |cx, window, _| {
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1 - 240.)));
            })
            .expect("the capture window is open");
            frames(cx, window, 2);
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)));
            })
            .expect("the capture window is open");
            frames(cx, window, 3);
        }),
    ];

    let mut cx = context();
    for (name, scale, setup) in states {
        if only.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        let (window, page) = open(&mut cx, &items);
        // The zoom is one global for the whole app (§7.2), so every state says which
        // scale it draws at rather than inheriting the last state's.
        set_zoom(&mut cx, scale);
        frames(&mut cx, window, 3);
        setup(&mut cx, window, &page);
        // Let anything the state set in motion come to rest before the picture is taken:
        // an animation's steps, and the formulas the app lays out off the frame — a
        // formula a frame first meets draws its source, and the picture lands once the
        // executor has run the job and the document has laid out again.
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
            frames(&mut cx, window, 2);
        }

        let report = measure(&mut cx, window, &page);
        say(name, &report);

        for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            cx.update_window(window, |_, window, cx| {
                gpui_kit::component::Theme::change(mode, Some(window), cx);
            })
            .expect("the capture window is open");
            // A theme is another ink, and a formula's picture is keyed by its ink: the
            // change sends every formula on screen back to the executor, and the frames
            // in between draw its source. Wait for the pictures as after the setup, or
            // the capture shows TeX where the app would show it only for a moment.
            for _ in 0..12 {
                std::thread::sleep(Duration::from_millis(20));
                cx.run_until_parked();
                frames(&mut cx, window, 2);
            }
            let image = cx.capture_screenshot(window).expect("a frame to capture");
            let path = dir.join(format!("{name}-{suffix}.png"));
            image.save(&path).expect("write the picture");
            println!(
                "[states] {name:<16} {}x{} -> {}",
                image.width(),
                image.height(),
                path.display()
            );
        }
    }
}
