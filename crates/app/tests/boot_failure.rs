//! A swarm that never comes up says why, and offers a Retry (§9.7).
//!
//! What this one adds over a tab's own tests is the whole app in the path: the
//! binary paths in `app.json` are what a window's tabs start swarms with, so a
//! user who points `evo_swarm` at something that is not there gets the engine's
//! own reason (`TabState::Failed`'s `message`) on the failure screen — not an
//! empty window, and not an empty log box. The log tail is the evidence behind
//! the reason and may be empty; the reason may not.
//!
//! The engine runs on threads of its own and hands its updates to a gpui task, so
//! these tests pump the UI thread (`wait_for`) and opt into parking, which is what
//! GPUI asks for when real threads wake its tasks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{launch_env, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContent, TabContentEvent, TabState, WorkspaceView};

/// How long a test waits for a boot that cannot succeed to say so.
const WAIT: Duration = Duration::from_secs(60);

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

fn state(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
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
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A window on a temp app root, with the binaries written into it: the test's own
/// `app.json`, in effect.
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    binaries: Binaries,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            let state = AppState {
                binaries,
                ..AppState::default()
            };
            Shell::new(root, log, state, ModelCache::default()).install(cx);
            let config = Arc::new(launch_env(cx));
            gpui_kit::open_window(
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
        })
        .expect("the window");
    (window, view)
}

/// Start this tab in `folder`, the way the empty tab does.
fn launch(cx: &mut TestAppContext, tab: &Entity<TabContent>, folder: &std::path::Path) {
    let folder = folder.to_path_buf();
    let tab = tab.clone();
    cx.update(move |cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: LaunchPlan::default(),
            })
        })
    });
}

/// The failure the tab is showing: its folder, and the reason it has to show.
fn failure(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> (PathBuf, String) {
    let TabState::Failed {
        folder, message, ..
    } = state(cx, tab)
    else {
        panic!("the tab is not showing a failure: {:?}", state(cx, tab));
    };
    let message = message.expect("a failure has a reason to show");
    assert!(
        !message.trim().is_empty(),
        "and it is not blank: {message:?}"
    );
    (folder, message)
}

fn binaries(evo_swarm: &str, evo_agent: &str) -> Binaries {
    Binaries {
        evo_swarm: PathBuf::from(evo_swarm),
        evo_agent: PathBuf::from(evo_agent),
    }
}

#[gpui_kit::test]
fn a_binary_that_cannot_run_shows_the_reason_and_retries(cx: &mut TestAppContext) {
    // The engine's threads wake this test's tasks: that is the point.
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let root = temp_root("boot-failure");
    let folder = root.path().join("project");
    std::fs::create_dir_all(&folder).expect("a folder to start in");

    let (window, view) = open(
        cx,
        &root,
        binaries("/nonexistent/evo-swarm", "/nonexistent/evo-agent"),
    );
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    launch(cx, &tab, &folder);

    wait_for(cx, "the failure screen", |cx| {
        matches!(state(cx, &tab), TabState::Failed { .. })
    });
    let (failed_in, message) = failure(cx, &tab);
    assert_eq!(
        failed_in, folder,
        "the screen names the folder it could not start in"
    );
    assert!(
        message.contains("evo-swarm"),
        "the reason names the binary that could not run: {message:?}"
    );

    // The screen the user is looking at: the reason itself, and its Retry.
    let (drawn_reason, retry_visible) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let reason = window.find("boot-failure-reason");
            (
                reason.label().or_else(|| reason.value()).map(str::to_owned),
                window.find("tab-retry").visible(),
            )
        })
        .expect("a drawn frame");
    assert!(retry_visible, "the failure screen offers a Retry");
    assert!(
        drawn_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("evo-swarm")),
        "the screen itself names what could not run: {drawn_reason:?}"
    );

    // Clicking it asks the window to launch again — the failure screen's own path
    // (§9.7), not a call this test makes behind the UI's back.
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&tab, move |_, event: &TabContentEvent, _| {
            recorded.borrow_mut().push(event.clone())
        })
    });
    cx.update_window(window, |_, window, cx| window.click("tab-retry", cx))
        .expect("the retry click");
    assert!(
        events
            .borrow()
            .iter()
            .any(|event| matches!(event, TabContentEvent::RetryRequested(_))),
        "the click went through the screen's Retry: {:?}",
        events.borrow()
    );

    // And it fails again, the same way, with the same reason to show.
    wait_for(cx, "the second failure screen", |cx| {
        matches!(state(cx, &tab), TabState::Failed { .. })
    });
    let (_, again) = failure(cx, &tab);
    assert_eq!(
        again, message,
        "the same binary is still missing, so the same reason comes back"
    );

    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn a_folder_the_swarm_cannot_start_in_fails_the_tab(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let root = temp_root("boot-failure-folder");
    let (window, view) = open(
        cx,
        &root,
        // The binaries this machine has: the folder is the only thing wrong here.
        binaries(
            &store::cli::swarm_bin().display().to_string(),
            &store::cli::agent_bin().display().to_string(),
        ),
    );
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // A *file* where the project folder should be: the child cannot start in it,
    // so there is nothing to boot and the screen has to say so.
    let not_a_folder = root.path().join("not-a-folder");
    std::fs::write(&not_a_folder, "this is not a directory\n").expect("a file to trip over");
    launch(cx, &tab, &not_a_folder);

    wait_for(cx, "the failure screen", |cx| {
        matches!(state(cx, &tab), TabState::Failed { .. })
    });
    let (failed_in, message) = failure(cx, &tab);
    assert_eq!(failed_in, not_a_folder);
    assert!(
        message.contains("not-a-folder") || message.contains("directory"),
        "the reason points at what the child could not do: {message:?}"
    );
    let _ = window;
    let _ = std::fs::remove_dir_all(root.path());
}
