//! UI tests for the window chrome: the tab strip and the empty tab (§7.1, §7.2).
//!
//! Each test opens the real window through the production entry point
//! ([`workspace::window_options`] plus [`workspace::WorkspaceView`]) in a headless
//! GPUI test window and drives it the way a user would.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    base::Root, px, size, App, AppContext, Bounds, ElementId, Entity, InputEvent as _, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions,
};
use session::LaunchPlan;
use workspace::{Launch, LaunchEnv, TabState, WorkspaceView};

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
/// wake lands after the test's last frame. Joining the engine before the frames
/// end is what makes a test that launches a tab (a binary that is not there
/// reaches its folder without a process) deterministic.
macro_rules! retire_engine {
    ($tab:expr, $cx:expr) => {{
        let engine = $tab.update($cx, |tab, cx| tab.take_engine(cx));
        if let Some(mut engine) = engine {
            engine.shutdown();
            engine.join();
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
