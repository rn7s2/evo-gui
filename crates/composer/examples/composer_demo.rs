//! composer_demo — the composer, live, over a scripted topic.
//!
//! It cycles the topic's busy flag, so the one button changes its face between `Send`
//! and `Stop swarm`, and it prints the events an owner turns into ops (`input.send`,
//! `run.interrupt` with scope `session` or `swarm`, `model.set`, `thinking.set`). The
//! chips are the topic's own `segments`, the todos its own todos and the goal its own
//! goal (CONTRACT §4.2), so the box is showing what a server published rather than a
//! picture of one.
//!
//! ```sh
//! cargo run --example composer_demo
//! ```

use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    div, px, size, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _,
    Render, Styled as _, Task, WeakEntity, Window, WindowBounds, WindowOptions,
};
use session::TopicState;

use composer::{Candidate, Composer, ComposerEvent, ModelRow};

const WINDOW_SIZE: (f32, f32) = (1000., 720.);

/// The topic's own state, as a server would publish it (`GET /snapshot`): the
/// segments, the model, the effort, the goal and the todos the box draws — the box
/// composes none of them.
fn state(busy: bool) -> TopicState {
    let tokens = 48000;
    TopicState::from_json(&serde_json::json!({
        "status": if busy { "running" } else { "idle" },
        "model": {"id": "stub-a", "provider": "openai", "ready": true},
        "thinking": "high",
        "context": {"tokens": tokens, "window": 936000, "source": "usage"},
        "goal": {"goal_id": "a1b2c3d4", "objective": "Ship the redesign: every screen taken \
                  from the design, every state bound to the real view model, and the gate \
                  green.", "status": "active", "budget": 50000, "tokens": 12000},
        "todos": [
            {"text": "port the view model", "status": "done"},
            {"text": "trim the workspace", "status": "in_progress"},
            {"text": "re-take the screens", "status": "pending"},
        ],
        "segments": [
            {"name": "model", "order": 100, "side": "left", "text": "stub-a", "data": {}},
            {"name": "thinking", "order": 200, "side": "left", "text": "high", "data": {}},
            {"name": "context", "order": 300, "side": "left",
             "text": format!("ctx {}/936k (5%)", session::k_tokens(tokens)), "data": {}},
            {"name": "cache_stats", "order": 350, "side": "left", "text": "97% cached",
             "data": {}},
            {"name": "goal", "order": 400, "side": "left",
             "text": "goal a1b2c3d4 (active) 12k/50k", "data": {}},
        ],
    }))
}

/// The models the drawer offers, as `GET /catalog` lists them (§5.6): the detail line and
/// the levels beside it are the catalog's own — a model that takes the whole ladder, one
/// whose provider offers three rungs, and one that takes no effort setting at all.
fn models() -> Vec<ModelRow> {
    vec![
        ModelRow {
            id: "stub-a".to_string(),
            provider: "openai".to_string(),
            detail: "200k ctx · vision · effort low, medium, high, xhigh, max".to_string(),
            effort_levels: rungs(&["low", "medium", "high", "xhigh", "max"]),
            reason: None,
        },
        ModelRow {
            id: "stub-b".to_string(),
            provider: "openai".to_string(),
            detail: "936k ctx · effort low, high, max".to_string(),
            effort_levels: rungs(&["low", "high", "max"]),
            reason: None,
        },
        ModelRow {
            id: "stub-c".to_string(),
            provider: "proxy".to_string(),
            detail: "1M ctx".to_string(),
            effort_levels: Vec::new(),
            reason: Some("no credential".to_string()),
        },
    ]
}

fn rungs(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// The commands the registry lists, as `GET /catalog` would: what a `/word`
/// completes against, and what `command.run` answers for.
fn commands() -> Vec<Candidate> {
    [
        ("help", "commands and keys"),
        ("image", "attach an image"),
        ("lore", "durable guidance"),
        ("memory", "what is remembered"),
        ("reload", "reload the image's own code"),
        ("theme", "switch the light/dark theme"),
        ("eval", "evaluate one form in the live image"),
    ]
    .iter()
    .map(|(name, description)| Candidate {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

struct Demo {
    composer: Entity<Composer>,
    busy: bool,
    _cycle: Task<()>,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        cx.subscribe_in(
            &composer,
            window,
            |_this, _composer, event: &ComposerEvent, _window, _cx| {
                // A real owner sends one op here.
                match event {
                    ComposerEvent::Send(outgoing) => println!("input.send: {outgoing:?}"),
                    ComposerEvent::Interrupt => println!("run.interrupt scope=session"),
                    ComposerEvent::StopSwarm => println!("run.interrupt scope=swarm"),
                    ComposerEvent::ModelSet { id, provider } => {
                        println!("model.set: {id}@{provider}")
                    }
                    ComposerEvent::ThinkingSet(level) => println!("thinking.set: {level}"),
                    ComposerEvent::Command { name, args } => {
                        println!("command.run: {name} {args:?} — /help to see the registry")
                    }
                    ComposerEvent::Complete { text, cursor } => println!(
                        "complete: what is at {cursor} in {text:?} — the popup's own question"
                    ),
                }
            },
        )
        .detach();

        let mut demo = Self {
            composer,
            busy: false,
            _cycle: Task::ready(()),
        };
        demo.publish(cx);
        demo._cycle = cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if this
                .update(cx, |demo, cx| {
                    demo.busy = !demo.busy;
                    demo.publish(cx);
                })
                .is_err()
            {
                return;
            }
        });
        demo
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        let busy = self.busy;
        let state = state(busy);
        self.composer.update(cx, |composer, cx| {
            composer.set_pane_height(px(WINDOW_SIZE.1 - 200.), cx);
            composer.set_agent(&state, "Coordinator", true, cx);
            composer.set_catalog(
                vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "xhigh".to_string(),
                    "max".to_string(),
                ],
                models(),
                commands(),
                cx,
            );
            composer.set_swarm_busy(busy, cx);
        });
        cx.notify();
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // The conversation column, with the box at its foot — where the tab
                // page puts it.
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .child(div().p_4().child("transcript and todos")),
                    )
                    .child(self.composer.clone()),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui_kit::point(px(120.), px(120.)),
                        size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| Demo::new(window, cx)),
            )
            .expect("open the composer window");
        });
}
