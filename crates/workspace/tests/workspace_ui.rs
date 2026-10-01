//! UI tests for the window chrome: the tab strip and the empty tab (§7.1, §7.2).
//!
//! Each test opens the real window through the production entry point
//! ([`workspace::window_options`] plus [`workspace::WorkspaceView`]) in a headless
//! GPUI test window and drives it the way a user would.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    base::Root, px, size, App, AppContext, Bounds, ElementId, Entity, InputEvent as _, KeyBinding,
    KeyUpEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point,
    TestAppContext, WindowBounds, WindowHandle, WindowOptions,
};
use session::LaunchPlan;
use workspace::{
    HoldToQuit, Launch, LaunchEnv, QuitHeld, TabContentEvent, TabState, WorkspaceView, QUIT_HOLD,
};

/// Opens the app's window with one empty tab, at a deterministic size.
fn open_workspace(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<WorkspaceView>) {
    open_window_with(cx, WorkspaceView::new)
}

/// The same window, built by `build` — for the one test that needs a window whose
/// tabs cannot start a process.
fn open_window_with(
    cx: &mut TestAppContext,
    build: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::Context<WorkspaceView>) -> WorkspaceView
        + 'static,
) -> (WindowHandle<Root>, Entity<WorkspaceView>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1280.), px(800.)),
        };
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| build(window, cx)),
        )
        .expect("open the workspace window");
        (window.downcast::<Root>().expect("base Root"), view)
    })
}

/// The tab's label element, which is also its click target (§7.1).
fn tab_label(id: u64) -> ElementId {
    ElementId::NamedInteger("tab-label".into(), id)
}

/// The tab itself, whose box is where its pixels are (§7.1).
fn tab_box(id: u64) -> ElementId {
    ElementId::NamedInteger("tab".into(), id)
}

/// The tab's close button (§7.1).
fn tab_close(id: u64) -> ElementId {
    ElementId::NamedInteger("tab-close".into(), id)
}

fn tab_id(view: &Entity<WorkspaceView>, index: usize, cx: &App) -> u64 {
    view.read(cx).tabs()[index].read(cx).id().get()
}

/// Stop a tab's engine, and wait for its thread to end.
///
/// A tab with a process behind it has a thread of its own, and that thread wakes
/// the view when it has something to say — a wake from another thread is what
/// gpui's test scheduler calls non-deterministic, and it fires the instant the
/// wake lands after the test's last frame. Waiting the engine out before the
/// frames end is what makes a test that launches a tab (a binary that is not
/// there reaches its folder without a process) deterministic.
///
/// The handle is shared — the page's own rows hold one — so this waits on what
/// the tab handed back rather than joining it: `is_running` is false the moment
/// the engine's thread is over, which is the join a test needs.
macro_rules! retire_engine {
    ($tab:expr, $cx:expr) => {{
        let engine = $tab.update($cx, |tab, cx| tab.take_engine(cx));
        if let Some(engine) = engine {
            engine.shutdown();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while engine.is_running() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(!engine.is_running(), "the engine's thread never ended");
        }
    }};
}

/// Another tab, asked for directly — what a resume, a capture or a test wants.
///
/// Not what the `+` or ⌘T does: a strip keeps at most one empty tab, so those
/// show the one already there instead of adding a second (§7.1).
fn open_another_tab(window: &mut gpui_kit::Window, view: &Entity<WorkspaceView>, cx: &mut App) {
    view.update(cx, |view, cx| {
        view.open_empty_tab(window, cx);
    });
}

/// One window, one tab, nothing chosen: what the app opens with (§14.6).
#[gpui_kit::test]
fn the_app_opens_with_one_empty_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1);
        assert_eq!(view.selected_index(), 0);

        let tab = view.selected_tab().read(cx);
        assert_eq!(tab.state(), &TabState::Empty);
        assert_eq!(tab.title().as_ref(), "New Swarm");
        assert_eq!(tab.folder(), None, "an empty tab has no folder yet");
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("empty-tab").visible());
        assert!(
            window.find("select-folder").visible(),
            "the folder button spans the choosers"
        );
        assert!(window.find("history").visible());
        assert!(window.find("tab-add").visible(), "the + is always present");
    })
    .unwrap();
}

/// A tab with no catalog yet has no model to name and says so in evo's own words —
/// nothing is called `Default` (§7.2) — and the count is already the one the launch
/// passes.
#[gpui_kit::test]
fn the_empty_tab_has_no_default_to_fall_back_on(cx: &mut TestAppContext) {
    let (_handle, view) = open_workspace(cx);

    cx.update(|cx| {
        let tab = view.read(cx).selected_tab().read(cx);
        assert_eq!(
            tab.coordinator_model(cx).as_ref(),
            "No models are registered"
        );
        assert_eq!(tab.lanes_model(cx).as_ref(), "No models are registered");
        assert_eq!(tab.workers(cx).as_ref(), "6");
    });
}

/// §7.1: there is one New Swarm tab at a time. A second `+` shows the empty tab
/// that is already there — selected, and with the keyboard, the same as clicking
/// it — and a `+` with no empty tab on the strip opens one.
#[gpui_kit::test]
fn a_second_add_shows_the_empty_tab_instead_of_making_another(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Twice: the window opened with one empty tab, and that is still the one
        // tab the strip has.
        window.click("tab-add", cx);
        window.click("tab-add", cx);
        assert_eq!(view.read(cx).tabs().len(), 1);
        assert_eq!(view.read(cx).selected_index(), 0);
        assert_eq!(
            view.read(cx).selected_tab().read(cx).state(),
            &TabState::Empty
        );
    })
    .unwrap();
}

/// The same rule from the other side: a tab that has started something is not
/// empty, so the `+` opens a New Swarm tab for it to sit beside (§7.1).
#[gpui_kit::test]
fn the_add_button_opens_a_tab_when_none_is_empty(cx: &mut TestAppContext) {
    let home = std::env::temp_dir().join(format!("workspace-ui-one-empty-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("a home to work in");
    let root = home.clone();

    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                // A tab reaches its folder without a process behind it: this test
                // is about the tab set, and a swarm would only add noise.
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(root),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let folder = home.join("a-folder-of-its-own");
        std::fs::create_dir_all(&folder).expect("a folder to work in");
        let started = view.read(cx).selected_tab().clone();
        started.update(cx, |tab, cx| {
            tab.launch(
                Launch::New {
                    folder,
                    plan: LaunchPlan::default(),
                },
                window,
                cx,
            )
        });
        // The launched tab runs on a thread of its own; it is stopped and joined
        // here so the strip is what the test's frames see (see `retire_engine!`).
        retire_engine!(started, cx);
        window.render_frame(cx);

        // The shown tab has started something: `+` opens a New Swarm tab.
        window.click("tab-add", cx);
        assert_eq!(view.read(cx).tabs().len(), 2);
        assert_eq!(view.read(cx).selected_index(), 1, "the new tab is selected");
        assert_eq!(
            view.read(cx).selected_tab().read(cx).state(),
            &TabState::Empty
        );
        assert_ne!(
            view.read(cx).tabs()[0].read(cx).state(),
            &TabState::Empty,
            "the tab that started something keeps it"
        );

        // And now there is one: a second `+` shows it rather than making another.
        window.click("tab-add", cx);
        assert_eq!(view.read(cx).tabs().len(), 2);
        assert_eq!(view.read(cx).selected_index(), 1);
    })
    .unwrap();
}

/// §7.1: the `+` is the next thing after the last tab — the design's `.tab-row`
/// is one line of tabs with the `.tab-add` 8px along — and not the strip's own
/// right-hand end, which would leave the room the tabs did not take empty.
#[gpui_kit::test]
fn the_add_button_follows_the_last_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        window.render_frame(cx);

        let first = window.find(tab_box(tab_id(&view, 0, cx))).bounds();
        let last = window.find(tab_box(tab_id(&view, 1, cx))).bounds();
        let add = window.find("tab-add").bounds();

        assert_eq!(
            first.size.width,
            px(store::design::TAB_BASIS),
            "a tab is its basis wide: {first:?}"
        );
        // The row starts a corner's room before the traffic area ends, so the first
        // tab stands on the design's own x — `TRAFFIC_WIDTH + TAB_ROW_PAD.0` —
        // which is what leaves its outward corner room outside the clip edge.
        if cfg!(target_os = "macos") {
            assert_eq!(
                first.left(),
                px(store::design::TRAFFIC_WIDTH + store::design::TAB_ROW_PAD.0),
                "the first tab is where the design puts it: {first:?}"
            );
        }
        assert_eq!(last.left(), first.right(), "the tabs are side by side");
        assert_eq!(
            add.left() - last.right(),
            px(store::design::ADD_GAP),
            "the + is 8px after the last tab: tab ends at {:?}, + begins at {:?}",
            last.right(),
            add.left()
        );
        assert!(
            add.right() < window.bounds().right() - px(store::design::TAB_BASIS),
            "and nowhere near the strip's right edge: + {:?} in a {:?} window",
            add,
            window.bounds()
        );
    })
    .unwrap();
}

/// Clicking a tab selects it (§7.1).
#[gpui_kit::test]
fn clicking_a_tab_selects_it(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let first = cx.update(|cx| tab_id(&view, 0, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        assert_eq!(view.read(cx).selected_index(), 1);
        window.click(tab_label(first), cx);
    })
    .unwrap();

    cx.update(|cx| {
        assert_eq!(view.read(cx).selected_index(), 0);
    });
}

/// Closing a tab removes it and leaves the neighbour selected (§7.1).
#[gpui_kit::test]
fn closing_a_tab_selects_its_neighbour(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let first = cx.update(|cx| tab_id(&view, 0, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        // Only the tab being shown carries a close button, so the first tab is
        // selected before it is closed — which is what a user does.
        window.click(tab_label(first), cx);
        window.click(tab_close(first), cx);
    })
    .unwrap();

    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1, "one of the two tabs is gone");
        assert_eq!(view.selected_index(), 0);
    });
}

/// The window always keeps at least one tab: closing the last one leaves a
/// fresh empty tab rather than an empty window (§7.2).
#[gpui_kit::test]
fn closing_the_last_tab_leaves_a_fresh_empty_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let first = cx.update(|cx| tab_id(&view, 0, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(tab_close(first), cx);
    })
    .unwrap();

    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1);
        assert_eq!(view.selected_index(), 0);
        let tab = view.selected_tab().read(cx);
        assert_eq!(tab.state(), &TabState::Empty);
        assert_ne!(
            tab.id().get(),
            first,
            "it is a fresh tab, not the closed one"
        );
    });
}

/// Only the tab being shown carries a close button: an × on every tab is noise,
/// and an invisible one would still be clickable (§7.1).
#[gpui_kit::test]
fn a_tab_shows_its_close_button_when_the_pointer_is_on_it(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let first = cx.update(|cx| tab_id(&view, 0, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        let second = tab_id(&view, 1, cx);
        window.render_frame(cx);

        // Every tab carries its `×`, and only the tab being shown shows it while
        // the pointer is elsewhere: a hidden one is out of sight, not absent.
        assert!(
            window.find(tab_close(second)).visible(),
            "the tab being shown shows its close button"
        );
        assert!(
            !window.find(tab_close(first)).visible(),
            "the tab that is not shown keeps its close button out of sight"
        );

        // Hovering the other tab brings its own `×` out — the pointer never has
        // to find a 16 px box it cannot see.
        let label = window.find(tab_label(first)).bounds();
        window.simulate_mouse_move(label.center(), cx);
        window.render_frame(cx);
        assert!(
            window.find(tab_close(first)).visible(),
            "hovering a tab shows its close button"
        );
        assert!(
            window.find(tab_close(second)).visible(),
            "the tab being shown keeps its own out, wherever the pointer is"
        );

        // Selecting the first tab: now *it* is the shown one with its `×` out,
        // and the second is back to showing nothing until it is hovered.
        window.click(tab_label(first), cx);
        window.render_frame(cx);
        assert!(window.find(tab_close(first)).visible());
        assert!(
            !window.find(tab_close(second)).visible(),
            "a tab that is neither shown nor hovered shows no close button"
        );
    })
    .unwrap();
}

/// ⌘T and ⌘W: the window's own add and close, which the app's key bindings call
/// (§7.1). They are the same paths the `+` button and the tab's `×` take.
///
/// The window is given a swarm binary that is not there, so a tab reaches its
/// folder without a process behind it — what these tests need of a tab is that it
/// has stopped being empty.
#[gpui_kit::test]
fn the_keyboard_shortcuts_add_and_close_tabs(cx: &mut TestAppContext) {
    let home = std::env::temp_dir().join(format!("workspace-ui-keys-{}", std::process::id()));
    let folder = home.join("a-folder-of-its-own");
    std::fs::create_dir_all(&folder).expect("a folder to work in");
    let root = home.clone();

    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(root),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });

    // ⌘T on a strip whose one tab is empty: the tab is shown, not doubled (§7.1).
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.add_tab(window, cx);
        });
        assert_eq!(view.read(cx).tabs().len(), 1, "one New Swarm tab, not two");
    })
    .unwrap();

    // A tab that has started something is not empty, so ⌘T opens a new tab.
    cx.update_window(handle.into(), |_, window, cx| {
        let started = view.read(cx).selected_tab().clone();
        started.update(cx, |tab, cx| {
            tab.launch(
                Launch::New {
                    folder: folder.clone(),
                    plan: LaunchPlan::default(),
                },
                window,
                cx,
            )
        });
        retire_engine!(started, cx);
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.add_tab(window, cx);
        });
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 2);
        assert_eq!(view.selected_index(), 1, "the new tab is selected");
    });

    // ⌘W closes the tab being shown and the tab that was there takes its place.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1, "the shown tab was closed");
        assert_eq!(view.selected_index(), 0);
    });

    // ⌘W on the last tab leaves a fresh empty one: the window is never empty.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1);
        assert_eq!(view.selected_tab().read(cx).state(), &TabState::Empty);
    });
}

/// §9.8: what the window hands the app to persist — every tab in strip order,
/// with the folder and the session each one is running, and which one is shown.
#[gpui_kit::test]
fn the_window_lists_its_tabs_for_persistence(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
    })
    .unwrap();

    cx.update(|cx| {
        let view = view.read(cx);
        let records = view.tab_records(cx);
        assert_eq!(records.len(), 2, "one record per tab, in strip order");
        assert_eq!(records[0].window_id, view.tabs()[0].read(cx).id());
        assert_eq!(records[1].window_id, view.tabs()[1].read(cx).id());
        assert!(
            records.iter().all(|record| record.folder.is_none()
                && record.session.is_none()
                && record.store_id.is_none()),
            "a tab that never started a swarm has no folder, session or tab directory"
        );
        assert_eq!(
            view.selected_index(),
            1,
            "the selected index indexes the records"
        );
    });
}

/// §7.1: the strip answers the keyboard the way a browser's does — ⌘1…⌘8 is the
/// tab with that number, ⌘9 is the last one, and ⌃⇥ / ⌃⇧⇥ (and the ⌘⇧] / ⌘⇧[
/// pair) walk it, wrapping.
#[gpui_kit::test]
fn the_strip_answers_the_keyboard(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Four tabs, so ⌘9 has somewhere to go and ⌘8 has nowhere — asked for
        // one at a time: `+` keeps a strip at one empty tab (§7.1).
        for _ in 0..3 {
            open_another_tab(window, &view, cx);
        }
        assert_eq!(view.read(cx).tabs().len(), 4);
        assert_eq!(view.read(cx).selected_index(), 3);

        window.press("cmd-1", cx);
        assert_eq!(view.read(cx).selected_index(), 0, "⌘1 is the first tab");
        window.press("cmd-2", cx);
        assert_eq!(view.read(cx).selected_index(), 1);
        window.press("cmd-4", cx);
        assert_eq!(view.read(cx).selected_index(), 3);

        // A number with no tab behind it does nothing: ⌘8 is not "the last tab",
        // which is what ⌘9 is for.
        window.press("cmd-2", cx);
        window.press("cmd-8", cx);
        assert_eq!(view.read(cx).selected_index(), 1, "there is no eighth tab");

        window.press("cmd-9", cx);
        assert_eq!(view.read(cx).selected_index(), 3, "⌘9 is the last tab");

        window.press("cmd-1", cx);
        window.press("ctrl-tab", cx);
        assert_eq!(view.read(cx).selected_index(), 1, "⌃⇥ is the next tab");
        window.press("ctrl-tab", cx);
        window.press("ctrl-tab", cx);
        assert_eq!(view.read(cx).selected_index(), 3);
        window.press("ctrl-tab", cx);
        assert_eq!(
            view.read(cx).selected_index(),
            0,
            "past the last tab is the first"
        );

        window.press("ctrl-shift-tab", cx);
        assert_eq!(
            view.read(cx).selected_index(),
            3,
            "and back past the first is the last"
        );
        window.press("cmd-shift-[", cx);
        assert_eq!(view.read(cx).selected_index(), 2);
        window.press("cmd-shift-]", cx);
        assert_eq!(view.read(cx).selected_index(), 3);
    })
    .unwrap();
}

/// §7.1: the Window menu's tab items are the workspace's actions, and macOS
/// draws a menu item's key equivalent from the **app's** keymap, read once, when
/// the menu bar is built — which happens at install, before any window exists.
///
/// So the tab keys are bound app-wide, with no key context: this drives the same
/// lookup the platform does (the bindings for one action, with the keystroke
/// each carries) and asserts that every one of them is unscoped and in the app's
/// keymap before a window is opened.
#[gpui_kit::test]
fn the_tab_keys_are_bound_app_wide_so_the_menu_can_show_them(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // The state the menu bar is built in: no window yet, only the app.
    cx.update(workspace::bind_tab_keys);

    /// The keystrokes the app's keymap holds for one action, and whether each
    /// binding is scoped to a key context.
    fn bindings(cx: &mut TestAppContext, action: &dyn gpui_kit::Action) -> Vec<(String, bool)> {
        let keymap = cx.update(|cx| cx.key_bindings());
        let keymap = keymap.borrow();
        keymap
            .bindings_for_action(action)
            .map(|binding| {
                let keys = binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.unparse())
                    .collect::<Vec<_>>()
                    .join(" ");
                (keys, binding.predicate().is_some())
            })
            .collect()
    }

    let next = bindings(cx, &workspace::SelectNextTab);
    assert_eq!(
        next,
        [
            ("ctrl-tab".to_string(), false),
            ("cmd-shift-]".to_string(), false)
        ],
        "the menu shows the first of these, and both have to be in the app's keymap"
    );
    assert_eq!(
        bindings(cx, &workspace::SelectPreviousTab),
        [
            ("ctrl-shift-tab".to_string(), false),
            ("cmd-shift-[".to_string(), false)
        ]
    );
    assert_eq!(
        bindings(cx, &workspace::SelectLastTab),
        [("cmd-9".to_string(), false)]
    );
    // ⌘1…⌘8, one action per number.
    for index in 0..8 {
        assert_eq!(
            bindings(cx, &workspace::SelectTab(index)),
            [(format!("cmd-{}", index + 1), false)],
            "⌘{} is the tab with that number",
            index + 1
        );
    }

    // Binding them again — what opening a window does — does not double them up.
    cx.update(workspace::bind_tab_keys);
    assert_eq!(bindings(cx, &workspace::SelectNextTab).len(), 2);
}

/// §7.1: a middle click closes the tab it lands on — what the `×` does, without
/// having to aim at it.
#[gpui_kit::test]
fn a_middle_click_closes_a_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
    })
    .unwrap();
    let middle = cx.update(|cx| tab_id(&view, 1, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let at = window.find(tab_label(middle)).bounds().center();
        // GPUI's test helpers click with the left button; a browser's middle
        // click is the same two events with a different button, so they are
        // dispatched directly — at the middle of the tab's own label.
        window.dispatch_event(
            MouseMoveEvent {
                position: at,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        for event in [0, 1] {
            let button = MouseButton::Middle;
            let input = if event == 0 {
                MouseDownEvent {
                    button,
                    position: at,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input()
            } else {
                MouseUpEvent {
                    button,
                    position: at,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input()
            };
            window.dispatch_event(input, cx);
        }
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1, "the middle click closed one tab");
        assert!(
            view.tabs()
                .iter()
                .all(|tab| tab.read(cx).id().get() != middle),
            "and it was the one the pointer was on"
        );
    });
}

/// §7.1: fourteen tabs do not fit in a window. An overflowing strip scrolls, the
/// `+` stays at the strip's right edge rather than leaving the window, and the tab
/// being shown is the one on screen, whichever end of the strip it is at.
#[gpui_kit::test]
fn an_overflowing_strip_keeps_the_add_button_and_shows_the_selected_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            for _ in 1..14 {
                view.open_empty_tab(window, cx);
            }
        });
        window.render_frame(cx);

        let right = window.bounds().right();
        assert_eq!(
            view.read(cx).selected_index(),
            13,
            "the newest tab is shown"
        );
        // The `+` is a tab strip's most important button: it does not leave the
        // window when the tabs run out of room.
        let add = window.find("tab-add");
        assert!(add.visible(), "the + is there");
        assert!(
            add.bounds().right() <= right,
            "and inside the window: {:?} ends past {right:?}",
            add.bounds()
        );

        // The shown tab is on screen: the strip scrolled to it when it was added.
        let last = window.find(tab_label(tab_id(&view, 13, cx)));
        assert!(
            last.visible() && last.bounds().left() >= px(0.) && last.bounds().right() <= right,
            "the tab being shown is in the strip's visible part: {:?}",
            last.bounds()
        );

        // And back: a tab selected from the far end scrolls into view too.
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
        window.render_frame(cx);
        let first = window.find(tab_label(tab_id(&view, 0, cx)));
        assert!(
            first.visible() && first.bounds().left() >= px(0.) && first.bounds().right() <= right,
            "the first tab came back into view: {:?}",
            first.bounds()
        );
    })
    .unwrap();
}

/// §7.1: a strip scrolls once its tabs are at their floor — `min-width: 72px` —
/// and the tab being shown is the one on screen, whichever end of the strip it is
/// at. Twenty tabs in a 1280px window are past that floor, so the box is the room
/// there is and the tabs run out of it.
///
/// The tabs here are asked for one at a time (`open_empty_tab`): `+` and ⌘T would
/// show the one New Swarm tab rather than adding a second (§7.1).
#[gpui_kit::test]
fn a_strip_past_the_tabs_own_floor_scrolls_the_shown_tab_into_view(cx: &mut TestAppContext) {
    const TABS: usize = 20;
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        for _ in 1..TABS {
            open_another_tab(window, &view, cx);
        }
        window.render_frame(cx);

        // The floor is what makes it scroll: even at 72px a tab there are more
        // tabs than the room holds.
        let strip = window.find("tab-strip-scroll").bounds();
        let floor = px(TABS as f32 * store::design::TAB_MIN_WIDTH);
        assert!(
            strip.size.width < floor,
            "the tabs cannot all fit at their floor: strip {strip:?} against {floor:?}"
        );

        // The newest tab is shown, and it is on screen whole — not half past the
        // edge with its name cut mid-word.
        let right = window.bounds().right();
        assert_eq!(
            view.read(cx).selected_index(),
            TABS - 1,
            "the newest tab is shown"
        );
        // The tab's own box, not its label: at the 72px floor a tab is all slot,
        // close button and padding, and the label is elided to nothing.
        let last = window.find(tab_box(tab_id(&view, TABS - 1, cx)));
        assert!(
            last.visible() && last.bounds().left() >= px(0.) && last.bounds().right() <= right,
            "the tab being shown is in the strip's visible part: {:?}",
            last.bounds()
        );

        // Where the tabs are clipped is where the `+` begins: nothing is drawn
        // under the button, and the button is still in the window.
        let add = window.find("tab-add").bounds();
        assert!(
            strip.right() <= add.left() && add.right() <= right,
            "the tabs are clipped at the +: strip {strip:?}, + {add:?}, window {right:?}"
        );

        // And back: the first tab scrolls into view, at the design's own x — the
        // strip's scroll at its start is the layout the design draws.
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
        window.render_frame(cx);
        let first = window.find(tab_box(tab_id(&view, 0, cx))).bounds();
        assert!(
            first.left() >= px(0.) && first.right() <= right,
            "the first tab came back into view: {first:?}"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                first.left(),
                px(store::design::TRAFFIC_WIDTH + store::design::TAB_ROW_PAD.0),
                "and stands where the design puts it: {first:?}"
            );
        }

        // And one from the middle, which is neither end's scroll.
        view.update(cx, |view, cx| view.select_tab(10, window, cx));
        window.render_frame(cx);
        let middle = window.find(tab_box(tab_id(&view, 10, cx)));
        assert!(
            middle.visible()
                && middle.bounds().left() >= px(0.)
                && middle.bounds().right() <= right,
            "the tenth tab is on screen too: {:?}",
            middle.bounds()
        );
    })
    .unwrap();
}

/// §7.1: the window is named after what it is working on — Mission Control, ⌘`
/// and the window menu show this — and after nothing but the app while the tab
/// being shown has no folder.
///
/// The window here is given a swarm binary that is not there: a tab reaches its
/// folder without a process behind it (§9.7's failure screen, covered elsewhere).
#[gpui_kit::test]
fn the_window_is_named_after_the_folder_of_the_tab_being_shown(cx: &mut TestAppContext) {
    let home = std::env::temp_dir().join(format!("workspace-ui-title-{}", std::process::id()));
    let folder = home.join("a-folder-of-its-own");
    std::fs::create_dir_all(&folder).expect("a folder to work in");

    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(home),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).window_title(),
            "Evo Desktop",
            "an empty tab leaves the window the app's own name"
        );

        let tab = view.update(cx, |view, cx| {
            let tab = view.selected_tab().clone();
            tab.update(cx, |tab, cx| {
                tab.launch(
                    Launch::New {
                        folder: folder.clone(),
                        plan: LaunchPlan::default(),
                    },
                    window,
                    cx,
                )
            });
            tab
        });
        // The folder is the tab's from the launch on; the engine is not what this
        // test is about, so it is stopped and joined here rather than left to wake
        // the test's scheduler from its own thread.
        retire_engine!(tab, cx);
        window.render_frame(cx);
        let named = "a-folder-of-its-own — Evo Desktop";
        assert_eq!(
            view.read(cx).window_title(),
            named,
            "the folder's name, then the app's"
        );

        // A new empty tab is what is being shown now, so the window is the app's
        // again — and the other tab still names its folder when it comes back.
        view.update(cx, |view, cx| {
            view.add_tab(window, cx);
        });
        window.render_frame(cx);
        assert_eq!(view.read(cx).window_title(), "Evo Desktop");
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
        window.render_frame(cx);
        assert_eq!(view.read(cx).window_title(), named);
    })
    .unwrap();
}

/// Resuming a session from the New Swarm page's history turns *that* page into the
/// resumed swarm, as picking a folder does: no second tab, and no New Swarm page
/// left behind next to it.
#[gpui_kit::test]
fn resuming_from_the_history_replaces_the_new_swarm_page(cx: &mut TestAppContext) {
    let home = std::env::temp_dir().join(format!("workspace-ui-resume-{}", std::process::id()));
    let folder = home.join("resumed-here");
    std::fs::create_dir_all(&folder).expect("a folder to resume in");
    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(home),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });

    let tab = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let tab = view.read(cx).selected_tab().clone();
            tab.update(cx, |_, cx| {
                cx.emit(TabContentEvent::Resume {
                    session_path: folder.join("session.sexp"),
                    // As the session index keeps a cwd: with a trailing slash.
                    folder: PathBuf::from(format!("{}/", folder.display())),
                    swarm: true,
                })
            });
            tab
        })
        .unwrap();
    // The window hears the event once the update that emitted it has ended.
    cx.update_window(handle.into(), |_, window, cx| {
        for tab in view.read(cx).tabs().to_vec() {
            retire_engine!(tab, cx);
        }
        window.render_frame(cx);
        let view = view.read(cx);
        assert_eq!(
            view.tabs().len(),
            1,
            "the page became the swarm; no second tab"
        );
        let shown = view.selected_tab().read(cx);
        assert_eq!(shown.id(), tab.read(cx).id(), "the same tab, not a new one");
        assert_ne!(shown.state(), &TabState::Empty, "no New Swarm page is left");
        assert_eq!(shown.title().as_ref(), "resumed-here");
        assert_eq!(
            shown.folder(),
            Some(folder.as_path()),
            "no trailing slash kept"
        );
    })
    .unwrap();
}

/// §7.1: an overflowing strip scrolls **past** the `+`, not under it. The `+` is
/// the last thing on the strip's row and the tabs are clipped where it begins, so
/// no tab's name is ever drawn beneath it.
///
/// The bug this pins down: the button used to be the tab bar's own suffix, laid
/// over the tabs as they scrolled by, so the last visible tab read
/// `● evo-desktoj +` — the `+` sitting on the `p`.
#[gpui_kit::test]
fn the_add_button_never_sits_on_a_tab(cx: &mut TestAppContext) {
    let home = std::env::temp_dir().join(format!("workspace-ui-strip-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("a home to work in");
    let root = home.clone();

    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                // A tab reaches its folder without a process behind it: the strip is
                // what this test is about, and a swarm would only add noise.
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(root),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            for index in 0..14 {
                // Names long enough to take the strip's own tab width — the shape
                // that has tabs running past the window's edge.
                let folder = home.join(format!("evo-desktop-visual-review-deep-context-{index}"));
                std::fs::create_dir_all(&folder).expect("a folder to work in");
                let tab = if index == 0 {
                    view.selected_tab().clone()
                } else {
                    view.open_empty_tab(window, cx)
                };
                tab.update(cx, |tab, cx| {
                    tab.launch(
                        Launch::New {
                            folder,
                            plan: LaunchPlan::default(),
                        },
                        window,
                        cx,
                    )
                });
                // Fourteen tabs means fourteen engines; each is stopped and joined
                // as it is made, so the strip is what this test measures and no
                // thread of a tab's own is left to wake the scheduler.
                retire_engine!(tab, cx);
            }
            // A tab in the middle of the strip: the seven tabs to its right are
            // past the window's edge, which is where the `+` is.
            view.select_tab(6, window, cx);
        });
        window.render_frame(cx);
        // One more frame: the strip is its widest now, so this is the layout with
        // the most tabs behind the button.
        window.render_frame(cx);

        let add = window.find("tab-add");
        let add_bounds = add.bounds();
        let strip = window.find("tab-strip-scroll").bounds();
        assert!(
            strip.right() <= add_bounds.left(),
            "the tabs are clipped where the + begins: strip {strip:?}, + {add_bounds:?}"
        );

        for index in 0..14 {
            let label = window.find(tab_label(tab_id(&view, index, cx))).bounds();
            // What a reader can see of it: the label's own bounds, cut at the
            // strip's edge — the part the clip leaves on the screen.
            let visible = label.left().max(strip.left())..label.right().min(strip.right());
            if visible.is_empty() {
                continue;
            }
            assert!(
                visible.end <= add_bounds.left(),
                "tab {index}'s name is drawn under the +: label {label:?} shows {visible:?}, \
                 and the + is at {add_bounds:?}"
            );
        }
    })
    .unwrap();
}

/// A window whose tabs launch a swarm binary that is not there: a tab gets an engine
/// (whose boot fails at once) without a real process behind it.
fn open_unlaunchable(
    cx: &mut TestAppContext,
    name: &str,
) -> (WindowHandle<Root>, Entity<WorkspaceView>, PathBuf) {
    let home = std::env::temp_dir().join(format!("workspace-ui-{name}-{}", std::process::id()));
    let folder = home.join("project");
    std::fs::create_dir_all(&folder).expect("a folder to work in");
    let root = home.clone();
    let (handle, view) = open_window_with(cx, move |window, cx| {
        WorkspaceView::with_config(
            Arc::new(LaunchEnv {
                swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                root: store::paths::Root::at(root),
                ..LaunchEnv::default()
            }),
            window,
            cx,
        )
    });
    (handle, view, folder)
}

/// Let a closed tab's engine thread end and the window notice it: the window polls
/// on a timer, so the test moves the clock while the real thread finishes.
fn settle_terminating(cx: &mut TestAppContext, view: &Entity<WorkspaceView>, tabs: usize) {
    for _ in 0..100 {
        if cx.read(|cx| view.read(cx).tabs().len()) == tabs {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
    }
    panic!("the closed tab never left the strip");
}

/// §7.1: closing a tab whose swarm is running does not drop it out of sight — the tab
/// stays on the strip, frozen under "Terminating swarm.", and leaves once its swarm
/// has exited. A second × meanwhile does nothing.
#[gpui_kit::test]
fn closing_a_swarm_tab_freezes_it_until_the_swarm_exits(cx: &mut TestAppContext) {
    let (handle, view, folder) = open_unlaunchable(cx, "terminate");

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tab = view.read(cx).selected_tab().clone();
        tab.update(cx, |tab, cx| {
            tab.launch(
                Launch::New {
                    folder: folder.clone(),
                    plan: LaunchPlan::default(),
                },
                window,
                cx,
            )
        });
        open_another_tab(window, &view, cx);
        let id = tab.read(cx).id();
        view.update(cx, |view, cx| view.close_tab(id, window, cx));
        assert_eq!(
            view.read(cx).tabs().len(),
            2,
            "the tab stays while its swarm stops"
        );
        assert!(tab.read(cx).is_terminating());
        // A quit asked for now waits for this engine too: the session it holds is
        // still being written, even though the tab has stopped showing it (§9.8).
        assert_eq!(
            view.read(cx).swarms(cx).len(),
            1,
            "the window still has the terminating tab's engine"
        );
        // A second × on a tab already on its way out changes nothing.
        view.update(cx, |view, cx| view.close_tab(id, window, cx));
        assert_eq!(view.read(cx).tabs().len(), 2);

        view.update(cx, |view, cx| view.select_tab(0, window, cx));
        window.render_frame(cx);
        assert_eq!(
            window.find("tab-terminating-label").label(),
            Some("Terminating swarm."),
            "the frozen page says what is happening"
        );
        assert!(window.find("tab-terminating").visible());
    })
    .unwrap();

    settle_terminating(cx, &view, 1);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.selected_tab().read(cx).state(), &TabState::Empty);
    });
}

/// A tab with no swarm behind it closes at once: there is nothing to wait for.
#[gpui_kit::test]
fn closing_an_empty_tab_is_immediate(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
        assert_eq!(view.read(cx).tabs().len(), 1);
    })
    .unwrap();
}

/// §7.1: a History row for a session another tab already has open shows that tab;
/// a second swarm on the same journal would fork it. The path is matched however
/// it is spelled (here `/var/…` against its `/private/var/…` canonical form).
#[gpui_kit::test]
fn resuming_a_session_open_in_another_tab_shows_that_tab(cx: &mut TestAppContext) {
    let (handle, view, folder) = open_unlaunchable(cx, "dedupe");
    let session = folder.join("20261001T000000Z_session.sexp");
    std::fs::write(&session, "(:type :session)\n").expect("a session file");
    let spelled_otherwise = std::fs::canonicalize(&session).expect("canonical path");

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let running = view.read(cx).selected_tab().clone();
        running.update(cx, |tab, cx| {
            tab.launch(
                Launch::Resume {
                    folder: folder.clone(),
                    session: session.clone(),
                    swarm: true,
                },
                window,
                cx,
            )
        });
        open_another_tab(window, &view, cx);
        let asking = view.read(cx).selected_tab().clone();
        asking.update(cx, |_, cx| {
            cx.emit(TabContentEvent::Resume {
                session_path: spelled_otherwise.clone(),
                folder: folder.clone(),
                swarm: true,
            })
        });
    })
    .unwrap();

    // The window hears the event once the update that emitted it has ended.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let running = view.read(cx).tabs()[0].clone();
        let asking = view.read(cx).tabs()[1].clone();
        assert_eq!(
            view.read(cx).selected_index(),
            0,
            "the tab that has it is shown"
        );
        assert_eq!(view.read(cx).tabs().len(), 2, "no tab is added");
        assert_eq!(
            asking.read(cx).state(),
            &TabState::Empty,
            "the New Swarm page that asked starts nothing"
        );
        retire_engine!(running, cx);
    })
    .unwrap();
}

/// §9.8: ⌘Q is a hold, not a tap. The key going down puts the toast up and
/// starts the clock; letting it go before the hold is over takes the toast away
/// and quits nothing; holding it through is the quit.
#[gpui_kit::test]
fn a_tap_of_the_quit_key_does_not_quit_and_a_hold_does(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    // What the app binds (menus.rs), and what the window root handles.
    cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-q", HoldToQuit, None)]));
    let quits = Rc::new(Cell::new(0u32));
    // The subscription lives as long as the test: dropping it unsubscribes.
    let _quit_events = cx.update(|cx| {
        let quits = quits.clone();
        cx.subscribe(&view, move |_, _: &QuitHeld, _| quits.set(quits.get() + 1))
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A tap: down and up in one breath.
        window.press("cmd-q", cx);
        window.render_frame(cx);
        assert!(window.try_find("quit-hold-toast").is_none(), "no toast");
        assert!(
            !view.read(cx).is_holding_quit(),
            "and nothing is still holding"
        );
    })
    .unwrap();

    // The tap is over, so the hold it was not would have run out by now.
    cx.executor().advance_clock(QUIT_HOLD * 2);
    cx.run_until_parked();
    assert_eq!(quits.get(), 0, "a tap of ⌘Q is not a quit");

    // Now hold it: down, and nothing else.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_keystroke(quit_key(), cx);
        window.render_frame(cx);
        assert!(view.read(cx).is_holding_quit(), "⌘Q is down");
        assert_eq!(
            window.find("quit-hold-toast").label(),
            Some("Hold ⌘Q to Quit"),
            "the window says what the key is waiting for"
        );
        // The key repeats while it is held: each repeat is answered by the hold
        // that is already running, and none of them restarts its clock.
        for _ in 0..3 {
            window.dispatch_keystroke(quit_key(), cx);
        }
    })
    .unwrap();

    // Just past the hold, and only that far: the repeats did not put the quit
    // off, and the tap before them did not make one.
    cx.executor()
        .advance_clock(QUIT_HOLD + Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(quits.get(), 1, "a held ⌘Q is the quit");
    cx.update(|cx| assert!(!view.read(cx).is_holding_quit()));

    // The release after it is not a second one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_event(
            KeyUpEvent {
                keystroke: quit_key(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.executor().advance_clock(QUIT_HOLD * 2);
    cx.run_until_parked();
    assert_eq!(quits.get(), 1, "one hold, one quit");
}

/// ⌘Q as the window's keymap names it.
fn quit_key() -> Keystroke {
    Keystroke::parse("cmd-q").expect("a keystroke")
}

/// §9.8: the quitting screen covers the window, says how many swarms are being
/// terminated — one is not several — and takes the keyboard off the page under
/// it, which is a session on its way out.
#[gpui_kit::test]
fn the_quitting_screen_says_what_is_being_terminated(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("quit-screen").is_none(),
            "nothing is quitting yet"
        );

        view.update(cx, |view, cx| view.show_quitting(2, cx));
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.find("quit-screen").visible());
        assert_eq!(
            window.find("quit-screen-label").label(),
            Some("Terminating swarms."),
            "two swarms are being terminated"
        );
        assert_eq!(
            window.find("quit-screen").focused(),
            Some(true),
            "the keyboard is the screen's, not the page's"
        );

        view.update(cx, |view, cx| view.show_quitting(1, cx));
        window.render_frame(cx);
        assert_eq!(
            window.find("quit-screen-label").label(),
            Some("Terminating swarm."),
            "one swarm is not several"
        );
    })
    .unwrap();
}

// --- the Settings tab (§7.1, §13) -----------------------------------------

/// A test's own evo home: the four files a Settings tab reads live in it, and
/// nothing here touches the machine's own `~/.evo`. The directory is not made — a
/// missing file is what a project that has never been configured looks like, and
/// reading one creates nothing.
fn hermetic_evo_home() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join(format!("workspace-settings-{}", std::process::id()));
        std::env::set_var("EVO_HOME", home);
    });
}

/// §7.1: the gear at the far right of the strip opens the app's Settings, there is
/// only ever one of it, and it is the strip's last tab — a tab opened later goes
/// *before* it, which is what keeps the strip and the stored tab set the same order.
#[gpui_kit::test]
fn the_gear_opens_one_settings_tab_and_shows_it_again(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("tab-settings").is_some(),
            "the gear is on the strip"
        );
        let strip = window.find("tab-strip-scroll").bounds();
        let gear = window.find("tab-settings").bounds();
        assert!(
            gear.left() >= strip.right() - px(1.),
            "the gear is past the tabs' own scroller, so scrolling cannot take it away"
        );

        window.click("tab-settings", cx);
        window.render_frame(cx);
        let settings = {
            let view = view.read(cx);
            assert_eq!(view.tabs().len(), 2, "the empty tab and the settings tab");
            assert_eq!(view.selected_index(), 1, "and Settings is what is shown");
            let tab = view.selected_tab().read(cx);
            assert_eq!(tab.state(), &TabState::Settings);
            assert_eq!(tab.title(), "Settings", "the strip's own label for it");
            window.find(tab_label(tab.id().get()));
            tab.id()
        };

        // Another tab goes before Settings, which stays last.
        open_another_tab(window, &view, cx);
        window.render_frame(cx);
        {
            let view = view.read(cx);
            assert_eq!(view.tabs().len(), 3);
            assert_eq!(
                view.tabs()[2].read(cx).id(),
                settings,
                "the settings tab is still the last one"
            );
            assert_eq!(view.selected_index(), 1, "the new tab is the one shown");
        }

        // The gear again: the one that is there is shown, not a second one made.
        window.click("tab-settings", cx);
        window.render_frame(cx);
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 3, "no second settings tab");
        assert_eq!(view.selected_index(), 2, "the one that is there is shown");
    })
    .unwrap();
}

/// §9.8: the Settings tab is not one an app stores — no folder, no swarm, no
/// session — and the index the window hands over still addresses the records it
/// lists, with Settings left out.
#[gpui_kit::test]
fn the_settings_tab_is_not_part_of_the_stored_tab_set(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-settings", cx);
        window.render_frame(cx);
        open_another_tab(window, &view, cx);
        window.render_frame(cx);
    })
    .unwrap();

    // With Settings shown, there is no record to select: the index the app reads is
    // the one past the records rather than a record that is not there.
    cx.update(|cx| {
        let view = view.read(cx);
        let records = view.tab_records(cx);
        assert_eq!(
            records.len(),
            2,
            "the two swarm tabs, without the settings tab"
        );
        assert!(
            records
                .iter()
                .all(|record| record.folder.is_none() && record.session.is_none()),
            "and the settings tab contributed nothing to either of them"
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(tab_label(tab_id(&view, 2, cx)), cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(
            view.selected_index(),
            view.tab_records(cx).len(),
            "the selected index of a window showing Settings names no record"
        );
    });

    // A swarm tab being shown: the index is the record, in strip order.
    cx.update_window(handle.into(), |_, window, cx| {
        window.click(tab_label(tab_id(&view, 0, cx)), cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.selected_index(), 0);
        assert_eq!(
            view.tab_records(cx)[0].window_id,
            view.tabs()[0].read(cx).id(),
            "the index the app is handed is the record of the tab being shown"
        );
    });
}

/// §7.1, §13: a Settings tab with edits that are not on disk is asked about before
/// it goes — Discard Changes, or keep editing. The tab keeps its draft while the
/// question is up, and the answer is what closes it.
#[gpui_kit::test]
fn closing_a_settings_tab_with_unsaved_edits_asks_first(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);
    let window = handle.into();

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-settings", cx);
        window.render_frame(cx);
        // Nothing here focuses anything: showing the tab is what puts the caret in
        // its editor, so what is typed lands in the file the page is showing.
        window.input("(a)", cx);
        window.render_frame(cx);
    })
    .unwrap();
    // The editors read their four files on the background executor; the draft is
    // measured against what was read, so let the first read land before asking a
    // question about it.
    cx.run_until_parked();
    cx.update(|cx| {
        let editor = view
            .read(cx)
            .selected_tab()
            .read(cx)
            .settings_editor()
            .cloned()
            .expect("the settings tab has its editors");
        assert!(
            editor.read(cx).is_dirty(cx),
            "what was typed is a draft, not a file"
        );
    });

    // The `×` asks rather than closes.
    cx.update_window(window, |_, window, cx| {
        window.click(tab_close(tab_id(&view, 1, cx)), cx);
        window.render_frame(cx);
        assert!(
            window.try_find("settings-close-prompt").is_some(),
            "the tab asks before it goes"
        );
        assert_eq!(view.read(cx).tabs().len(), 2, "and it has not gone");

        // Keep Editing: the question goes, the tab stays, the draft stays.
        window.click("settings-close-keep", cx);
        window.render_frame(cx);
        assert!(window.try_find("settings-close-prompt").is_none());
        assert_eq!(view.read(cx).tabs().len(), 2);
        let editor = view.read(cx).tabs()[1]
            .read(cx)
            .settings_editor()
            .cloned()
            .expect("the editors");
        assert!(editor.read(cx).is_dirty(cx), "the draft was kept");
    })
    .unwrap();

    // Discard Changes: the draft goes back to the file, and the tab goes with it —
    // through the tab's own `CloseRequested`, which the window hears on its next
    // effect cycle, exactly as the app's own loop would.
    cx.update_window(window, |_, window, cx| {
        window.click(tab_close(tab_id(&view, 1, cx)), cx);
        window.render_frame(cx);
        assert!(
            window.try_find("settings-close-prompt").is_some(),
            "asking again asks again"
        );
        window.click("settings-close-discard", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("settings-close-prompt").is_none(),
            "the question is answered"
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).tabs().len(), 1, "the tab went");
        assert_eq!(view.read(cx).selected_index(), 0);
    })
    .unwrap();
}

/// §7.1: the Settings tab is one of the strip's tabs for the keyboard too — the
/// cycle walks onto it and off it, ⌘9 is it while it is last, and ⌘W closes it the
/// way it closes any tab with nothing behind it.
#[gpui_kit::test]
fn the_settings_tab_takes_the_strip_keys(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-settings", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).selected_index(), 1);

        window.press("cmd-9", cx);
        assert_eq!(
            view.read(cx).selected_index(),
            1,
            "⌘9 is the settings tab while it is the last one"
        );
        window.press("ctrl-shift-tab", cx);
        assert_eq!(
            view.read(cx).selected_index(),
            0,
            "the cycle walks off it onto the tab before it"
        );
        window.press("ctrl-tab", cx);
        assert_eq!(view.read(cx).selected_index(), 1, "and back onto it");

        // Nothing behind it: ⌘W's own close takes it at once, with no swarm to wait
        // for. (⌘W is the app's binding, over `menus`; this is the half the window
        // owns, which is what the menu item calls.)
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
        window.render_frame(cx);
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 1, "the settings tab went");
        assert_eq!(view.selected_index(), 0);
        assert_eq!(view.tabs()[0].read(cx).state(), &TabState::Empty);
    })
    .unwrap();
}

/// §7.1, §13: a settings page that is *writing* is not closed under the write. The
/// close waits, and the page comes to the front while it does — its own status line
/// says `Saving…`, which is the feedback a press that did nothing would not give.
#[gpui_kit::test]
fn a_settings_tab_that_is_writing_is_not_closed_under_the_write(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);
    let window = handle.into();

    let editor = |cx: &App| {
        view.read(cx)
            .selected_tab()
            .read(cx)
            .settings_editor()
            .cloned()
            .expect("the settings tab has its editors")
    };

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-settings", cx);
        window.render_frame(cx);
        window.input("(a)", cx);
    })
    .unwrap();
    // The page reads its documents first; a draft is measured against what was read.
    cx.run_until_parked();
    assert!(
        cx.read(|cx| editor(cx).read(cx).is_dirty(cx)),
        "what was typed is a draft"
    );

    cx.update_window(window, |_, window, cx| {
        // The page's own save: the document being shown, on its way to disk.
        let page = editor(cx);
        page.update(cx, |page, cx| page.save_selected(window, cx));
        assert!(
            page.read(cx).is_saving(),
            "the write is in flight, on the background executor"
        );

        // The close does not walk away from it.
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).tabs().len(),
            2,
            "the tab stays while the write is in flight"
        );
        assert_eq!(
            window.find(settings::CONFIG_STATUS_ID).label(),
            Some("Saving…"),
            "and the page's own status line is what says so"
        );
    })
    .unwrap();

    // The write lands — one way or the other, on this machine — and the page stops
    // saying it is writing.
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let page = editor(cx);
        assert!(
            !page.read(cx).is_saving(),
            "the write is not still in flight after the executor ran"
        );
        assert_eq!(view.read(cx).tabs().len(), 2, "and the tab is still here");
    })
    .unwrap();
}

/// §7.1, §9.8: a window can be left holding nothing but the Settings tab — the last
/// swarm tab closes into it, the way the last tab closes into a fresh New Swarm page —
/// and the index it hands the app is still the one past its records, so nothing
/// invisible is ever selected. `+` is the way back, and it opens *before* Settings.
#[gpui_kit::test]
fn a_window_holding_only_settings_still_maps_its_records(cx: &mut TestAppContext) {
    hermetic_evo_home();
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-settings", cx);
        window.render_frame(cx);

        // ⌘W's own close, on the New Swarm tab rather than on Settings.
        view.update(cx, |view, cx| view.select_tab(0, window, cx));
        view.update(cx, |view, cx| view.close_selected_tab(window, cx));
        window.render_frame(cx);
        {
            let shown = view.read(cx);
            assert_eq!(shown.tabs().len(), 1, "only the settings tab is left");
            assert_eq!(shown.tabs()[0].read(cx).state(), &TabState::Settings);
            assert_eq!(shown.selected_index(), 0);
            assert!(shown.tab_records(cx).is_empty(), "and it is not a record");
            assert_eq!(
                shown.selected_index(),
                shown.tab_records(cx).len(),
                "so the app is handed an index that names no record"
            );
        }

        // `+` is the way back, and the new tab goes before Settings.
        window.click("tab-add", cx);
        window.render_frame(cx);
        let shown = view.read(cx);
        assert_eq!(shown.tabs().len(), 2);
        assert_eq!(shown.tabs()[0].read(cx).state(), &TabState::Empty);
        assert_eq!(
            shown.tabs()[1].read(cx).state(),
            &TabState::Settings,
            "settings is still the last tab"
        );
        assert_eq!(shown.tab_records(cx).len(), 1);
        assert_eq!(
            shown.selected_index(),
            0,
            "and the tab being shown is the record it names"
        );
    })
    .unwrap();
}
