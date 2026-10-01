//! transcript_demo — a live window over one transcript.
//!
//! Every row kind the new view model has is built from the wire's own JSON (CONTRACT
//! §4.1), so what you see here is what the server would send: a user turn and the turn it
//! opened, an assistant message streaming in chunks, its tool calls (one open, one whose
//! result the server shortened), a goal transition, a lane's report and one of its
//! transitions, a notice, an injected memory snapshot, a compaction divider, a run that
//! ended badly — and the reader's own next words, queued and cancellable.
//!
//! ```sh
//! cargo run --example transcript_demo
//! ```
//!
//! The message streams a chunk every [`CHUNK_DELAY`] so a reader can watch the markdown
//! document grow without a server: `set_text` extends one retained document, never a
//! second parse per delta.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    div, px, size, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _,
    Render, Styled as _, Window, WindowBounds, WindowOptions,
};
use serde_json::{json, Value};
use session::{Item, Queue};

use transcript::TranscriptView;

const WINDOW_SIZE: (f32, f32) = (1000., 760.);
const CHUNK_CHARS: usize = 12;
const CHUNK_DELAY: Duration = Duration::from_millis(30);

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis()
}

fn item(value: Value) -> Item {
    Item::from_json(&value).expect("every demo item has an id")
}

fn user(id: &str, text: &str, status: &str) -> Item {
    item(json!({ "id": id, "ts": epoch_ms(), "kind": "user", "text": text, "status": status }))
}

fn assistant(id: &str, text: &str, streaming: bool) -> Item {
    item(json!({
        "id": id, "ts": epoch_ms(), "kind": "assistant", "text": text,
        "thinking": "One document per message, extended with set_text.",
        "status": if streaming { "streaming" } else { "final" },
        "model": "stub-a", "provider": "stub",
        "usage": { "input": 1200, "output": 300, "cache_read": 4000, "cache_write": 0 },
    }))
}

/// The message the demo streams: every construct a reader meets, half-typed at some point.
const SOURCE: &str = r#"# Streaming markdown

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

fn source_at(count: usize) -> String {
    chunks().iter().take(count).cloned().collect()
}

/// The source in chunks of [`CHUNK_CHARS`], never splitting a character.
fn chunks() -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in SOURCE.chars() {
        current.push(character);
        if current.chars().count() >= CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The transcript as a session builds it: a turn of work behind the reader, then the
/// message that is still arriving.
fn settled_items() -> Vec<Item> {
    vec![
        user("e_1", "port the view model", "sent"),
        item(json!({
            "id": "e_2", "ts": epoch_ms(), "kind": "context", "key": "global-memory",
            "text": "- [mem-1] keep responses short and dense\n- [mem-2] never run benchmarks"
        })),
        item(json!({
            "id": "e_3", "ts": epoch_ms(), "kind": "goal", "event": "created",
            "goal_id": "a1b2c3d4", "objective": "ship the redesign", "budget": 50000, "tokens": 12000
        })),
        item(json!({
            "id": "t_call_1", "ts": epoch_ms(), "kind": "tool", "call_id": "call_1", "name": "bash",
            "args": { "command": "cargo test -p session", "timeout": 120 },
            "status": "ok",
            "result": { "text": "test result: ok. 34 passed; 0 failed", "chars": 39, "truncated": false },
            "parent": "e_4"
        })),
        item(json!({
            "id": "t_call_2", "ts": epoch_ms(), "kind": "tool", "call_id": "call_2", "name": "read",
            "args": { "path": "/Users/you/coding/evo/wt/gui-model/crates/session/src/tab.rs" },
            "status": "ok",
            "result": { "text": "//! One topic's mirror…", "chars": 24000, "truncated": true },
            "parent": "e_4"
        })),
        item(json!({
            "id": "e_5", "ts": epoch_ms(), "kind": "lane_report", "lane": 2,
            "done": "ported the item model", "evidence": "cargo test -p session",
            "next": "the view crates", "blocked": "", "requests": "", "goal": "active"
        })),
        item(json!({
            "id": "e_6", "ts": epoch_ms(), "kind": "lane_event", "lane": 3, "event": "crashed",
            "detail": "its process exited", "severity": "error"
        })),
        item(json!({
            "id": "e_7", "ts": epoch_ms(), "kind": "notice", "severity": "warn",
            "text": "lane 3 was restarted by its supervisor", "source": "swarm", "durable": true
        })),
        item(json!({
            "id": "e_8", "ts": epoch_ms(), "kind": "compaction",
            "summary": "The redesign was planned; the session crate is next.",
            "tokens_before": 210000, "tokens_after": 12000, "manual": false
        })),
        item(json!({
            "id": "e_9", "ts": epoch_ms(), "kind": "run_outcome", "outcome": "aborted"
        })),
        assistant("e_4", "", false),
    ]
}

struct Demo {
    transcript: Entity<TranscriptView>,
    items: Vec<Item>,
    streamed: usize,
}

impl Demo {
    fn new(cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(TranscriptView::new);
        let mut demo = Self {
            transcript,
            items: settled_items(),
            streamed: 0,
        };
        demo.publish(cx);

        // The message that is still arriving: one chunk every CHUNK_DELAY, exactly as a
        // stream of `item.append` ops would land.
        let chunks = chunks();
        cx.spawn(async move |demo, cx| {
            for count in 1..=chunks.len() {
                cx.background_executor().timer(CHUNK_DELAY).await;
                let landed = demo.update(cx, |demo, cx| {
                    demo.streamed = count;
                    demo.publish(cx);
                });
                if landed.is_err() {
                    break;
                }
            }
        })
        .detach();

        demo
    }

    /// Hand the view the items it holds: the settled turn, and the message as far as it has
    /// arrived.
    fn publish(&mut self, cx: &mut Context<Self>) {
        let text = source_at(self.streamed);
        let streaming = self.streamed < chunks().len();
        let mut items = self.items.clone();
        items.push(assistant("e_4", &text, streaming));
        items.push(user("e_10", "and then the view crates", "queued"));
        let view = self.transcript.clone();
        view.update(cx, |view, cx| view.replace(items, cx));
        cx.notify();
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "the coordinator's transcript · {} chunks landed",
                        self.streamed
                    )),
            )
    }
}

fn main() {
    let _ = Queue::AfterRun;
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let bounds = Bounds {
                origin: gpui_kit::point(px(120.), px(120.)),
                size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
            };
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |_window, cx| cx.new(Demo::new),
            )
            .expect("open the demo window");
        });
}
