//! The window chrome in pictures: the tab strip, the empty tab (§7.2) and the
//! tab page skeleton (§7.3).
//!
//! ```sh
//! cargo run -p workspace --example workspace_snapshot                       # live window
//! cargo run -p workspace --example workspace_snapshot -- --capture <dir>    # PNGs
//! ```
//!
//! Capture mode drives the same views through GPUI's headless renderer, so the
//! pictures do not need a window on screen — which is what makes them
//! reproducible on a machine whose screen is locked. The swarm wiring does not
//! exist yet, so the states that only a live swarm could produce (booting, a
//! failed boot, a running tab page) are driven directly here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::TitleBar;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, HeadlessAppContext, WindowBounds,
    WindowOptions,
};
use workspace::{TabState, WorkspaceView};

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// A tab transition samples the app clock; the headless context only advances it
/// when asked.
const SETTLE: Duration = Duration::from_millis(400);

/// A folder the capture pretends to have chosen, so the boot screens have a path
/// to show.
const FOLDER: &str = "/Users/bytedance/coding/evo-gui";

/// The tail a boot failure shows instead of a log viewer (§3, §9.7).
const LOG_TAIL: &str = "\
evo-swarm: serve on 127.0.0.1:54113
evo-swarm: workers 6, model default
evo-agent: no provider registered for model 'ark-deepseek-v4.1-flash'
evo-agent: registered models: none
evo-swarm: coordinator exited with status 1
evo-swarm: shutting down lanes";

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
            eprintln!("usage: workspace_snapshot [--capture <dir>]");
            std::process::exit(2);
        }
    }
}

fn run_window() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(workspace::window_options(cx), cx, |window, cx| {
                cx.new(|cx| WorkspaceView::new(window, cx))
            })
            .expect("open the workspace snapshot window");
        });
}

/// The window options the capture uses: the production title bar, at the size
/// the app opens with, without touching the screen.
fn capture_window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
        })),
        focus: false,
        show: false,
        ..TitleBar::window_options()
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(capture_window_options(), cx, |window, cx| {
            cx.new(|cx| WorkspaceView::new(window, cx))
        })
    })?;
    let window: AnyWindowHandle = window.into();

    // 1. What the app opens with: one empty tab, the choosers and the history.
    shot(&mut cx, window, dir, "01-empty-tab.png")?;

    // 2. The `+` appended a second tab and selected it (§7.1).
    click(&mut cx, window, "tab-add")?;
    shot(&mut cx, window, dir, "02-two-tabs.png")?;

    // 3. A folder was chosen: the tab is booting its swarm (§3).
    let folder = PathBuf::from(FOLDER);
    view.update(&mut cx, |view, cx| {
        view.selected_tab()
            .update(cx, |tab, cx| tab.begin_boot(folder.clone(), cx));
    });
    shot(&mut cx, window, dir, "03-booting.png")?;

    // 4. The swarm answered: the tab page skeleton (§7.3).
    view.update(&mut cx, |view, cx| {
        view.selected_tab()
            .update(cx, |tab, cx| tab.mark_running(cx));
    });
    shot(&mut cx, window, dir, "04-tab-page.png")?;

    // 5. The swarm never came up: the log tail and a retry (§9.7).
    view.update(&mut cx, |view, cx| {
        view.selected_tab()
            .update(cx, |tab, cx| tab.fail(LOG_TAIL.to_string(), cx));
    });
    shot(&mut cx, window, dir, "05-boot-failed.png")?;

    // The captured tab is the one that failed: the pictures match the states
    // the run drove, not whatever the renderer left behind.
    let state = cx.update(|cx| view.read(cx).selected_tab().read(cx).state().clone());
    assert_eq!(
        state,
        TabState::Failed {
            folder: PathBuf::from(FOLDER),
            log_tail: LOG_TAIL.to_string(),
        }
    );

    Ok(())
}

/// Draw a frame and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Tab selection animates on the app clock, which the headless context only
    // advances when asked; the sleep lets the same settle happen on the wall
    // clock the text fade uses.
    cx.advance_clock(SETTLE);
    std::thread::sleep(SETTLE);
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

fn click(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    id: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })?;
    Ok(())
}
