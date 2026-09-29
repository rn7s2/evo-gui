//! §7.1: light, dark, and following the system.
//!
//! What a test can settle here is the *choice*: which mode a window gets, and what
//! the log line says about it — that line is how a launch's light/dark is checked
//! from outside the UI. That the app then *stays* with the system is a window
//! observer macOS drives; the test platform has no lever for an appearance change
//! (`TestAppContext::test_window` is gpui-internal), so the live switch is
//! verified against a real launch instead — see `docs/proofs.md`.

use evo_desktop::{AppLog, Shell, follow_appearance};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Point, TestAppContext, WindowAppearance, WindowBounds,
    WindowOptions, px, size,
};
use store::app_state::{AppState, Theme};
use store::model_cache::ModelCache;
use store::paths::Root;

fn temp_root(name: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Root::at(dir)
}

/// A window on a temp root, with `app.json`'s theme choice installed.
fn open(cx: &mut TestAppContext, theme: Theme) -> (AnyWindowHandle, AppLog) {
    cx.update(gpui_kit::init);
    let root = temp_root("appearance");
    let log = AppLog::open(&root);
    let (window, _view) = cx
        .update({
            let log = log.clone();
            move |cx| {
                let state = AppState { theme, ..AppState::default() };
                Shell::new(root, log, state, ModelCache::default()).install(cx);
                let config = std::sync::Arc::new(evo_desktop::swarm_config(cx));
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size: size(px(1000.), px(700.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| workspace::WorkspaceView::with_config(config, window, cx))
                    },
                )
            }
        })
        .expect("the window");
    (window, log)
}

fn log_text(log: &AppLog) -> String {
    std::fs::read_to_string(log.path()).unwrap_or_default()
}

fn mode(cx: &mut TestAppContext) -> ThemeMode {
    cx.update(|cx| cx.theme().mode)
}

fn appearance(cx: &mut TestAppContext, window: AnyWindowHandle) -> WindowAppearance {
    cx.update_window(window, |_, window, _cx| window.appearance()).expect("the window")
}

/// Apply the startup half: what `run` does when the window is up.
fn start(cx: &mut TestAppContext, window: AnyWindowHandle) {
    let subscription = cx
        .update_window(window, |_, window, cx| follow_appearance(cx, window))
        .expect("the window");
    // The app keeps this for the window's life; the test only needs it to stay
    // registered.
    std::mem::forget(subscription);
}

#[gpui_kit::test]
fn the_system_choice_is_what_the_window_reports(cx: &mut TestAppContext) {
    let (window, log) = open(cx, Theme::System);
    start(cx, window);

    let reported = appearance(cx, window);
    let expected = match reported {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
        _ => ThemeMode::Light,
    };
    assert_eq!(mode(cx), expected, "the system's answer on a {reported:?} window");
    assert!(
        log_text(&log).contains(&format!(
            "theme: {} (the system appearance)",
            if expected == ThemeMode::Dark { "dark" } else { "light" }
        )),
        "the log says which mode, and why: {}",
        log_text(&log)
    );
}

#[gpui_kit::test]
fn a_choice_in_app_json_wins_over_the_window(cx: &mut TestAppContext) {
    // The test platform's window is light, so `Light` alone would prove nothing;
    // `Dark` on that same window is the choice, not an inference.
    let (window, log) = open(cx, Theme::Dark);
    start(cx, window);

    assert_eq!(appearance(cx, window), WindowAppearance::Light, "the platform's window");
    assert_eq!(mode(cx), ThemeMode::Dark);
    assert!(log_text(&log).contains("theme: dark (app.json)"), "{}", log_text(&log));

    let (window, log) = open(cx, Theme::Light);
    start(cx, window);
    assert_eq!(mode(cx), ThemeMode::Light);
    assert!(log_text(&log).contains("theme: light (app.json)"), "{}", log_text(&log));
}
