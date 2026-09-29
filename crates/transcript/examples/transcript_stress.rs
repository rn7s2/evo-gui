//! transcript_stress — what a large transcript costs, measured headless.
//!
//! ```sh
//! cargo run -q -p transcript --example transcript_stress
//! cargo run -q -p transcript --example transcript_stress -- 2000 20000 5
//! ```
//!
//! Three passes, all in a real window with real text shaping and a window's
//! worth of layout, so the numbers mean something:
//!
//! 1. **Install.** `ROWS` mixed rows — long markdown messages (headings, lists,
//!    tables, code fences), tool rows and dim lines — pushed in one `replace`,
//!    the way `/transcript` installs a rebuilt transcript. Every assistant row
//!    gets a document, so this is also what the parser pays for rows nobody has
//!    scrolled to.
//! 2. **At rest.** A frame per step, timed, with the list at the tail: the
//!    distribution of what a frame costs, how many of the rows the list actually
//!    mounted, and how many quads it painted.
//! 3. **Streaming.** A 20 000-character assistant message arriving in 5-character
//!    deltas. Two shapes: a frame after every delta (the worst case: one frame
//!    per update), and fifty deltas per frame (what the tab page's pump hands
//!    over when a burst arrives between two frames).
//!
//! Frames are measured through `Window::render_frame`, which is the UI thread's
//! work — layout, paint and text shaping into a scene. Rasterizing that scene
//! (what `capture_screenshot` does) is a separate, later step and is measured
//! separately once at the end.

use std::time::{Duration, Instant};

use gpui_kit::base::TextViewState;
use gpui_kit::component::text::TextViewMotion;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InputEvent as _, IntoElement, MouseMoveEvent, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, Window, WindowBounds, WindowOptions,
};
use session::{DimStyle, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

use transcript::TranscriptView;

const WINDOW_SIZE: (f32, f32) = (1600., 1000.);
/// Real time to let a fade or a scrollbar settle before a capture, so two
/// captures of the same state are the same picture.
const SETTLE: Duration = Duration::from_millis(500);
/// Default step count and message size; the first two arguments override them.
const DEFAULT_ROWS: usize = 2_000;
const DEFAULT_STREAM_CHARS: usize = 20_000;

/// The rows the streamed messages land on, and the user turns that ask for them:
/// one fresh row per pass, so a pass is never dropped as a stale version.
const STREAM_ROW: RowId = 9_000_001;
const STREAM_ROW_BURST: RowId = 9_000_002;
const STREAM_ASK: RowId = 9_000_000;
const STREAM_ASK_BURST: RowId = 9_000_003;
/// The row the unmounted pass streams into.
const STREAM_ROW_UNMOUNTED: RowId = 9_000_004;
/// How long each installed message is, in characters. The fourth argument
/// overrides it, which is what the control run does: a short message makes the
/// fixed cost of a frame visible.
const MESSAGE_CHARS: usize = 1_500;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rows: usize = args
        .first()
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let stream_chars: usize = args
        .get(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(DEFAULT_STREAM_CHARS);
    let delta_chars: usize = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or(5);
    let message_chars: usize = args
        .get(3)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(MESSAGE_CHARS);

    if let Err(error) = run(rows, stream_chars, delta_chars, message_chars) {
        eprintln!("[stress] {error}");
        std::process::exit(1);
    }
}

fn run(
    row_count: usize,
    stream_chars: usize,
    delta_chars: usize,
    message_chars: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        std::sync::Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, host) = cx.update(|cx| {
        gpui_kit::open_window(window_options(WINDOW_SIZE), cx, |_window, cx| {
            cx.new(|cx| Host::new(cx))
        })
    })?;
    let window: AnyWindowHandle = window.into();

    let rss_start = rss_kb();
    println!(
        "[stress] rows={row_count} message_chars={message_chars} stream_chars={stream_chars} delta_chars={delta_chars} window={WINDOW_SIZE:?} rss_start={}",
        kb(rss_start)
    );

    // 1. Install one rebuilt transcript, as `/transcript` does.
    let rows = mixed_rows(row_count, message_chars);
    let assistant_rows = rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Assistant { .. }))
        .count();
    let source_chars: usize = rows
        .iter()
        .map(|row| match &row.kind {
            RowKind::Assistant { markdown, .. } => markdown.len(),
            _ => 0,
        })
        .sum();
    let installed = Instant::now();
    host.update(&mut cx, |host, cx| {
        host.transcript
            .update(cx, |view, cx| view.replace(1, rows.clone(), cx));
    });
    let install = installed.elapsed();
    frame(&mut cx, window)?;
    println!(
        "[stress] install {install:?} ({assistant_rows} assistant rows, {} markdown chars, {} rows total)",
        source_chars,
        rows.len()
    );

    // 2. At rest: what a frame costs with the list sitting at the tail.
    let mut frames = Timings::default();
    for _ in 0..40 {
        frames.note(frame(&mut cx, window)?);
    }
    println!("[stress] rss after install {}", kb(rss_kb()));

    // What a bare `notify` costs here: this headless platform draws the window
    // when a mounted view says it changed, so a delta and a frame come in pairs
    // and the numbers below include that draw. The pass at the end — streaming
    // into a view nobody is looking at — measures a delta with no frame behind
    // it, which is what the tab page's own loop looks like when a burst of
    // events lands in one turn.
    let mut notify = Timings::default();
    for _ in 0..20 {
        let started = Instant::now();
        host.update(&mut cx, |host, cx| {
            host.transcript.update(cx, |_, cx| cx.notify());
        });
        notify.note(started.elapsed());
    }
    println!("[stress] notify on a mounted view {}", notify.summary());

    // 2. At rest: what a frame costs with the list sitting at the tail.
    let mut frames = Timings::default();
    for _ in 0..40 {
        frames.note(frame(&mut cx, window)?);
    }
    println!("[stress] rss after install {}", kb(rss_kb()));

    // What a bare `notify` costs: whether this platform draws a frame when an
    // entity says it changed, or only when one is asked for.
    let mut notify = Timings::default();
    for _ in 0..20 {
        let started = Instant::now();
        host.update(&mut cx, |host, cx| {
            host.transcript.update(cx, |_, cx| cx.notify());
        });
        notify.note(started.elapsed());
    }
    println!("[stress] bare notify (view) {}", notify.summary());

    // What is that time? A plain entity's notify, an empty app update, and the
    // list's own entity, for comparison with the view and the frame.
    let plain = cx.new(|_| 0u32);
    let mut plain_notify = Timings::default();
    for _ in 0..20 {
        let started = Instant::now();
        plain.update(&mut cx, |_, cx| cx.notify());
        plain_notify.note(started.elapsed());
    }
    let mut empty_update = Timings::default();
    for _ in 0..20 {
        let started = Instant::now();
        cx.update(|_| {});
        empty_update.note(started.elapsed());
    }
    let mut frame_only = Timings::default();
    for _ in 0..20 {
        frame_only.note(frame(&mut cx, window)?);
    }
    println!(
        "[stress] bare notify (plain entity) {}",
        plain_notify.summary()
    );
    println!("[stress] empty app update {}", empty_update.summary());
    println!("[stress] render_frame {}", frame_only.summary());
    let ids: Vec<RowId> = rows.iter().map(|row| row.id).collect();
    let mounted = mounted_rows(&mut cx, window, &ids);
    let quads = painted_quads(&mut cx, window)?;
    println!(
        "[stress] at rest: frames {} mounted {mounted}/{} quads {quads} following_tail={}",
        frames.summary(),
        ids.len(),
        following_tail(&mut cx, &host)
    );

    // What `set_text` costs on its own, one call per delta, on a document with
    // the same motion as a row's.
    let standalone = cx.new(|cx| {
        let mut state = TextViewState::markdown("", cx);
        state.set_motion(stream_motion());
        state
    });
    let streamed = streamed_message(stream_chars);
    let deltas: Vec<&str> = chunks(&streamed, delta_chars);
    let mut standalone_costs = Timings::default();
    let mut source = String::new();
    for delta in &deltas {
        source.push_str(delta);
        let started = Instant::now();
        standalone.update(&mut cx, |state, cx| state.set_text(&source, cx));
        standalone_costs.note(started.elapsed());
    }
    println!(
        "[stress] set_text alone: {} calls {} total {}",
        standalone_costs.count(),
        standalone_costs.summary(),
        nanos(standalone_costs.total())
    );
    drop(standalone);
    println!(
        "[stress] rss after a standalone stream {} (channel backlog included)",
        kb(rss_kb())
    );

    // 3a. Streaming, a frame after every delta: the worst case for a view that
    // re-parses or copies the message per update.
    let (delta_costs, frame_costs, tail_rows) = stream(
        &mut cx,
        window,
        &host,
        (STREAM_ROW, STREAM_ASK),
        &streamed,
        &deltas,
        1,
    )?;
    println!(
        "[stress] stream one delta per frame: {} upserts {} total",
        delta_costs.count(),
        delta_costs.summary(),
    );
    println!(
        "[stress] stream one delta per frame: frames {} mounted {tail_rows} following_tail={}",
        frame_costs.summary(),
        following_tail(&mut cx, &host)
    );

    println!("[stress] rss after one frame per delta {}", kb(rss_kb()));

    // 3b. The same message again, fifty deltas between frames: what a burst of
    // queued events costs when it lands in one turn of the loop.
    let (burst_costs, burst_frames, _) = stream(
        &mut cx,
        window,
        &host,
        (STREAM_ROW_BURST, STREAM_ASK_BURST),
        &streamed,
        &deltas,
        50,
    )?;
    println!(
        "[stress] stream 50 deltas per frame: {} upserts {} total",
        burst_costs.count(),
        burst_costs.summary(),
    );
    println!(
        "[stress] stream 50 deltas per frame: frames {}",
        burst_frames.summary()
    );

    println!("[stress] rss after fifty deltas per frame {}", kb(rss_kb()));

    // A view nobody is looking at: a delta arrives, no frame follows. This is
    // the per-update cost of the view itself, with the draw taken out of it.
    let unmounted = cx.new(|cx| TranscriptView::new(cx));
    let mut unmounted_costs = Timings::default();
    let mut source = String::new();
    let mut version = 1;
    unmounted.update(&mut cx, |view, cx| {
        view.upsert(
            1,
            Row {
                id: STREAM_ROW_UNMOUNTED,
                version,
                kind: RowKind::Assistant {
                    markdown: String::new(),
                    thinking: String::new(),
                    streaming: true,
                    error: None,
                },
            },
            cx,
        );
    });
    for delta in &deltas {
        source.push_str(delta);
        version += 1;
        let markdown = source.clone();
        let started = Instant::now();
        unmounted.update(&mut cx, |view, cx| {
            view.upsert(
                1,
                Row {
                    id: STREAM_ROW_UNMOUNTED,
                    version,
                    kind: RowKind::Assistant {
                        markdown: markdown.clone(),
                        thinking: String::new(),
                        streaming: true,
                        error: None,
                    },
                },
                cx,
            );
        });
        unmounted_costs.note(started.elapsed());
    }
    println!(
        "[stress] up to a view nobody is looking at (no frame behind a delta): {} upserts {} total",
        unmounted_costs.count(),
        unmounted_costs.summary()
    );
    drop(unmounted);

    // 4. A resync: the same transcript handed over again, as a rebuild from
    // `/transcript` does. The reader is wheeled away from the tail first, because
    // their place is what a resync has to keep — and the frame that follows must
    // be the frame that was there: nothing re-parsed, nothing re-created,
    // nothing moving.
    wheel(&mut cx, window, px(600.), 3)?;
    std::thread::sleep(SETTLE);
    frame(&mut cx, window)?;
    let before = cx.capture_screenshot(window)?;

    // The whole transcript as it stands, handed back the way `/transcript`
    // rebuilds it: same rows, same ids, same versions.
    let current = host.update(&mut cx, |host, cx| {
        host.transcript.read(cx).rows(cx).to_vec()
    });
    let resync = Instant::now();
    host.update(&mut cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.replace(3, current, cx);
        });
    });
    let resync_cost = resync.elapsed();
    let resync_frame = frame(&mut cx, window)?;
    let after = cx.capture_screenshot(window)?;
    println!(
        "[stress] resync: replace {resync_cost:?} (its own frame included), next frame {resync_frame:?} following_tail={}",
        following_tail(&mut cx, &host)
    );
    println!(
        "[stress] resync pixels identical: {} ({} bytes a frame) — {}",
        before.as_raw() == after.as_raw(),
        before.as_raw().len(),
        pixel_diff(&before, &after)
    );
    if let Ok(dir) = std::env::var("STRESS_DUMP") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir)?;
        before.save(dir.join("resync-before.png"))?;
        after.save(dir.join("resync-after.png"))?;
        println!("[stress] frames written to {}", dir.display());
    }

    // Rasterizing the last frame is a separate cost: one draw of the whole
    // window, which is what the display does with the scene the frames above
    // built.
    let rasterize = Instant::now();
    let image = cx.capture_screenshot(window)?;
    println!(
        "[stress] rasterize {}x{} in {:?}",
        image.width(),
        image.height(),
        rasterize.elapsed()
    );

    let rss_end = rss_kb();
    println!(
        "[stress] rss_end={} growth={}",
        kb(rss_end),
        kb(rss_end
            .zip(rss_start)
            .map(|(end, start)| end.saturating_sub(start)))
    );
    Ok(())
}

/// Stream `deltas` into `id`, rendering a frame every `per_frame` deltas.
///
/// Answers the per-upsert costs, the per-frame costs, and how many rows the list
/// had mounted at the end.
fn stream(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    host: &Entity<Host>,
    (id, ask): (RowId, RowId),
    streamed: &str,
    deltas: &[&str],
    per_frame: usize,
) -> Result<(Timings, Timings, usize), Box<dyn std::error::Error>> {
    // A fresh row for each pass, behind a user turn that asks for the work, so
    // the transcript looks like a real one while it streams.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.upsert(
                2,
                Row {
                    id: ask,
                    version: 1,
                    kind: RowKind::User {
                        text: "Rewrite the transcript rows and tell me what it costs".into(),
                    },
                },
                cx,
            );
            view.upsert(
                2,
                Row {
                    id,
                    version: 1,
                    kind: RowKind::Assistant {
                        markdown: String::new(),
                        thinking: String::new(),
                        streaming: true,
                        error: None,
                    },
                },
                cx,
            );
        });
    });
    frame(cx, window)?;

    let mut source = String::new();
    let mut delta_costs = Timings::default();
    let mut inner_costs = Timings::default();
    let mut upsert_costs = Timings::default();
    let mut frame_costs = Timings::default();
    let mut version = 1;
    let mut since_frame = 0;
    for delta in deltas {
        source.push_str(delta);
        version += 1;
        let markdown = source.clone();
        // Three depths, so the numbers say where the time goes: the entity
        // update and its effects, the view's own runner, and `upsert` itself.
        let mut inner = Duration::ZERO;
        let mut upsert = Duration::ZERO;
        let started = Instant::now();
        host.update(cx, |host, cx| {
            let before_view = Instant::now();
            host.transcript.update(cx, |view, cx| {
                let before_upsert = Instant::now();
                view.upsert(
                    2,
                    Row {
                        id,
                        version,
                        kind: RowKind::Assistant {
                            markdown: markdown.clone(),
                            thinking: String::new(),
                            streaming: true,
                            error: None,
                        },
                    },
                    cx,
                );
                upsert = before_upsert.elapsed();
            });
            inner = before_view.elapsed();
        });
        delta_costs.note(started.elapsed());
        inner_costs.note(inner);
        upsert_costs.note(upsert);

        since_frame += 1;
        if since_frame == per_frame {
            since_frame = 0;
            frame_costs.note(frame(cx, window)?);
        }
    }
    // The message ends.
    host.update(cx, |host, cx| {
        host.transcript.update(cx, |view, cx| {
            view.upsert(
                2,
                Row {
                    id,
                    version: version + 1,
                    kind: RowKind::Assistant {
                        markdown: streamed.to_string(),
                        thinking: String::new(),
                        streaming: false,
                        error: None,
                    },
                },
                cx,
            );
        });
    });
    frame_costs.note(frame(cx, window)?);

    // The list is at the tail, so the rows it mounted at the end are the ones
    // the streamed message occupies: the list mounts a window's worth, not the
    // transcript.
    let visible = mounted_rows(cx, window, &[ask, id]);
    println!(
        "[stress]   of which the view runner {}",
        inner_costs.summary()
    );
    println!(
        "[stress]   of which upsert itself {}",
        upsert_costs.summary()
    );
    Ok((delta_costs, frame_costs, visible))
}

/// Where two frames of the same size differ, as a count and a bounding box of
/// device pixels — enough to say whether a change is a corner or the transcript.
fn pixel_diff(before: &image::RgbaImage, after: &image::RgbaImage) -> String {
    if before.dimensions() != after.dimensions() {
        return format!(
            "different sizes: {:?} vs {:?}",
            before.dimensions(),
            after.dimensions()
        );
    }
    let (mut count, mut min_x, mut min_y, mut max_x, mut max_y) =
        (0u64, u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, pixel) in before.enumerate_pixels() {
        if after.get_pixel(x, y) != pixel {
            count += 1;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if count == 0 {
        "no differing pixel".into()
    } else {
        format!("{count} pixels differ, in ({min_x},{min_y})..({max_x},{max_y})")
    }
}

/// Send `times` wheel steps of `y` px over the transcript, moving the pointer
/// there first: a scroll needs something under the cursor.
fn wheel(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    y: gpui_kit::Pixels,
    times: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let position = cx
        .update_window(window, |_, window, _| window.bounds())?
        .center();
    for _ in 0..times {
        cx.update_window(window, |_, window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            // The frame is what puts the element under the pointer: without it
            // the wheel has nothing to land on.
            window.render_frame(cx);
            let wheel = ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), y)),
                ..Default::default()
            };
            window.dispatch_event(wheel.to_platform_input(), cx);
            window.render_frame(cx);
        })?;
    }
    Ok(())
}

/// Draw one frame and answer how long it took.
fn frame(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let started = Instant::now();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    Ok(started.elapsed())
}

/// How many of `ids` the window has mounted: the list renders what is on screen
/// (plus a margin), and nothing else.
fn mounted_rows(cx: &mut HeadlessAppContext, window: AnyWindowHandle, ids: &[RowId]) -> usize {
    cx.update_window(window, |_, window, _| {
        ids.iter()
            .filter(|id| window.try_find(("transcript-row", **id)).is_some())
            .count()
    })
    .unwrap_or(0)
}

fn painted_quads(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(cx.update_window(window, |_, window, _| window.painted_quads().len())?)
}

fn following_tail(cx: &mut HeadlessAppContext, host: &Entity<Host>) -> bool {
    cx.update(|cx| {
        let transcript = host.read(cx).transcript.clone();
        transcript.read(cx).is_following_tail(cx)
    })
}

/// The motion a streaming row is rendered with, as the crate's own rows have it.
fn stream_motion() -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(Duration::from_millis(350))
        .with_stream_fade_stagger(Duration::from_millis(30))
}

struct Host {
    transcript: Entity<TranscriptView>,
}

impl Host {
    fn new(cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| TranscriptView::new(cx));
        transcript.update(cx, |view, cx| {
            view.set_todos(
                vec![
                    Todo {
                        text: "measure the frame".into(),
                        status: TodoStatus::InProgress,
                    },
                    Todo {
                        text: "stream a message".into(),
                        status: TodoStatus::Pending,
                    },
                ],
                cx,
            );
        });
        Self { transcript }
    }
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The tab page's centre column: a header line, the transcript, the todo
        // panel — the same arrangement the app measures.
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("transcript stress · 2 000 rows"),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
    }
}

fn window_options(window_size: (f32, f32)) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(window_size.0), px(window_size.1)),
        })),
        focus: false,
        show: false,
        ..Default::default()
    }
}

/// `count` rows of the shapes a busy tab has: assistant messages with headings,
/// lists, a table and a code fence; tool calls; dim status lines; and the user
/// turns that open each group.
fn mixed_rows(count: usize, message_chars: usize) -> Vec<Row> {
    let mut rows = Vec::with_capacity(count + 1);
    for index in 0..count {
        let id = index as RowId + 1;
        let row = match index % 4 {
            0 => Row {
                id,
                version: 1,
                kind: RowKind::Tool {
                    call_id: format!("call-{id}"),
                    name: if index % 8 == 0 { "read_file" } else { "bash" }.into(),
                    arguments: format!(
                        "{{\"path\":\"crates/transcript/src/rows.rs\",\"line\":{index},\"limit\":200}}"
                    ),
                    result: Some(ToolResult {
                        is_error: index % 40 == 0,
                        content: format!("{} lines of output", index % 97 + 1),
                        content_chars: Some(((index % 97 + 1) * 42) as u64),
                    }),
                },
            },
            1 => Row {
                id,
                version: 1,
                kind: RowKind::Dim {
                    style: match index % 12 {
                        0 => DimStyle::Notice,
                        4 => DimStyle::Status,
                        _ => DimStyle::Dim,
                    },
                    text: format!("step {index}: the lane is working on its part of the turn"),
                },
            },
            2 => Row {
                id,
                version: 1,
                kind: RowKind::Assistant {
                    markdown: message(index, message_chars),
                    thinking: if index % 8 == 2 {
                        "Weighing the two shapes: a document per row, extended per delta.".into()
                    } else {
                        String::new()
                    },
                    streaming: false,
                    error: None,
                },
            },
            _ => Row {
                id,
                version: 1,
                kind: RowKind::User {
                    text: format!("Turn {index}: fold the transcript rows into the session model."),
                },
            },
        };
        rows.push(row);
    }
    rows
}

/// One assistant message of about `target` characters.
fn message(index: usize, target: usize) -> String {
    let mut text = format!("## Step {index}\n\n");
    while text.len() < target {
        text.push_str(
            "The reader asked for live markdown, so the row keeps one document and \
             extends it with `set_text` on every delta.\n\n",
        );
        text.push_str("- one document per row\n- extended, never recreated\n\n");
        text.push_str("| part | cost |\n| --- | --- |\n| parse | one per update |\n\n");
        text.push_str("```rust\nfn step(index: usize) -> usize {\n    index + 1\n}\n```\n\n");
    }
    text
}

/// The message the streaming passes send, built to the same shape.
fn streamed_message(chars: usize) -> String {
    let mut text = String::from("# The long answer\n\n");
    let mut index = 0;
    while text.len() < chars {
        index += 1;
        text.push_str(&format!(
            "Paragraph {index}: the transcript is a virtual list, so only the rows \
             on screen are rendered, and the document of a message is extended as it \
             arrives.\n\n"
        ));
        text.push_str("| pass | what it measures |\n| --- | --- |\n| install | one replace |\n| stream | one upsert per delta |\n\n");
        text.push_str("```rust\nlet frame = view.render_frame(cx);\n```\n\n");
    }
    text
}

fn chunks(text: &str, size: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + size).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// A distribution of durations, summarised the way the target is stated: the
/// median, the 95th percentile and the total.
#[derive(Default)]
struct Timings {
    samples: Vec<Duration>,
}

impl Timings {
    fn note(&mut self, sample: Duration) {
        self.samples.push(sample);
    }

    fn count(&self) -> usize {
        self.samples.len()
    }

    fn total(&self) -> Duration {
        self.samples.iter().sum()
    }

    fn percentile(&self, fraction: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted = self.samples.clone();
        sorted.sort();
        let index = ((sorted.len() as f64 - 1.) * fraction).round() as usize;
        sorted[index]
    }

    fn summary(&self) -> String {
        if self.samples.is_empty() {
            return "n=0".into();
        }
        format!(
            "n={} p50={:.2}ms p95={:.2}ms max={:.2}ms sum={:.2}ms",
            self.samples.len(),
            self.percentile(0.5).as_secs_f64() * 1e3,
            self.percentile(0.95).as_secs_f64() * 1e3,
            self.samples
                .iter()
                .max()
                .copied()
                .unwrap_or_default()
                .as_secs_f64()
                * 1e3,
            self.total().as_secs_f64() * 1e3,
        )
    }
}

fn nanos(duration: Duration) -> String {
    format!("{:.1}ms", duration.as_secs_f64() * 1e3)
}

/// This process's resident set in kilobytes, or `None` where `ps` cannot say.
fn rss_kb() -> Option<u64> {
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn kb(value: Option<u64>) -> String {
    match value {
        Some(kb) => format!("{:.1}MiB", kb as f64 / 1024.),
        None => "?".into(),
    }
}
