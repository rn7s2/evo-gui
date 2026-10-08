//! A slash command's output is in the transcript, in both kinds of tab.
//!
//! The regression this covers: a command's lines are published as session `notice`
//! items with `durable: false` — the server does not journal them — and the transcript
//! dropped every notice the server does not keep, so `/lore`, `/model`, `/tree` and
//! every extension command answered into nothing. The reader now sees them, with the
//! severity they were said at.
//!
//! ```sh
//! EVO_AGENT_BIN=~/coding/evo-agent/build/evo-agent \
//! EVO_SWARM_BIN=~/coding/evo-agent/build/evo-swarm \
//! EVO_AGENT_REPO=~/coding/evo-agent \
//! EVO_STUB_MESSAGES=~/coding/evo-agent/tests/stub-messages.py \
//!   cargo test -p evo-desktop --test command_output -- --nocapture
//! ```
//!
//! Everything is the real thing: the app's own tab, its engine, and the server the
//! launch starts, with the scripted model behind it. The line is *typed*, the way a
//! person types one — popup and all — and what the test reads is the item the
//! transcript holds and the row the window drew for it.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, ElementId, Entity,
    Point, TestAppContext, WindowBounds, WindowOptions,
};
use proofs::fixture::Fixture;
use session::{ItemKind, LaunchPlan, Notice, NoticeSeverity, NoticeSource};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContent, TabContentEvent, TabState, WorkspaceView};

/// The window these runs happen in: the app's own size, so the transcript is as wide as
/// it really is and the row is measured inside it.
const WINDOW_SIZE: (f32, f32) = (1440., 900.);

/// How long anything the server has to answer gets.
const WAIT: Duration = Duration::from_secs(90);

/// How long the app's own loads (the catalog, the session index) are given before the
/// tab is started: they are threads of their own.
const LOADS: Duration = Duration::from_secs(3);

/// A command and the line it must put in the transcript, at the severity it is said
/// with. Two are the core's own (`/lore` with nothing set, `/model`'s list); two are an
/// extension's, so the warn and error inks are exercised through a real server too.
const SAID: [(&str, &str, NoticeSeverity); 4] = [
    (
        "/lore",
        "no lore — /lore <text> adds durable guidance",
        NoticeSeverity::Info,
    ),
    ("/model", "stub  stub-a", NoticeSeverity::Info),
    (
        "/note-warn",
        "a warning from an extension",
        NoticeSeverity::Warn,
    ),
    (
        "/note-error",
        "an error from an extension",
        NoticeSeverity::Error,
    ),
];

#[gpui_kit::test]
fn a_commands_output_lands_in_the_transcript(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new("command-output");
    // The stub home gains two commands an extension answers, so the warn and error
    // severities come off a real serve (the same `host-notice` a command's own output
    // takes) and not off a hand-built item.
    let init = fixture.home.join(".evo").join("init.lisp");
    let mut lisp = std::fs::read_to_string(&init).expect("the stub home's init.lisp");
    lisp.push_str(
        "(evo:register-command \"note-warn\"\n\
           (lambda (ctx)\n\
             (evo.command:host-notice (getf ctx :host) \"a warning from an extension\"\n\
                                      :severity :warn)\n\
             nil))\n\
         (evo:register-command \"note-error\"\n\
           (lambda (ctx)\n\
             (evo.command:host-notice (getf ctx :host) \"an error from an extension\"\n\
                                      :severity :error)\n\
             nil))\n",
    );
    std::fs::write(&init, lisp).expect("the stub home's init.lisp, with the two commands");
    fixture.enter();
    let (window, view) = open(cx, &fixture.root, bins(&fixture));
    cx.update(evo_desktop::start_background_loads);
    pump(cx, LOADS);

    // One tab per program: a swarm is the coordinator's server, one `evo-agent` is
    // `Main`, and the command's line is in the transcript either way (§7.2, §7.3).
    for swarm in [false, true] {
        let tab = match swarm {
            false => cx.update(|cx| view.read(cx).selected_tab().clone()),
            true => cx
                .update_window(window, |_, window, cx| {
                    view.update(cx, |view, cx| view.open_empty_tab(window, cx))
                })
                .expect("the window"),
        };
        launch(cx, &tab, &fixture.folder, swarm);
        wait_for(cx, "the tab to run", |cx| {
            matches!(tab_state(cx, &tab), TabState::Running { .. }).then_some(())
        });
        println!(
            "[command] a tab is running (swarm={swarm}) in {}",
            fixture.folder.display()
        );

        for (command, needle, severity) in SAID {
            say(cx, window, &tab, command);
            // The first `Enter` may take a completion row for the word; the second
            // sends the line. Both are what a person does.
            enter(cx, window, &tab);
            let (id, said) = wait_for(cx, &format!("the line for {command}"), |cx| {
                notice(cx, &tab, needle)
            });
            assert_eq!(
                said.severity, severity,
                "{command} said its line at the right strength: {said:?}"
            );
            assert_eq!(
                said.source,
                NoticeSource::Command,
                "{command}'s line is the command's own"
            );
            let drawn = drawn_row(cx, window, &id);
            println!("[command] swarm={swarm} {command} -> {id}: {drawn:?}");
            let drawn = drawn.expect("the line's row");
            assert!(
                drawn.starts_with("command · ") && drawn.contains(needle),
                "{command}'s line is drawn, as the command said it: {drawn:?}"
            );
        }

        // The server this tab started, stopped: the next kind starts its own.
        for swarm in cx.update(|cx| view.read(cx).swarms(cx)) {
            swarm.shutdown();
        }
        pump(cx, Duration::from_secs(2));
    }
}

/// A window with the app's own shell on it, offscreen, over a stub home the fixture
/// made (`Fixture::enter`, never the real one).
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    bins: Binaries,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let root = root.clone();
    cx.update(gpui_kit::init);
    cx.update(move |cx| {
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
                let _appearance = evo_desktop::follow_appearance(cx, window);
                cx.new(|cx| WorkspaceView::with_config(config, window, cx))
            },
        )
        .expect("the window");
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        (window, view)
    })
}

fn bins(fixture: &Fixture) -> Binaries {
    Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    }
}

/// Type the words and press `Enter` twice: the popup the `/` raises may take the first
/// one, and the second sends the line.
fn say(cx: &mut TestAppContext, window: AnyWindowHandle, tab: &Entity<TabContent>, text: &str) {
    let tab = tab.clone();
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        window.input(text, cx);
    })
    .expect("the window");
    pump(cx, Duration::from_millis(300));
    enter(cx, window, &tab);
}

/// `Enter` in the box: the same keystroke that sends, and the one that does nothing
/// when there is nothing to send.
fn enter(cx: &mut TestAppContext, window: AnyWindowHandle, tab: &Entity<TabContent>) {
    let tab = tab.clone();
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        window.press("enter", cx);
    })
    .expect("the window");
    pump(cx, Duration::from_millis(200));
}

fn launch(cx: &mut TestAppContext, tab: &Entity<TabContent>, folder: &Path, swarm: bool) {
    let folder = folder.to_path_buf();
    let tab = tab.clone();
    cx.update(move |cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: LaunchPlan {
                    swarm,
                    // One lane: the tab is the thing under test, and a pool of them is
                    // only a longer boot.
                    workers: Some(1),
                    ..LaunchPlan::default()
                },
            })
        })
    });
}

fn tab_state(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
}

/// The notice item the transcript holds whose text carries `needle`, with the words it
/// was said with.
fn notice(
    cx: &mut TestAppContext,
    tab: &Entity<TabContent>,
    needle: &str,
) -> Option<(String, Notice)> {
    let view = cx.update(|cx| tab.read(cx).transcript().cloned())?;
    let items = cx.update(|cx| view.read(cx).items(cx).to_vec());
    items.iter().find_map(|item| match &item.kind {
        ItemKind::Notice(notice) if notice.text.contains(needle) => {
            Some((item.id.clone(), notice.clone()))
        }
        _ => None,
    })
}

/// The label the transcript drew for the row with `id` — `None` until the list has
/// built that row.
fn drawn_row(cx: &mut TestAppContext, window: AnyWindowHandle, id: &str) -> Option<String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find(row("transcript-notice", id))
            .and_then(|row| row.label().map(str::to_owned))
    })
    .expect("the window")
}

/// The element id a transcript row is drawn under.
fn row(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
}

fn wait_for<T>(
    cx: &mut TestAppContext,
    what: &str,
    mut step: impl FnMut(&mut TestAppContext) -> Option<T>,
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

/// Let the app's own threads work — the engine, the server, the loads — and the frame
/// be drawn again: the timers the box debounces on are on the background executor,
/// which only moves when the clock does.
fn pump(cx: &mut TestAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
}
