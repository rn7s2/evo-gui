//! §13, from the app's side: a quit with unsaved Settings edits asks first, and a
//! quit during a write waits for it.
//!
//! The Settings editors are the `settings` crate's and the tab that holds them is
//! the workspace's (`has_dirty_settings`, `has_saving_settings`). What is this
//! crate's half is the quit: that it is held *before* anything of it happens — no
//! `app.json`, not one swarm stopped — the question's two answers, that asking
//! again does not stack a second question, and that a write in flight is waited
//! for rather than answered.
//!
//! The drafts here are real: a Settings tab is opened and a key is typed into its
//! editor. `EVO_HOME` points at a temporary home holding one `init.lisp`, so the
//! four documents are read from nowhere near the person's own files and the only
//! thing a save can write over is the test's own copy of it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use evo_desktop::{
    asking_about_settings, begin_quit, is_quitting, AppLog, Shell, DISCARD_ID, KEEP_EDITING_ID,
    LOG_NAME, SAVING_HELD_LOG,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, ElementId, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use settings::ConfigEditor;
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use store::{ConfigFile, ConfigScope};
use workspace::{LaunchEnv, WorkspaceView};

/// What the test home's `init.lisp` holds: a line, so the editor has a file
/// behind its draft and Save has something to write.
const INIT: &str = "(in-package :evo-user)\n";

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

/// `EVO_HOME` is the process's, and every test here reads through it: they hold
/// this lock for their whole life, so that one test's home is never the home
/// another test's editor is in the middle of reading. Serialising four tests is
/// cheaper than pretending the variable is per-thread.
static HOME: Mutex<()> = Mutex::new(());

/// A home for one test's Settings tab: an `init.lisp` under `<temp>/…-<test>`,
/// made `EVO_HOME` for as long as the returned guard lives.
fn evo_home(test: &str) -> (PathBuf, MutexGuard<'static, ()>) {
    let held = HOME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = std::env::temp_dir().join(format!("evo-desktop-evo-home-{test}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("a home for the Settings tab");
    std::env::set_var("EVO_HOME", &home);
    // Resolved through the same code the page resolves it through, so the file
    // this test writes is the one Save would write.
    let init = store::config_file::path(&ConfigScope::Global, ConfigFile::Init);
    std::fs::write(&init, INIT).expect("a document to edit");
    (init, held)
}

/// A window on a temp app root, wired the way `run` wires it.
fn open(cx: &mut TestAppContext, root: &AppRoot) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            Shell::new(
                root.clone(),
                log,
                AppState {
                    binaries: Binaries::default(),
                    ..AppState::default()
                },
                ModelCache::default(),
            )
            .install(cx);
            let config = Arc::new(LaunchEnv {
                swarm_bin: cx.global::<Shell>().binaries.evo_swarm.clone(),
                agent_bin: cx.global::<Shell>().binaries.evo_agent.clone(),
                root,
                env: Vec::new(),
                env_remove: Vec::new(),
            });
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
    cx.update(|cx| {
        cx.global_mut::<Shell>().view = Some(view.downgrade());
    });
    (window, view)
}

/// Open the window's Settings tab, and wait for the four documents to have been
/// read: once this returns, the page is where its file is.
fn settings_tab(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    view: &Entity<WorkspaceView>,
) -> Entity<ConfigEditor> {
    let page = cx
        .update_window(window, |_, window, cx| {
            let tab = view.update(cx, |view, cx| view.open_settings_tab(window, cx));
            tab.read(cx)
                .settings_editor()
                .expect("the Settings tab's own editor")
                .clone()
        })
        .expect("the Settings tab");
    cx.run_until_parked();
    page
}

/// One keystroke in the page's editor: a draft, the way a person leaves one
/// behind. Nothing is saved before this, so the draft is all there is.
fn draft(cx: &mut TestAppContext, window: AnyWindowHandle, page: &Entity<ConfigEditor>) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        page.update(cx, |page, cx| page.focus_into(window, cx));
        window.input("x", cx);
        window.render_frame(cx);
    })
    .expect("typing in the Settings editor");
    cx.run_until_parked();
}

/// Start a write of the document being shown, without letting the test's
/// executor run: it is in flight until the test says otherwise.
fn start_a_write(cx: &mut TestAppContext, window: AnyWindowHandle, page: &Entity<ConfigEditor>) {
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| page.save_selected(window, cx));
    })
    .expect("the write");
}

/// How many frames a control is given to stop moving: the alert animates in.
const SETTLE_FRAMES: usize = 60;

/// Render frames until the element stops moving, and leave the last one rendered —
/// the same reason `tests/settings.rs` does it: a click placed from a frame the
/// layout has already left behind lands beside the control.
fn settle(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    let mut last = None;
    let mut agreed = 0;
    for _ in 0..SETTLE_FRAMES {
        let bounds = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.find(id.clone()).bounds()
            })
            .expect("the window");
        agreed = if last == Some(bounds) { agreed + 1 } else { 0 };
        if agreed >= 2 {
            return;
        }
        last = Some(bounds);
    }
    panic!("{id:?} is still moving after {SETTLE_FRAMES} frames");
}

fn active_dialog(cx: &mut TestAppContext, window: AnyWindowHandle) -> bool {
    cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
        .expect("the window")
}

fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
    settle(cx, window, id);
    cx.update_window(window, |_, window, cx| {
        window.click(id, cx);
        window.render_frame(cx);
    })
    .expect("the click");
}

/// What the window says in toasts (§13): a quit during a write is answered with
/// one rather than with a question.
fn notifications(cx: &mut TestAppContext, window: AnyWindowHandle) -> usize {
    cx.update_window(window, |_, window, cx| window.notifications(cx).len())
        .expect("the notifications")
}

fn writing(cx: &mut TestAppContext, view: &Entity<WorkspaceView>) -> bool {
    cx.update(|cx| view.read(cx).has_saving_settings(cx))
}

fn log_text(root: &AppRoot) -> String {
    std::fs::read_to_string(root.path().join(LOG_NAME)).unwrap_or_default()
}

/// Wait, bounded, for something the quit path reaches on another thread.
fn waited(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
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

#[gpui_kit::test]
fn a_quit_with_nothing_unsaved_starts_at_once(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let root = temp_root("quit-settings-clean");
    let (window, _view) = open(cx, &root);

    cx.update(begin_quit);

    assert!(
        !active_dialog(cx, window),
        "no question is asked: nothing is unsaved"
    );
    cx.update(|cx| {
        assert!(!asking_about_settings(cx));
        assert!(is_quitting(cx), "the quit runs as it always has");
    });
    waited(cx, "the quit to run", |_| {
        log_text(&root).contains("quitting: stopping every tab")
    });
    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn discard_and_quit_holds_the_quit_then_runs_it(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let (_init, _home) = evo_home("quit-settings-discard");
    let root = temp_root("quit-settings-discard");
    let (window, view) = open(cx, &root);
    let page = settings_tab(cx, window, &view);
    draft(cx, window, &page);

    cx.update(begin_quit);

    // Held: the question is up, and nothing of the quit has happened yet — the
    // order §9.8 asks for is that `app.json` and the swarms come after.
    assert!(
        active_dialog(cx, window),
        "the unsaved-Settings question is on screen"
    );
    cx.update(|cx| {
        assert!(asking_about_settings(cx), "the guard is asking");
        assert!(!is_quitting(cx), "the quit has not begun");
    });
    assert!(
        !log_text(&root).contains("quitting:"),
        "no swarm has been stopped:\n{}",
        log_text(&root)
    );
    assert!(!root.app_json().exists(), "and app.json was not written");

    // A second quit — ⌘Q over the question, or the × again — is answered by the
    // dialog that is already up rather than stacking another one. One click then
    // has to be the end of it: a stacked pair would leave the lower one on screen.
    cx.update(begin_quit);
    cx.update(begin_quit);
    assert!(active_dialog(cx, window), "still the one question");

    click(cx, window, DISCARD_ID);
    assert!(
        !active_dialog(cx, window),
        "one click answered the question, so only one of them was up"
    );
    cx.update(|cx| {
        assert!(
            !view.read(cx).has_dirty_settings(cx),
            "the drafts went with the answer"
        );
    });

    waited(cx, "the held-back quit to run", |_| {
        log_text(&root).contains("quitting: stopping every tab")
    });
    cx.update(|cx| {
        assert!(is_quitting(cx), "and the quit that was held back runs");
        assert!(!asking_about_settings(cx), "the question is done with");
    });
    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn keep_editing_drops_the_quit_and_leaves_the_session_alone(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let (_init, _home) = evo_home("quit-settings-keep");
    let root = temp_root("quit-settings-keep");
    let (window, view) = open(cx, &root);
    let page = settings_tab(cx, window, &view);
    draft(cx, window, &page);

    cx.update(begin_quit);
    assert!(active_dialog(cx, window), "the question is up");

    // Escape is the same answer as the button: the quit is dropped.
    cx.update_window(window, |_, window, cx| window.press("escape", cx))
        .expect("escape");
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("a frame");
    assert!(
        !active_dialog(cx, window),
        "the question is closed without quitting"
    );
    cx.update(|cx| {
        assert!(!asking_about_settings(cx));
        assert!(!is_quitting(cx), "nothing of the quit ran");
    });
    let text = log_text(&root);
    assert!(!text.contains("quitting:"), "no swarm was stopped:\n{text}");
    assert!(!root.app_json().exists(), "and app.json was not written");

    // The window is still there and the draft is still in it, so the next ⌘Q asks
    // again — and the button is Escape's answer too.
    cx.update(|cx| assert!(view.read(cx).has_dirty_settings(cx)));
    cx.update(begin_quit);
    assert!(active_dialog(cx, window), "the next quit asks again");
    click(cx, window, KEEP_EDITING_ID);
    assert!(
        !active_dialog(cx, window),
        "Keep Editing closes the question as Escape does"
    );
    cx.update(|cx| {
        assert!(!is_quitting(cx));
        assert!(
            view.read(cx).has_dirty_settings(cx),
            "the draft is still there"
        );
    });
    let _ = std::fs::remove_dir_all(root.path());
}

/// A write in flight is not a question (§13): the quit waits for it, says so, and
/// the person quits again when it is done — nothing is remembered for them.
#[gpui_kit::test]
fn a_quit_during_a_write_is_held_until_the_write_is_done(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let (init, _home) = evo_home("quit-settings-writing");
    let root = temp_root("quit-settings-writing");
    let (window, view) = open(cx, &root);
    let page = settings_tab(cx, window, &view);
    draft(cx, window, &page);

    // The file changes on disk under the editor, so the write this test starts is
    // refused and the draft stays behind: what the second half needs is a window
    // that is dirty again once the write is over.
    std::fs::write(
        &init,
        "(in-package :evo-user) ; rewritten under the editor\n",
    )
    .expect("the file changes under the editor");
    start_a_write(cx, window, &page);
    assert!(writing(cx, &view), "the write is in flight");

    cx.update(begin_quit);

    // Held, and with nothing to answer: no question, and no quit.
    assert!(
        !active_dialog(cx, window),
        "there is no question to put up: nothing can be answered about a write"
    );
    cx.update(|cx| {
        assert!(!asking_about_settings(cx));
        assert!(!is_quitting(cx), "the quit waits for the write");
    });
    assert_eq!(
        notifications(cx, window),
        1,
        "and the person is told why the quit did not happen"
    );
    waited(cx, "the log to say what happened", |_| {
        log_text(&root).contains(SAVING_HELD_LOG)
    });
    let text = log_text(&root);
    assert!(!text.contains("quitting:"), "no swarm was stopped:\n{text}");
    assert!(!root.app_json().exists(), "and app.json was not written");

    // Letting the test's executor run is what lands the write. It is refused —
    // the file changed under the editor — so the draft is still there, and the
    // person quitting again is a question this time, whose answer runs.
    waited(cx, "the write to land", |cx| {
        !cx.update(|cx| view.read(cx).has_saving_settings(cx))
    });
    cx.update(begin_quit);
    assert!(
        active_dialog(cx, window),
        "the write is done, so now there is something to ask about"
    );
    click(cx, window, DISCARD_ID);
    waited(cx, "the quit to run", |_| {
        log_text(&root).contains("quitting: stopping every tab")
    });
    cx.update(|cx| assert!(is_quitting(cx)));
    let _ = std::fs::remove_dir_all(root.path());
}

/// The question's "Discard and Quit" is checked against a write starting while it
/// is up (§13): nothing is discarded, nothing quits, and the write is waited for
/// like any other.
#[gpui_kit::test]
fn discard_does_not_run_the_quit_while_a_write_is_in_flight(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let (init, _home) = evo_home("quit-settings-discard-writing");
    let root = temp_root("quit-settings-discard-writing");
    let (window, view) = open(cx, &root);
    let page = settings_tab(cx, window, &view);
    draft(cx, window, &page);

    cx.update(begin_quit);
    assert!(active_dialog(cx, window), "the question is up");

    // The file changes on disk under the editor, so the write is refused and the
    // draft survives it — the draft is what the question below is about.
    std::fs::write(
        &init,
        "(in-package :evo-user) ; rewritten under the editor\n",
    )
    .expect("the file changes under the editor");

    // A write starts while the question is up. The question is modal, so the click
    // that would start one cannot be made — which is exactly why the answer
    // re-checks before it acts instead of trusting what it saw when it was asked.
    start_a_write(cx, window, &page);
    assert!(writing(cx, &view), "the write is in flight");

    click(cx, window, DISCARD_ID);

    cx.update(|cx| assert!(!is_quitting(cx), "the quit did not run"));
    assert!(!active_dialog(cx, window), "and the question is gone");
    assert!(
        writing(cx, &view),
        "the write is untouched, and still going"
    );
    assert_eq!(
        notifications(cx, window),
        1,
        "the person is told why the quit did not happen"
    );

    // When it is done the person quits again, and that answer runs.
    waited(cx, "the write to land", |cx| {
        !cx.update(|cx| view.read(cx).has_saving_settings(cx))
    });
    cx.update(begin_quit);
    assert!(active_dialog(cx, window), "the question comes back");
    click(cx, window, DISCARD_ID);
    waited(cx, "the quit to run", |_| {
        log_text(&root).contains("quitting: stopping every tab")
    });
    cx.update(|cx| assert!(is_quitting(cx)));
    let _ = std::fs::remove_dir_all(root.path());
}

/// A write in flight is waited for even when there is a draft to ask about as
/// well (§13): the question is never put up over a write, because its "Discard"
/// is not an answer that can be given while one is going.
///
/// The state a save cannot leave behind — a dirty editor and a write at the same
/// time — is two documents: a draft on `init.lisp`, and a write of `lore.sexp`,
/// the document the page is showing.
#[gpui_kit::test]
fn a_write_in_flight_wins_over_a_draft(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let (_init, _home) = evo_home("quit-settings-both");
    let root = temp_root("quit-settings-both");
    let (window, view) = open(cx, &root);
    let page = settings_tab(cx, window, &view);
    draft(cx, window, &page);

    // Onto another document, and a write of that one: `init.lisp` keeps its draft
    // (switching documents touches no draft), and `lore.sexp` is now being written.
    cx.update_window(window, |_, window, cx| {
        page.update(cx, |page, cx| page.select(ConfigFile::Lore, window, cx));
    })
    .expect("another document");
    draft(cx, window, &page);
    start_a_write(cx, window, &page);
    cx.update(|cx| {
        assert!(
            view.read(cx).has_dirty_settings(cx),
            "a draft is still here"
        );
        assert!(
            view.read(cx).has_saving_settings(cx),
            "and a write is going"
        );
    });

    cx.update(begin_quit);

    assert!(
        !active_dialog(cx, window),
        "the draft is not asked about while a write is in flight"
    );
    cx.update(|cx| {
        assert!(!asking_about_settings(cx));
        assert!(!is_quitting(cx), "the quit waits for the write");
    });
    assert_eq!(
        notifications(cx, window),
        1,
        "and the person is told why the quit did not happen"
    );
    let text = log_text(&root);
    assert!(!text.contains("quitting:"), "no swarm was stopped:\n{text}");
    assert!(!root.app_json().exists(), "and app.json was not written");
    let _ = std::fs::remove_dir_all(root.path());
}
