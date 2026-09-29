//! The empty tab (§7.2), rendered and captured without a screen.
//!
//! ```sh
//! cargo run -p workspace --example empty_tab_demo -- --capture <dir>
//! ```
//!
//! Ten pictures: the catalog loading, the catalog loaded with a lanes model chosen (the
//! `swarm.lisp` note under the chooser), a history list of many rows — one of them a very
//! long path — an empty history, and the two states with no rows yet (scanning, and a scan
//! that failed) — the first four each in the light and the dark theme.
//!
//! Capture mode drives GPUI's headless renderer, so the pictures do not depend on a window
//! being on screen (the machine may be locked).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, ElementId, HeadlessAppContext,
    InputEvent as _, WindowBounds, WindowOptions,
};
use store::{ModelCache, Root};
use workspace::{SwarmConfig, WorkspaceView};

/// The app's own default window (§7.1), so the pictures are the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// How long to let a hover or a popup settle before the frame is saved.
const SETTLE: Duration = Duration::from_millis(400);

/// The clock the pictures are taken at: a fixed instant, so two runs look the same.
const NOW: i64 = 1_774_000_000;

/// A catalog as a `--no-userspace` probe answers it: three models under two wire APIs — one
/// of them registered twice, under two providers — and the kernel's own api set, which is
/// what the lanes chooser measures against (§9.4).
const REGISTRY: &str = r#"{
  "version": 1,
  "fetched_at": "2026-09-29T09:25:44Z",
  "kernel_apis": ["anthropic-messages"],
  "registry": {
    "models": [
      {
        "id": "ark-deepseek-v4.1-flash",
        "provider": "aiden",
        "api": "openai-chat",
        "context_window": 200000,
        "vision": false,
        "effort": ["low", "high"]
      },
      {
        "id": "claude-opus-4.5",
        "provider": "anthropic",
        "api": "anthropic-messages",
        "context_window": 1000000,
        "vision": true,
        "effort": ["low", "max"]
      },
      {
        "id": "ark-deepseek-v4.1-flash",
        "provider": "volcengine",
        "api": "openai-chat",
        "context_window": 200000,
        "vision": false,
        "effort": ["low", "high"]
      }
    ],
    "apis": ["anthropic-messages"],
    "settings": { "swarm_workers": 6 }
  }
}"#;

/// The sessions the history list shows: five swarms, one of them in a folder whose path is
/// far too deep for the row to hold (§9.5).
fn history() -> Vec<session::HistoryEntry> {
    let entries: [(&str, &str, i64, u32, &str); 5] = [
        (
            "/Users/you/.evo/sessions/3f1a/2026-09-29T09-25-44.sexp",
            "/Users/you/coding/evo-gui",
            12,
            6,
            "claude-opus-4.5",
        ),
        (
            "/Users/you/.evo/sessions/9c02/2026-09-29T07-02-10.sexp",
            "/Users/you/coding/evo-agent",
            130,
            4,
            "ark-deepseek-v4.1-flash",
        ),
        (
            "/Users/you/.evo/sessions/77bd/2026-09-28T22-41-00.sexp",
            "/Users/you/coding/experiments/a-very-long-project-directory-name/nested/deeper/\
             evo-desktop-render-explorations",
            900,
            2,
            "claude-sonnet-4.5",
        ),
        (
            "/Users/you/.evo/sessions/5a1e/2026-09-27T11-15-32.sexp",
            "/Users/you/coding/dotfiles",
            2_600,
            1,
            "",
        ),
        (
            "/Users/you/.evo/sessions/b088/2026-09-19T16-44-08.sexp",
            "/Users/you/coding/notes",
            86_000,
            8,
            "ark-deepseek-v4.1-flash",
        ),
    ];
    entries
        .into_iter()
        .map(
            |(session, folder, minutes_ago, lanes, model)| session::HistoryEntry {
                session_path: session.to_string(),
                folder: folder.to_string(),
                when: session::When::Epoch(NOW - minutes_ago * 60),
                lanes: Some(lanes),
                coordinator_model: (!model.is_empty()).then(|| model.to_string()),
                lanes_model: None,
                source: session::HistorySource::Scan,
            },
        )
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, dir] if flag == "--capture" => {
            if let Err(error) = capture(Path::new(dir)) {
                eprintln!("capture failed: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: empty_tab_demo --capture <dir>");
            std::process::exit(2);
        }
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    // The app's own folder, under a temporary home: a capture must not read the user's
    // `~/.evo/desktop`, and must not leave anything behind.
    let home = std::env::temp_dir().join(format!("evo-desktop-capture-{}", std::process::id()));
    std::fs::create_dir_all(home.join(".evo/desktop"))?;
    std::env::set_var("HOME", &home);

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(window_options(), cx, |window, cx| {
            cx.new(|cx| WorkspaceView::with_config(Arc::new(SwarmConfig::default()), window, cx))
        })
    })?;
    let window: AnyWindowHandle = window;
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // 1. The catalog is not known yet: Default in every chooser, and the hint that says why.
    shot(&mut cx, window, dir, "01-loading-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "02-loading-dark.png")?;
    light(&mut cx);

    // 2. The cached catalog: the models are in the choosers, and the lanes chooser knows
    // which ones a lane could register.
    let cache = temp_catalog(&home);
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.set_model_cache(&cache, window, cx))
    })?;
    // Choose a lanes model the way a person does: open the chooser, walk to an option and
    // commit it — the same path a click takes.
    cx.update_window(window, |_, window, cx| window.click("lanes-model", cx))?;
    for key in ["down", "down", "enter"] {
        cx.update_window(window, |_, window, cx| window.press(key, cx))?;
    }
    shot(&mut cx, window, dir, "03-lanes-note-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "04-lanes-note-dark.png")?;
    light(&mut cx);

    // 3. The history: five swarms, one of them in a folder far too deep for a row.
    tab.update(&mut cx, |tab, cx| {
        tab.set_history_entries(&history(), NOW, 0, Some("/Users/you"), cx)
    });
    tab.update(&mut cx, |tab, cx| tab.set_scanning(false, cx));
    shot(&mut cx, window, dir, "05-history-light.png")?;

    // 3b. The two hover fills and the keyboard focus ring: states a still picture cannot
    // show on its own. The card is reached by walking the tab stops, which is what Tab does.
    for _ in 0..4 {
        cx.update_window(window, |_, window, cx| window.focus_next(cx))?;
    }
    shot(&mut cx, window, dir, "12-folder-card-focus-light.png")?;
    pointer(&mut cx, window, ElementId::Name(FOLDER_ID.into()))?;
    shot(&mut cx, window, dir, "13-folder-card-hover-light.png")?;
    pointer(&mut cx, window, history_row(1))?;
    shot(&mut cx, window, dir, "14-history-row-hover-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "06-history-dark.png")?;
    light(&mut cx);

    // 4. Nothing to resume, and nothing being scanned.
    tab.update(&mut cx, |tab, cx| {
        tab.set_history_entries(&[], NOW, 0, None, cx)
    });
    shot(&mut cx, window, dir, "07-empty-history-light.png")?;
    dark(&mut cx);
    shot(&mut cx, window, dir, "08-empty-history-dark.png")?;
    light(&mut cx);

    // 5. The note names a real file once the app knows the folder it expects.
    tab.update(&mut cx, |tab, cx| {
        tab.set_home(Some("/Users/you".to_string()), cx);
        tab.set_folder_hint(Some(PathBuf::from("/Users/you/coding/evo-gui")), cx);
    });
    shot(&mut cx, window, dir, "11-lanes-note-folder-hint-light.png")?;

    // 6. While the scan runs, and when it fails: the two states with no rows yet.
    tab.update(&mut cx, |tab, cx| tab.set_scanning(true, cx));
    shot(&mut cx, window, dir, "09-scanning-light.png")?;
    tab.update(&mut cx, |tab, cx| {
        tab.set_scanning(false, cx);
        tab.set_history_error(Some("~/.evo/sessions is not readable".to_string()), cx);
    });
    shot(&mut cx, window, dir, "10-scan-error-light.png")?;

    let _ = std::fs::remove_dir_all(&home);
    Ok(())
}

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
        ))),
        ..Default::default()
    }
}

fn light(cx: &mut HeadlessAppContext) {
    cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
}

fn dark(cx: &mut HeadlessAppContext) {
    cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
}

/// A model cache on disk, as §9.4's probe leaves it, loaded the way the app loads it.
fn temp_catalog(home: &Path) -> ModelCache {
    let root = Root::at(home.join(".evo/desktop"));
    std::fs::write(root.model_cache(), REGISTRY).expect("write the catalog");
    ModelCache::load(&root)
}

/// The element ids the captures point the pointer at: the folder card is a Button, so it
/// is named; a history row is one of the list's numbered items.
const FOLDER_ID: &str = "select-folder";
fn history_row(index: usize) -> ElementId {
    ElementId::NamedInteger("history-row".into(), index as u64)
}

/// Move the pointer over an element's centre, so its hover style is what the next frame
/// paints.
fn pointer(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    id: ElementId,
) -> Result<(), Box<dyn std::error::Error>> {
    let position = cx
        .update_window(window, |_, window, _| window.find(id).bounds())?
        .center();
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    Ok(())
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
