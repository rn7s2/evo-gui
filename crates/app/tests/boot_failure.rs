//! M4 (hardening) — a swarm that never comes up shows why, and offers a Retry
//! (§9.7).
//!
//! What this one adds over a workspace test is the whole app in the path: the
//! binary paths in `app.json` are what a window's tabs start swarms with, so a
//! user who points `evo_swarm` at something that is not there gets the reason on
//! the failure screen — not an empty window and not an empty log box.
//!
//! The engine runs on threads of its own and hands its updates to a gpui task, so
//! these tests pump the UI thread (`wait_for`) and opt into parking, which is
//! what GPUI asks for when real threads wake its tasks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell, swarm_config};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};
use session::LaunchPlan;
use store::app_state::Binaries;
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
fn open(cx: &mut TestAppContext, root: &AppRoot, binaries: Binaries) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            Shell::new(root, log, binaries, Vec::new(), ModelCache::default()).install(cx);
            let config = Arc::new(swarm_config(cx));
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
    (window.into(), view)
}

#[gpui_kit::test]
fn a_swarm_binary_that_is_not_there_shows_the_reason_and_retries(cx: &mut TestAppContext) {
    // The engine's threads wake this test's tasks: that is the point.
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let root = temp_root("boot-failure");
    let folder = root.path().join("project");
    std::fs::create_dir_all(&folder).expect("a folder to start in");

    let (window, view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from("/nonexistent/evo-swarm"),
            evo_agent: PathBuf::from("/nonexistent/evo-agent"),
        },
    );
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // A folder was picked: this is the event the empty tab sends, and the window
    // is what turns it into a launch (§7.2).
    cx.update(|cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch { folder: folder.clone(), plan: LaunchPlan::default() })
        });
    });

    wait_for(cx, "the failure screen", |cx| matches!(state(cx, &tab), TabState::Failed { .. }));
    let TabState::Failed { folder: failed_in, log_tail } = state(cx, &tab) else {
        unreachable!("just matched")
    };
    assert_eq!(failed_in, folder, "the screen names the folder it could not start in");
    assert!(
        log_tail.contains("evo-swarm"),
        "the reason names the binary that could not run: {log_tail:?}"
    );

    // The screen the user is looking at: the log box and its Retry.
    let (drawn_tail, retry_visible) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let tail = window.find("boot-log-tail");
            let retry = window.find("tab-retry");
            (
                tail.label().map(str::to_owned).or_else(|| tail.value().map(str::to_owned)),
                retry.visible(),
            )
        })
        .expect("a drawn frame");
    assert!(retry_visible, "the failure screen offers a Retry");

    // Clicking it asks the window to launch again — the failure screen's own path
    // (§9.7), not a call this test makes behind the UI's back.
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded = events.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&tab, move |_, event: &TabContentEvent, _| recorded.borrow_mut().push(event.clone()))
    });
    cx.update_window(window, |_, window, cx| window.click("tab-retry", cx))
        .expect("the retry click");
    assert!(
        events.borrow().iter().any(|event| matches!(event, TabContentEvent::RetryRequested(_))),
        "the click went through the screen's Retry: {:?}",
        events.borrow()
    );
    // The retry has already started the launch again by the time the click's
    // update is over: the tab is booting, not sitting on a dead screen.
    assert!(
        matches!(state(cx, &tab), TabState::Booting { .. }),
        "the retry started a boot: {:?}",
        state(cx, &tab)
    );

    // And it fails again, the same way, with the same reason to show.
    wait_for(cx, "the second failure screen", |cx| matches!(state(cx, &tab), TabState::Failed { .. }));
    let TabState::Failed { log_tail, .. } = state(cx, &tab) else { unreachable!("just matched") };
    assert!(log_tail.contains("evo-swarm"), "still the reason, not an empty box: {log_tail:?}");

    if let Some(tail) = drawn_tail {
        // The box's own text, when the element reports it.
        assert!(!tail.trim().is_empty(), "the log box is not empty: {tail:?}");
    }
    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn a_folder_that_cannot_be_written_fails_the_tab_before_anything_starts(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let root = temp_root("boot-failure-folder");
    let (window, view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from("/nonexistent/evo-swarm"),
            evo_agent: PathBuf::from("/nonexistent/evo-agent"),
        },
    );
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // A folder that does not exist: the prep cannot write the tab's own directory
    // or the folder's `swarm.lisp` block (§9.6), so there is nothing to boot.
    let nowhere = root.path().join("no").join("such").join("folder");
    cx.update(|cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch { folder: nowhere.clone(), plan: LaunchPlan::default() })
        });
    });
    cx.run_until_parked();

    let TabState::Failed { folder, log_tail } = state(cx, &tab) else {
        panic!("a folder that cannot be written is a failure, not a boot: {:?}", state(cx, &tab));
    };
    assert_eq!(folder, nowhere);
    assert!(!log_tail.trim().is_empty(), "the screen says what could not be prepared");
    let _ = window;
    let _ = std::fs::remove_dir_all(root.path());
}
