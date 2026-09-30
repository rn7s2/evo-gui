//! composer_demo — the composer, live, over a scripted swarm.
//!
//! It cycles the swarm's busy flag, so the one button changes its face between `Send` and
//! `Stop swarm`, and it prints the events an owner turns into ops (`input.send`,
//! `run.interrupt` with scope `session` or `swarm`) instead of posting anything. The
//! status row is built from a topic's own `segments` (CONTRACT §4.2), rendered as they
//! are.
//!
//! ```sh
//! cargo run --example composer_demo
//! ```

use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    div, px, size, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Task, WeakEntity, Window, WindowBounds, WindowOptions,
};
use session::{Segment, Side};

use composer::{Composer, ComposerEvent};

const COLUMN_WIDTH: f32 = 360.;
const WINDOW_SIZE: (f32, f32) = (1000., 720.);

/// The swarm topic's left-hand segments, as the server's registry builds them.
fn left_segments(lanes: u64) -> Vec<Segment> {
    let tokens = 48000 + lanes * 1000;
    vec![
        segment("model", 100, "stub-a".to_string()),
        segment("thinking", 200, "high".to_string()),
        segment(
            "context",
            300,
            format!("ctx {}/936k (5%)", session::k_tokens(tokens)),
        ),
        segment("cache-stats", 350, "97% cached".to_string()),
        segment("goal", 400, "goal a1b2c3d4 (active) 12k/50k".to_string()),
    ]
}

/// The swarm's own count, which the row puts at its right-hand end.
fn right_segments(lanes: u64, waiting: bool) -> Vec<Segment> {
    vec![segment(
        "swarm",
        1,
        if waiting {
            format!("{lanes} lanes · waiting on them")
        } else {
            format!("{lanes} lanes")
        },
    )]
}

fn segment(name: &str, order: i64, text: String) -> Segment {
    Segment {
        name: name.to_string(),
        order,
        side: Side::Left,
        text,
        data: Default::default(),
    }
}

struct Demo {
    composer: Entity<Composer>,
    lanes: u64,
    busy: bool,
    waiting: bool,
    _cycle: Task<()>,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        cx.subscribe_in(
            &composer,
            window,
            |_this, _composer, event: &ComposerEvent, _window, _cx| {
                // A real owner sends one op here: `input.send`, or `run.interrupt` with
                // scope `session` (esc) / `swarm` (the button while the swarm is busy).
                match event {
                    ComposerEvent::Send(text) => println!("input.send: {text:?}"),
                    ComposerEvent::Interrupt => println!("run.interrupt scope=session"),
                    ComposerEvent::StopSwarm => println!("run.interrupt scope=swarm"),
                }
            },
        )
        .detach();

        let mut demo = Self {
            composer,
            lanes: 6,
            busy: false,
            waiting: false,
            _cycle: Task::ready(()),
        };
        demo.publish(cx);
        demo._cycle = cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if this
                .update(cx, |demo, cx| {
                    (demo.busy, demo.waiting) = match (demo.busy, demo.waiting) {
                        (false, _) => (true, false),
                        (true, false) => (true, true),
                        (true, true) => (false, false),
                    };
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
        let (lanes, busy) = (self.lanes, self.busy);
        let waiting = self.waiting;
        self.composer.update(cx, |composer, cx| {
            composer.set_segments(&left_segments(lanes), &right_segments(lanes, waiting), cx);
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
                div()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .text_color(cx.theme().muted_foreground)
                    .child("tab page: lanes | transcript | todos"),
            )
            .child(
                div()
                    .id("composer-column")
                    .w(px(COLUMN_WIDTH))
                    .h_full()
                    .p_3()
                    .border_color(cx.theme().border)
                    .border_l_1()
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
