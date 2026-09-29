//! One assistant message streams into a [`TranscriptView`], one small chunk
//! every 30 ms, followed by the rest of a turn's rows.
//!
//! The markdown source deliberately contains a heading, bold runs, a list, a
//! table and a fenced code block, and is cut into 12-character chunks so that
//! half-typed constructs (`**bo`, an open ``` fence, a bisected table row) are
//! on screen while the message grows.
//!
//! ```sh
//! cargo run --example transcript_demo                    # live window
//! cargo run --example transcript_demo -- --capture <dir> # render the stream to PNGs
//! ```
//!
//! Capture mode drives the same frames through GPUI's headless Metal renderer,
//! so the pictures do not depend on a window being on screen — which is what
//! makes them reproducible on a locked machine.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InputEvent as _, IntoElement, MouseMoveEvent, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, Task, Window, WindowBounds, WindowOptions,
};
use session::{DimStyle, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

use transcript::{TodoPanel, TranscriptView};

/// How much of the message each delta carries, and how long it waits first.
const CHUNK_CHARS: usize = 12;
const CHUNK_DELAY: Duration = Duration::from_millis(30);

/// A live window is screen-shaped; a capture window is tall enough to show a
/// whole turn at once.
const WINDOW_SIZE: (f32, f32) = (1000., 760.);
const CAPTURE_SIZE: (f32, f32) = (1100., 1500.);
/// A window the finished turn overflows, so the reader can leave the tail.
const SCROLL_SIZE: (f32, f32) = (1000., 560.);

/// Time to let the stream fade settle before a capture, so the picture shows
/// the text at full color rather than mid-fade.
const FADE_SETTLE: Duration = Duration::from_millis(500);

/// The stream positions worth a picture: the chunk count applied, and the file
/// name. Each one lands while a construct is still half-typed.
const STAGES: [(usize, &str); 6] = [
    (6, "01-mid-stream-bold-open.png"),
    (14, "02-mid-stream-heading-typed.png"),
    (22, "03-mid-stream-list-partial.png"),
    (28, "04-mid-stream-table-partial.png"),
    (34, "05-mid-stream-open-fence.png"),
    (38, "06-mid-stream-code-partial.png"),
];
const MESSAGE_END_SHOT: &str = "07-message-end.png";
const TURN_SHOT: &str = "08-turn-tool-report-dim.png";
const THINKING_SHOT: &str = "09-thinking-revealed.png";
/// The scroller with the reader away from the tail.
const JUMP_SHOT: &str = "10-scrolled-up-jump-button.png";

const ASSISTANT_ID: RowId = 2;

/// The message the demo streams. Headings, bold, a list, a table and a fenced
/// code block — all of them half-typed at some point mid-stream.
const ASSISTANT_SOURCE: &str = r#"# Streaming markdown

The paragraph below arrives **a few characters at a time**, and every character
is rendered as formatted markdown the moment it lands.

## What the reader sees

- headings, **bold text** and lists
- a table and a fenced code block
- half-typed constructs while the message grows

| part | arrives |
| --- | --- |
| heading | first |
| table | in the middle |
| code fence | last |

```rust
fn main() {
    println!("hello from a streaming code fence");
}
```
"#;

const THINKING: &str = "The reader asked for live markdown: keep one document per \
message and extend it with set_text on every delta.";

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis()
}

fn assistant_row(version: u64, markdown: &str, streaming: bool) -> Row {
    Row {
        id: ASSISTANT_ID,
        version,
        kind: RowKind::Assistant {
            markdown: markdown.into(),
            thinking: THINKING.into(),
            streaming,
            error: None,
        },
    }
}

fn user_row(id: RowId, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::User { text: text.into() },
    }
}

fn dim_row(id: RowId, style: DimStyle, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::Dim {
            style,
            text: text.into(),
        },
    }
}

fn tool_row(
    id: RowId,
    version: u64,
    name: &str,
    arguments: &str,
    result: Option<ToolResult>,
) -> Row {
    Row {
        id,
        version,
        kind: RowKind::Tool {
            call_id: format!("call-{id}"),
            name: name.into(),
            arguments: arguments.into(),
            result,
        },
    }
}

/// The rows of the turn that follow the message: three tool calls (one still
/// running, one ok, one failed), the lane's report, and the dim lines.
fn turn_rows() -> Vec<Row> {
    vec![
        tool_row(
            4,
            1,
            "read_file",
            "{\"path\":\"crates/transcript/src/lib.rs\"}",
            Some(ToolResult {
                is_error: false,
                content: "//! transcript — the center column of a tab.\n//!\n//! The view is fed rows from the `session` crate.".into(),
                content_chars: Some(148),
            }),
        ),
        tool_row(
            5,
            1,
            "bash",
            "{\"command\":\"cargo test -p transcript\"}",
            Some(ToolResult {
                is_error: true,
                content: "error: could not compile `transcript` (lib)".into(),
                content_chars: Some(45),
            }),
        ),
        tool_row(
            6,
            1,
            "write_file",
            "{\"path\":\"crates/transcript/src/tests.rs\"}",
            None,
        ),
        Row {
            id: 7,
            version: 1,
            kind: RowKind::Report {
                done: "Stood up the transcript view: rows, retained markdown documents, todo panel"
                    .into(),
                evidence: "cargo test -p transcript: 5 passed".into(),
                next: "".into(),
                blocked: "".into(),
                requests: "".into(),
            },
        },
        dim_row(8, DimStyle::Notice, "compaction finished · 12 messages → 9"),
        dim_row(9, DimStyle::Dim, "provider-retry 1/3 in 500ms"),
        dim_row(10, DimStyle::Error, "lane 3 exited: model not registered"),
        dim_row(11, DimStyle::Status, "run-end · outcome ok"),
    ]
}

fn todos() -> Vec<Todo> {
    vec![
        Todo {
            text: "fold /transcript into rows".into(),
            status: TodoStatus::Done,
        },
        Todo {
            text: "stream one markdown document".into(),
            status: TodoStatus::InProgress,
        },
        Todo {
            text: "append the tool and report rows".into(),
            status: TodoStatus::Pending,
        },
    ]
}

/// The markdown source cut into [`CHUNK_CHARS`] pieces, on char boundaries.
fn chunks(source: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in source.chars() {
        current.push(character);
        if current.chars().count() == CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

struct Demo {
    transcript: Entity<TranscriptView>,
    /// The streaming task, in the live window only.
    stream: Option<Task<()>>,
}

impl Demo {
    /// The window root, streaming the message on its own clock.
    fn new(cx: &mut Context<Self>) -> Self {
        let mut demo = Self::staged(cx);
        demo.stream = Some(Self::stream(demo.transcript.clone(), cx));
        demo
    }

    /// The same root without the stream task; a capture drives the deltas.
    fn staged(cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| TranscriptView::new(cx));

        transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![
                    user_row(1, "Render my transcript: markdown live while it streams."),
                    dim_row(3, DimStyle::Status, "run-start · turn 1"),
                ],
                cx,
            );
            view.set_todos(todos(), cx);
        });

        Self {
            transcript,
            stream: None,
        }
    }

    /// Stream the assistant message, then finish the turn with its tool,
    /// report and status rows.
    fn stream(transcript: Entity<TranscriptView>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |_this, cx| {
            let started = epoch_ms();
            let chunks = chunks(ASSISTANT_SOURCE);
            let mut source = String::new();

            println!(
                "[demo] unix_ms={started} streaming {} chunks of <= {CHUNK_CHARS} chars",
                chunks.len()
            );

            for (index, chunk) in chunks.iter().enumerate() {
                cx.background_executor().timer(CHUNK_DELAY).await;
                source.push_str(chunk);
                let row = assistant_row(index as u64 + 2, &source, true);
                transcript.update(cx, |view, cx| {
                    view.upsert(1, row, cx);
                });
                println!(
                    "[demo] t+{}ms delta {}/{} ({} bytes rendered)",
                    epoch_ms() - started,
                    index + 1,
                    chunks.len(),
                    source.len()
                );
            }

            // The message ends: one more version, no longer streaming.
            let finished = assistant_row(chunks.len() as u64 + 2, ASSISTANT_SOURCE, false);
            transcript.update(cx, |view, cx| {
                view.upsert(1, finished, cx);
            });
            println!("[demo] t+{}ms message-end", epoch_ms() - started);

            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;

            for (offset, row) in turn_rows().into_iter().enumerate() {
                transcript.update(cx, |view, cx| {
                    view.upsert(1, row, cx);
                });
                println!(
                    "[demo] t+{}ms appended row {}",
                    epoch_ms() - started,
                    offset + 1
                );
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
            }

            // The per-view thinking toggle: dim text, hidden until asked for.
            transcript.update(cx, |view, cx| view.set_show_thinking(true, cx));
            println!(
                "[demo] t+{}ms run complete — the window stays open, Ctrl-C to quit",
                epoch_ms() - started
            );
        })
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let transcript = self.transcript.clone();
        let thinking = transcript.read(cx).is_showing_thinking(cx);

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
                            .child("transcript demo · streaming markdown"),
                    )
                    .child(
                        Button::new("toggle-thinking")
                            .ghost()
                            .small()
                            .label(if thinking {
                                "hide thinking"
                            } else {
                                "show thinking"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.transcript
                                    .update(cx, |view, cx| view.toggle_thinking(cx));
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(TodoPanel::new(self.transcript.read(cx).todos()))
    }
}

fn window_options(window_size: (f32, f32), show: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(100.), px(100.)),
            size: size(px(window_size.0), px(window_size.1)),
        })),
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some("evo-desktop transcript demo".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        focus: show,
        show,
        ..Default::default()
    }
}

fn run_window() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(window_options(WINDOW_SIZE, true), cx, |_window, cx| {
                cx.new(|cx| Demo::new(cx))
            })
            .expect("open the demo window");
        });
}

/// Render the stream to one PNG per [`STAGES`] entry, then the finished turn.
fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        std::sync::Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, demo) = open_capture_window(&mut cx, CAPTURE_SIZE)?;

    let chunks = chunks(ASSISTANT_SOURCE);
    let mut source = String::new();

    for (index, chunk) in chunks.iter().enumerate() {
        source.push_str(chunk);
        let markdown = source.clone();
        let version = index as u64 + 2;
        demo.update(&mut cx, |demo, cx| {
            demo.transcript.update(cx, |view, cx| {
                view.upsert(1, assistant_row(version, &markdown, true), cx);
            });
        });

        let applied = index + 1;
        if let Some((_, name)) = STAGES.iter().find(|(at, _)| *at == applied) {
            shot(&mut cx, window, dir, name)?;
        }
    }

    demo.update(&mut cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.upsert(
                1,
                assistant_row(chunks.len() as u64 + 2, ASSISTANT_SOURCE, false),
                cx,
            );
        });
    });
    shot(&mut cx, window, dir, MESSAGE_END_SHOT)?;

    for row in turn_rows() {
        demo.update(&mut cx, |demo, cx| {
            demo.transcript.update(cx, |view, cx| {
                view.upsert(1, row.clone(), cx);
            });
        });
    }
    shot(&mut cx, window, dir, TURN_SHOT)?;

    demo.update(&mut cx, |demo, cx| {
        demo.transcript
            .update(cx, |view, cx| view.set_show_thinking(true, cx));
    });
    shot(&mut cx, window, dir, THINKING_SHOT)?;

    // The reader leaves the tail: in a window the turn overflows, the scroller
    // offers its jump affordance.
    let (short, short_demo) = open_capture_window(&mut cx, SCROLL_SIZE)?;
    install_turn(&mut cx, &short_demo);
    let mut following = wheel(&mut cx, short, &short_demo, px(240.), 8)?;
    if following {
        following = wheel(&mut cx, short, &short_demo, px(-240.), 8)?;
    }
    println!("[capture] following the tail after the wheel: {following}");
    // The jump affordance fades in on the app clock, which the headless
    // context only advances when asked.
    cx.advance_clock(Duration::from_millis(400));
    shot(&mut cx, short, dir, JUMP_SHOT)?;

    Ok(())
}

fn open_capture_window(
    cx: &mut HeadlessAppContext,
    size: (f32, f32),
) -> Result<(AnyWindowHandle, Entity<Demo>), Box<dyn std::error::Error>> {
    let (handle, demo) = cx.update(|cx| {
        gpui_kit::open_window(window_options(size, false), cx, |_window, cx| {
            cx.new(|cx| Demo::staged(cx))
        })
    })?;
    Ok((handle.into(), demo))
}

/// Replay the finished turn into a window as one rebuild, the way `/transcript`
/// installs it.
fn install_turn(cx: &mut HeadlessAppContext, demo: &Entity<Demo>) {
    let mut rows = vec![
        user_row(1, "Render my transcript: markdown live while it streams."),
        dim_row(3, DimStyle::Status, "run-start · turn 1"),
        assistant_row(
            chunks(ASSISTANT_SOURCE).len() as u64 + 2,
            ASSISTANT_SOURCE,
            false,
        ),
    ];
    rows.extend(turn_rows());

    demo.update(cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.replace(1, rows, cx);
            view.set_show_thinking(true, cx);
        });
    });
}

/// Send `times` wheel steps of `y` px at the window's centre — moving the
/// pointer there first, as a real scroll needs the list under the cursor — and
/// answer whether the transcript is still following its tail.
fn wheel(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
    y: gpui_kit::Pixels,
    times: usize,
) -> Result<bool, Box<dyn std::error::Error>> {
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
            let wheel = ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), y)),
                ..Default::default()
            };
            window.dispatch_event(wheel.to_platform_input(), cx);
            window.render_frame(cx);
        })?;
    }

    Ok(cx.update(|cx| {
        let transcript = demo.read(cx).transcript.clone();
        transcript.read(cx).is_following_tail(cx)
    }))
}

/// Let the stream fade finish, draw a frame, and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::thread::sleep(FADE_SETTLE);
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let image = cx.capture_screenshot(window)?;
    let path = dir.join(name);
    image.save(&path)?;
    println!(
        "[capture] {}x{} -> {}",
        image.width(),
        image.height(),
        path.display()
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run_window(),
        [flag, dir] if flag == "--capture" => {
            if let Err(error) = capture(Path::new(dir)) {
                eprintln!("capture failed: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: transcript_demo [--capture <dir>]");
            std::process::exit(2);
        }
    }
}
