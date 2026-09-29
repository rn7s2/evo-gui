//! First run, captured headlessly: an empty `~/.evo/desktop`, no model cache.
//!
//! ```sh
//! cargo run -p evo-desktop --example first_run_snapshot -- --capture <dir>
//! ```
//!
//! Two pictures, and nothing scripted about either: the window at the app's own
//! size with the app's own `Shell`, and the catalog probe really running the
//! installed `evo-agent` against the harness's temp `EVO_HOME` (whose `init.lisp`
//! registers the scripted stub provider). The first frame is taken while the probe
//! is still in flight — the empty tab on its loading hint — and the second once
//! the catalog has landed and the choosers have options.
//!

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{start_background_loads, start_version_probe, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, HeadlessAppContext, Point, WindowBounds,
    WindowOptions,
};
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root;
use swarm_client::harness::{Fixture, HarnessConfig};
use workspace::WorkspaceView;

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// How long the catalog probe is given before the second picture is taken anyway.
const CATALOG_WAIT: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => {
                i += 1;
                dir = args.get(i).cloned();
            }
            other => {
                eprintln!("unexpected argument {other:?}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(dir) = dir else {
        eprintln!("usage: first_run_snapshot --capture <dir>");
        std::process::exit(2);
    };
    if let Err(error) = capture(Path::new(&dir)) {
        eprintln!("capture failed: {error}");
        std::process::exit(1);
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let fixture = Fixture::new(HarnessConfig::default())?;
    // The probe runs in this process and inherits this environment: the stub home
    // is what makes the real `evo-agent` answer with the stub provider's registry.
    std::env::set_var("EVO_HOME", &fixture.home);
    // The first run: the desktop directory does not exist yet. `<EVO_HOME>/desktop`
    // is `~/.evo/desktop` with `EVO_HOME` at the fixture's temp home.
    let root = Root::at(fixture.home.join("desktop"));
    assert!(!root.path().exists(), "a first run has no state directory");

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The probe's threads wake this context's tasks: real I/O is expected.
    cx.allow_parking();

    let bins = fixture.bins.clone();
    let (window, view) = cx.update({
        let root = root.clone();
        move |cx| {
            let log = AppLog::open(&root);
            let state = AppState {
                binaries: Binaries {
                    evo_swarm: bins.swarm.clone(),
                    evo_agent: bins.agent.clone(),
                },
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
    // What `run` does with the window it opened: the app keeps the view, which is
    // where the empty tabs are fed.
    cx.update(|cx| cx.global_mut::<Shell>().view = Some(view.downgrade()));

    // The two startup loads (§7.1): the binaries' versions, then the cache, the
    // session scan and the catalog probe.
    cx.update(start_version_probe);
    let started = Instant::now();
    cx.update(start_background_loads);

    // 1. The frame the user gets while the probe is still running.
    shot(&mut cx, window, dir, "first-run-01-loading.png", false)?;
    println!(
        "[capture] the window was up and loading after {:?}",
        started.elapsed()
    );

    // 2. The same window once the catalog has landed.
    let deadline = Instant::now() + CATALOG_WAIT;
    loop {
        let known = cx.update(|cx| !cx.global::<Shell>().launcher.cache.is_empty());
        if known || Instant::now() >= deadline {
            break;
        }
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
    }
    let models = cx.update(|cx| {
        cx.global::<Shell>()
            .launcher
            .cache
            .models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<String>>()
    });
    println!(
        "[capture] the probed catalog: {models:?} ({:?})",
        started.elapsed()
    );
    shot(&mut cx, window, dir, "first-run-02-loaded.png", true)?;

    let _ = std::fs::remove_dir_all(root.path());
    Ok(())
}

/// Draw a frame and save its pixels.
///
/// `settle` waits out the animations — a capture of the loaded tab is taken after
/// the update it shows, so its frame has to be the one that paints it.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
    settle: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if settle {
        std::thread::sleep(Duration::from_millis(300));
    }
    // Two frames: the first lays out what the last update changed, the second
    // paints it.
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let image = cx.capture_screenshot(window)?;
    let path: PathBuf = dir.join(name);
    image.save(&path)?;
    println!(
        "[capture] {}x{} -> {}",
        image.width(),
        image.height(),
        path.display()
    );
    Ok(())
}
