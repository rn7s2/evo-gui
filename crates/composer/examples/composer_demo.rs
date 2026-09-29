//! A window with just the composer in a 360 px column, as the tab page's right
//! column renders it (§7.3).
//!
//! The activity cycles Idle → Running → Compacting on a timer so the one button
//! can be watched changing its face; Enter / Shift+Enter / Esc and both button
//! faces print what they asked for, and a send clears the input only once the
//! (simulated) request comes back ok.
//!
//! ```sh
//! cargo run -p composer --example composer_demo                    # live window
//! cargo run -p composer --example composer_demo -- --capture <dir> # render states to PNGs
//! ```
//!
//! Capture mode drives the same frames through GPUI's headless renderer, so the
//! pictures do not depend on a window being on screen — which is what makes them
//! reproducible on a locked machine.

use std::path::Path;
use std::time::Duration;

use composer::{Composer, ComposerEvent};
use gpui_kit::component::{h_flex, ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, IntoElement, InteractiveElement as _, ParentElement as _, Render,
    Styled as _, Subscription, Task, WeakEntity, Window, WindowBounds, WindowOptions,
};
use session::Activity;

/// The line the TUI shows for this session, wider than the column on purpose.
const READOUT: &str = "ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%) · 97% cached · \
                       goal a1b2c3d4 (active) 12k/50k";
/// The same line once the session is further along: wider than the window, so
/// the status row ellipsizes it and the tooltip carries the whole thing.
const LONG_READOUT: &str = "ark-deepseek-v4.1-flash · max · ctx 736k/936k (79%) · 97% cached · \
                            goal a1b2c3d4 (active) 12k/50k · turn 84 · 2 lanes working · \
                            3 reports since the last resync";

/// The tab page's right column (§7.3), and the window the capture opens.
const COLUMN_WIDTH: f32 = 360.;
const WINDOW_SIZE: (f32, f32) = (1000., 720.);
const CAPTURE_SIZE: (f32, f32) = (1000., 720.);

/// What the input holds once it has a draft.
const DRAFT: &str = "Fold the transcript rows into the new session model, then re-run the \
                     proofs and tell me what changed.";

/// The states worth a picture: the input empty (Send disabled), the input with a
/// draft and the caret in it, a running coordinator (the same button reading
/// Stop), and a readout too long for the column.
const IDLE_EMPTY_SHOT: &str = "01-idle-empty-send-disabled.png";
const DRAFT_SHOT: &str = "02-idle-with-draft-focus-ring.png";
const RUNNING_SHOT: &str = "03-running-stop.png";
const LONG_READOUT_SHOT: &str = "04-long-readout-tooltip.png";
const DARK_IDLE_EMPTY_SHOT: &str = "05-dark-idle-empty.png";
const DARK_DRAFT_SHOT: &str = "06-dark-idle-with-draft.png";
const DARK_RUNNING_SHOT: &str = "07-dark-running-stop.png";
const DARK_LONG_READOUT_SHOT: &str = "08-dark-long-readout-tooltip.png";

struct Demo {
    composer: Entity<Composer>,
    activity: Activity,
    /// The activity cycle; kept so it is not cancelled. The capture drives the
    /// activity by hand instead.
    _cycle: Option<Task<()>>,
    /// The simulated in-flight request whose reply finishes the composer.
    _finish: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut demo = Self::staged(window, cx);
        demo._cycle = Some(demo.cycle(cx));
        demo
    }

    /// The window root without the activity cycle: a capture drives every state
    /// itself, so nothing moves between two shots.
    fn staged(window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        Self {
            composer,
            activity: Activity::Idle,
            _cycle: None,
            _finish: None,
            _subscriptions: vec![subscription],
        }
    }

    /// Cycle the coordinator's activity on a timer, so the live window shows the
    /// one button changing its face.
    fn cycle(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if this.update(cx, |this, cx| this.cycle_activity(cx)).is_err() {
                return;
            }
        })
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
                // The tab page's right column (§7.3): the composer at the top,
                // and the rest of the column left empty.
                div()
                    .id("composer-column")
                    .w(px(COLUMN_WIDTH))
                    .h_full()
                    .p_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(self.composer.clone()),
            )
    }
}

fn window_options(window_size: (f32, f32), show: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(120.), px(120.)),
            size(px(window_size.0), px(window_size.1)),
        ))),
        focus: show,
        show,
        ..Default::default()
    }
}

fn run_window() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            gpui_kit::open_window(window_options(WINDOW_SIZE, true), cx, |window, cx| {
                cx.new(|cx| Demo::new(window, cx))
            })
            .expect("failed to open the composer demo window");
        });
}

/// Render the composer's states to one PNG each, in both themes: idle and empty,
/// idle with a draft and the caret in the input, a running coordinator, and a
/// readout too long for the column with its tooltip shown.
fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        std::sync::Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, demo) = open_capture_window(&mut cx, CAPTURE_SIZE)?;

    // Idle and empty: the one button is a Send with nothing to send, so it is
    // disabled and says so.
    shot(&mut cx, window, dir, IDLE_EMPTY_SHOT)?;

    // A draft, with the caret in the input so the focus ring shows.
    type_draft(&mut cx, window, &demo)?;
    shot(&mut cx, window, dir, DRAFT_SHOT)?;

    // The coordinator is running: the same button reads Stop.
    set_activity(&mut cx, window, &demo, Activity::Running)?;
    shot(&mut cx, window, dir, RUNNING_SHOT)?;

    // A readout wider than the column: ellipsized, with the whole line on hover.
    hover_long_readout(&mut cx, window, &demo)?;
    shot(&mut cx, window, dir, LONG_READOUT_SHOT)?;

    cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
    println!("[capture] theme mode: dark");

    // Back to the idle states for the dark series: the theme changed, the
    // coordinator is not running any more and the readout is short again.
    set_activity(&mut cx, window, &demo, Activity::Idle)?;
    set_readout(&mut cx, window, &demo, READOUT)?;
    clear_draft(&mut cx, window, &demo)?;
    shot(&mut cx, window, dir, DARK_IDLE_EMPTY_SHOT)?;

    type_draft(&mut cx, window, &demo)?;
    shot(&mut cx, window, dir, DARK_DRAFT_SHOT)?;

    set_activity(&mut cx, window, &demo, Activity::Running)?;
    shot(&mut cx, window, dir, DARK_RUNNING_SHOT)?;

    hover_long_readout(&mut cx, window, &demo)?;
    shot(&mut cx, window, dir, DARK_LONG_READOUT_SHOT)?;

    Ok(())
}

fn open_capture_window(
    cx: &mut HeadlessAppContext,
    size: (f32, f32),
) -> Result<(AnyWindowHandle, Entity<Demo>), Box<dyn std::error::Error>> {
    let (handle, demo) = cx.update(|cx| {
        gpui_kit::open_window(window_options(size, false), cx, |window, cx| {
            cx.new(|cx| Demo::staged(window, cx))
        })
    })?;
    Ok((handle.into(), demo))
}

/// Put the caret in the input and type the draft, as a reader would.
fn type_draft(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
) -> Result<(), Box<dyn std::error::Error>> {
    focus_input(cx, window, demo)?;
    cx.update_window(window, |_, window, cx| window.input(DRAFT, cx))?;
    Ok(())
}

/// Select the draft and delete it, leaving the input empty again.
fn clear_draft(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
) -> Result<(), Box<dyn std::error::Error>> {
    focus_input(cx, window, demo)?;
    cx.update_window(window, |_, window, cx| {
        window.press("cmd-a", cx);
        window.press("backspace", cx);
    })?;
    Ok(())
}

fn focus_input(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        let composer = demo.read(cx).composer.clone();
        composer.update(cx, |composer, cx| composer.focus_input(window, cx));
    })?;
    Ok(())
}

fn set_activity(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
    activity: Activity,
) -> Result<(), Box<dyn std::error::Error>> {
    demo.update(cx, |demo, cx| {
        demo.activity = activity;
        demo.composer
            .update(cx, |composer, cx| composer.set_activity(activity, cx));
    });
    settle(cx, window)?;
    Ok(())
}

fn set_readout(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
    line: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    demo.update(cx, |demo, cx| {
        demo.composer
            .update(cx, |composer, cx| composer.set_readout(line, cx));
    });
    settle(cx, window)?;
    Ok(())
}

/// Put the pointer on the readout after a line too long for the column, and let
/// the tooltip's delay elapse.
fn hover_long_readout(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
) -> Result<(), Box<dyn std::error::Error>> {
    set_readout(cx, window, demo, LONG_READOUT)?;

    cx.update_window(window, |_, window, cx| {
        window.hover(composer::READOUT_ID, cx);
    })?;
    // The tooltip appears on the app clock, which the headless context only
    // advances when asked.
    cx.advance_clock(Duration::from_millis(1200));
    settle(cx, window)?;
    Ok(())
}

/// Let anything animating settle and draw the frame again: a capture taken
/// mid-transition is not a capture of the state.
fn settle(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    std::thread::sleep(Duration::from_millis(400));
    for _ in 0..2 {
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    }
    Ok(())
}

/// Draw a frame and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    settle(cx, window)?;
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
            eprintln!("usage: composer_demo [--capture <dir>]");
            std::process::exit(2);
        }
    }
}
