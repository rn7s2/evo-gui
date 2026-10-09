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
//! The scripted model is this example's own adapter, `examples/screens_model.py`:
//! the same test stub the proofs run, driven so that the few prompts a picture
//! needs are answered in words a reader would read — one natural prompt in the
//! transcript instead of a `CALL … {json}` line, prose instead of the stub's
//! `ok: …` and its `slow0 slow1 …` filler. The tool calls are still made by that
//! model and run by the real binaries. It is named through `EVO_STUB_MESSAGES`,
//! before the fixture exists, which is where the fixture reads it.
//!
//! Every state is the swarm's own: the integration build serves tabs, so the
//! pictures below are the launch page and one of its choosers, the pool coming up,
//! a lane at work, the report that lane sends when its job is done, a tool row
//! opened, a prompt queued behind a run, and the history a session leaves behind.

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
use session::{AgentKey, Item, ItemKind, LaneStatus, Status};
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

/// The prompt the README's picture is about: one natural delegation, which the
/// scripted model makes the coordinator hand to lane 1 — with this same sentence
/// as the task, so the tool row reads like the person's own words.
const DELEGATE_PROMPT: &str = "Review local image rendering and report back.";

/// The sources, shown by the tool the transcript already has: the scripted model
/// turns this into `bash {"command": "ls crates/transcript/src"}`, and the files
/// below are really in the fixture's folder, so the listing is true.
const SOURCES_PROMPT: &str = "Show the transcript source files.";

/// An answer that streams, so the picture is taken in the middle of it: the
/// scripted model streams a real explanation for this prompt.
const EXPLAIN_PROMPT: &str = "Explain how the image cache works.";

/// A prompt typed while the answer above is still being written.
const QUEUED_PROMPT: &str = "Then summarise the trade-offs.";

/// The transcript crate's own sources, by name: the ones the demo's `ls` shows
/// and the report links to. Laid into the fixture's folder before the launch, so
/// that both are about files that exist — and named one by one, so only these
/// five public sources of the crate can ever be copied, and nothing else from
/// this checkout. `images.rs` is the crate's newest file, beside `imgcheck.rs`
/// and the view it feeds.
const TRANSCRIPT_SOURCES: &[&str] = &[
    "images.rs",
    "imgcheck.rs",
    "lib.rs",
    "markdown.rs",
    "rows.rs",
];

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
    // The model these pictures run: this example's own adapter, which the fixture
    // starts where it would start the test stub. It has to be named before the
    // fixture exists — that is when the stub is started.
    std::env::set_var("EVO_STUB_MESSAGES", model_script());
    let fixture = Fixture::new("screens");
    // The app's own reads (`sessions --json`) and every child it spawns: the stub
    // home, the scripted model's address, the agent its lanes run.
    fixture.enter();
    // And the project folder is a real one for the pictures: the sources the demo
    // lists and links to are laid into it before anything runs.
    seed_sources(&fixture.folder)?;

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

    // --- one of those fields, open ---------------------------------------------
    //
    // The trigger says `provider · id`; the rows under it are where a registration's
    // own detail line is, and that line is what the catalog spells per model —
    // context window, modalities, and the levels that model takes, in its own order
    // and in full (`GET /catalog`'s `effort_levels`; the composer's drawer offers
    // the same model's own rungs). A height is given to the open menu below: the
    // field is one registration in the stub home, and the picture is about the row.
    click(&mut cx, window, "coordinator-model".into())?;
    pump(&mut cx, Duration::from_millis(600));
    shot(&mut cx, window, dir, "01b-model-menu")?;
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
    })?;
    pump(&mut cx, Duration::from_millis(200));

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
            // Open sessions are excluded from resume history. Close this fixture's
            // running tab through the app's shutdown path before showing its entry.
            let finished = cx.update(|cx| tab.read(cx).id());
            let tabs = view.clone();
            cx.update_window(window, |_, window, cx| {
                tabs.update(cx, |view, cx| {
                    view.add_tab(window, cx);
                    view.close_tab(finished, window, cx);
                });
            })?;
            wait_for(&mut cx, "the session tab to close", |cx| {
                cx.update(|cx| {
                    view.read(cx)
                        .tabs()
                        .iter()
                        .all(|tab| tab.read(cx).id() != finished)
                        .then_some(())
                })
            });
            cx.update(evo_desktop::start_background_loads);
            wait_for(&mut cx, "the completed session in history", |cx| {
                cx.update(|cx| {
                    (!view
                        .read(cx)
                        .selected_tab()
                        .read(cx)
                        .history_rows(cx)
                        .is_empty())
                    .then_some(())
                })
            });
            pump(&mut cx, Duration::from_millis(400));
            shot(&mut cx, window, dir, "07-history")?;
        }
        other => println!("[capture] the tab settled in {other:?}: no live states"),
    }

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
    let swarms = cx.update(|cx| view.read(cx).swarms(cx));
    println!("[capture] stopping {} tab(s)", swarms.len());
    for swarm in swarms {
        swarm.shutdown();
    }
    pump(cx, Duration::from_secs(2));
}

/// The tab page, as a person builds it: one lane given one review, the report it
/// sends when that is done, the sources behind a tool row, and an answer still
/// being written with a prompt typed behind it.
fn live_states(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    tab: &Entity<TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    // --- the pool, and one lane given work --------------------------------------
    //
    // One delegation, sent as a sentence: the scripted model is what turns it into
    // the coordinator's `delegate` call, so the transcript holds the person's own
    // words instead of a `CALL … {json}` line, and the task the lane is given is
    // that same sentence. The lane is waited for first — the tool hands work to a
    // lane that is up, and this one delegation is what the picture is about.
    wait_for(cx, "the lanes to come up", |cx| {
        (lanes(cx, tab) >= 1).then_some(())
    });
    println!("[capture] {} lane(s)", lanes(cx, tab));
    wait_for(cx, "lane 1 to be ready", |cx| {
        lane_idle(cx, tab, 1).then_some(())
    });
    type_text(cx, window, tab, DELEGATE_PROMPT)?;
    wait_for(cx, "lane 1 working", |cx| {
        lane_busy(cx, tab, 1).then_some(())
    });
    shot(cx, window, dir, "02-lanes-working")?;

    // --- the report the lane sends when that work is done ------------------------
    //
    // A lane reports by calling the `report` tool: that item is the coordinator's
    // copy of it, and it arrives as a `lane_report` (§4.1) — fields, not prose.
    // This report is the end of the job the picture above handed over, and it is
    // taken here, before the tool row and the streaming answer below: the card is
    // what a reader is meant to see, not the tail of a long scroll.
    wait_for(cx, "lane 1's report", |cx| reported(cx, tab).then_some(()));
    pump(cx, Duration::from_millis(800));
    shot(cx, window, dir, "06-report")?;

    // --- the sources, by the tool the transcript already has ---------------------
    //
    // The prompt is a sentence; the model turns it into the `bash` call. The row is
    // left open: its arguments and the listing it made are part of the picture
    // below, the one taken while an answer is still being written.
    wait_for(cx, "the coordinator to settle", |cx| {
        coordinator_idle(cx, tab).then_some(())
    });
    type_text(cx, window, tab, SOURCES_PROMPT)?;
    let tool = wait_for(cx, "the completed bash row", |cx| tool_item(cx, tab));
    click(cx, window, row("transcript-tool", &tool))?;
    pump(cx, Duration::from_millis(200));

    // --- an answer still being written, and a prompt typed behind it -------------
    //
    // The answer streams a word at a time for about six seconds, so the picture
    // lands in the middle of it; the second prompt is typed then and waits its turn
    // behind the run — the queued card this picture is about.
    wait_for(cx, "the coordinator to settle", |cx| {
        coordinator_idle(cx, tab).then_some(())
    });
    type_text(cx, window, tab, EXPLAIN_PROMPT)?;
    wait_for(cx, "the answer to start streaming", |cx| {
        streaming(cx, tab).then_some(())
    });
    type_text(cx, window, tab, QUEUED_PROMPT)?;
    wait_for(cx, "the queued prompt", |cx| queued(cx, tab).then_some(()));
    shot(cx, window, dir, "05-queued")?;
    Ok(())
}

/// The scripted model this capture runs: the adapter beside this file, which the
/// fixture starts where it would start the test stub. Everything the model *says*
/// is there; everything the app does with it is the app's own.
fn model_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/screens_model.py")
}

/// The checkout this example is built in: `crates/app` → `crates` → the root.
fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/app is under the checkout")
        .to_path_buf()
}

/// Lay the demo's sources into the fixture's folder, at the path a project keeps
/// them: `crates/transcript/src`, so the tool row's `ls` prints them and the
/// report's links name files that are really there. Only [`TRANSCRIPT_SOURCES`],
/// and a missing one is a loud failure rather than a picture that lies.
fn seed_sources(folder: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let from = checkout().join("crates/transcript/src");
    let into = folder.join("crates/transcript/src");
    std::fs::create_dir_all(&into)?;
    for name in TRANSCRIPT_SOURCES {
        let source = from.join(name);
        if !source.is_file() {
            return Err(format!(
                "screens: {} is missing from this checkout — the demo's `ls` would lie",
                source.display()
            )
            .into());
        }
        std::fs::copy(&source, into.join(name))?;
    }
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

/// The bash call just sent, not an earlier delegate row.
fn tool_item(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> Option<String> {
    items(cx, tab)
        .into_iter()
        .find(|item| {
            matches!(&item.kind, ItemKind::Tool(tool)
                if tool.name == "bash" && tool.result.is_some() && !tool.status.is_running())
        })
        .map(|item| item.id)
}

/// The element id a transcript row is drawn under.
fn row(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
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

/// Whether lane N is up and idle: what the coordinator's `delegate` hands work to.
///
/// A row says `idle` once that lane's own process has published its state, so this
/// is the lane being ready rather than merely listed by the swarm.
fn lane_idle(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>, n: u32) -> bool {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| {
                model
                    .lane_rows()
                    .iter()
                    .any(|lane| lane.n == n && lane.status == LaneStatus::Idle)
            })
            .unwrap_or(false)
    })
}

/// Whether the coordinator is idle: not running a turn, and not held by its lanes.
///
/// What a prompt needs to be sent rather than queued behind a run — a queued card
/// is not what the report picture is about.
fn coordinator_idle(cx: &mut HeadlessAppContext, tab: &Entity<TabContent>) -> bool {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.activity() == Status::Idle)
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
        cx.background_executor
            .advance_clock(Duration::from_millis(20));
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
