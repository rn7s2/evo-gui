//! A window with just the composer in a 360 px column, as the tab page's right
//! column renders it (§7.3).
//!
//! The activity cycles Idle → Running → Compacting on a timer so the one button
//! can be watched changing its face; Enter / Shift+Enter / Esc and both button
//! faces print what they asked for, and a send clears the input only once the
//! (simulated) request comes back ok.
//!
//! ```sh
//! cargo run -p composer --example composer_demo
//! ```

use std::time::Duration;

use composer::{Composer, ComposerEvent};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, WeakEntity, Window, WindowBounds, WindowOptions, div, point,
    px, size,
};
use session::Activity;

/// The line the TUI shows for this session, wider than the column on purpose.
const READOUT: &str = "ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%) · 97% cached · \
                       goal a1b2c3d4 (active) 12k/50k";

struct Demo {
    composer: Entity<Composer>,
    activity: Activity,
    /// The activity cycle; kept so it is not cancelled.
    _cycle: Task<()>,
    /// The simulated in-flight request whose reply finishes the composer.
    _finish: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        composer.update(cx, |composer, cx| composer.set_readout(READOUT, cx));

        let subscription = cx.subscribe_in(
            &composer,
            window,
            |this, composer, event: &ComposerEvent, window, cx| {
                // A real owner posts /prompt (or /interrupt) here and calls
                // `request_finished` when the reply lands — ok only if the
                // server took the text. A failed send keeps the draft.
                let reply = match event {
                    ComposerEvent::Send(text) => {
                        println!("send: {text:?}  (in flight until the reply)");
                        true
                    }
                    ComposerEvent::Interrupt => {
                        println!("interrupt  (the draft is untouched)");
                        false
                    }
                };
                this._finish = Some(cx.spawn_in(window, {
                    let composer = composer.clone();
                    async move |_, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(700))
                            .await;
                        composer
                            .update_in(cx, |composer, window, cx| {
                                composer.request_finished(reply, window, cx)
                            })
                            .ok();
                    }
                }));
                cx.notify();
            },
        );

        let cycle = cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if this.update(cx, |this, cx| this.cycle_activity(cx)).is_err() {
                return;
            }
        });

        Self {
            composer,
            activity: Activity::Idle,
            _cycle: cycle,
            _finish: None,
            _subscriptions: vec![subscription],
        }
    }

    fn cycle_activity(&mut self, cx: &mut Context<Self>) {
        self.activity = match self.activity {
            Activity::Idle => Activity::Running,
            Activity::Running => Activity::Compacting,
            Activity::Compacting => Activity::Idle,
        };
        println!("activity: {:?}", self.activity);
        self.composer
            .update(cx, |composer, cx| composer.set_activity(self.activity, cx));
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .items_stretch()
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
                // The tab page's right column (§7.3).
                div()
                    .w(px(360.))
                    .h_full()
                    .p_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(self.composer.clone()),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(120.), px(120.)),
                    size(px(900.), px(420.)),
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Demo::new(window, cx)))
                .expect("failed to open the composer demo window");
        });
}
