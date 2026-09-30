//! The pictures `docs/screens.md` shows: the real window, the real binaries, the
//! scripted model — no mocks and no drawn-by-hand states.
//!
//! ```sh
//! EVO_SWARM_BIN=…/evo-swarm EVO_AGENT_BIN=…/evo-agent \
//!   cargo run -p evo-desktop --example screens -- --capture docs/screens
//! ```
//!
//! What it drives is the app's own path, the same one a person drives: the
//! `Shell` the window is built from, the empty tab's choosers and history as
//! `startup` loads them, a tab's launch through `TabContentEvent::Launch`, the
//! composer's own keystrokes (`window.input`, `window.press("enter")`) and its
//! own button (`composer-action`). Nothing here pokes a widget's fields.
//!
//! The world is `crates/proofs`' fixture: a throwaway `HOME` whose `init.lisp`
//! registers the scripted model, the folder a swarm runs in, and the tab
//! directory the server publishes its ready file into. So a capture run leaves
//! the real `~/.evo` alone, and the two binaries are the ones the environment
//! names (`EVO_SWARM_BIN` / `EVO_AGENT_BIN`, else `/usr/local/bin`).
//!
//! Every state is the swarm's own: the integration build serves tabs, so the
//! pictures below are the pool coming up, a lane at work and its own transcript, a
//! lane's report arriving as an item, a tool row opened, a prompt queued behind a
//! run, the check's own line for a binary that cannot run, and the history a
//! session leaves behind.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, ElementId, Entity,
    HeadlessAppContext, Point, WindowBounds, WindowOptions,
};
use proofs::fixture::Fixture;
use session::{AgentKey, Item, ItemKind};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContent, TabContentEvent, TabState, WorkspaceView};

/// The window the app opens, and the size these pictures are taken at.
const WINDOW_SIZE: (f32, f32) = (1440., 900.);

/// How long a state gets to arrive before the capture gives up on it.
const WAIT: Duration = Duration::from_secs(60);

/// What the app's own launch-time loads are given before the first picture: they
/// are threads of their own (`sessions --json`, then `catalog --json`), and the
/// empty tab is what this waits on.
const LOADS: Duration = Duration::from_secs(3);

/// The prompt that makes the scripted model call a tool.
const TOOL_PROMPT: &str = "CALL bash {\"command\": \"ls crates\"}";

/// A prompt whose answer streams in slowly: sixty deltas, a tenth of a second
/// apart. Long enough to take a picture in the middle of it.
const SLOW_PROMPT: &str = "SLOW the quick brown fox jumps over the lazy dog";

/// A prompt typed while the answer above is still being written.
const QUEUED_PROMPT: &str = "and then check the proofs";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => {
                i += 1;
                dir = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("screens: unexpected argument {other:?}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(dir) = dir else {
        eprintln!("usage: screens --capture <dir>");
        std::process::exit(2);
    };
    match capture(&dir) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("capture failed: {error}");
            std::process::exit(1);
        }
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let fixture = Fixture::new("screens");
    // The app's own reads (`sessions --json`) and every child it spawns: the stub
    // home, the scripted model's address, the agent its lanes run.
    fixture.enter();

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx.allow_parking();

    let bins = Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    };
    println!(
        "[capture] swarm {} / agent {}",
        bins.evo_swarm.display(),
        bins.evo_agent.display()
    );

    // --- the window the app opens, and its first frame -------------------------
    let (window, view) = open(&mut cx, &fixture.root, bins)?;
    // The app's own loads: the cache, the session index, the catalog.
    cx.update(evo_desktop::start_background_loads);
    pump(&mut cx, LOADS);
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    shot(&mut cx, window, dir, "01-empty")?;

    // --- a tab that starts a swarm ---------------------------------------------
    launch(&mut cx, &tab, &fixture.folder);
    let state = wait_for(&mut cx, "the tab to settle", |cx| {
        let state = tab_state(cx, &tab);
        (!matches!(state, TabState::Empty | TabState::Booting { .. })).then_some(state)
    });
    match state {
        TabState::Running { .. } => {
            println!("[capture] the tab came up: {}", fixture.folder.display());
            live_states(&mut cx, window, dir, &view, &tab)?;
            // The same window, one tab older: its history now has the session
            // that tab just ran.
            let tabs = view.clone();
            cx.update_window(window, |_, window, cx| {
                tabs.update(cx, |view, cx| {
                    view.add_tab(window, cx);
                });
            })?;
            // The loads again, the way the app runs them at startup: the session
            // that tab just ran is in the index now, and this is the read that
            // puts it in the history list.
            cx.update(evo_desktop::start_background_loads);
            pump(&mut cx, LOADS);
            shot(&mut cx, window, dir, "07-history")?;
        }
        other => println!("[capture] the tab settled in {other:?}: no live states"),
    }

    // --- a swarm that cannot run at all ----------------------------------------
    //
    // The same window machinery with a path that holds nothing: the empty tab's
    // check line is about a binary that could not be run, and a launch in a folder
    // is the failure screen.
    let (broken_window, broken_view) = open(
        &mut cx,
        &fixture.root,
        Binaries {
            evo_swarm: PathBuf::from("/nonexistent/evo-swarm"),
            evo_agent: fixture.bins.agent.clone(),
        },
    )?;
    cx.update(evo_desktop::start_background_loads);
    pump(&mut cx, LOADS);
    shot(&mut cx, broken_window, dir, "08-check-problem")?;
    boot_failure(&mut cx, broken_window, &broken_view, dir, &fixture.folder)?;
    stop_tabs(&mut cx, &broken_view);

    // The tabs' servers are told to stop, the way the app's own quit tells them.
    stop_tabs(&mut cx, &view);
    // And the fixture goes first, while the app's context is still here: its `Drop`
    // stops every process group this run started, and the context's own drop is
    // where this process ends.
    drop(fixture);
    Ok(())
}

/// Every tab's server gets the app's own stop: the pipe closes, and the engine
/// thread runs the rest of the ladder in its own time.
fn stop_tabs(cx: &mut HeadlessAppContext, view: &Entity<WorkspaceView>) {
    let engines = cx.update(|cx| view.update(cx, |view, cx| view.take_engines(cx)));
    println!("[capture] stopping {} tab(s)", engines.len());
    for mut engine in engines {
        engine.shutdown();
    }
    pump(cx, Duration::from_secs(2));
}

/// The tab page, as a person builds it: a lane at work, that lane's own
/// transcript, a tool row opened, a prompt typed behind a run, and the report a
/// lane sends when its work is done.
fn live_states(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    view: &Entity<WorkspaceView>,
    tab: &Entity<TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    // --- the pool, and one lane given work --------------------------------------
    wait_for(cx, "the lanes to come up", |cx| {
        (lanes(cx, tab) >= 1).then_some(())
    });
    println!("[capture] {} lane(s)", lanes(cx, tab));
    type_text(cx, window, tab, &delegate(1, &lane_task()))?;
    wait_for(cx, "lane 1 working", |cx| {
        lane_busy(cx, tab, 1).then_some(())
    });
    shot(cx, window, dir, "02-lanes-working")?;

    // --- the strip itself ------------------------------------------------------
    //
    // Four tabs, one of them working, one of them under the pointer: the design's
    // own subject (`TabStrip.tsx`), and the only state that shows the outward
    // corners at the ends of a tab, the dividers between tabs and what a hover
    // does to them. The working tab is the one this capture is on.
    let extra = {
        let view = view.clone();
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                (0..3)
                    .map(|_| view.add_tab(window, cx).read(cx).id())
                    .collect::<Vec<_>>()
            })
        })?
    };
    let under_the_pointer = extra[0];
    // The tab that is working is the one these states are about, so it stays the
    // one being shown: the hovered tab is the second, and the strip is now the
    // design's own picture — an active tab with its corners, a hovered one beside
    // it, and the dividers the two of them hide.
    cx.update_window(window, |_, window, cx| {
        let view = view.clone();
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
    })?;
    cx.update_window(window, |_, window, cx| {
        let at = window
            .find(ElementId::NamedInteger(
                "tab-label".into(),
                under_the_pointer.get(),
            ))
            .bounds()
            .center();
        window.simulate_mouse_move(at, cx);
    })?;
    pump(cx, Duration::from_millis(400));
    shot(cx, window, dir, "10-tabs")?;
    // Back to the tab the rest of these states are about.
    cx.update_window(window, |_, window, cx| {
        let view = view.clone();
        view.update(cx, |view, cx| {
            view.select_tab(0, window, cx);
            for id in extra {
                view.close_tab(id, window, cx);
            }
        });
    })?;
    pump(cx, Duration::from_millis(400));

    // --- the lane's own transcript ---------------------------------------------
    click(cx, window, agent_row(AgentKey::Lane(1)))?;
    wait_for(cx, "lane 1 selected", |cx| {
        (cx.update(|cx| tab.read(cx).selected_agent()) == AgentKey::Lane(1)).then_some(())
    });
    pump(cx, Duration::from_millis(600));
    shot(cx, window, dir, "03-lane-transcript")?;
    click(cx, window, agent_row(AgentKey::Coordinator))?;

    // --- a tool row, opened -----------------------------------------------------
    type_text(cx, window, tab, TOOL_PROMPT)?;
    let tool = wait_for(cx, "a tool row", |cx| tool_item(cx, tab));
    click(cx, window, row("transcript-tool", &tool))?;
    pump(cx, Duration::from_millis(600));
    shot(cx, window, dir, "04-tool")?;

    // --- an answer still being written, and a prompt typed behind it -------------
    type_text(cx, window, tab, SLOW_PROMPT)?;
    wait_for(cx, "the answer to start streaming", |cx| {
        streaming(cx, tab).then_some(())
    });
    type_text(cx, window, tab, QUEUED_PROMPT)?;
    wait_for(cx, "the queued prompt", |cx| queued(cx, tab).then_some(()));
    shot(cx, window, dir, "05-queued")?;

    // --- what the lane says when it is done -------------------------------------
    //
    // A lane reports by calling the `report` tool: that item is the coordinator's
    // copy of it, and it arrives as a `lane_report` (§4.1) — fields, not prose.
    wait_for(cx, "the queue to be taken", |cx| {
        (!queued(cx, tab)).then_some(())
    });
    type_text(cx, window, tab, &delegate(1, &report_task()))?;
    wait_for(cx, "lane 1's report", |cx| reported(cx, tab).then_some(()));
    pump(cx, Duration::from_millis(800));
    shot(cx, window, dir, "06-report")?;
    Ok(())
}

/// `CALL delegate {lane, task}` — the coordinator's own tool for giving a lane
/// work, sent the way a person sends a prompt.
fn delegate(lane: u32, task: &str) -> String {
    format!(
        "CALL delegate {}",
        serde_json::json!({ "lane": lane, "task": task })
    )
}

/// A lane's first job: three seconds of delay, then a checklist — long enough to
/// take the picture while the lane is working.
fn lane_task() -> String {
    format!(
        "DELAY3 CALL todo {}",
        serde_json::json!({ "items": [
            { "text": "read the tab page", "status": "in-progress" },
            { "text": "fix the empty column", "status": "pending" },
        ]})
    )
}

/// A lane's second job: report back, which is what the coordinator hears.
fn report_task() -> String {
    format!(
        "CALL report {}",
        serde_json::json!({
            "done": "the tab page reads the swarm's own topics",
            "evidence": "crates/workspace/src/tab_page.rs, 61 workspace tests",
            "next": "nothing",
        })
    )
}

/// A swarm that cannot come up says so, and offers a Retry (§9.7).
/// A swarm that cannot come up says so, and offers a Retry (§9.7).
fn boot_failure(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    view: &Entity<WorkspaceView>,
    dir: &Path,
    folder: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    launch(cx, &tab, folder);
    wait_for(cx, "the failure screen", |cx| {
        matches!(tab_state(cx, &tab), TabState::Failed { .. }).then_some(())
    });
    shot(cx, window, dir, "09-boot-failure")?;
    Ok(())
}

/// A window on this fixture's root, with the shell the app installs: its own
/// `app.json` binaries, and the view registered so the loads reach the tabs.
fn open(
    cx: &mut HeadlessAppContext,
    root: &AppRoot,
    bins: Binaries,
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Box<dyn std::error::Error>> {
    let root = root.clone();
    let (window, view) = cx.update(move |cx| {
        let log = AppLog::open(&root);
        let state = AppState {
            binaries: bins,
            theme: Theme::System,
            ..AppState::default()
        };
        Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
        let config = Arc::new(evo_desktop::launch_env(cx));
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                })),
                focus: false,
                show: false,
                ..workspace::window_options(cx)
            },
            cx,
            |window, cx| {
                // The app's own light or dark, and the design's palettes under it:
                // a capture is the window the app opens, down to the theme. The
                // subscription is not kept — nothing here changes the system's
                // appearance while the pictures are being taken.
                let _appearance = evo_desktop::follow_appearance(cx, window);
                cx.new(|cx| WorkspaceView::with_config(config, window, cx))
            },
        )?;
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        Ok::<_, Box<dyn std::error::Error>>((window, view))
    })?;
    Ok((window, view))
}

/// Start this tab in `folder`, the way the empty tab does.
fn launch(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>, folder: &Path) {
    let folder = folder.to_path_buf();
    let tab = tab.clone();
    cx.update(move |cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: session::LaunchPlan::default(),
            })
        })
    });
}

/// Put the caret where a running tab's goes (§7.1): the composer. Clicking a
/// transcript row takes it away, so every keystroke below asks for it first.
fn focus(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tab = tab.clone();
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| {
            tab.focus_primary(window, cx);
        });
    })?;
    Ok(())
}

/// Type a prompt into the composed tab's own input, then press Enter: the same
/// two calls a person makes.
fn type_text(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<TabContent>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    focus(cx, window, tab)?;
    cx.update_window(window, |_, window, cx| {
        window.input(text, cx);
        window.press("enter", cx);
    })?;
    Ok(())
}

fn tab_state(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
}

fn items(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> Vec<Item> {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.items(AgentKey::Coordinator).to_vec())
            .unwrap_or_default()
    })
}

/// The id of the first tool row in the coordinator's transcript.
fn tool_item(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> Option<String> {
    items(cx, tab)
        .into_iter()
        .find(|item| matches!(item.kind, ItemKind::Tool(_)))
        .map(|item| item.id)
}

/// The element id a transcript row is drawn under.
fn row(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
}

/// The element id of a row in the agents column: `main` is 0, lane N is N (§7.3).
fn agent_row(key: AgentKey) -> ElementId {
    let index = match key {
        AgentKey::Coordinator => 0,
        AgentKey::Lane(n) => u64::from(n),
    };
    ElementId::NamedInteger("agent-row".into(), index)
}

fn click(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    id: ElementId,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| window.click(id, cx))?;
    Ok(())
}

/// How many lanes the pool is running.
fn lanes(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> usize {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.lane_rows().len())
            .unwrap_or(0)
    })
}

/// Whether lane N is working (or compacting) right now.
fn lane_busy(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>, n: u32) -> bool {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| {
                model
                    .lane_rows()
                    .iter()
                    .any(|lane| lane.n == n && lane.is_busy())
            })
            .unwrap_or(false)
    })
}

/// Whether an answer is still being written.
fn streaming(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> bool {
    items(cx, tab)
        .iter()
        .any(|item| matches!(&item.kind, ItemKind::Assistant(a) if a.is_streaming()))
}

/// Whether a prompt is waiting to be taken.
fn queued(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> bool {
    items(cx, tab)
        .iter()
        .any(|item| matches!(&item.kind, ItemKind::User(u) if u.is_queued()))
}

/// Whether a lane's report has reached the coordinator's transcript.
fn reported(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> bool {
    items(cx, tab)
        .iter()
        .any(|item| matches!(item.kind, ItemKind::LaneReport(_)))
}

/// Wait for something the app's threads have to deliver; `None` means not yet.
fn wait_for<T>(
    cx: &mut HeadlessAppContext,
    what: &str,
    mut step: impl FnMut(&mut HeadlessAppContext) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = step(cx) {
            return value;
        }
        assert!(Instant::now() < deadline, "no {what} before the deadline");
        pump(cx, Duration::from_millis(20));
    }
}

/// Let the frames, the tasks and the engine's own threads run for a while.
fn pump(cx: &mut HeadlessAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One state, in both themes: the light picture and the dark one.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        // The theme change is one frame; the assets (the app icon) are decoded on
        // another thread and repaint when they are ready.
        pump(cx, Duration::from_millis(400));
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
        let image = cx.capture_screenshot(window)?;
        let path = dir.join(format!("{name}-{suffix}.png"));
        image.save(&path)?;
        println!(
            "[capture] {}x{} -> {}",
            image.width(),
            image.height(),
            path.display()
        );
    }
    Ok(())
}
