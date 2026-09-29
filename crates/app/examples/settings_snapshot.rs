//! The Settings panel over the app's window, rendered to PNGs without a window on
//! screen.
//!
//! ```sh
//! cargo run -p evo-desktop --example settings_snapshot -- --capture <dir>
//! ```
//!
//! Capture mode drives the production path through GPUI's headless renderer: the
//! app's own `Shell` and window options, and the same call the Settings… menu item
//! makes. Two pictures fall out — the panel in the light theme and in the dark one
//! — which is also the check that it sits over the workspace rather than replacing
//! it, and that its two path rows have answered (the real prober runs the real
//! binaries' `--version`).
//!
//! The paths shown are the installed ones, so the pictures say what a person sees
//! on a working machine. `EVO_SWARM_BIN` / `EVO_AGENT_BIN` move them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{open_settings_panel, AppLog, Shell};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, HeadlessAppContext, Point, WindowBounds,
    WindowOptions,
};
use settings::{AGENT_STATUS_ID, SWARM_STATUS_ID};
use store::app_state::{AppState, Theme};
use store::model_cache::ModelCache;
use store::paths::Root;
use workspace::WorkspaceView;

/// The window the panel sits over: §7.1's own size, so the picture shows the
/// dialog *in* the app rather than in a box made for it.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// How long the two path checks are given before the picture is taken without
/// them: running two binaries is real process work.
const CHECKS_WAIT: Duration = Duration::from_secs(20);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, dir] if flag == "--capture" => {
            if let Err(error) = capture(std::path::Path::new(dir)) {
                eprintln!("capture failed: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: settings_snapshot --capture <dir>");
            std::process::exit(2);
        }
    }
}

fn capture(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let root =
        Root::at(std::env::temp_dir().join(format!("evo-desktop-settings-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(root.path());

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The path checks run on threads of their own: real I/O is expected.
    cx.allow_parking();

    let (window, _view) = cx.update({
        let root = root.clone();
        move |cx| {
            let log = AppLog::open(&root);
            let state = AppState {
                theme: Theme::System,
                ..AppState::default()
            };
            Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
            let config = Arc::new(evo_desktop::swarm_config(cx));
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                    })),
                    focus: false,
                    show: false,
                    ..workspace::window_options(cx)
                },
                cx,
                |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
            )
        }
    })?;
    let window: AnyWindowHandle = window;

    // The Settings… menu item's own call, over the window the app opened.
    cx.update_window(window, |_, window, cx| {
        open_settings_panel(window, cx);
    })?;
    wait_for_checks(&mut cx, window);

    for (mode, name) in [
        (ThemeMode::Light, "settings-light.png"),
        (ThemeMode::Dark, "settings-dark.png"),
    ] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        shot(&mut cx, window, dir, name)?;
    }
    let _ = std::fs::remove_dir_all(root.path());
    Ok(())
}

/// Wait until both rows have an answer, or give up — the picture is still the
/// picture, and a `checking…` row is what it would show.
fn wait_for_checks(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    let answered = |cx: &mut HeadlessAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            [SWARM_STATUS_ID, AGENT_STATUS_ID].iter().all(|id| {
                window
                    .try_find(*id)
                    .and_then(|status| {
                        status
                            .label()
                            .map(str::to_owned)
                            .or_else(|| status.value().map(str::to_owned))
                    })
                    .is_some_and(|line| !line.contains("checking"))
            })
        })
        .unwrap_or(false)
    };
    let deadline = Instant::now() + CHECKS_WAIT;
    while !answered(cx) && Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &std::path::Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Two frames: the first lays out what the last update changed (the dialog),
    // the second paints it.
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
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
