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
    WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use swarm_client::harness::{Fixture, HarnessConfig, STUB_MODEL};
use workspace::{Launch, SwarmConfig, TabState, WorkspaceView};

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// A transition samples the app clock; advance it before a capture.
const SETTLE: Duration = Duration::from_millis(400);

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
    view.update(&mut cx, |view, cx| view.select_tab(0, cx));
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
    wait_until(&mut cx, |cx| {
        matches!(
            tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Running { .. }
        )
    })?;
    shot(&mut cx, window, dir, "03-tab-page.png")?;

    // 4. A turn is streaming: the assistant row is rendered markdown while it
    //    grows (§2.8). "SLOW" makes the stub send 60 deltas a tenth apart.
    prompt(&mut cx, window, &tab, "SLOW say something long")?;
    // Wait until the assistant row is actually growing, so the picture shows
    // markdown rendered mid-stream rather than an empty page (§2.8).
    wait_until(&mut cx, |cx| text_chars(cx, &tab) > 40)?;
    shot(&mut cx, window, dir, "04-streaming.png")?;

    // 5. The swarm was settled before the next turn asks it to work.
    wait_until(&mut cx, |cx| !working(cx, &tab))?;

    // 6. Lane 1 is chosen first — its stream is opened while it is idle — and
    //    then the coordinator delegates to it: the center column follows the
    //    lane's own transcript while it works (§7.3, §9.3). The composer still
    //    types to the coordinator (§14.4).
    select_lane(&mut cx, window, &tab, 1)?;
    prompt(
        &mut cx,
        window,
        &tab,
        "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the picture\"}",
    )?;
    wait_until(&mut cx, |cx| {
        lane_working(cx, &tab) && text_chars(cx, &tab) > 40
    })?;
    shot(&mut cx, window, dir, "05-lane-selected.png")?;

    // 7. Back to the coordinator, with the report the lane sent back.
    select_main(&mut cx, window, &tab)?;
    wait_until(&mut cx, |cx| rows(cx, &tab) >= 3)?;
    shot(&mut cx, window, dir, "06-coordinator-after-delegation.png")?;

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
    wait_until(&mut cx, |cx| {
        matches!(
            broken_tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Failed { .. }
        )
    })?;
    shot(&mut cx, broken_window, dir, "07-boot-failed.png")?;

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
    Ok((window.into(), view))
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
fn wait_until(
    cx: &mut HeadlessAppContext,
    done: impl Fn(&HeadlessAppContext) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + WAIT;
    let mut ticks = 0u32;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return Ok(());
        }
        if std::env::var_os("SNAPSHOT_TRACE").is_some() && ticks % 20 == 0 {
            eprintln!("[wait] tick {ticks}");
        }
        ticks += 1;
        if Instant::now() >= deadline {
            return Err("timed out waiting for the swarm".into());
        }
        std::thread::sleep(Duration::from_millis(50));
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
