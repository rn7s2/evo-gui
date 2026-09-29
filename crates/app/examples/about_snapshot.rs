//! The About dialog, rendered to PNGs without a window on screen.
//!
//! ```sh
//! cargo run -p evo-desktop --example about_snapshot -- --capture <dir>
//! ```
//!
//! Capture mode drives the production path through GPUI's headless renderer: the
//! app's own `Shell`, the app's window options, the version probe on its thread,
//! and the same call the About menu item makes. Two pictures fall out — the dialog
//! in the light theme and in the dark one — which is also the check that it is
//! drawn in theme tokens rather than in colours of its own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, HeadlessAppContext, Point, WindowBounds,
    WindowOptions,
};
use store::app_state::{AppState, Theme};
use store::model_cache::ModelCache;
use store::paths::Root;
use workspace::WorkspaceView;

/// A window just big enough to hold the dialog with a margin: the subject is the
/// dialog, not the window around it.
const WINDOW_SIZE: (f32, f32) = (760., 620.);

/// How long the version probe is given before the picture is taken without it.
const VERSIONS_WAIT: Duration = Duration::from_secs(10);

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
            eprintln!("usage: about_snapshot --capture <dir>");
            std::process::exit(2);
        }
    }
}

fn capture(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let root =
        Root::at(std::env::temp_dir().join(format!("evo-desktop-about-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(root.path());

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The version probe reads it on a thread of its own: real I/O is expected.
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

    // What `run` does with a window: the theme, then the binaries' versions.
    cx.update(evo_desktop::start_version_probe);
    wait_for_versions(&mut cx, window);

    // The About menu item's own call.
    cx.update(evo_desktop::open_about);

    for (mode, name) in [
        (ThemeMode::Light, "about-light.png"),
        (ThemeMode::Dark, "about-dark.png"),
    ] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        shot(&mut cx, window, dir, name)?;
    }
    let _ = std::fs::remove_dir_all(root.path());
    Ok(())
}

/// Wait until both binaries have answered, or give up — the dialog shows what it
/// has either way, and the picture is still the picture.
fn wait_for_versions(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    let deadline = Instant::now() + VERSIONS_WAIT;
    loop {
        let ready = cx
            .update_window(window, |_, _, cx| {
                let versions = &cx.global::<Shell>().versions;
                versions.swarm.is_some() && versions.agent.is_some()
            })
            .unwrap_or(false);
        if ready || Instant::now() >= deadline {
            return;
        }
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
    // The icon is an embedded PNG: `img()` hands it to gpui's asset system, which
    // decodes it off the UI thread and repaints when it is ready. Pump until it
    // has had the time, so the picture shows the icon rather than an empty box.
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    }
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
