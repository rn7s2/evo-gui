//! A window that streams one assistant message into a [`TranscriptView`],
//! one small chunk every 30 ms, and then appends the rest of a turn's rows.
//!
//! The markdown source deliberately contains a heading, bold runs, a list, a
//! table and a fenced code block, and is cut into 12-character chunks so that
//! half-typed constructs (`**bo`, an open ``` fence, a bisected table row) are
//! on screen while the message grows.
//!
//! Run: `cargo run --example transcript_demo`

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::Sizable as _;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Task, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use session::{DimStyle, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

use transcript::{TodoPanel, TranscriptView};

/// How much of the message each delta carries, and how long it waits first.
const CHUNK_CHARS: usize = 12;
const CHUNK_DELAY: Duration = Duration::from_millis(30);

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

fn tool_row(id: RowId, version: u64, name: &str, arguments: &str, result: Option<ToolResult>) -> Row {
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
    _stream: Task<()>,
}

impl Demo {
    fn new(cx: &mut Context<Self>) -> Self {
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
            view.set_todos(
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
                ],
                cx,
            );
        });

        let stream = Self::stream(transcript.clone(), cx);
        Self {
            transcript,
            _stream: stream,
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
                let version = index as u64 + 2;
                let row = assistant_row(version, &source, true);
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
            let version = chunks.len() as u64 + 2;
            let finished = assistant_row(version, ASSISTANT_SOURCE, false);
            transcript.update(cx, |view, cx| {
                view.upsert(1, finished, cx);
            });
            println!("[demo] t+{}ms message-end", epoch_ms() - started);

            cx.background_executor().timer(Duration::from_millis(500)).await;

            // The rest of the turn, appended row by row.
            let rows: Vec<Row> = vec![
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
                tool_row(6, 1, "write_file", "{\"path\":\"crates/transcript/src/tests.rs\"}", None),
                Row {
                    id: 7,
                    version: 1,
                    kind: RowKind::Report {
                        done: "Stood up the transcript view: rows, retained markdown documents, todo panel".into(),
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
            ];

            for (offset, row) in rows.into_iter().enumerate() {
                transcript.update(cx, |view, cx| {
                    view.upsert(1, row, cx);
                });
                println!(
                    "[demo] t+{}ms appended row {}",
                    epoch_ms() - started,
                    offset + 1
                );
                cx.background_executor().timer(Duration::from_millis(120)).await;
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.clone()),
            )
            .child(TodoPanel::new(self.transcript.read(cx).todos()))
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(100.), px(100.)),
                    size: size(px(1000.), px(760.)),
                })),
                titlebar: Some(gpui_kit::TitlebarOptions {
                    title: Some("evo-desktop transcript demo".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                focus: true,
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_window, cx| cx.new(|cx| Demo::new(cx)))
                .expect("open the demo window");
        });
}
