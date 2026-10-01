//! frame_cost — what one frame of a long transcript costs, in a headless window the
//! shape of the app's own.
//!
//! The freeze this prices: a window whose transcript holds a long record — a
//! coordinator's journal of a thousand items — with something *else* animating on its own
//! schedule (a busy lane's breathing dot, `Animation::repeat` →
//! `request_animation_frame`). Every frame that dot asks for is a frame the whole window
//! is laid out in, and gpui re-renders every view it has not been told to cache. The
//! transcript is the one subtree where that costs anything.
//!
//! ```sh
//! cargo run --release -p workspace --example frame_cost
//! ```
//!
//! # What it draws, and what it times
//!
//! * **The page** is a stand-in for `crates/workspace/src/tab_page.rs`: the transcript
//!   embedded the way this tree embeds it (see [`embed`]), in a `flex_1` box, with a dot
//!   under it. The dot breathes and notifies itself once per frame, as a busy lane's does
//!   for as long as a lane works.
//! * **A frame** is `Window::draw` — the app's own main-thread frame, triggered by that
//!   notification rather than by a forced redraw: layout, prepaint and paint. The GPU
//!   present is not in the clock, and nothing in that path scales with row count.
//! * **The record** is 1024 items of the kinds a session writes: user turns, markdown
//!   answers with headings, tables and code fences, tool calls with long arguments and
//!   long results, lane reports, notices, goal transitions. 300 frames per scenario,
//!   after 30 warm-up frames.
//! * **The platform** is headless with the real macOS text system
//!   (`gpui_kit::platform::current_platform(true)`), so rows are shaped and measured for
//!   real — and no window appears on anyone's screen.
//!
//! Five scenarios, and a sweep:
//!
//! | | |
//! |---|---|
//! | `steady` | a stray frame: the dot animates, the transcript does not change |
//! | `streaming` | a delta a frame lands on the view (`upsert`), the app's own streaming path |
//! | `select (cold view)` | an agent shown for the first time: a new `TranscriptView`, seeded with the record, swapped into the column, first frame timed — markdown parsed fresh |
//! | `select (back to a lane / to the coordinator)` | two transcripts alive at once, as `TabContent::transcripts` keeps them, switching between them |
//! | `record 32…2048` | the same steady frame as the record grows |
//!
//! # The guards — why a cheap frame is not an empty one
//!
//! A cached view is laid out from the style it is handed and never measured from its rows,
//! so a style that does not size it leaves it 0px tall: nothing built, nothing drawn, and
//! a wonderful number (bae5a68 is that bug). Every scenario therefore asserts, before its
//! figure is printed, that
//!
//! * the transcript's box is **over half the page** (688px of 700 here);
//! * the **record's own tail rows were built** into it (element probes for
//!   `("transcript-row", id)`): 16/16 before, 3/16 after — the pane;
//! * the frame **has ink** in that box (the frame rendered to an image; 28–49% of the box
//!   differs from its own background), so the view is drawn, not merely laid out;
//! * the **page rendered** on every timed frame, and where the counters exist the
//!   **view rendered** at least once per select or switch — a frame that rendered nothing
//!   is not cheap, it is empty.
//!
//! # Results
//!
//! Apple M4 Pro, 14 cores, 48 GB; 1024-item record; window 1000x700 headless. `before` =
//! 7ca92c5 (the tree the fix lands on top of), `after` = d547ad0 (the merge); each figure
//! is the median over six runs of that run's median (and of that run's p95).
//!
//! | scenario | before med/p95 ms | after med/p95 ms | speedup |
//! |---|---|---|---|
//! | steady (dot animating) | 34.80 / 36.05 | 0.12 / 0.12 | 290× |
//! | streaming (a delta a frame) | 35.15 / 37.36 | 0.54 / 0.58 | 65× |
//! | select (cold view) | 36.84 / 39.27 | 0.13 / 0.17 | 283× |
//! | select (back to a lane) | 15.87 / 17.02 | 0.15 / 0.21 | 106× |
//! | select (back to the coordinator) | 35.97 / 37.34 | 0.11 / 0.12 | 327× |
//!
//! Implied frame rate, before → after: 29 → 8333 fps steady, 28 → 1852 streaming, 27 →
//! 7692 cold, 63 → 6667 back to a lane, 28 → 9091 back to the coordinator. Rows a frame
//! built, after: 0.0 steady (a stray frame does not re-render the cached view at all),
//! 8.0 streaming, 10.0 cold, 7.2 and 3.0 per switch — a pane of rows, whatever the
//! journal holds. The before tree has no such counters, but its guard shows the whole
//! 256-item window (`WINDOW_ITEMS = PAGE_ITEMS = 256`) built on every frame.
//!
//! The sweep, median ms/frame (one run each side): 32 → 3.4, 64 → 7.7, 128 → 15.6, 256 →
//! 36.4, 512 → 35.9, 2048 → 36.9 before; 0.11, 0.12, 0.15, 0.13, 0.11, 0.15 after. Before
//! is linear in the record until the window is full and flat after; after is flat
//! throughout.
//!
//! # Caveats
//!
//! * Headless: no OS compositor, and the GPU present is outside the clock (the one place
//!   pixels are read is the guard's screenshot).
//! * The machine was shared with other builds: before's steady median moved between 34.3
//!   and 36.2 ms across six runs, and its p95 by more. The after column is stable to
//!   0.01 ms.
//! * The page mirrors `render_conversation_column`'s *embedding*, not the whole
//!   `TabContent` — driving the real tab from an example is impossible (`transcripts` and
//!   `state` are `pub(crate)`, and `select_agent` returns early with no live session).
//! * The record is a synthetic journal of realistic shape, not a replayed real one.
//!
//! # Reproducing the before column
//!
//! The file is portable between the two trees; two things differ, and both are marked in
//! it:
//!
//! 1. **The embedding.** `git worktree add --detach /tmp/before 7ca92c5`, copy this file
//!    to `crates/workspace/examples/frame_cost.rs` there, and replace [`embed`]'s body
//!    with the plain-child version quoted in its doc comment (that tree's
//!    `render_conversation_column`).
//! 2. **The counters.** That tree's transcript has no `counted` module and no
//!    `TranscriptView::renders`, so make the `counters` module at the foot of this file
//!    say so: `reset` an empty body, `rows`/`renders` returning `None`, and drop its
//!    `use transcript::counted;` line. The bench then prints `-` for rows and renders,
//!    and the row guard still reports what was built.
//!
//! Then `CARGO_TARGET_DIR=/tmp/bench-target cargo run --release -p workspace --example
//! frame_cost` in each tree.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::{v_flex, Theme};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, point, px, size, AnyElement, AnyWindowHandle, AppContext as _, Bounds, Context, ElementId,
    Entity, HeadlessAppContext, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Window, WindowBounds, WindowOptions,
};
use session::{AgentKey, Item};
use transcript::TranscriptView;

/// The window the app's own tests open a tab in.
const WINDOW: (f32, f32) = (1000., 700.);
/// How many items the record holds in the headline scenarios: a real journal's size,
/// and well past the 256-item window an unvirtualised transcript keeps built.
const ITEMS: usize = 1024;
/// Frames timed per scenario, after the warm-up.
const FRAMES: usize = 300;
/// Frames drawn before the clock starts: the first frame measures and shapes
/// everything cold, and the caches are warm for every frame after it.
const WARMUP: usize = 30;
/// Frames per step of the scaling sweep, which is a shape not a measurement.
const SWEEP_FRAMES: usize = 60;
/// How many items the lane in the switching scenario holds: a lane's transcript is
/// shorter than the coordinator's, and switching must not care.
const LANE_ITEMS: usize = 128;
/// How many times the clock is run per scenario that draws a single frame per run.
const SELECTS: usize = 20;

// --- the bench's own page ------------------------------------------------------------------

/// A busy lane's dot: 8px, breathing, and asking the window for the next frame every
/// frame it draws — `agent_list`'s `BreathingDot`, which is what keeps a window with a
/// working lane drawing at all.
struct BreathingDot {
    phase: f32,
}

impl Render for BreathingDot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let swell = 1.0 + 0.25 * self.phase.sin();
        div()
            .id("breathing-dot")
            .w_full()
            .h(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .size(px(8. * swell))
                    .rounded_full()
                    .bg(gpui_kit::rgb(0x2f6f4f)),
            )
    }
}

impl BreathingDot {
    /// One breath: the dot asks for the next frame, as an animation does. The bench
    /// draws that frame itself, so the request is the notify.
    fn breathe(&mut self, cx: &mut Context<Self>) {
        self.phase += 0.35;
        cx.notify();
    }
}

thread_local! {
    /// How many times the bench's page has rendered — one per frame drawn.
    static PAGE_RENDERS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn page_renders() -> u64 {
    PAGE_RENDERS.with(std::cell::Cell::get)
}

/// The bench's page: the transcript in its box, the dot under it, and nothing else —
/// the smallest window that shows the freeze.
struct Page {
    transcript: Entity<TranscriptView>,
    dot: Entity<BreathingDot>,
}

impl Page {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
            dot: cx.new(|_| BreathingDot { phase: 0.0 }),
        }
    }

    /// Show another agent's transcript in the conversation column, as selecting an agent
    /// does: the page renders again, and what it embeds is a different view.
    fn show(&mut self, transcript: Entity<TranscriptView>, cx: &mut Context<Self>) {
        if self.transcript == transcript {
            return;
        }
        self.transcript = transcript;
        cx.notify();
    }
}

impl Render for Page {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Proof that the frame really was a frame: gpui renders the root view
        // unconditionally, so this is one per draw — and a draw that did not render the
        // page would not have re-rendered an uncached transcript either, which is the
        // whole comparison.
        PAGE_RENDERS.with(|renders| renders.set(renders.get() + 1));
        v_flex()
            .id("bench-page")
            .test_support()
            .size_full()
            .child(embed(self.transcript.clone()))
            .child(self.dot.clone())
    }
}

/// How this tree embeds a transcript in the conversation column.
///
/// Copied from `crates/workspace/src/tab_page.rs` — `render_conversation_column` — which
/// is the file the app renders. This is the one function the two trees differ in, and it
/// is the fix being measured:
///
/// ```ignore
/// // before: a plain child — a view gpui re-renders with every frame that touches the page
/// div().flex_1().min_h_0().children(Some(view)).into_any_element()
///
/// // after: a cached child — re-rendered only when it or the data it reads is notified.
/// // `size_full()`, not `flex_1()`: a cached view is laid out from this style alone and
/// // never measured from its rows, so the style has to size it outright (bae5a68).
/// div().flex_1().min_h_0()
///     .child(AnyView::from(view).cached(StyleRefinement::default().size_full()))
///     .into_any_element()
/// ```
fn embed(transcript: Entity<TranscriptView>) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .child(
            gpui_kit::AnyView::from(transcript)
                .cached(gpui_kit::StyleRefinement::default().size_full()),
        )
        .into_any_element()
}

// --- the record ----------------------------------------------------------------------------

/// A journal the shape and size of a real session's: markdown answers, tool calls with
/// long arguments and results, lane reports, notices, goal transitions, and the user
/// turns between them.
fn record(items: usize) -> Vec<Item> {
    let mut out = Vec::with_capacity(items);
    let mut n = 0;
    while out.len() < items {
        n += 1;
        for item in cycle(n) {
            if out.len() == items {
                break;
            }
            out.push(item);
        }
    }
    out
}

/// The ids of one cycle's items, in the order [`cycle`] writes them: what the guard
/// looks for on screen.
fn cycle_ids(n: usize) -> [String; 10] {
    [
        format!("u_{n}"),
        format!("a_{n}"),
        format!("x_{n}_bash"),
        format!("s_{n}"),
        format!("r_{n}"),
        format!("n_{n}"),
        format!("x_{n}_grep"),
        format!("g_{n}"),
        format!("t_{n}"),
        format!("x_{n}_read"),
    ]
}

/// The ids of the record's own last `tail` items, newest last.
fn tail_ids(items: usize, tail: usize) -> Vec<String> {
    let mut ids = Vec::new();
    let mut n = 0;
    while ids.len() < items {
        n += 1;
        for id in cycle_ids(n) {
            if ids.len() == items {
                break;
            }
            ids.push(id);
        }
    }
    let keep = tail.min(ids.len());
    ids.split_off(ids.len() - keep)
}

/// One round of the conversation: the kinds a session actually writes, in the order it
/// writes them.
fn cycle(n: usize) -> Vec<Item> {
    let [user, answer, bash, short, report, notice, grep, goal, thinking, read] = cycle_ids(n);
    vec![
        user_turn(n, user),
        markdown_answer(n, answer),
        tool_call(n, bash, "bash", false),
        short_answer(n, short),
        lane_report(n, report),
        notice_line(n, notice),
        tool_call(n, grep, "grep", true),
        goal_line(n, goal),
        answer_with_thinking(n, thinking),
        tool_call(n, read, "read", false),
    ]
}

fn item(value: serde_json::Value) -> Item {
    Item::from_json(&value).expect("the bench's own fixtures have ids")
}

fn user_turn(n: usize, id: String) -> Item {
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_000u64 + n as u64, "kind": "user",
        "status": "sent",
        "text": format!(
            "Step {n}: the transcript still stutters while a lane is working.\n\
             Look at how the rows are built each frame, and say what you changed.\n\
             Keep the reading position where I left it."
        ),
    }))
}

/// A multi-paragraph answer with everything the renderer has to lay out: a heading, a
/// list, a table, a fenced code block and prose.
fn markdown_answer(n: usize, id: String) -> Item {
    let text = format!(
        "## Step {n}: the rows are built per frame\n\n\
         The view holds the whole record, but only the newest {window} items are built \
         into rows, and it builds all {window} of them on every frame — there is no \
         list state, so nothing is measured once and reused. The element tree is \
         therefore about {rows} rows deep every 16 ms, each one a paragraph of shaped \
         text, and the layout pass walks all of it.\n\n\
         What the frame does, in order:\n\n\
         - the root view renders, which is what a notification from anywhere does;\n\
         - every view under it that is not cached renders too — the transcript among \
           them, since it is a plain child of the column;\n\
         - the transcript builds its window of rows into one scroll box;\n\
         - the layout engine measures the box, then paints it.\n\n\
         | what | before | after |\n\
         | --- | --- | --- |\n\
         | rows built per frame | {window} | a pane |\n\
         | list | one box | virtual |\n\
         | cost | grows with the window | flat |\n\n\
         The fix is two things, and they are independent:\n\n\
         ```rust\n\
         // 1. the rows: gpui::list + ListState, over the whole record\n\
         let list = ListState::new(record.len(), ListAlignment::Top, OVERDRAW);\n\
         \n\
         // 2. the view: a cached child, so a frame that is not about it does not render it\n\
         AnyView::from(view).cached(StyleRefinement::default().size_full())\n\
         ```\n\n\
         Neither alone is enough: the list without the cache still builds a pane of rows \
         per stray frame, and the cache without the list still builds the whole window \
         whenever the transcript itself changes — which, while an answer streams, is \
         every frame.\n",
        window = 256,
        rows = 256,
    );
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_100u64 + n as u64, "kind": "assistant",
        "status": "final", "text": text,
    }))
}

fn short_answer(n: usize, id: String) -> Item {
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_200u64 + n as u64, "kind": "assistant",
        "status": "final",
        "text": format!("Row {n} lands where it should: the list measures a row once and \
                         keeps the measurement, so the wheel is cheap too."),
    }))
}

fn answer_with_thinking(n: usize, id: String) -> Item {
    let thinking = (0..6)
        .map(|i| {
            format!(
                "Consideration {i} for step {n}: the list owns the rows' positions, so a \
                 row that is spliced in does not move the ones above it — unless the \
                 reader is at the tail, where following the newest row is the point.\n"
            )
        })
        .collect::<String>();
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_300u64 + n as u64, "kind": "assistant",
        "status": "final", "thinking": thinking,
        "text": format!("Done with step {n}. The rows are flat now; next I will re-take \
                         the numbers on the cold path."),
    }))
}

fn tool_call(n: usize, id: String, name: &str, truncated: bool) -> Item {
    let result: String = (0..18)
        .map(|line| {
            format!(
                "crates/transcript/src/lib.rs:{}: warning: row {line} of step {n} was \
                 rebuilt on a frame that did not touch it\n",
                900 + line
            )
        })
        .collect();
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_400u64 + n as u64, "kind": "tool",
        "call_id": format!("call_{n}"), "name": name, "status": "ok",
        "args": {
            "command": format!(
                "cargo test -p transcript --release -- --nocapture step_{n}"
            ),
            "cwd": "/Users/dev/coding/evo/evo-gui",
            "timeout_ms": 120_000,
            "env": {"RUST_BACKTRACE": "0", "EVO_HOME": "/Users/dev/.evo"},
            "note": "the long argument the row has to elide rather than draw whole",
        },
        "result": {
            "text": result,
            "chars": result.chars().count(),
            "truncated": truncated,
        },
    }))
}

fn lane_report(n: usize, id: String) -> Item {
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_500u64 + n as u64, "kind": "lane_report",
        "lane": 1 + (n % 6),
        "done": format!("virtualised the rows of step {n}; the list now owns the layout"),
        "evidence": "cargo test -p transcript --release (113 passed); \
                     cargo run --release -p workspace --example frame_cost",
        "next": "re-take the numbers with a busy lane beside it",
        "blocked": "",
        "requests": "a `cached` view is what keeps a stray frame out; it is in the tree now",
        "goal": "active",
    }))
}

fn notice_line(n: usize, id: String) -> Item {
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_600u64 + n as u64, "kind": "notice",
        "severity": "warn", "source": "swarm", "durable": true,
        "text": format!(
            "step {n}: lane {} is still working; the coordinator is held while it does",
            1 + (n % 6)
        ),
    }))
}

fn goal_line(n: usize, id: String) -> Item {
    item(serde_json::json!({
        "id": id, "ts": 1_700_000_000_700u64 + n as u64, "kind": "goal",
        "event": "continue", "goal_id": format!("g-{n:04x}"),
        "objective": "make a long transcript cost a pane of rows a frame, not the whole window",
        "budget": 500_000, "tokens": 120_000 + n as u64,
    }))
}

/// The answer an assistant is still writing: what the streaming scenario grows.
fn confirming_item(n: usize) -> Item {
    item(serde_json::json!({
        "id": format!("stream_{n}"), "ts": 1_700_000_001_000u64 + n as u64, "kind": "assistant",
        "status": "streaming",
        "text": "The list is measured once. ".repeat(4),
    }))
}

// --- the transcript's own bookkeeping -------------------------------------------------------

/// The rows a frame built, and the frames the view rendered, where the tree's transcript
/// keeps them (`transcript::counted`, behind its `test-support` feature) — and nothing
/// where it does not.
/// The transcript's own bookkeeping — the rows a frame built (`transcript::counted`) and
/// the frames the view rendered (`TranscriptView::renders`) — both behind that crate's
/// `test-support` feature, which this crate's dev-dependency turns on for its tests and
/// its examples.
///
/// **This module is the file's one link to the post-fix tree.** On a tree whose transcript
/// predates the counters (7ca92c5 — the before column) there is no `counted` module and no
/// `TranscriptView::renders`: make `reset` an empty body, `rows` and `renders` return
/// `None`, and delete the `use transcript::counted;` line. The bench then prints `-` where
/// the counters would be, and the row guard still says whether the window's rows were
/// built.
mod counters {
    use gpui_kit::{App, Entity};
    use transcript::counted;
    use transcript::TranscriptView;

    pub fn reset() {
        counted::reset();
    }

    pub fn rows() -> Option<u64> {
        Some(counted::rows())
    }

    pub fn renders(view: &Entity<TranscriptView>, cx: &App) -> Option<u64> {
        Some(view.read(cx).renders())
    }
}

// --- checking the number means something ----------------------------------------------------

/// The row element an item is drawn in — `row_id` in `crates/transcript/src/rows.rs`,
/// which is private to that crate: the row's own name and the item's id.
fn row_id(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
}

/// How much of a box the screenshot has ink in: the fraction of sampled pixels that
/// differ from the box's own most common colour. A view that is laid out but never
/// drawn — or drawn 0px tall — is `0.0`, however healthy its element bounds look.
fn ink_in(image: &image::RgbaImage, box_: Bounds<gpui_kit::Pixels>, scale: f32) -> f64 {
    use std::collections::HashMap;
    let left = (f32::from(box_.origin.x) * scale).max(0.) as u32;
    let top = (f32::from(box_.origin.y) * scale).max(0.) as u32;
    let right = ((f32::from(box_.origin.x + box_.size.width) * scale) as u32).min(image.width());
    let bottom = ((f32::from(box_.origin.y + box_.size.height) * scale) as u32).min(image.height());
    if right <= left || bottom <= top {
        return 0.0;
    }
    let mut counts: HashMap<[u8; 3], u64> = HashMap::new();
    let mut samples = 0u64;
    for y in (top..bottom).step_by(2) {
        for x in (left..right).step_by(2) {
            let pixel = image.get_pixel(x, y).0;
            let key = [pixel[0] >> 3, pixel[1] >> 3, pixel[2] >> 3];
            *counts.entry(key).or_default() += 1;
            samples += 1;
        }
    }
    let Some((background, _)) = counts.iter().max_by_key(|(_, count)| **count) else {
        return 0.0;
    };
    let mut different = 0u64;
    for y in (top..bottom).step_by(2) {
        for x in (left..right).step_by(2) {
            let pixel = image.get_pixel(x, y).0;
            let key = [pixel[0] >> 3, pixel[1] >> 3, pixel[2] >> 3];
            if key != *background {
                different += 1;
            }
        }
    }
    different as f64 / samples as f64
}

/// What a scenario proved about the transcript being on screen at all.
#[derive(Clone, Copy)]
struct Guard {
    /// The transcript's own box, and the page it is in, in px.
    transcript: f32,
    page: f32,
    /// How many of the record's last 16 rows were built, and how many of those are
    /// inside the transcript's box rather than clipped out of it.
    built: usize,
    on_screen: usize,
    /// The fraction of the transcript's own box the screenshot has ink in.
    ink: f64,
}

impl Guard {
    fn describe(&self) -> String {
        format!(
            "transcript {:.0}px of {:.0}px, {}/16 tail rows built, {} on screen, {:.1}% of \
             the box has ink",
            self.transcript,
            self.page,
            self.built,
            self.on_screen,
            self.ink * 100.
        )
    }
}

/// Prove the transcript is really in the window, at the size of the pane, with the
/// record's own rows built into it — then its number means something.
///
/// This is the check that catches the one way this bench can lie: a cached view is laid
/// out from the style it is handed and never measured from its rows, so a style that
/// does not size it (`flex_1` in a block box) leaves it 0px tall, draws nothing, builds
/// no rows and costs nothing — the numbers look wonderful and the app shows an empty
/// column (`bae5a68`).
fn assert_showing(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    record_items: usize,
) -> Guard {
    let (transcript, page_box, built, on_screen, scale) = cx
        .update_window(window, |_, window, cx| {
            // A frame first, so what is inspected is what the last draw left behind.
            page.update(cx, |page, cx| {
                page.dot.update(cx, |dot, cx| dot.breathe(cx))
            });
            window.draw(cx).clear(cx);

            let transcript = window.find("transcript").bounds();
            let page_box = window.find("bench-page").bounds();
            assert!(
                transcript.size.height > page_box.size.height / 2.,
                "the transcript is not on screen: its box is {:?} in a {:?} page. A cached \
                 view laid out from a style that does not size it is 0px tall.",
                transcript,
                page_box
            );
            assert!(
                transcript.size.width > px(0.),
                "the transcript has no width: {transcript:?}"
            );

            let mut built = 0;
            let mut on_screen = 0;
            for id in tail_ids(record_items, 16) {
                let Some(row) = window.try_find(row_id("transcript-row", &id)) else {
                    continue;
                };
                built += 1;
                // The row is painted (not clipped away) if its own box overlaps the
                // transcript's: rows below the pane are built and laid out, but they are
                // not on screen, and only the ones on screen are rows a reader sees.
                let row_box = row.bounds();
                if row_box.bottom() > transcript.top() && row_box.top() < transcript.bottom() {
                    on_screen += 1;
                }
            }
            assert!(
                on_screen > 0,
                "no row of the record's tail is on screen ({built}/16 built): the transcript \
                 is up but empty, which is not a frame cost worth reporting"
            );
            (
                transcript,
                page_box,
                built,
                on_screen,
                window.scale_factor(),
            )
        })
        .expect("the bench's window");

    // And the pixels: element bounds are a view's own say-so, so the frame is rendered
    // to an image and the transcript's box is looked at. A view that is laid out but
    // never drawn has no ink in it.
    let image = cx
        .capture_screenshot(window)
        .expect("a screenshot of the bench's window");
    let ink = ink_in(&image, transcript, scale);
    assert!(
        ink > 0.005,
        "the transcript's box is blank: {:.3}% of it differs from its own background \
         (the view is not being drawn, whatever its bounds say)",
        ink * 100.
    );

    Guard {
        transcript: f32::from(transcript.size.height),
        page: f32::from(page_box.size.height),
        built,
        on_screen,
        ink,
    }
}

// --- measuring ------------------------------------------------------------------------------

/// What one scenario cost.
struct Report {
    name: &'static str,
    frames: Vec<Duration>,
    /// Rows the transcript built, summed over the timed frames (`None`: no counters).
    rows: Option<u64>,
    /// Frames the transcript view rendered, summed over the timed frames.
    renders: Option<u64>,
    /// Frames the page itself rendered, summed over the timed frames: one per draw, so
    /// a scenario whose page never rendered is not a scenario at all.
    page_renders: u64,
    /// What the scenario proved about being on screen at all.
    guard: Guard,
}

impl Report {
    fn sorted(&self) -> Vec<Duration> {
        let mut times = self.frames.clone();
        times.sort();
        times
    }

    fn median(&self) -> Duration {
        let times = self.sorted();
        times[times.len() / 2]
    }

    fn p95(&self) -> Duration {
        let times = self.sorted();
        times[(times.len() * 95) / 100]
    }

    fn max(&self) -> Duration {
        *self.sorted().last().expect("frames were timed")
    }

    fn mean(&self) -> Duration {
        self.frames.iter().sum::<Duration>() / self.frames.len() as u32
    }

    fn fps(&self) -> f64 {
        1.0 / self.median().as_secs_f64()
    }

    /// Rows built per frame, averaged over the timed frames.
    fn rows_per_frame(&self) -> Option<f64> {
        self.rows.map(|rows| rows as f64 / self.frames.len() as f64)
    }

    fn renders_per_frame(&self) -> Option<f64> {
        self.renders
            .map(|renders| renders as f64 / self.frames.len() as f64)
    }

    /// A cheap frame is only meaningful if it was a frame at all: gpui renders the root
    /// view on every draw, so every timed frame must have rendered the page that holds
    /// the transcript. This is what keeps a "0.1 ms" from meaning "nothing happened".
    ///
    /// At least one, not exactly one: a scenario that changes the record asks for
    /// another draw inside the same frame (the streaming case renders the page twice),
    /// and that second pass is inside the clock like the first.
    fn assert_a_frame_per_frame(&self) {
        assert!(
            self.page_renders >= self.frames.len() as u64,
            "{}: the page rendered {} times over {} timed frames — a frame that rendered \
             nothing is not cheap, it is empty",
            self.name,
            self.page_renders,
            self.frames.len()
        );
    }

    /// The page's own renders per timed frame.
    fn page_renders_per_frame(&self) -> f64 {
        self.page_renders as f64 / self.frames.len() as f64
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

/// Draw one frame the way the app draws it: a notification from the dot, then the
/// window's own draw — the frame a person waiting on the UI thread pays for. The clock
/// covers the draw alone.
fn frame(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) -> Duration {
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| {
            page.dot.update(cx, |dot, cx| dot.breathe(cx))
        });
        let started = Instant::now();
        window.draw(cx).clear(cx);
        started.elapsed()
    })
    .expect("the bench's window")
}

/// The first frame after the view was notified — selecting the transcript, or a
/// snapshot landing on it. Nothing is notified here: the frame is drawn because the
/// view asked for one.
fn frame_now(cx: &mut HeadlessAppContext, window: AnyWindowHandle) -> Duration {
    cx.update_window(window, |_, window, cx| {
        let started = Instant::now();
        window.draw(cx).clear(cx);
        started.elapsed()
    })
    .expect("the bench's window")
}

/// Time `frames` frames of the window as it stands, after a warm-up of the same kind.
fn steady(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    name: &'static str,
    record_items: usize,
    frames: usize,
    warmup: usize,
) -> Report {
    let view = cx.update(|cx| page.read(cx).transcript.clone());
    for _ in 0..warmup {
        frame(cx, window, page);
    }
    let renders_before = cx.update(|cx| counters::renders(&view, cx));
    let page_before = page_renders();
    counters::reset();
    let mut times = Vec::with_capacity(frames);
    for _ in 0..frames {
        times.push(frame(cx, window, page));
    }
    let rows = counters::rows();
    let renders = cx
        .update(|cx| counters::renders(&view, cx))
        .map(|after| after - renders_before.unwrap_or(after));
    let page_renders = page_renders() - page_before;
    let guard = assert_showing(cx, window, page, record_items);
    Report {
        name,
        frames: times,
        rows,
        renders,
        page_renders,
        guard,
    }
}

/// Selecting an agent that has never been shown: a transcript view that has never been
/// on screen, seeded with the whole record, swapped into the conversation column — what
/// `TabContent::select_agent` does the first time an agent is drawn, with every markdown
/// document in the pane's rows parsed from scratch.
///
/// This is not the same measurement as replacing items into the view already on screen:
/// that view keeps its retained documents and its measured heights, and the new code has
/// nothing to do. A cold view is the case that parses.
fn selecting_cold(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    name: &'static str,
    runs: usize,
) -> Report {
    let items = record(ITEMS);
    let counting = counters::rows().is_some();
    let mut rows = 0;
    let mut renders = 0;
    let mut times = Vec::with_capacity(runs);
    let page_before = page_renders();
    for _ in 0..runs {
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(items.clone(), cx);
                view.set_agent(AgentKey::Coordinator, cx);
            })
        });
        counters::reset();
        cx.update(|cx| page.update(cx, |page, cx| page.show(view.clone(), cx)));
        times.push(frame_now(cx, window));
        rows += counters::rows().unwrap_or(0);
        renders += cx.update(|cx| counters::renders(&view, cx)).unwrap_or(0);
    }
    let page_renders = page_renders() - page_before;
    if counting {
        assert!(
            renders >= runs as u64,
            "the cold view never rendered: {renders} renders over {runs} selections, so \
             nothing here is the cost of showing a transcript for the first time"
        );
    }
    let guard = assert_showing(cx, window, page, ITEMS);
    Report {
        name,
        frames: times,
        rows: counting.then_some(rows),
        renders: counting.then_some(renders),
        page_renders,
        guard,
    }
}

/// Selecting an agent that has already been shown, then the other one again: two
/// transcripts alive at once — `TabContent::transcripts` keeps one per agent — and the
/// first frame after each switch timed. Each view keeps its own documents, heights and
/// scroll position, so this is the cheap end of selecting.
///
/// The two directions are reported apart: a lane's transcript is a shorter record than
/// the coordinator's, and in the before tree the cost is the window's rows, so a switch
/// to the short one is cheaper than a switch to the long one.
fn selecting_back(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    pairs: usize,
) -> Vec<Report> {
    let coordinator = cx.update(|cx| cx.new(TranscriptView::new));
    cx.update(|cx| {
        coordinator.update(cx, |view, cx| {
            view.replace(record(ITEMS), cx);
            view.set_agent(AgentKey::Coordinator, cx);
        })
    });
    let lane = cx.update(|cx| cx.new(TranscriptView::new));
    cx.update(|cx| {
        lane.update(cx, |view, cx| {
            view.replace(record(LANE_ITEMS), cx);
            view.set_agent(AgentKey::Lane(1), cx);
        })
    });

    // The coordinator first, and drawn twice: the frame the clock starts on is a switch,
    // not the page's own cold start.
    cx.update(|cx| page.update(cx, |page, cx| page.show(coordinator.clone(), cx)));
    frame_now(cx, window);
    frame_now(cx, window);

    let counting = counters::rows().is_some();
    let mut to_lane = Vec::with_capacity(pairs);
    let mut to_coordinator = Vec::with_capacity(pairs);
    let (mut lane_rows, mut lane_renders) = (0, 0);
    let (mut coordinator_rows, mut coordinator_renders) = (0, 0);
    let page_before = page_renders();
    for _ in 0..pairs {
        let (elapsed, rows, renders) = switch(cx, window, page, &lane);
        to_lane.push(elapsed);
        lane_rows += rows;
        lane_renders += renders;
        let (elapsed, rows, renders) = switch(cx, window, page, &coordinator);
        to_coordinator.push(elapsed);
        coordinator_rows += rows;
        coordinator_renders += renders;
    }
    let page_renders = page_renders() - page_before;

    // Each direction's own guard, from the view that direction shows: the lane first,
    // then back to the coordinator for the frame the rest of the bench continues from.
    cx.update(|cx| page.update(cx, |page, cx| page.show(lane.clone(), cx)));
    frame_now(cx, window);
    let lane_guard = assert_showing(cx, window, page, LANE_ITEMS);
    cx.update(|cx| page.update(cx, |page, cx| page.show(coordinator.clone(), cx)));
    frame_now(cx, window);
    let coordinator_guard = assert_showing(cx, window, page, ITEMS);

    if counting {
        assert!(
            lane_renders >= to_lane.len() as u64
                && coordinator_renders >= to_coordinator.len() as u64,
            "a switch that did not render the view it switched to is not a switch: \
             {lane_renders} renders over {} switches to the lane, {coordinator_renders} \
             over {} back to the coordinator",
            to_lane.len(),
            to_coordinator.len()
        );
    }
    // Two switches (and two timed frames) per pair, one per direction: each direction
    // gets its own share of the frames the page drew.
    let page_renders = page_renders / 2;
    vec![
        Report {
            name: "select (back to a lane)",
            frames: to_lane,
            rows: counting.then_some(lane_rows),
            renders: counting.then_some(lane_renders),
            page_renders,
            guard: lane_guard,
        },
        Report {
            name: "select (back to the coordinator)",
            frames: to_coordinator,
            rows: counting.then_some(coordinator_rows),
            renders: counting.then_some(coordinator_renders),
            page_renders,
            guard: coordinator_guard,
        },
    ]
}

/// Show `view` in the conversation column and time the frame it is drawn in — one
/// switch, with the rows and the renders that one frame cost. The view lives on across
/// switches, so its render count is read as the difference this frame made, not as the
/// total since it was made.
fn switch(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    view: &Entity<TranscriptView>,
) -> (Duration, u64, u64) {
    let renders_before = cx.update(|cx| counters::renders(view, cx));
    counters::reset();
    cx.update(|cx| page.update(cx, |page, cx| page.show(view.clone(), cx)));
    let elapsed = frame_now(cx, window);
    let rows = counters::rows().unwrap_or(0);
    let renders = match (renders_before, cx.update(|cx| counters::renders(view, cx))) {
        (Some(before), Some(after)) => after - before,
        _ => 0,
    };
    (elapsed, rows, renders)
}

/// The record changes under the view every frame — an answer streaming a delta, which
/// is the one thing the cache cannot help with — and the frame is timed.
fn streaming(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    name: &'static str,
    frames: usize,
    warmup: usize,
) -> Report {
    let view = cx.update(|cx| page.read(cx).transcript.clone());
    for _ in 0..warmup {
        stream_delta(&view, cx);
        frame(cx, window, page);
    }
    let renders_before = cx.update(|cx| counters::renders(&view, cx));
    let page_before = page_renders();
    counters::reset();
    let mut times = Vec::with_capacity(frames);
    for _ in 0..frames {
        stream_delta(&view, cx);
        times.push(frame(cx, window, page));
    }
    let rows = counters::rows();
    let renders = cx
        .update(|cx| counters::renders(&view, cx))
        .map(|after| after - renders_before.unwrap_or(after));
    let page_renders = page_renders() - page_before;
    let guard = assert_showing(cx, window, page, ITEMS + 1);
    Report {
        name,
        frames: times,
        rows,
        renders,
        page_renders,
        guard,
    }
}

/// One `text-delta`'s worth of the app's own path: the item the answer is arriving in,
/// grown by a sentence, upserted — the same id every frame, which is what the tab's
/// plan does with `text-delta`s between two frames.
fn stream_delta(view: &Entity<TranscriptView>, cx: &mut HeadlessAppContext) {
    thread_local! {
        static GROWN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let n = GROWN.with(|grown| {
        grown.set(grown.get() + 1);
        grown.get()
    });
    let mut item = confirming_item(0);
    let session::ItemKind::Assistant(assistant) = &mut item.kind else {
        unreachable!("the streaming fixture is an answer")
    };
    // A paragraph, not a growing wall: the answer streams for the length of the run and
    // stays inside the pane, so the record's own rows are still the ones around it.
    assistant.text = "The list is measured once, and the sentence after this one arrives \
                      with the next delta of the answer being written now. "
        .repeat(1 + n % 8);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.upsert(item, cx);
        })
    });
}

/// The shape rather than the number: how the head of the window's frame cost moves with
/// the size of the record.
fn sweep(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
) -> Vec<(usize, Report)> {
    let view = cx.update(|cx| page.read(cx).transcript.clone());
    [32usize, 64, 128, 256, 512, 2048]
        .into_iter()
        .map(|items| {
            cx.update(|cx| view.update(cx, |view, cx| view.replace(record(items), cx)));
            let report = steady(cx, window, page, "sweep", items, SWEEP_FRAMES, 5);
            (items, report)
        })
        .collect()
}

fn main() {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));

    let (window, page) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(WINDOW.0), px(WINDOW.1)),
                    ))),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(Page::new),
            )
        })
        .expect("the bench's window");

    let view = cx.update(|cx| page.read(cx).transcript.clone());
    cx.update(|cx| view.update(cx, |view, cx| view.replace(record(ITEMS), cx)));
    cx.update(|cx| view.update(cx, |view, cx| view.set_agent(AgentKey::Coordinator, cx)));

    let mut reports = Vec::new();
    reports.push(steady(
        &mut cx,
        window,
        &page,
        "steady (dot animating)",
        ITEMS,
        FRAMES,
        WARMUP,
    ));
    reports.push(streaming(
        &mut cx,
        window,
        &page,
        "streaming (a delta a frame)",
        FRAMES,
        10,
    ));
    reports.push(selecting_cold(
        &mut cx,
        window,
        &page,
        "select (cold view)",
        SELECTS,
    ));
    reports.extend(selecting_back(&mut cx, window, &page, SELECTS));

    println!(
        "\n\
         machine          {} — {} cores, {} GB\n\
         window           {}x{} at 1.0, headless\n\
         text system      macOS CoreText (gpui_kit::platform::current_platform(true))\n\
         theme            light\n\
         record           {} items: user turns, markdown answers (headings, lists, tables, \
         code fences), tool calls with long args and results, lane reports, notices, goal \
         transitions\n\
         frames           {} per scenario, after {} warm-up frames ({} for the sweep)\n\
         timed            Window::draw — the main thread's frame, triggered by the dot's \
         notify; no forced redraw, no GPU present in the clock\n\
         guard            every scenario asserts the transcript's box is over half the page \
         and that the record's own tail rows are built and on screen\n",
        machine(),
        std::thread::available_parallelism().map_or(0, |cores| cores.get()),
        memory_gb(),
        WINDOW.0,
        WINDOW.1,
        ITEMS,
        FRAMES,
        WARMUP,
        SWEEP_FRAMES,
    );

    println!(
        "| scenario | median ms | p95 ms | max ms | mean ms | implied fps | rows/frame | \
         view renders | page renders/frame |\n\
         |---|---|---|---|---|---|---|---|---|"
    );
    for report in &reports {
        report.assert_a_frame_per_frame();
        println!(
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.0} | {} | {} | {:.2} |",
            report.name,
            ms(report.median()),
            ms(report.p95()),
            ms(report.max()),
            ms(report.mean()),
            report.fps(),
            report
                .rows_per_frame()
                .map_or_else(|| "-".to_string(), |rows| format!("{rows:.1}")),
            report
                .renders_per_frame()
                .map_or_else(|| "-".to_string(), |renders| format!("{renders:.2}")),
            report.page_renders_per_frame(),
        );
    }
    for report in &reports {
        println!("guard: {} — {}", report.name, report.guard.describe());
    }

    let sizes = sweep(&mut cx, window, &page);
    println!("\n| record | median ms | p95 ms | implied fps | rows/frame |");
    println!("|---|---|---|---|---|");
    for (items, report) in &sizes {
        report.assert_a_frame_per_frame();
        println!(
            "| {items} | {:.2} | {:.2} | {:.0} | {} |",
            ms(report.median()),
            ms(report.p95()),
            report.fps(),
            report
                .rows_per_frame()
                .map_or_else(|| "-".to_string(), |rows| format!("{rows:.1}")),
        );
    }
    for (items, report) in &sizes {
        println!("guard: record {items} — {}", report.guard.describe());
    }
}

fn machine() -> String {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .unwrap_or_default();
    out.trim().to_string()
}

fn memory_gb() -> u64 {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .unwrap_or_default();
    out.trim().parse::<u64>().unwrap_or(0) / 1024 / 1024 / 1024
}
