//! One assistant message streams into a [`TranscriptView`], one small chunk
//! every 30 ms, followed by the rest of a turn's rows and a second turn that
//! carries the content a transcript has to survive: a long path, a token with
//! nothing to wrap on, a wide table and a long code line.
//!
//! The markdown source deliberately contains a heading, bold runs, a list, a
//! table and a fenced code block, and is cut into 12-character chunks so that
//! half-typed constructs (`**bo`, an open ``` fence, a bisected table row) are
//! on screen while the message grows.
//!
//! Later stages show what the rest of a transcript looks like: a turn of tool,
//! report and status rows; a long todo list, scrolled, with the panel's own
//! thumb; and two calls — a `bash` one and a `write_file` carrying a whole file
//! — opened so their arguments and results read as a key/value list rather than
//! as the JSON they arrived in.
//!
//! The last stages are the states before any of that: a fresh tab, where the
//! transcript invites the reader to ask for something; a lane with no work yet;
//! and an assistant message that has started without sending a word, which holds
//! its place with the waiting pips.
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
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InputEvent as _, IntoElement, MouseMoveEvent, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, Task, Window, WindowBounds, WindowOptions,
};
use session::{AgentKey, DimStyle, Row, RowId, RowKind, Todo, TodoStatus, ToolResult};

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
/// The second turn: long content that must stay inside the measure.
const LONG_CONTENT_SHOT: &str = "11-turn-two-long-content.png";
/// A todo list longer than the panel, which scrolls on its own.
const LONG_TODOS_SHOT: &str = "12-long-todo-list.png";
/// The dark theme, mid-stream and whole.
const DARK_STREAM_SHOT: &str = "13-dark-mid-stream.png";
const DARK_TURN_SHOT: &str = "14-dark-turn.png";
/// A `bash` call and a `write_file` call, opened: what an expanded row shows
/// instead of the JSON the call carried.
const TOOL_ARGUMENTS_SHOT: &str = "15-tool-arguments-expanded.png";
const DARK_TOOL_ARGUMENTS_SHOT: &str = "16-dark-tool-arguments-expanded.png";
/// The todo panel while its list scrolls, with the thumb the panel draws.
const TODO_SCROLLBAR_SHOT: &str = "17-todo-panel-scrollbar.png";
/// A tab that has done nothing yet: the transcript's own invitation.
const FRESH_TAB_SHOT: &str = "18-fresh-tab-invitation.png";
/// The same column showing a lane that has not been given work.
const LANE_EMPTY_SHOT: &str = "19-lane-with-no-work.png";
/// A message that has started and not sent a word yet.
const WAITING_SHOT: &str = "20-message-waiting.png";
/// The invitation and the waiting pips in the dark theme.
const DARK_FRESH_TAB_SHOT: &str = "21-dark-fresh-tab-invitation.png";
const DARK_WAITING_SHOT: &str = "22-dark-message-waiting.png";

const ASSISTANT_ID: RowId = 2;
const SECOND_ASSISTANT_ID: RowId = 21;
/// The two calls of the tool-arguments stage.
const BASH_CALL: RowId = 30;
const WRITE_CALL: RowId = 31;

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

/// A deep path: long, but it has slashes to wrap on.
fn long_path() -> String {
    let mut path = String::from("~/coding/evo-gui/crates/transcript");
    for index in 0..10 {
        path.push_str(&format!("/nested-{index:02}"));
    }
    path.push_str("/src/rows.rs");
    path
}

/// One unbroken token, 280 characters with no space and no punctuation to
/// break on.
fn long_token() -> String {
    "0123456789abcdef".repeat(17) + "01234567"
}

/// A single code line far wider than the measure.
fn long_code_line() -> String {
    format!(
        "cargo test -p transcript --lib -- --nocapture --test-threads=1 {}",
        "assert_eq!(view.rows(cx)[index].kind, RowKind::Assistant { .. }); ".repeat(2)
    )
}

/// The message source after `count` deltas have landed.
fn source_at(count: usize) -> String {
    chunks(ASSISTANT_SOURCE)
        .iter()
        .take(count)
        .cloned()
        .collect::<Vec<String>>()
        .concat()
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

/// A run that ended badly, as the session's own row kind spells it: the raw
/// outcome, plus the line to show.
fn run_outcome_row(id: RowId, outcome: &str, text: &str) -> Row {
    Row {
        id,
        version: 1,
        kind: RowKind::RunOutcome {
            outcome: outcome.into(),
            text: text.into(),
        },
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
/// running, one ok, one failed), the lane's report, and the status lines — all
/// in the words a reader wants, none of them protocol.
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
        dim_row(8, DimStyle::Notice, "Compacted 12 messages into 9"),
        dim_row(9, DimStyle::Dim, "Retrying the provider (1/3) in 500 ms"),
        dim_row(10, DimStyle::Error, "Lane 3 exited: model not registered"),
        run_outcome_row(11, "aborted", "Run aborted"),
    ]
}

/// The second turn: the content a transcript has to keep inside its column.
fn long_turn_rows() -> Vec<Row> {
    let markdown = format!(
        r#"## Long content stays inside the column

A deep path, which has slashes to wrap on:

{path}

One 280-character token, which has nothing to wrap on:

{token}

| tool | calls | errors | p50 | p99 | tokens in | tokens out | cache | longest argument |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| read_file | 128 | 0 | 12ms | 41ms | 184k | 6k | 91% | {token} |
| bash | 37 | 2 | 320ms | 2.4s | 96k | 21k | 88% | {{\"command\":\"cargo test -p transcript --lib -- --nocapture\"}} |
| write_file | 54 | 1 | 18ms | 60ms | 42k | 9k | 93% | crates/transcript/src/rows.rs |

```text
{code}
```
"#,
        path = long_path(),
        token = long_token(),
        code = long_code_line(),
    );

    let mut rows = vec![
        user_row(
            20,
            "Now make the rows compact and keep long content in the column.",
        ),
        Row {
            id: SECOND_ASSISTANT_ID,
            version: 1,
            kind: RowKind::Assistant {
                markdown,
                thinking: String::new(),
                streaming: false,
                error: None,
            },
        },
    ];
    rows.extend([
        tool_row(
            22,
            1,
            "read_file",
            "{\"path\":\"crates/transcript/src/rows.rs\"}",
            Some(ToolResult {
                is_error: false,
                content: "fn render_row(data: &TranscriptData, index: usize) -> AnyElement".into(),
                content_chars: Some(66),
            }),
        ),
        dim_row(23, DimStyle::Dim, &long_path()),
        run_outcome_row(24, "error", "Run failed: the provider returned 429"),
    ]);
    rows
}

/// The output of the `bash` call: more lines than an expanded row shows, so the
/// row has to say how much it left out.
const BASH_OUTPUT: &str = "\
   Compiling transcript v0.1.0 (/Users/you/coding/evo-gui/crates/transcript)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.03s
     Running unittests src/lib.rs (target/debug/deps/transcript-1a3b6858d047bf83)

running 14 tests
test tests::the_todo_panel_counts_its_items_caps_its_height_and_aligns_its_glyphs ... ok
test tests::a_tool_row_opens_on_click ... ok
test tests::tool_arguments_read_as_a_key_value_list ... ok
test tests::a_multi_line_string_becomes_a_capped_block ... ok
test tests::a_long_todo_list_scrolls_inside_the_panel ... ok
test tests::an_open_tool_row_renders_its_arguments_as_key_value_rows ... ok
test tests::an_assistant_document_is_retained_across_deltas ... ok
test tests::long_content_wraps_inside_a_centred_reading_measure ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
";

/// The file the `write_file` call wrote, as it travels inside the call's JSON:
/// one string with line breaks in it.
const WRITTEN_FILE: &str = "\
use gpui_kit::{div, AnyElement, IntoElement, ParentElement as _, Styled as _};
use session::RowId;

/// A tool call: one compact line that opens onto its arguments.
pub(crate) fn tool_row(id: RowId, name: &str, arguments: &str) -> AnyElement {
    div()
        .id((\"transcript-tool\", id))
        .flex()
        .items_center()
        .child(name.to_string())
        .into_any_element()
}
";

/// The calls of the tool-arguments stage: a `bash` call whose command is longer
/// than the value column, and a `write_file` call carrying the whole file. Both
/// answer with a result, one of them JSON.
fn tool_argument_rows() -> Vec<Row> {
    vec![
        user_row(
            29,
            "Show me what those two calls actually did, not the JSON.",
        ),
        tool_row(
            BASH_CALL,
            1,
            "bash",
            r#"{"command":"cargo test -p transcript --lib -- --nocapture --test-threads=1 2>&1 | tee /tmp/transcript.log | tail -40","timeout":120,"cwd":"~/coding/evo-gui"}"#,
            Some(ToolResult {
                is_error: false,
                content: BASH_OUTPUT.into(),
                content_chars: None,
            }),
        ),
        tool_row(
            WRITE_CALL,
            1,
            "write_file",
            &format!(
                r#"{{"path":"crates/transcript/src/rows.rs","content":"{}"}}"#,
                escape(WRITTEN_FILE),
            ),
            Some(ToolResult {
                is_error: false,
                content: r#"{"written":1482,"path":"crates/transcript/src/rows.rs","diff":{"added":16,"removed":2}}"#.into(),
                content_chars: None,
            }),
        ),
    ]
}

/// `text` as the body of the JSON string that carries it.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
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

/// More todos than the panel shows at once, so its own scroll has work to do.
fn long_todos() -> Vec<Todo> {
    let texts = [
        "fold /transcript into rows",
        "stream one markdown document",
        "append the tool and report rows",
        "cap the reading measure",
        "group consecutive tool rows",
        "scale the headings for chat",
        "replace the run markers",
        "align the todo glyphs",
        "scroll a long todo list",
        "keep long content in the column",
        "capture the dark theme",
        "write the polish pass up",
    ];
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| Todo {
            text: (*text).into(),
            status: match index {
                0..=2 => TodoStatus::Done,
                3 => TodoStatus::InProgress,
                _ => TodoStatus::Pending,
            },
        })
        .collect()
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

    /// The same root with nothing in it: no rows and no todos, the way a tab
    /// looks before its first turn.
    fn blank(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(|cx| TranscriptView::new(cx)),
            stream: None,
        }
    }

    /// The same root without the stream task; a capture drives the deltas.
    fn staged(cx: &mut Context<Self>) -> Self {
        let demo = Self::blank(cx);
        let transcript = demo.transcript.clone();

        transcript.update(cx, |view, cx| {
            view.replace(
                1,
                vec![user_row(
                    1,
                    "Render my transcript: markdown live while it streams.",
                )],
                cx,
            );
            view.set_todos(todos(), cx);
        });

        demo
    }

    /// Stream the assistant message, then finish the turn with its tool,
    /// report and status rows, then play the long second turn.
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

            let mut rows = turn_rows();
            rows.extend(long_turn_rows());
            for (offset, row) in rows.into_iter().enumerate() {
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

            transcript.update(cx, |view, cx| {
                view.set_todos(long_todos(), cx);
                view.set_show_thinking(true, cx);
            });
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

/// Render the stream to one PNG per [`STAGES`] entry, then the finished turn,
/// the long second turn, a scrolling todo list, and two frames in the dark
/// theme.
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

    // A second turn, with the content that has to stay inside the measure.
    for row in long_turn_rows() {
        demo.update(&mut cx, |demo, cx| {
            demo.transcript.update(cx, |view, cx| {
                view.upsert(1, row.clone(), cx);
            });
        });
    }
    shot(&mut cx, window, dir, LONG_CONTENT_SHOT)?;

    // A table wider than the measure scrolls inside its own block: walk down the
    // message that holds it and wheel sideways, with the pointer moved into
    // place first so a hover highlight cannot be mistaken for a scroll.
    let measure = cx.update_window(window, |_, window, _| {
        window
            .find(("transcript-measure", SECOND_ASSISTANT_ID))
            .bounds()
    })?;
    let mut scrolled_at = None;
    for step in 1..=8u8 {
        let position = point(
            measure.center().x,
            (measure.origin.y + measure.size.height * (step as f32 / 10.)).min(px(1460.)),
        );
        wheel_at(&mut cx, window, position, point(px(0.), px(0.)), 1)?;
        settle(&mut cx, window)?;
        let before = pixels(&mut cx, window)?;
        // Sideways, either way: the table starts at its left edge, so only one
        // of the two directions has anywhere to go.
        wheel_at(&mut cx, window, position, point(px(120.), px(0.)), 3)?;
        wheel_at(&mut cx, window, position, point(px(-120.), px(0.)), 3)?;
        settle(&mut cx, window)?;
        if pixels(&mut cx, window)? != before {
            scrolled_at = Some(position.y);
            break;
        }
    }
    println!("[capture] a sideways wheel moved the wide table at y={scrolled_at:?}");

    demo.update(&mut cx, |demo, cx| {
        demo.transcript
            .update(cx, |view, cx| view.set_todos(long_todos(), cx));
    });
    shot(&mut cx, window, dir, LONG_TODOS_SHOT)?;

    // The todo list scrolls on its own: the wheel stays with the panel instead
    // of dragging the transcript. Measured from the panel's own bounds, which
    // are in the same coordinates the pointer events use.
    let panel = cx.update_window(window, |_, window, _| window.find("todo-panel").bounds())?;
    let over_panel = point(
        panel.center().x,
        panel.origin.y + panel.size.height - px(40.),
    );
    // Pointer into place first: hovering the panel shows its scrollbar, and that
    // alone must not read as a scroll.
    wheel_at(&mut cx, window, over_panel, point(px(0.), px(0.)), 1)?;
    settle(&mut cx, window)?;
    let before = pixels(&mut cx, window)?;
    // Downwards: the panel starts at the top of its list.
    wheel_at(&mut cx, window, over_panel, point(px(0.), px(-240.)), 4)?;
    settle(&mut cx, window)?;
    let after = pixels(&mut cx, window)?;
    let following = cx.update(|cx| {
        let transcript = demo.read(cx).transcript.clone();
        transcript.read(cx).is_following_tail(cx)
    });
    println!(
        "[capture] long todo list scrolled on its own: {} (transcript still at the tail: {following})",
        before != after
    );
    // The panel's thumb, while the list is away from its top.
    shot(&mut cx, window, dir, TODO_SCROLLBAR_SHOT)?;

    // A fresh tab: nothing has happened yet, so the transcript is the
    // invitation (§7.3) — and the same column for a lane with no work.
    let (blank, blank_demo) = open_blank_window(&mut cx, WINDOW_SIZE)?;
    shot(&mut cx, blank, dir, FRESH_TAB_SHOT)?;

    blank_demo.update(&mut cx, |demo, cx| {
        demo.transcript
            .update(cx, |view, cx| view.set_agent(AgentKey::Lane(3), cx));
    });
    shot(&mut cx, blank, dir, LANE_EMPTY_SHOT)?;

    // And a coordinator's message that has started without sending a word: the
    // pips hold its place until the first delta.
    blank_demo.update(&mut cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.set_agent(AgentKey::Coordinator, cx);
            view.replace(
                1,
                vec![
                    user_row(1, "What changed in the transcript crate?"),
                    assistant_row(2, "", true),
                ],
                cx,
            );
        });
    });
    shot(&mut cx, blank, dir, WAITING_SHOT)?;

    // What an expanded tool row shows: the calls' own JSON as a key/value list,
    // a multi-line value as a block, and both shapes of result.
    let (tools, tools_demo) = open_capture_window(&mut cx, CAPTURE_SIZE)?;
    tools_demo.update(&mut cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.replace(1, tool_argument_rows(), cx);
            view.set_expanded(BASH_CALL, true, cx);
            view.set_expanded(WRITE_CALL, true, cx);
        });
    });
    shot(&mut cx, tools, dir, TOOL_ARGUMENTS_SHOT)?;

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

    cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
    println!("[capture] theme mode: dark");

    // Mid-stream in the dark: a rebuild that leaves the message half-typed.
    let streamed = source_at(34);
    demo.update(&mut cx, |demo, cx| {
        let mut rows = vec![
            user_row(1, "Render my transcript: markdown live while it streams."),
            assistant_row(36, &streamed, true),
        ];
        rows.extend(turn_rows());
        demo.transcript.update(cx, |view, cx| {
            view.replace(2, rows, cx);
            view.set_show_thinking(false, cx);
            view.set_todos(todos(), cx);
        });
    });
    shot(&mut cx, window, dir, DARK_STREAM_SHOT)?;

    demo.update(&mut cx, |demo, cx| {
        let mut rows = vec![
            user_row(1, "Render my transcript: markdown live while it streams."),
            assistant_row(37, ASSISTANT_SOURCE, false),
        ];
        rows.extend(turn_rows());
        rows.extend(long_turn_rows());
        demo.transcript.update(cx, |view, cx| {
            view.replace(2, rows, cx);
            view.set_show_thinking(true, cx);
            view.set_todos(long_todos(), cx);
        });
    });
    shot(&mut cx, window, dir, DARK_TURN_SHOT)?;

    // And the open tool rows in the dark theme.
    shot(&mut cx, tools, dir, DARK_TOOL_ARGUMENTS_SHOT)?;

    // The states before a transcript has anything in it, in the dark.
    blank_demo.update(&mut cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.replace(2, Vec::new(), cx);
        });
    });
    shot(&mut cx, blank, dir, DARK_FRESH_TAB_SHOT)?;

    blank_demo.update(&mut cx, |demo, cx| {
        demo.transcript.update(cx, |view, cx| {
            view.replace(
                2,
                vec![
                    user_row(1, "What changed in the transcript crate?"),
                    assistant_row(2, "", true),
                ],
                cx,
            );
        });
    });
    shot(&mut cx, blank, dir, DARK_WAITING_SHOT)?;

    Ok(())
}

/// The same capture window on a tab that has done nothing yet.
fn open_blank_window(
    cx: &mut HeadlessAppContext,
    size: (f32, f32),
) -> Result<(AnyWindowHandle, Entity<Demo>), Box<dyn std::error::Error>> {
    let (handle, demo) = cx.update(|cx| {
        gpui_kit::open_window(window_options(size, false), cx, |_window, cx| {
            cx.new(|cx| Demo::blank(cx))
        })
    })?;
    Ok((handle.into(), demo))
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

/// Replay the finished first turn into a window as one rebuild, the way
/// `/transcript` installs it.
fn install_turn(cx: &mut HeadlessAppContext, demo: &Entity<Demo>) {
    let mut rows = vec![
        user_row(1, "Render my transcript: markdown live while it streams."),
        assistant_row(
            chunks(ASSISTANT_SOURCE).len() as u64 + 2,
            ASSISTANT_SOURCE,
            false,
        ),
    ];
    rows.extend(turn_rows());
    rows.extend(long_turn_rows());

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
    wheel_at(cx, window, position, point(px(0.), y), times)?;

    Ok(cx.update(|cx| {
        let transcript = demo.read(cx).transcript.clone();
        transcript.read(cx).is_following_tail(cx)
    }))
}

/// Send `times` wheel steps of `y` px at `position`, after moving the pointer
/// there: a scroll needs something under the cursor.
fn wheel_at(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    delta: gpui_kit::Point<gpui_kit::Pixels>,
    times: usize,
) -> Result<(), Box<dyn std::error::Error>> {
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
                delta: ScrollDelta::Pixels(delta),
                ..Default::default()
            };
            window.dispatch_event(wheel.to_platform_input(), cx);
            window.render_frame(cx);
        })?;
    }
    Ok(())
}

/// Let anything animating settle and draw the frame again: hover reveals a
/// scrollbar, and a capture taken mid-transition is not a capture of the state.
fn settle(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    std::thread::sleep(FADE_SETTLE);
    for _ in 0..2 {
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    }
    Ok(())
}

/// The pixels of the window's current frame.
fn pixels(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    Ok(cx.capture_screenshot(window)?.into_raw())
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
