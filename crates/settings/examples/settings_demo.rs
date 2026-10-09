//! The Settings dialog on its own (§13): the two binary paths with their version checks, the
//! theme, the terminal's font, and Reset / Cancel / Save.
//!
//! ```sh
//! cargo run -p settings --example settings_demo                    # live window
//! cargo run -p settings --example settings_demo -- --capture <dir> # render frames to PNGs
//! ```
//!
//! Capture mode drives the same panel through GPUI's headless renderer, so the pictures do
//! not depend on a window being on screen — which is what makes them reproducible on a locked
//! machine. It answers the two paths from a table read *before* the first frame, with
//! [`settings::probe`] on the real paths: the same answers the panel would have waited for,
//! delivered without waiting on a process, so a picture does not depend on one finishing. The
//! live window uses the panel's own prober — a real `--version` on the app's executor —
//! because there nobody is waiting on a still frame.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, IntoElement, ParentElement as _, Render, Styled as _, Subscription, Window,
    WindowBounds, WindowOptions,
};
use settings::{probe, Check, SettingsEvent, SettingsPanel, SettingsValues, THEME_ID};
use store::Binaries;

/// The live window: the dialog, with the page it sits on around it.
const WINDOW_SIZE: (f32, f32) = (720., 520.);
/// A capture is the dialog plus a margin, so the edge of it is on screen.
const CAPTURE_SIZE: (f32, f32) = (640., 440.);

/// How long to let the app clock settle before a capture, so a theme change or a hover fill
/// is at full strength rather than mid-transition.
const SETTLE: Duration = Duration::from_millis(60);

/// The path the demo points `evo-swarm` at when it shows a broken one: nothing is there, so
/// the row says the one thing a real probe would say about it too.
const BROKEN_PATH: &str = "/usr/local/bin/evo-swarm-old";

/// The panel's answers for a capture, read here instead of by the panel: on this machine they
/// are the binaries' real `--version` lines, and on a machine without them the rows say why.
fn verdicts() -> Vec<(PathBuf, Check)> {
    let defaults = Binaries::default();
    [
        (defaults.evo_swarm, "evo-swarm"),
        (defaults.evo_agent, "evo-agent"),
    ]
    .into_iter()
    .map(|(path, name)| {
        let answer = probe(&path, name);
        (path, answer)
    })
    .collect()
}

struct Demo {
    panel: Entity<SettingsPanel>,
    /// Every Save is printed, so a live window says what it handed over.
    _subscription: Subscription,
}

impl Demo {
    /// `scripted` is capture mode: the paths are answered from [`verdicts`] rather than by
    /// running them, so a frame does not wait on a process.
    fn new(window: &mut Window, cx: &mut Context<Self>, scripted: bool) -> Demo {
        let panel = cx.new(|cx| SettingsPanel::new(SettingsValues::default(), window, cx));
        let subscription = cx.subscribe(&panel, |_, _, event: &SettingsEvent, _| {
            let SettingsEvent::Saved(values) = event;
            println!(
                "[demo] Save: evo-swarm={} evo-agent={} theme={} font={}",
                values.evo_swarm.display(),
                values.evo_agent.display(),
                values.theme.as_str(),
                values.terminal_font
            );
        });
        if scripted {
            panel.update(cx, |panel, cx| panel.set_verdicts(verdicts(), cx));
        }
        Demo {
            panel,
            _subscription: subscription,
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .child(self.panel.clone())
    }
}

fn window_options(window_size: (f32, f32)) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(80.), px(80.)),
            size(px(window_size.0), px(window_size.1)),
        ))),
        ..Default::default()
    }
}

fn run_window() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(window_options(WINDOW_SIZE), cx, |window, cx| {
                cx.new(|cx| Demo::new(window, cx, false))
            })
            .expect("open the settings window");
        });
}

/// Render the dialog to one PNG per state: as it opens in each theme, then with a path that
/// is not there, then with Dark chosen through the row itself.
fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        std::sync::Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, demo) = cx.update(|cx| {
        gpui_kit::open_window(window_options(CAPTURE_SIZE), cx, |window, cx| {
            cx.new(|cx| Demo::new(window, cx, true))
        })
    })?;
    light(&mut cx);

    // 1. As the app opens it: both paths answered by the binaries themselves, System chosen.
    shot(&mut cx, window, dir, "01-paths-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "02-paths-dark.png")?;
    light(&mut cx);

    // 2. A path with nothing at it: the row says why, in the danger tone, and the path is
    //    still there to be corrected.
    focus_into(&mut cx, window, &demo)?;
    type_over(&mut cx, window, BROKEN_PATH)?;
    shot(&mut cx, window, dir, "03-broken-path-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "04-broken-path-dark.png")?;
    light(&mut cx);

    // 3. Dark, taken through the row itself: the pointer lands on the last of the three
    //    choices, and the window behind the dialog is already dark — the live part of the
    //    panel. The row keeps the keyboard afterwards, so the arrows work from there.
    choose_dark(&mut cx, window)?;
    shot(&mut cx, window, dir, "05-dark-chosen.png")?;

    Ok(())
}

/// Where the app puts the keyboard when the dialog opens.
fn focus_into(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    demo: &Entity<Demo>,
) -> Result<(), Box<dyn std::error::Error>> {
    let panel = cx.update(|cx| demo.read(cx).panel.clone());
    cx.update_window(window, |_, window, cx| {
        panel.update(cx, |panel, cx| panel.focus_into(window, cx));
        window.render_frame(cx);
    })?;
    Ok(())
}

/// Type over the focused field's whole value, the way correcting a path starts.
fn type_over(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.press("cmd-a", cx);
        window.input(text, cx);
        window.render_frame(cx);
    })?;
    Ok(())
}

/// Click the last of the three theme choices — Dark — as a person would: the row is the
/// target, and the offset lands in its third tab rather than needing the tab's own id.
fn choose_dark(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // The row is exactly the width of its three choices, so the middle of the last third
        // is `Dark` — no arithmetic on the layout, and no id inside the kit's tab bar.
        let row = window.find(THEME_ID).bounds().size;
        window.click_at(THEME_ID, point(row.width * 5. / 6., row.height / 2.), cx);
        window.render_frame(cx);
    })?;
    Ok(())
}

fn light(cx: &mut HeadlessAppContext) {
    cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
}

fn dark(cx: &mut HeadlessAppContext) {
    cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
}

/// Let the app clock settle, draw a frame, and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..2 {
        cx.advance_clock(SETTLE);
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    }
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
            eprintln!("usage: settings_demo [--capture <dir>]");
            std::process::exit(2);
        }
    }
}
