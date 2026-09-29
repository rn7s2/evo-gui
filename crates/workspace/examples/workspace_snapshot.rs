//! The window, its chrome, and a tab driving a **real** swarm — rendered to PNGs.
//!
//! ```sh
//! cargo run -p workspace --example workspace_snapshot                 # live window (the app's own)
//! cargo run -p workspace --example workspace_snapshot -- --capture <dir>
//! ```
//!
//! Capture mode drives the production views through GPUI's headless renderer, so
//! the pictures do not need a window on screen — which is what makes them
//! reproducible on a machine whose screen is locked. It starts a real `evo-swarm`
//! through `swarm_client`'s harness (a temp `HOME` with a scripted stub model),
//! launches a tab into it, prompts it, and captures the tab mid-stream and with a
//! lane selected.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::TitleBar;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, HeadlessAppContext,
    InputEvent as _, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use swarm_client::harness::{Fixture, HarnessConfig, STUB_MODEL};
use workspace::{Launch, SwarmConfig, TabState, WorkspaceView};

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// A transition samples the app clock; advance it before a capture.
const SETTLE: Duration = Duration::from_millis(400);

/// One turn of a wait loop. The app clock — which a headless context only moves
/// when it is asked — is advanced by the same amount, so a wait of four real
/// seconds is four app seconds: a notice's lifetime, a stream's fade and a
/// spinner all move while the batch works, instead of standing still and putting
/// something in a picture that a user would never see.
const TICK: Duration = Duration::from_millis(50);

/// How long a capture waits for the swarm to answer (§3 gives boot 90 s).
const WAIT: Duration = Duration::from_secs(90);

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
            eprintln!("usage: workspace_snapshot --capture <dir>");
            std::process::exit(2);
        }
    }
}

/// The window options the capture uses: the production title bar, at the size the
/// app opens with, without touching the screen.
fn capture_window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
        })),
        focus: false,
        show: false,
        ..TitleBar::window_options()
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let fixture = Fixture::new(HarnessConfig {
        workers: 2,
        model: STUB_MODEL.to_owned(),
        ..HarnessConfig::default()
    })?;
    let config = Arc::new(SwarmConfig {
        swarm_bin: fixture.bins.swarm.clone(),
        agent_bin: fixture.bins.agent.clone(),
        root: store::paths::Root::at(fixture.dir.join("app")),
        env: fixture.env(),
        env_remove: fixture.env_remove(),
    });

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The engine boots on its own threads and its updates arrive on their own
    // channel: this test scheduler has to be told that real I/O is expected.
    cx.allow_parking();

    let (window, view) = open_workspace(&mut cx, config.clone())?;

    // 1. What the app opens with: one empty tab, the choosers and the history.
    shot(&mut cx, window, dir, "01-empty-tab.png")?;

    // 2. The `+` appended a second tab and selected it (§7.1).
    click(&mut cx, window, "tab-add")?;
    shot(&mut cx, window, dir, "02-two-tabs.png")?;

    // The rest of the pictures are of the first tab, which is the one that will
    // run a swarm: select it again and type into the composer it shows.
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
    })?;
    let tab = view.read_with(&cx, |view, _| view.tabs()[0].clone());

    // 3. A folder was chosen: the tab boots a real swarm in it (§3).
    launch(
        &mut cx,
        window,
        &tab,
        Launch::New {
            folder: fixture.project.clone(),
            plan: LaunchPlan {
                workers: Some(2),
                ..LaunchPlan::default()
            },
        },
    )?;
    // 3. The tab while the swarm starts: what is starting, and where (§3). Taken
    //    before the first pump, which is the only moment it is on screen.
    shot(&mut cx, window, dir, "03-booting.png")?;

    wait_until(&mut cx, "the swarm to answer /health", |cx| {
        matches!(
            tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Running { .. }
        )
    })?;
    shot(&mut cx, window, dir, "04-tab-page.png")?;

    // 4. A turn is streaming: the assistant row is rendered markdown while it
    //    grows (§2.8). "SLOW" makes the stub send 60 deltas a tenth apart.
    prompt(&mut cx, window, &tab, "SLOW say something long")?;
    // Wait until the assistant row is actually growing, so the picture shows
    // markdown rendered mid-stream rather than an empty page (§2.8).
    wait_until(&mut cx, "the reply to start streaming", |cx| {
        text_chars(cx, &tab) > 40
    })?;
    shot(&mut cx, window, dir, "05-streaming.png")?;

    // 5. The swarm was settled before the next turn asks it to work.
    wait_until(&mut cx, "the run to settle", |cx| !working(cx, &tab))?;

    // 6. Lane 1 is chosen first — its stream is opened while it is idle — and
    //    then the coordinator delegates to it: the center column follows the
    //    lane's own transcript while it works (§7.3, §9.3). The composer still
    //    types to the coordinator (§14.4).
    // The swarm's lanes are `starting` until their baseline is evaluated, and the
    // delegate tool takes an idle lane: wait for lane 1 to be ready (§9.3).
    wait_until(&mut cx, "lane 1 to be ready for work", |cx| {
        tab.read_with(cx, |tab, _| {
            tab.model().is_some_and(|model| {
                model
                    .lane_rows()
                    .iter()
                    .any(|lane| lane.n == 1 && lane.status == session::LaneStatus::Idle)
            })
        })
    })?;
    select_lane(&mut cx, window, &tab, 1)?;
    prompt(
        &mut cx,
        window,
        &tab,
        "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the picture\"}",
    )?;
    wait_until(&mut cx, "the lane to take the work and stream", |cx| {
        lane_working(cx, &tab) && text_chars(cx, &tab) > 40
    })?;
    shot(&mut cx, window, dir, "06-lane-selected.png")?;

    // 7. Back to the coordinator, with the report the lane sent back.
    select_main(&mut cx, window, &tab)?;
    wait_until(&mut cx, "the coordinator to show the delegation", |cx| {
        rows(cx, &tab) >= 3
    })?;
    shot(&mut cx, window, dir, "07-coordinator-after-delegation.png")?;

    // 8. A swarm that cannot start: the tab shows the log tail, a Retry and a
    //    Close (§9.7). A second window with a binary that is not there.
    let broken = Arc::new(SwarmConfig {
        swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
        ..(*config).clone()
    });
    let (broken_window, broken_view) = open_workspace(&mut cx, broken)?;
    let broken_tab = broken_view.read_with(&cx, |view, _| view.selected_tab().clone());
    launch(
        &mut cx,
        broken_window,
        &broken_tab,
        Launch::New {
            folder: fixture.project.clone(),
            plan: LaunchPlan::default(),
        },
    )?;
    wait_until(&mut cx, "the boot to fail", |cx| {
        matches!(
            broken_tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Failed { .. }
        )
    })?;
    let failed = broken_tab.read_with(&cx, |tab, _| tab.state().clone());
    if std::env::var_os("SNAPSHOT_TRACE").is_some() {
        eprintln!("[trace] failed state: {failed:?}");
    }
    shot(&mut cx, broken_window, dir, "08-boot-failed.png")?;

    // 9. §9.7: the swarm stops answering. `evo-swarm serve` runs the server as its
    //    own child and supervises it, so killing *that* child leaves a supervisor
    //    watching a closed port: the stream goes to reconnecting, the tab keeps its
    //    page, and the supervisor brings the server back.
    let pid = tab
        .read_with(&cx, |tab, _| tab.swarm_pid())
        .expect("the swarm's pid, from /health");
    let server = served_by(pid).ok_or("the serving child of the supervisor")?;
    signal(server, "-KILL");
    wait_until(&mut cx, "the stream to go to reconnecting", |cx| {
        tab.read_with(cx, |tab, _| tab.is_reconnecting())
    })?;
    println!(
        "[capture] reconnecting — tab tooltip: {:?}",
        tab.read_with(&cx, |tab, _| tab.tooltip())
    );
    shot(&mut cx, window, dir, "09-reconnecting.png")?;

    // 10. A turn typed while the swarm cannot answer: the POST fails, and the
    //     failure is a line above the composer, with the draft still there (§4).
    prompt(&mut cx, window, &tab, "SLOW unreachable swarm")?;
    wait_until(&mut cx, "the refused turn to be shown", |cx| {
        tab.read_with(cx, |tab, _| tab.notice_text().is_some())
    })?;
    println!(
        "[capture] notice: {:?} (draft kept: {})",
        tab.read_with(&cx, |tab, _| tab.notice_text().map(str::to_owned)),
        tab.read_with(&cx, |tab, cx| tab.composer().read(cx).is_action_enabled(cx))
    );
    shot(&mut cx, window, dir, "10-post-failed-notice.png")?;

    // 10b. A notice is a line, not a state: it clears itself a few seconds later
    //      (§4). The capture runs on a simulated clock, so this is where the
    //      clock gets the chance to say so — the pictures from here on are of a
    //      tab that is not still apologising for a POST nine steps ago.
    wait_until(&mut cx, "the notice to clear itself", |cx| {
        tab.read_with(cx, |tab, _| tab.notice_text().is_none())
    })?;
    println!("[capture] the notice cleared itself");

    // 11. The supervisor put the server back (§3): the stream comes back on its way
    //     to the new process, and the badge goes with it.
    wait_until(&mut cx, "the stream to come back", |cx| {
        tab.read_with(cx, |tab, _| !tab.is_reconnecting())
    })?;
    shot(&mut cx, window, dir, "11-recovered.png")?;

    // 12. §7.1 past the window's width: fourteen tabs do not fit across 1600
    //     points. The strip scrolls the tab it is showing into view — the one just
    //     added is the last one — and the `+` stays on the screen, at the right
    //     edge of the strip's own width budget rather than past the window.
    for _ in 2..14 {
        click(&mut cx, window, "tab-add")?;
    }
    shot(&mut cx, window, dir, "12-strip-overflow.png")?;

    // Back to the tab that drives a swarm: the goal and the checklist are its
    // own state, and the page has to be the one on screen to show them.
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
    })?;
    let tab = view.read_with(&cx, |view, _| view.tabs()[0].clone());

    // 13. §7.3: the coordinator's todo panel, at the foot of the center column —
    //     the checklist the `todo` tool replaces wholesale. The stub's FINISH
    //     marker in the goal objective below is deliberate: an active goal makes
    //     the coordinator continue itself, and the marker is what makes the
    //     stub's continuation turn close it, so the run settles and the resync
    //     that follows puts the goal segment on the line.
    prompt(
        &mut cx,
        window,
        &tab,
        "CALL todo {\"items\":[\
         {\"text\":\"read the tab page against §7.3\",\"status\":\"done\"},\
         {\"text\":\"check the goal segment\",\"status\":\"in-progress\"},\
         {\"text\":\"capture the page\",\"status\":\"pending\"}]}",
    )?;
    wait_until(&mut cx, "the coordinator's todos", |cx| {
        tab.read_with(cx, |tab, _| {
            tab.model()
                .is_some_and(|model| model.selected_todos().len() == 3)
        })
    })?;

    prompt(
        &mut cx,
        window,
        &tab,
        "CALL create_goal {\"objective\":\"read the tab page §7.3 FINISH\",\"token-budget\":50000}",
    )?;
    wait_until(&mut cx, "the goal segment", |cx| {
        tab.read_with(cx, |tab, _| {
            tab.model()
                .is_some_and(|model| model.readout_text().contains("goal g-"))
        })
    })?;
    println!(
        "[capture] readout: {:?}",
        tab.read_with(&cx, |tab, _| tab.model().map(|model| model.readout_text()))
    );
    shot(&mut cx, window, dir, "13-goal-and-todos.png")?;

    // 14. §7.3, §9.2: the one button, mid-run. A turn streams while a draft is
    //     typed into the input, and the button reads **Stop** — it interrupts,
    //     never sends, and it leaves that draft exactly where it was.
    prompt(&mut cx, window, &tab, "SLOW say something long")?;
    wait_until(&mut cx, "the run to be in flight", |cx| {
        tab.read_with(cx, |tab, _| tab.is_running())
    })?;
    wait_until(&mut cx, "the reply to start streaming", |cx| {
        text_chars(cx, &tab) > 20
    })?;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.input("half a next turn", cx);
        window.render_frame(cx);
    })?;
    println!(
        "[capture] button: {:?}",
        cx.update_window(window, |_, window, _cx| {
            window.find(composer::BUTTON_ID).label().map(str::to_owned)
        })?
    );
    shot(&mut cx, window, dir, "14-stop-with-draft.png")?;
    // `Esc` is the same interrupt by another route, and it is how this capture
    // gets back to an idle coordinator before the swarm is killed.
    cx.update_window(window, |_, window, cx| window.press("escape", cx))?;
    wait_until(&mut cx, "the interrupt to end the run", |cx| {
        tab.read_with(cx, |tab, _| !tab.is_running())
    })?;

    // 15. §9.7: the swarm is **gone** — the whole process group killed at once,
    //     supervisor and server together, so nothing is left to bring the server
    //     back. The tab says so, with the log the server was writing.
    let pid = tab
        .read_with(&cx, |tab, _| tab.swarm_pid())
        .expect("the swarm's pid, from /health");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .status()?;
    if !killed.success() {
        return Err(format!("kill -KILL -{pid}").into());
    }
    wait_until(&mut cx, "the swarm-gone failure", |cx| {
        matches!(
            tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Failed { .. }
        )
    })?;
    println!(
        "[capture] gone tooltip: {:?}",
        tab.read_with(&cx, |tab, _| tab.tooltip())
    );
    shot(&mut cx, window, dir, "15-swarm-gone.png")?;

    // 16. §3/§9.7: Retry on a swarm that had been up resumes the session it was
    //     writing, so the conversation comes back with it.
    click(&mut cx, window, "tab-retry")?;
    wait_until(&mut cx, "the retried swarm to run", |cx| {
        matches!(
            tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Running { .. }
        )
    })?;
    wait_until(&mut cx, "the resumed transcript", |cx| rows(cx, &tab) > 2)?;
    shot(&mut cx, window, dir, "16-retry-resumed.png")?;

    // 17, 18, 19. §7.3: the splits between the three columns are draggable, and a
    //     double-click on one puts that side back to the width it starts at. These
    //     are the pixel-level gestures, not calls into the layout.
    let left = divider(&mut cx, window, "agent-column", Side::Trailing)?;
    drag_split(&mut cx, window, left, left + point(px(-80.), px(0.)))?;
    shot(&mut cx, window, dir, "17-narrow-agent-column.png")?;

    let right = divider(&mut cx, window, "composer-column", Side::Leading)?;
    drag_split(&mut cx, window, right, right + point(px(-120.), px(0.)))?;
    shot(&mut cx, window, dir, "18-wider-composer.png")?;

    let left = divider(&mut cx, window, "agent-column", Side::Trailing)?;
    double_click_split(&mut cx, window, left)?;
    let right = divider(&mut cx, window, "composer-column", Side::Leading)?;
    double_click_split(&mut cx, window, right)?;
    shot(&mut cx, window, dir, "19-columns-reset.png")?;

    Ok(())
}

/// Which edge of a column a divider runs along.
enum Side {
    Leading,
    Trailing,
}

/// A divider's own point: the edge of the panel it splits, at the middle of the
/// page, which is where the pointer has to be to take hold of it.
fn divider(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    column: &'static str,
    side: Side,
) -> Result<Point<Pixels>, Box<dyn std::error::Error>> {
    Ok(cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(column).bounds();
        let page = window.find("tab-page").bounds();
        let x = match side {
            Side::Leading => bounds.left(),
            Side::Trailing => bounds.right(),
        };
        point(x, page.center().y)
    })?)
}

/// A drag on a divider: press, move past the threshold a drag needs, then to
/// where the split belongs, and let go.
///
/// One event per turn of the event loop, each followed by a frame — a drag is not
/// a batch of events, and a capture that sent them all at once would leave the
/// pointer's own task with nothing to run on.
fn drag_split(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    from: Point<Pixels>,
    to: Point<Pixels>,
) -> Result<(), Box<dyn std::error::Error>> {
    press(cx, window, from, 1)?;
    for at in [from + point(px(6.), px(0.)), to] {
        move_pointer(cx, window, at)?;
    }
    release(cx, window, to, 1)
}

/// The same gesture the other way: two clicks, the second of which is what a
/// double-click is.
fn double_click_split(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    at: Point<Pixels>,
) -> Result<(), Box<dyn std::error::Error>> {
    for count in 1..=2 {
        press(cx, window, at, count)?;
        release(cx, window, at, count)?;
    }
    Ok(())
}

fn press(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    at: Point<Pixels>,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseDownEvent {
                position: at,
                modifiers: Modifiers::none(),
                button: MouseButton::Left,
                click_count: count,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    cx.run_until_parked();
    Ok(())
}

fn release(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    at: Point<Pixels>,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseUpEvent {
                position: at,
                modifiers: Modifiers::none(),
                button: MouseButton::Left,
                click_count: count,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    cx.run_until_parked();
    Ok(())
}

fn move_pointer(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    at: Point<Pixels>,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: at,
                modifiers: Modifiers::none(),
                pressed_button: Some(MouseButton::Left),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    cx.run_until_parked();
    Ok(())
}

fn open_workspace(
    cx: &mut HeadlessAppContext,
    config: Arc<SwarmConfig>,
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Box<dyn std::error::Error>> {
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(capture_window_options(), cx, |window, cx| {
            cx.new(|cx| WorkspaceView::with_config(config, window, cx))
        })
    })?;
    Ok((window, view))
}

/// Send a signal to one process this capture started (§9.7).
fn signal(pid: u32, which: &str) {
    let status = std::process::Command::new("kill")
        .arg(which)
        .arg(pid.to_string())
        .status()
        .expect("kill");
    assert!(status.success(), "kill {which} {pid}");
}

/// The process actually serving, which is the child of the reported pid: the
/// supervisor is what `/health` names, and killing it would take the tab's whole
/// swarm away instead of leaving a stream to reconnect (§9.7).
fn served_by(supervisor: u32) -> Option<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &supervisor.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn launch(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    launch: Launch,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.launch(launch, window, cx));
    })?;
    Ok(())
}

/// Type a turn into the composer and press Enter — the app's own send path (§9.2).
/// Type a turn into the composer and press Enter — the app's own send path (§9.2).
///
/// The input is emptied first: a POST the swarm never answered leaves its draft
/// exactly where it was (§9.2), and a capture that types on top of it would send
/// the two turns as one.
fn prompt(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| {
            let composer = tab.composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        });
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.input(text, cx);
        window.press("enter", cx);
    })?;
    Ok(())
}

fn select_lane(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    n: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| {
            tab.select_agent(session::AgentKey::Lane(n), cx)
        });
    })?;
    Ok(())
}

fn select_main(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| {
            tab.select_agent(session::AgentKey::Coordinator, cx)
        });
    })?;
    Ok(())
}

/// How much text the shown agent's transcript carries, in bytes — enough to tell
/// "nothing yet" from "streaming" inside a wait loop.
fn text_chars(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> usize {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| view.read(cx).rows(cx).iter().map(row_chars).sum::<usize>())
            .unwrap_or_default()
    })
}

fn row_chars(row: &session::Row) -> usize {
    match &row.kind {
        session::RowKind::User { text } => text.chars().count(),
        session::RowKind::Context { text, .. } => text.chars().count(),
        session::RowKind::LaneNotice { text, .. } => text.chars().count(),
        session::RowKind::GoalNudge { text, .. } => text.chars().count(),
        session::RowKind::CommandNote { text, .. } => text.chars().count(),
        session::RowKind::Assistant { markdown, .. } => markdown.chars().count(),
        session::RowKind::Tool { name, .. } => name.chars().count(),
        session::RowKind::Report { done, .. } => done.chars().count(),
        session::RowKind::Dim { text, .. } => text.chars().count(),
        session::RowKind::RunOutcome { text, .. } => text.chars().count(),
    }
}

/// How many rows the coordinator's transcript shows.
fn rows(cx: &HeadlessAppContext, tab: &gpui_kit::Entity<workspace::TabContent>) -> usize {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| view.read(cx).rows(cx).len())
            .unwrap_or_default()
    })
}

/// Whether the coordinator is mid-run.
fn working(cx: &HeadlessAppContext, tab: &gpui_kit::Entity<workspace::TabContent>) -> bool {
    tab.read_with(cx, |tab, _| {
        tab.model()
            .is_some_and(|model| model.activity() != session::Activity::Idle)
    })
}

/// Whether lane 1 has taken the delegated work.
fn lane_working(cx: &HeadlessAppContext, tab: &gpui_kit::Entity<workspace::TabContent>) -> bool {
    tab.read_with(cx, |tab, _| {
        tab.model().is_some_and(|model| {
            model
                .lane_rows()
                .iter()
                .any(|lane| lane.n == 1 && lane.status == session::LaneStatus::Working)
        })
    })
}

/// Spin the UI thread until `done` holds or the wait runs out. A capture is a
/// batch job, so a bounded wait loop is what it uses instead of events.
///
/// `what` is what a run that gave up says: a batch that dies at 90 s has to name
/// the state it was waiting for, or the log is a wall of pictures.
fn wait_until(
    cx: &mut HeadlessAppContext,
    what: &str,
    done: impl Fn(&HeadlessAppContext) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let deadline = started + WAIT;
    let mut ticks = 0u32;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return Ok(());
        }
        if std::env::var_os("SNAPSHOT_TRACE").is_some() && ticks.is_multiple_of(20) {
            eprintln!("[wait] tick {ticks} {what}");
        }
        ticks += 1;
        if Instant::now() >= deadline {
            return Err(
                format!("timed out after {:?} waiting for {what}", started.elapsed()).into(),
            );
        }
        cx.advance_clock(TICK);
        std::thread::sleep(TICK);
    }
}

/// Draw a frame and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Something that animates (a spinner, a stream fade) settles on the app
    // clock, which the headless context only advances when asked.
    cx.advance_clock(SETTLE);
    std::thread::sleep(SETTLE);
    // Two frames: the first lays out what the last update changed, the second
    // paints it — a scroll box's content is placed on the frame after its layout.
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
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

fn click(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    id: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })?;
    Ok(())
}
