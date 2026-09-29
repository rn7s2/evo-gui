//! What the app-level milestone proofs share: the app as `run` builds it, the
//! gestures a user makes, and the waits for the things that only happen on threads
//! of their own (a swarm booting, a ladder finishing).
//!
//! Both `m2_relaunch` and `m3_lane_ui` drive the real `Shell`, window, tabs,
//! composer and quit sequence against real `evo-swarm serve` processes started
//! from the installed binaries into a temp `EVO_HOME`
//! (`swarm_client::harness::Fixture`). The milestone's own assertions live in
//! those files; only the plumbing is here.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{begin_quit, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use session::{LaneRow, LaunchPlan, RowKind};
use store::app_state::AppState;
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use swarm_client::harness::Fixture;
use workspace::{SwarmConfig, TabContent, TabContentEvent, TabState, WorkspaceView};

/// How long one step of a proof is given: a swarm boot, a prompt's round trip, or
/// the quit sequence.
pub const WAIT: Duration = Duration::from_secs(180);

/// What every timing line is prefixed with, so `--nocapture` reads as one run.
pub const NOTE: &str = "[proof]";

// --- the app, as `run` builds it -------------------------------------------

/// A window on an app root, with the fixture's binaries and environment in place
/// of the installed ones: the swarms a tab starts are real, hermetic, and pointed
/// at the stub provider.
pub fn open(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    root: &AppRoot,
    state: AppState,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let config = Arc::new(SwarmConfig {
        swarm_bin: fixture.bins.swarm.clone(),
        agent_bin: fixture.bins.agent.clone(),
        root: root.clone(),
        env: fixture.env(),
        env_remove: fixture.env_remove(),
    });
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            Shell::new(root, log, state, ModelCache::default()).install(cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1400.), px(900.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
            )
        })
        .expect("the window");
    // What `run` does with the window it just opened: the app keeps the view, and
    // the quit path asks it for the tabs (§9.8).
    cx.update(|cx| {
        cx.global_mut::<Shell>().view = Some(view.downgrade());
    });
    (window, view)
}

/// The temp evo home everything this test starts writes under: the app's own
/// state, the swarms' tab directories, and the swarm's and lanes' argv.
pub fn set_evo_home(fixture: &Fixture) {
    // The app's session scan reads `$EVO_HOME` (`store::history::sessions_dir`),
    // and the swarms write their journals under the same temp home: one env var,
    // and nothing of the user's is read or written. The test process is this
    // file's alone.
    std::env::set_var("EVO_HOME", &fixture.home);
}

pub fn tab_state(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
}

/// The lanes as the app's own model has them, which is what the left column draws
/// (`GET /lanes` plus every `lane-state` event).
pub fn lane_rows(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> Vec<LaneRow> {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.lane_rows().to_vec())
            .unwrap_or_default()
    })
}

/// The empty tab's own launch event; the window is what turns it into a swarm
/// (§7.2).
pub fn launch(cx: &mut TestAppContext, tab: &Entity<TabContent>, folder: &Path, workers: u16) {
    let plan = LaunchPlan {
        model: None,
        lanes_model: None,
        workers: Some(workers),
    };
    let folder = folder.to_path_buf();
    let tab = tab.clone();
    cx.update(move |cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch { folder, plan })
        });
    });
}

/// The user's turn, through the composer: focus it, type, press Enter.
pub fn send(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    tab: &Entity<TabContent>,
    text: &str,
) {
    let target = tab.clone();
    cx.update_window(window, move |_, window, cx| {
        target.update(cx, |tab, cx| {
            let composer = tab.composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        });
    })
    .expect("the composer");
    cx.update_window(window, |_, window, cx| window.input(text, cx))
        .expect("typing the turn");
    cx.update_window(window, |_, window, cx| window.press("enter", cx))
        .expect("sending it");
}

/// Everything the selected agent's transcript holds, as text: what the tab page
/// draws.
pub fn transcript(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> String {
    cx.update(|cx| {
        let tab = tab.read(cx);
        let Some(view) = tab.transcript() else {
            return String::new();
        };
        view.read(cx)
            .rows(cx)
            .iter()
            .map(|row| match &row.kind {
                RowKind::User { text } => text.clone(),
                RowKind::Assistant { markdown, .. } => markdown.clone(),
                RowKind::Report {
                    done,
                    evidence,
                    next,
                    ..
                } => format!("{done} {evidence} {next}"),
                RowKind::Tool { name, .. } => name.clone(),
                RowKind::Dim { text, .. } | RowKind::RunOutcome { text, .. } => text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

// --- waiting ----------------------------------------------------------------

/// Pump the UI thread until `done` holds, or fail. The engine's threads wake this
/// test's tasks, which is why every proof opts into parking.
pub fn wait_for(
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

/// Wait until this tab's swarm is up and has said who it is: `/state` naming the
/// journal it writes, and `/health` naming the process. The tab says `Running`
/// before either — the engine is attached at launch — so this is the real gate.
pub fn wait_booted(
    cx: &mut TestAppContext,
    what: &str,
    tab: &Entity<TabContent>,
) -> (PathBuf, u32) {
    let started = Instant::now();
    wait_for(cx, what, |cx| {
        cx.update(|cx| {
            let tab = tab.read(cx);
            matches!(tab.state(), TabState::Running { .. })
                && tab.session_path().is_some()
                && tab.swarm_pid().is_some()
        })
    });
    let (session, pid) = cx.update(|cx| {
        let tab = tab.read(cx);
        (tab.session_path().map(Path::to_path_buf), tab.swarm_pid())
    });
    println!("{NOTE} {what}: {:?}", started.elapsed());
    (session.expect("a journal"), pid.expect("a pid"))
}

pub fn wait_for_text(cx: &mut TestAppContext, what: &str, tab: &Entity<TabContent>, needle: &str) {
    let started = Instant::now();
    wait_for(cx, what, |cx| transcript(cx, tab).contains(needle));
    println!("{NOTE} {what}: {:?}", started.elapsed());
}

/// Let every parked task finish.
///
/// The quit sequence's last step — the `cx.quit()` that ends the app — is logged a
/// moment *after* the `stopped; exiting` line, and an app released with a task
/// still parked on it panics the executor.
pub fn drain(cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The quit sequence (§9.8) run to its end, on this app.
pub fn quit(cx: &mut TestAppContext, log: &AppLog) {
    let started = Instant::now();
    cx.update(begin_quit);
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        cx.run_until_parked();
        if std::fs::read_to_string(log.path()).is_ok_and(|text| text.contains("stopped; exiting")) {
            println!("{NOTE} quit: {:?}", started.elapsed());
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "the quit sequence did not finish:\n{}",
        std::fs::read_to_string(log.path()).unwrap_or_default()
    );
}

// --- processes --------------------------------------------------------------

/// Nothing this test started is still running.
///
/// Every swarm and every lane it starts carries the temp evo home (or a tab
/// directory under it) in its argv, so one `pgrep` sweep is the whole check. The
/// ladder runs on a thread of its own — and a process is a zombie for a moment
/// after it exits — so the sweep is waited for rather than sampled once.
pub fn wait_nothing_left(cx: &mut TestAppContext, fixture: &Fixture, what: &str) {
    let needle = fixture.home.to_string_lossy().into_owned();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    loop {
        let left = processes_naming(&needle);
        if left.is_empty() {
            println!(
                "{NOTE} {what}: nothing left running ({:?})",
                started.elapsed()
            );
            return;
        }
        if Instant::now() >= deadline {
            let commands: Vec<String> = left.iter().map(|pid| command_line(*pid)).collect();
            panic!("{what}: still running after the quit: {commands:?}");
        }
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Whether a process is still running: `swarm_client`'s own rule, which asks a child
/// of ours for its status, so a swarm the app spawned stops being "alive" the moment
/// it exits rather than when someone waits on it (a zombie answers `kill(pid, 0)`).
pub fn running(pid: u32) -> bool {
    swarm_client::server::process_alive(pid)
}

/// The command line of a live process, for the `--resume` argument and for
/// failure messages.
pub fn command_line(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Every live process whose command line names `needle`: the swarms and lanes this
/// test started all carry their temp `EVO_HOME` (or a tab directory under it) in
/// their argv, so one sweep is the whole "nothing of mine is left" check.
pub fn processes_naming(needle: &str) -> Vec<u32> {
    let out = match std::process::Command::new("pgrep")
        .args(["-f", needle])
        .output()
    {
        Ok(out) => out,
        Err(error) => panic!("pgrep: {error}"),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .collect()
}

/// Whether two spellings name the same place.
///
/// The swarm canonicalizes the folder it reports back (`/var` is `/private/var`
/// on this platform, and the path comes with a trailing separator), while the app
/// stores the folder the user chose — so the two are compared as locations.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
