//! A swarm binary that cannot be run, said when the app starts (§9.7).
//!
//! `app.json` naming an `evo-swarm` that is not there used to cost a launch to
//! find out. The version probe the About dialog already runs answers the same
//! question at startup, so the empty tab says it before the user picks a folder —
//! and Settings re-reads the binaries, so pointing the path at a real one clears
//! the line.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use evo_desktop::{apply_settings, start_version_probe, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use settings::SettingsValues;
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use swarm_client::harness::Bins;
use workspace::WorkspaceView;

/// The path `app.json` names in the broken case.
const MISSING: &str = "/nonexistent/evo-swarm";

/// How long the probe — two `--version` calls — is given.
const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

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
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A window on a temp app root with the binaries written into it, as `run` builds
/// one — the app's own `app.json`, in effect.
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
            let config = std::sync::Arc::new(evo_desktop::swarm_config(cx));
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
    // What `run` does with the window it opened: the app keeps the view, which is
    // where the empty tabs are fed (§9.4, §9.5).
    cx.update(|cx| {
        cx.global_mut::<Shell>().view = Some(view.downgrade());
    });
    (window, view)
}

/// The line the empty tabs are shown, or `None` when the swarm binary is fine.
fn swarm_problem(cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| cx.global::<Shell>().launcher.swarm_problem.clone())
}

#[gpui_kit::test]
fn a_swarm_binary_that_cannot_be_run_is_said_at_startup_and_fixed_in_settings(
    cx: &mut TestAppContext,
) {
    // The probe's thread wakes this test's tasks: that is the point.
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let root = temp_root("missing-swarm");
    let bins = Bins::installed();
    let (window, _view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from(MISSING),
            evo_agent: bins.agent.clone(),
        },
    );

    // Nothing is claimed before the probe answers: the line appears with the
    // answer, not with the launch.
    assert_eq!(swarm_problem(cx), None);

    // What `run` does once the window exists (§7.1).
    let started = Instant::now();
    cx.update(start_version_probe);
    wait_for(cx, "evo-swarm to be reported missing", |cx| {
        swarm_problem(cx).is_some()
    });
    let line = swarm_problem(cx).expect("the line the tab shows");
    println!(
        "[proof] the missing swarm binary: {line:?} ({:?})",
        started.elapsed()
    );
    assert_eq!(
        line,
        format!("evo-swarm not found at {MISSING} — fix it in Settings…")
    );

    // The empty tab is drawn with it — the frame the user sees as the app comes up.
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("a drawn frame");

    // The path was fixed in Settings and saved: the app re-reads the binaries, so
    // the line goes — and it is what `app.json` now holds.
    if !bins.available() {
        println!("[proof] no installed evo-swarm to fix the path with: line kept");
        return;
    }
    let fixed = SettingsValues {
        evo_swarm: bins.swarm.clone(),
        evo_agent: bins.agent.clone(),
        theme: Theme::System,
    };
    cx.update(|cx| apply_settings(&fixed, cx));
    wait_for(cx, "the line to go", |cx| {
        cx.update(|cx| {
            let shell = cx.global::<Shell>();
            shell.launcher.swarm_problem.is_none() && shell.versions.swarm.is_some()
        })
    });
    assert_eq!(
        swarm_problem(cx),
        None,
        "a swarm binary that answers is no line"
    );

    let state = AppState::load(&root);
    assert_eq!(state.binaries.evo_swarm, bins.swarm);
    assert_eq!(state.binaries.evo_agent, bins.agent);

    let _ = std::fs::remove_dir_all(root.path());
}
