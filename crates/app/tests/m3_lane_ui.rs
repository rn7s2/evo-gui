//! M3 (§12) at the UI level: a lane dies, the swarm brings it back, and the left
//! column shows both — the ✗ row while it is down, and the restart it counted.
//!
//! One real `evo-swarm serve` with two lanes, a temp `EVO_HOME` with a stub
//! provider, and the app's own window, tab and agent list. The pid that gets
//! SIGKILLed is the one *this* swarm's `/lanes` reports for lane 1: nothing else
//! on the machine is touched.
//!
//! `cargo test -p evo-desktop --test m3_lane_ui -- --nocapture` prints the step
//! timings and the samples of the lane row.

mod common;

use std::time::{Duration, Instant};

use common::{
    drain, lane_rows, launch, open, quit, running, send, set_evo_home, wait_booted, wait_for,
    wait_for_text, wait_nothing_left, NOTE, WAIT,
};
use evo_desktop::AppLog;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, TestAppContext};
use session::LaneStatus;
use store::app_state::{AppState, Binaries};
use store::paths::Root as AppRoot;
use swarm_client::harness::{Fixture, HarnessConfig};

/// The lane row's own id in the left column: `agent_list` numbers the rows from 0,
/// with `main` first and lane N at N.
const AGENT_ROW: &str = "agent-row";

/// A turn sent after the lane was restarted, and what the stub model answers it.
const PROMPT: &str = "M3 after the restart";
const PROMPT_REPLY: &str = "ok: M3 after the restart";

/// One sample of the lane's row as the app's model had it.
type Sample = (String, LaneStatus, u64, Option<u64>);

#[gpui_kit::test]
fn a_killed_lane_goes_down_and_the_swarm_brings_it_back(cx: &mut TestAppContext) {
    let whole = Instant::now();
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let fixture = Fixture::new(HarnessConfig {
        workers: 2,
        ..Default::default()
    })
    .expect("the fixture: a stub provider, a temp evo home and the installed binaries");
    set_evo_home(&fixture);

    let root = AppRoot::at(fixture.home.join("desktop"));
    let folder = fixture.dir.join("project");
    std::fs::create_dir_all(&folder).expect("a folder to run in");
    let binaries = Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    };
    let log = AppLog::open(&root);
    let (window, view) = open(
        cx,
        &fixture,
        &root,
        AppState {
            binaries,
            ..Default::default()
        },
    );
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    launch(cx, &tab, &folder, 2);
    wait_booted(cx, "the swarm", &tab);

    // Both lanes idle in the app's own model — which is exactly what the left
    // column draws — and the row on screen says so.
    wait_for(cx, "two idle lanes", |cx| {
        let rows = lane_rows(cx, &tab);
        rows.len() == 2 && rows.iter().all(|row| row.status == LaneStatus::Idle)
    });
    let idle_line = agent_row(cx, window, 1).expect("lane 1's row is drawn");
    assert!(
        idle_line.contains("lane 1") && idle_line.contains("idle"),
        "the row on screen reads: {idle_line}"
    );

    let pid = lane_rows(cx, &tab)
        .into_iter()
        .find(|row| row.n == 1)
        .and_then(|row| row.pid)
        .expect("lane 1's pid, as this swarm reports it") as u32;
    assert!(running(pid), "lane 1 is a live process: {pid}");

    // Only this test's swarm: this pid came from its own `/lanes`.
    let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(
        killed,
        0,
        "SIGKILL to lane 1 ({pid}): {}",
        std::io::Error::last_os_error()
    );
    println!("{NOTE} killed lane 1 ({pid})");

    // Watch the row: down (✗), then back to idle with the restart counted. The
    // samples are kept for the failure message — the down state is a moment, not
    // a state the swarm stays in.
    let mut seen: Vec<Sample> = Vec::new();
    let mut down_line: Option<String> = None;
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if let Some(row) = lane_rows(cx, &tab).into_iter().find(|row| row.n == 1) {
            let sample: Sample = (row.state.clone(), row.status, row.restarts, row.pid);
            if seen.last().map(|last| (last.1, last.2)) != Some((sample.1, sample.2)) {
                seen.push(sample.clone());
            }
            if row.status == LaneStatus::Down && down_line.is_none() {
                down_line = agent_row(cx, window, 1);
                println!("{NOTE} lane 1 down: {:?}", down_line);
            }
            if row.status == LaneStatus::Idle && row.restarts >= 1 && down_line.is_some() {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!("lane 1 never went down and came back: {seen:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("{NOTE} lane 1 walked through: {seen:?}");

    assert!(
        seen.iter().any(|sample| sample.1 == LaneStatus::Down),
        "the model saw the lane down: {seen:?}"
    );
    let down_line = down_line.expect("the down row was drawn");
    assert!(
        down_line.contains("lane 1") && down_line.contains("down") && down_line.contains('✗'),
        "the row on screen while it was down reads: {down_line}"
    );

    let back = lane_rows(cx, &tab)
        .into_iter()
        .find(|row| row.n == 1)
        .expect("lane 1's row");
    assert_eq!(
        back.status,
        LaneStatus::Idle,
        "it is working again: {back:?}"
    );
    assert!(
        back.restarts >= 1,
        "the supervisor counted the restart: {back:?}"
    );
    assert!(
        back.pid.is_some_and(|pid| running(pid as u32)),
        "and a live process is behind it again: {back:?}"
    );

    // The tab keeps working: a turn after the restart, and its reply.
    send(cx, window, &tab, PROMPT);
    wait_for_text(cx, "the reply after the restart", &tab, PROMPT_REPLY);

    // And nothing of this test is left behind.
    quit(cx, &log);
    drain(cx);
    wait_nothing_left(cx, &fixture, "after the quit");
    println!("{NOTE} the whole run: {:?}", whole.elapsed());
}

/// The lane row as the screen has it: `agent_list`'s accessibility label, which
/// leads with the status glyph and names the lane and its state.
fn agent_row(cx: &mut TestAppContext, window: AnyWindowHandle, lane: usize) -> Option<String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find(ElementId::named_usize(AGENT_ROW, lane))
            .and_then(|row| row.label().map(str::to_owned))
    })
    .ok()
    .flatten()
}
