//! The history list's two kinds, as pictures: one row the swarm ran and one the single
//! agent ran, side by side, in both themes.
//!
//! ```sh
//! cargo run -p evo-desktop --example history_kinds -- --capture /tmp/history-kinds
//! ```
//!
//! The path is `screens.rs`'s: the same headless `Shell`, the same window the app opens
//! (`show:false`, no focus, nothing visible), and the empty tab as the app draws it. What
//! it feeds the tab is the rows a session index would — one entry per kind, built the way
//! `evo-agent sessions --json` builds them — through the same call the app's own startup
//! load makes (`TabContent::set_history_entries`). No process runs, and no model is called.
//!
//! It prints each row's glyph bounds so that the icons can be cropped out of the two
//! pictures afterwards.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, ElementId, Entity,
    HeadlessAppContext, Point, WindowBounds, WindowOptions,
};
use session::{HistoryEntry, HistorySource};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::WorkspaceView;

/// The window the app opens, and the size these pictures are taken at.
const WINDOW_SIZE: (f32, f32) = (1440., 900.);

/// The moment the rows are read against: two sessions, one an hour old and one a day old.
const NOW: i64 = 1_780_000_000;
const HOME: &str = "/Users/you";

/// The two element names a row's kind glyph wears — the workspace's own, repeated here
/// because a capture has to ask the tree where the glyph landed.
const KIND_AGENT_ID: &str = "history-kind-agent";
const KIND_SWARM_ID: &str = "history-kind-swarm";

fn main() {
    let mut dir = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => {
                dir = PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--capture needs a directory");
                    std::process::exit(2);
                }))
            }
            other => {
                eprintln!("usage: history_kinds --capture <dir>  (got {other})");
                std::process::exit(2);
            }
        }
    }
    if let Err(error) = capture(&dir) {
        eprintln!("capture failed: {error}");
        std::process::exit(1);
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    // A root of our own: no real `~/.evo`, nothing to find there, nothing to write.
    let root = AppRoot::at(std::env::temp_dir().join("evo-history-kinds"));
    std::fs::create_dir_all(root.path())?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx.allow_parking();

    let (window, view) = open(&mut cx, &root)?;
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // The rows the index would hand the empty tab: the swarm's session, then one agent's
    // — two folders, so the two titles are the folders' own names.
    let entries = rows();
    let tab_for_rows = tab.clone();
    cx.update(move |cx| {
        tab_for_rows.update(cx, |tab, cx| {
            tab.set_history_entries(&entries, NOW, 0, Some(HOME), cx)
        })
    });
    pump(&mut cx, Duration::from_millis(200));

    shot(&mut cx, window, dir, "history-kind")?;
    bounds(&mut cx, window, dir)?;
    Ok(())
}

/// One row per kind: the swarm's session in one folder, one agent's in another — built the
/// way `sessions --json` builds them, down to the folder each ran in.
fn rows() -> Vec<HistoryEntry> {
    let swarm = entry("/Users/you/coding/evo", true);
    let agent = entry("/Users/you/coding/harness", false);
    vec![swarm, agent]
}

fn entry(folder: &str, swarm: bool) -> HistoryEntry {
    HistoryEntry {
        session_path: format!("{folder}/.evo/session.sexp"),
        folder: folder.to_string(),
        when: Some(NOW - 3_600),
        lanes: swarm.then_some(6),
        coordinator_model: Some("claude-opus-4.5@anthropic".to_string()),
        lanes_model: swarm.then(|| "deepseek-v4.1-flash@ark".to_string()),
        swarm,
        source: HistorySource::Index,
        open_at_quit: false,
    }
}

/// A window on this root, with the shell the app installs — `screens.rs`'s own `open`, minus
/// the fixture: nothing here launches a swarm, so there is no binary to name.
fn open(
    cx: &mut HeadlessAppContext,
    root: &AppRoot,
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Box<dyn std::error::Error>> {
    let root = root.clone();
    let (window, view) = cx.update(move |cx| {
        let log = AppLog::open(&root);
        let state = AppState {
            binaries: Binaries {
                evo_swarm: PathBuf::from("/nonexistent/evo-swarm"),
                evo_agent: PathBuf::from("/nonexistent/evo-agent"),
            },
            theme: Theme::System,
            ..AppState::default()
        };
        Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
        let config = Arc::new(evo_desktop::launch_env(cx));
        let (window, view) = gpui_kit::open_window(
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
            |window, cx| {
                let _appearance = evo_desktop::follow_appearance(cx, window);
                cx.new(|cx| WorkspaceView::with_config(config, window, cx))
            },
        )?;
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        Ok::<_, Box<dyn std::error::Error>>((window, view))
    })?;
    Ok((window, view))
}

/// Where each kind's glyph landed, and where its row did: what a crop needs, in the
/// window's own coordinates, once per theme.
fn bounds(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut report = String::new();
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        pump(cx, Duration::from_millis(200));
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
        cx.update_window(window, |_, window, _| {
            for (row, (name, kind)) in [(KIND_SWARM_ID, "swarm"), (KIND_AGENT_ID, "agent")]
                .into_iter()
                .enumerate()
            {
                let glyph = window
                    .find(ElementId::NamedInteger(name.into(), row as u64))
                    .bounds();
                let row_bounds = window
                    .find(ElementId::NamedInteger("history-row".into(), row as u64))
                    .bounds();
                report.push_str(&format!(
                    "{suffix} {kind}: glyph {},{},{}x{}  row {},{},{}x{}\n",
                    glyph.origin.x,
                    glyph.origin.y,
                    glyph.size.width,
                    glyph.size.height,
                    row_bounds.origin.x,
                    row_bounds.origin.y,
                    row_bounds.size.width,
                    row_bounds.size.height,
                ));
            }
        })?;
    }
    let path = dir.join("history-kind-bounds.txt");
    std::fs::write(&path, report.clone())?;
    print!("[capture] {report}");
    println!("[capture] bounds -> {}", path.display());
    Ok(())
}

/// Let the frames and the tasks run for a while.
fn pump(cx: &mut HeadlessAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One state, in both themes: the light picture and the dark one (`screens.rs`'s own shot).
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        pump(cx, Duration::from_millis(400));
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
        let image = cx.capture_screenshot(window)?;
        let path = dir.join(format!("{name}-{suffix}.png"));
        image.save(&path)?;
        println!(
            "[capture] {}x{} -> {}",
            image.width(),
            image.height(),
            path.display()
        );
    }
    Ok(())
}
