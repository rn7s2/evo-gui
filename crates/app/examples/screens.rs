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
//! Which states get captured depends on what the binaries can do. A build whose
//! `evo-swarm` cannot serve yet (`Unknown argument: --ready-file`) gives the
//! boot-failure screen and the empty tab; a build that can gives the live tab
//! page, the tool row, the queued prompt, an interrupt and the history. The run
//! prints what it captured and what it could not, and still exits 0 — a picture
//! of a failure screen is a picture.

use std::os::unix::fs::PermissionsExt as _;
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
    let mut via_agent: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--capture" => {
                i += 1;
                dir = args.get(i).map(PathBuf::from);
            }
            // In a build where `evo-swarm` cannot serve a tab yet, the tab's
            // server is the agent's own `serve` — the same server a swarm's
            // coordinator is — with the flags only a swarm takes dropped by a
            // one-line shim. Without it, the swarm its `app.json` names is the
            // one used, and a launch that cannot come up is a picture too.
            "--via-agent" => {
                i += 1;
                via_agent = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("screens: unexpected argument {other:?}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let Some(dir) = dir else {
        eprintln!("usage: screens --capture <dir> [--via-agent <evo-agent>]");
        std::process::exit(2);
    };
    match capture(&dir, via_agent) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("capture failed: {error}");
            std::process::exit(1);
        }
    }
}

fn capture(dir: &Path, via_agent: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let fixture = Fixture::new("screens");
    // The app's own reads (`sessions --json`) and every child it spawns: the stub
    // home, the scripted model's address, the agent the lanes would run.
    fixture.enter();

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx.allow_parking();

    let agent = fixture.bins.agent.clone();
    let swarm = fixture.bins.swarm.clone();
    let live = match &via_agent {
        Some(agent_bin) => shim(&fixture, agent_bin)?,
        None => swarm.clone(),
    };
    println!(
        "[capture] swarm {} / agent {}",
        live.display(),
        agent.display()
    );

    // --- the window the app opens, and its first frame -------------------------
    let bins = Binaries {
        evo_swarm: live.clone(),
        evo_agent: agent.clone(),
    };
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
            live_states(&mut cx, window, dir, &tab)?;
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
            shot(&mut cx, window, dir, "06-history")?;
            // A second window, whose swarm cannot run at all: the failure screen.
            let (failed_window, failed_view) = open(
                &mut cx,
                &fixture.root,
                Binaries {
                    evo_swarm: swarm,
                    evo_agent: agent,
                },
            )?;
            boot_failure(&mut cx, failed_window, &failed_view, dir, &fixture.folder)?;
        }
        TabState::Failed { message, .. } => {
            println!(
                "[capture] the tab did not come up ({}): {}",
                live.display(),
                message.as_deref().unwrap_or("no reason given")
            );
            shot(&mut cx, window, dir, "07-boot-failure")?;
            println!(
                "[capture] the live states need a server this build can drive; \
                 pass --via-agent <evo-agent> to capture them against the agent's serve"
            );
        }
        other => println!("[capture] the tab settled in {other:?}: nothing captured"),
    }

    let _ = std::fs::remove_dir_all(&fixture.dir);
    Ok(())
}

/// The tab page, as a person builds it: a tool call, its row opened, an answer
/// streaming, a prompt typed behind it, and the interrupt that ends it.
fn live_states(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    tab: &Entity<TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    // --- a tool call, and its row opened ---------------------------------------
    type_text(cx, window, tab, TOOL_PROMPT)?;
    let tool = wait_for(cx, "a tool row", |cx| tool_item(cx, tab));
    cx.update_window(window, |_, window, cx| {
        window.click(row("transcript-tool", &tool), cx)
    })?;
    pump(cx, Duration::from_millis(600));
    shot(cx, window, dir, "03-tool")?;

    // --- an answer that is still being written ---------------------------------
    type_text(cx, window, tab, SLOW_PROMPT)?;
    wait_for(cx, "the answer to start streaming", |cx| {
        items(cx, tab)
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::Assistant(a) if a.is_streaming()))
            .then_some(())
    });
    shot(cx, window, dir, "02-live")?;

    // --- a prompt typed while it works: queued, and cancellable ----------------
    type_text(cx, window, tab, QUEUED_PROMPT)?;
    wait_for(cx, "the queued prompt", |cx| {
        items(cx, tab)
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::User(u) if u.is_queued()))
            .then_some(())
    });
    shot(cx, window, dir, "04-queued")?;

    // --- the stop key, and what the server did with it -------------------------
    //
    // `Esc` interrupts the coordinator's turn, and a turn is interrupted at a
    // step's end: the picture is taken once nothing is streaming and nothing is
    // waiting to be taken, which is the state the key left behind — an item
    // saying a person stopped the run, an answer that ended where it was, or
    // both, depending on the server.
    focus(cx, window, tab)?;
    cx.update_window(window, |_, window, cx| window.press("escape", cx))?;
    wait_for(cx, "the stop to settle", |cx| {
        let items = items(cx, tab);
        let streaming = items
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::Assistant(a) if a.is_streaming()));
        let queued = items
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::User(u) if u.is_queued()));
        (!streaming && !queued).then_some(())
    });
    pump(cx, Duration::from_millis(300));
    shot(cx, window, dir, "05-after-stop")?;
    Ok(())
}

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
    shot(cx, window, dir, "07-boot-failure")?;
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
            |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
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

/// A one-line `evo-agent` in the swarm's place: the flags only a swarm takes are
/// dropped, everything else is handed on. Only used for `--via-agent`.
fn shim(fixture: &Fixture, agent: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = fixture.dir.join("tab-server.sh");
    // The fixture's paths are temp paths with no spaces in them, so the word
    // splitting below cannot break an argument in half.
    let script = format!(
        "#!/bin/sh\n\
         # A stand-in for an `evo-swarm` this build cannot serve with yet: the app's\n\
         # swarm flags are dropped and the agent's own `serve` is the tab's server.\n\
         agent={agent}\n\
         shift_args=''\n\
         skip=0\n\
         for arg in \"$@\"; do\n\
         \x20 if [ \"$skip\" = 1 ]; then skip=0; continue; fi\n\
         \x20 case \"$arg\" in\n\
         \x20   --evo|--workers|--lane-model|--lane-thinking) skip=1; continue ;;\n\
         \x20 esac\n\
         \x20 shift_args=\"$shift_args $arg\"\n\
         done\n\
         exec \"$agent\" $shift_args\n",
        agent = agent.display()
    );
    std::fs::write(&path, script)?;
    let mut perms = std::fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms)?;
    Ok(path)
}
