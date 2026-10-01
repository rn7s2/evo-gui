//! Quitting while a swarm is running: the screen, and the wait (§9.8).
//!
//! The plain quit's own tests are `quit.rs`. What this one adds is a swarm with a
//! process behind it: a real `evo-swarm` from the proofs' fixture (the scripted
//! model), launched in a real window, so the whole path runs — the tab's engine,
//! the app's quit, the screen the window covers itself with, and the exit that
//! may not happen under a swarm that is still running.
//!
//! It keeps its own test binary because the fixture owns the process's
//! environment (`HOME`, `EVO_HOME`, the stub's address), which no other test in
//! this crate should be run beside.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{begin_quit, is_quitting, swarms, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, Entity, Point,
    TestAppContext, WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContentEvent, TabState, WorkspaceView};

/// How long a test waits for a swarm, or for a quit, to get somewhere.
const WAIT: Duration = Duration::from_secs(60);

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

/// A window on `root`, starting swarms with `binaries` — the whole app's own way
/// in (`Shell` installed, `launch_env` from it), without the run loop.
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    binaries: Binaries,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    cx.update(move |cx| {
        let state = AppState {
            binaries,
            ..AppState::default()
        };
        Shell::new(root, log, state, ModelCache::default()).install(cx);
        let config = Arc::new(evo_desktop::launch_env(cx));
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1200.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
        )
        .expect("the window");
        // What the app's own run loop does when it opens the window: the quit path
        // reaches the window through this.
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        (window, view)
    })
}

/// Pump the UI thread until `done` holds, or fail.
fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        // The quit's wait is a timer: the test's clock is the one that moves it.
        cx.executor().advance_clock(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log_text(root: &AppRoot) -> String {
    std::fs::read_to_string(AppLog::open(root).path()).unwrap_or_default()
}

/// §9.8: a quit that has a swarm to stop shows the screen and waits for it — the
/// app ends only once the engine's thread is over, and while it is running there
/// is no exit line to read.
#[gpui_kit::test]
fn quitting_with_a_swarm_shows_the_screen_and_waits_for_it(cx: &mut TestAppContext) {
    // The engine's threads and the swarm's own process wake this test's tasks.
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    // The proofs' world: a real `evo-swarm` and `evo-agent` behind a scripted
    // model, so a tab can be launched and quit without spending anything.
    let fixture = proofs::fixture::Fixture::new("quit-swarm");
    fixture.enter();
    let root = fixture.root.clone();
    let binaries = Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    };
    let (window, view) = open(cx, &root, binaries);

    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    let folder = fixture.folder.clone();
    let launching = tab.clone();
    cx.update(move |cx| {
        launching.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: LaunchPlan::default(),
            })
        })
    });
    wait_for(cx, "the swarm to come up", |cx| {
        matches!(
            cx.update(|cx| tab.read(cx).state().clone()),
            TabState::Running { .. }
        )
    });
    let screen = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find("tab-page").visible(),
                window.try_find("quit-screen").is_some(),
            )
        })
        .expect("a drawn frame");
    assert_eq!(
        screen,
        (true, false),
        "the page is what a running swarm shows, and nothing is quitting yet"
    );

    // The engine the quit has to wait for, in the test's own hands.
    let engine = cx
        .update(|cx| swarms(cx).pop())
        .expect("the tab's swarm is running");
    assert!(engine.is_running(), "and its thread is there");

    cx.update(begin_quit);
    assert!(
        cx.update(|cx| is_quitting(cx)),
        "the quit sequence has begun"
    );
    assert_eq!(
        cx.update(|cx| view.read(cx).quitting_swarms()),
        Some(1),
        "the window is showing the screen for one swarm"
    );
    let screen = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            (
                window.find("quit-screen").visible(),
                window.find("quit-screen-label").label().map(str::to_owned),
                window.find("quit-screen").focused(),
            )
        })
        .expect("a drawn frame");
    assert_eq!(
        screen,
        (true, Some("Terminating session.".to_owned()), Some(true)),
        "the screen covers the window, names one session, and holds the keyboard"
    );

    // And the app waits: while the engine's thread is there, there is no exit.
    wait_for(cx, "the quit to finish", |_cx| {
        if engine.is_running() {
            assert!(
                !log_text(&root).contains("stopped; exiting"),
                "the app ended under a swarm that was still running:\n{}",
                log_text(&root)
            );
        }
        log_text(&root).contains("stopped; exiting")
    });
    assert!(!engine.is_running(), "the swarm is gone before the exit");
    let text = log_text(&root);
    assert!(
        text.find("waiting for 1 swarm(s) to stop")
            .unwrap_or(usize::MAX)
            < text.find("stopped; exiting").unwrap_or(0),
        "the quit waited for it:\n{text}"
    );
    assert_eq!(
        text.matches("stopped; exiting").count(),
        1,
        "and ended once:\n{text}"
    );
    println!("[quit] waited for the swarm: {}", text.trim_end());

    let _ = std::fs::remove_dir_all(root.path());
}

/// §9.8: with nothing running there is nothing to wait for — the quit ends at
/// once and never shows the screen.
#[gpui_kit::test]
fn quitting_with_no_swarm_does_not_show_the_screen(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let root = temp_root("quit-clean");
    let (window, view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from("/nonexistent/evo-swarm"),
            evo_agent: PathBuf::from("/nonexistent/evo-agent"),
        },
    );

    cx.update(begin_quit);
    assert!(
        cx.update(|cx| view.read(cx).quitting_swarms()).is_none(),
        "an empty window has nothing to cover itself for"
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("quit-screen").is_none());
    })
    .expect("a drawn frame");

    wait_for(cx, "the quit to finish", |_cx| {
        log_text(&root).contains("stopped; exiting")
    });
    let text = log_text(&root);
    assert!(
        !text.contains("waiting for"),
        "nothing was waited for:\n{text}"
    );
    let _ = std::fs::remove_dir_all(root.path());
}
