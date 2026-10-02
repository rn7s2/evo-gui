//! The completion popup, end to end, against a real `evo-agent` and a real
//! `evo-swarm`: a `/command` word and a `/eval` symbol, in both kinds of tab.
//!
//! ```sh
//! EVO_AGENT_BIN=/usr/local/bin/evo-agent \
//! EVO_SWARM_BIN=/usr/local/bin/evo-swarm \
//! EVO_AGENT_REPO=~/coding/evo-agent \
//! EVO_STUB_MESSAGES=~/coding/evo-agent/tests/stub-messages.py \
//!   cargo test -p evo-desktop --test completion_popup -- --nocapture
//! ```
//!
//! Everything is the real thing: the app's own tab, its engine, and the server the
//! launch starts, with the scripted model behind it. The word is *typed*, the way a
//! person types it, and what the test reads is what the window drew — the popup and
//! its rows.
//!
//! The regression this covers: the box holds its next question until the one it asked
//! is answered, and a tab asks its first one — about the empty caret its catalog
//! arrived on — before there is any server to ask. A question nobody answers is the
//! popup never coming back, in every tab, for the rest of its life.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, Entity, Point,
    TestAppContext, WindowBounds, WindowOptions,
};
use proofs::fixture::Fixture;
use session::LaunchPlan;
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContent, TabContentEvent, TabState, WorkspaceView};

/// The window these runs happen in: the app's own size, so the box is as wide as it
/// really is and the popup is measured inside it.
const WINDOW_SIZE: (f32, f32) = (1440., 900.);

/// How long anything the server has to answer gets.
const WAIT: Duration = Duration::from_secs(60);

/// How long the app's own loads (the catalog, the session index) are given before the
/// tab is started: they are threads of their own.
const LOADS: Duration = Duration::from_secs(3);

/// The words the popup is raised by, and what is on it: a slash command's own shape
/// for the first two, and a symbol of the image for the third — which only the
/// running `evo.eval` can list.
const WORDS: [&str; 3] = ["/", "/co", "/eval (evo:all-to"];

#[gpui_kit::test]
fn typing_a_word_completes_in_both_kinds_of_tab(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixture = Fixture::new("completion-popup");
    fixture.enter();
    let (window, view) = open(cx, &fixture.root, bins(&fixture));
    cx.update(evo_desktop::start_background_loads);
    pump(cx, LOADS);

    // One tab per program: a swarm is the coordinator's server, one `evo-agent` is
    // `Main`, and the popup is the box's either way (§7.2, §7.3).
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
            "[popup] a tab is running (swarm={swarm}) in {}",
            fixture.folder.display()
        );

        let mut previous = "";
        for word in WORDS {
            // Each word from an empty box: a popup is about the caret's word, and the
            // last word's text is not this one's.
            clear(cx, window, &tab, previous);
            previous = word;
            cx.update_window(window, |_, window, cx| {
                tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
                window.input(word, cx);
            })
            .expect("the window");

            let rows = wait_for(cx, &format!("the popup for {word:?}"), |cx| {
                popup_rows(cx, window).filter(|rows| !rows.is_empty())
            });
            println!("[popup] swarm={swarm} {word:?} -> {rows:?}");
            match word {
                // Every command the registry lists, in its own words.
                "/" => {
                    assert!(
                        rows.len() > 1,
                        "a bare slash raises the registry's commands: {rows:?}"
                    );
                    assert!(
                        rows.iter().all(|row| row.starts_with('/')),
                        "and every row is a command: {rows:?}"
                    );
                }
                // The word narrows them: `co` is what its candidates begin with — or
                // hold in order, like every `/lo` that finds `/reload`.
                "/co" => {
                    assert!(
                        rows.iter().all(|row| row.starts_with('/')),
                        "a command word offers commands: {rows:?}"
                    );
                    assert!(
                        rows.iter().any(|row| row.to_lowercase().starts_with("/co")),
                        "and offers what the word begins: {rows:?}"
                    );
                }
                // A symbol: no slash, and a name only the image's own list can have.
                _ => {
                    assert!(
                        rows.iter().all(|row| !row.starts_with('/')),
                        "inside `/eval` the rows are symbols, not commands: {rows:?}"
                    );
                    assert!(
                        rows.iter().any(|row| row.starts_with("evo:all-to")),
                        "and they are the names of the token's own image: {rows:?}"
                    );
                }
            }
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

/// The candidates the popup is drawing, in the order they are drawn — the *names*
/// alone, off the label each row carries (`{name} · {description}`, as a reader that
/// cannot see it hears it). `None` when there is no popup at all.
fn popup_rows(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<Vec<String>> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find("completion-popup")?;
        Some(
            (0..16)
                .map_while(|index| {
                    window
                        .try_find(format!("completion-row-{index}"))
                        .and_then(|row| row.label().map(str::to_owned))
                })
                .map(|row| row.split(" · ").next().unwrap_or_default().to_string())
                .collect(),
        )
    })
    .expect("the window")
}

/// Empty the box, and let the empty caret settle: what is typed next is the word the
/// popup is about.
///
/// The way a person does it: backspaces, from the end of the word just typed. The box
/// has no draft to set from outside the crate, and neither should it — the test is
/// about what the window draws from what is typed into it.
fn clear(cx: &mut TestAppContext, window: AnyWindowHandle, tab: &Entity<TabContent>, typed: &str) {
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        for _ in 0..typed.chars().count() + 4 {
            window.press("backspace", cx);
        }
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
                    ..LaunchPlan::default()
                },
            })
        })
    });
}

fn tab_state(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
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

/// Let the app's own threads work — the engine, the server, the loads — and the
/// frame be drawn again: the timer the box debounces its question on is on the
/// background executor, which only moves when the clock does.
fn pump(cx: &mut TestAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
}
