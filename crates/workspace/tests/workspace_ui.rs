//! UI tests for the window chrome: the tab strip and the empty tab (§7.1, §7.2).
//!
//! Each test opens the real window through the production entry point
//! ([`workspace::window_options`] plus [`workspace::WorkspaceView`]) in a headless
//! GPUI test window and drives it the way a user would.

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    base::Root, px, size, App, AppContext, Bounds, ElementId, Entity, Point, TestAppContext,
    WindowBounds, WindowHandle, WindowOptions,
};
use workspace::{TabState, WorkspaceView};

/// Opens the app's window with one empty tab, at a deterministic size.
fn open_workspace(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<WorkspaceView>) {
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
            |window, cx| cx.new(|cx| WorkspaceView::new(window, cx)),
        )
        .expect("open the workspace window");
        (window.downcast::<Root>().expect("base Root"), view)
    })
}

/// The tab's label element, which is also its click target (§7.1).
fn tab_label(id: u64) -> ElementId {
    ElementId::NamedInteger("tab-label".into(), id)
}

/// The tab's close button (§7.1).
fn tab_close(id: u64) -> ElementId {
    ElementId::NamedInteger("tab-close".into(), id)
}

fn tab_id(view: &Entity<WorkspaceView>, index: usize, cx: &App) -> u64 {
    view.read(cx).tabs()[index].read(cx).id().get()
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
        assert_eq!(tab.title().as_ref(), "New tab");
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

/// Every chooser starts at `Default` — each one is only passed when chosen
/// (§7.2).
#[gpui_kit::test]
fn the_empty_tab_defaults_every_chooser(cx: &mut TestAppContext) {
    let (_handle, view) = open_workspace(cx);

    cx.update(|cx| {
        let tab = view.read(cx).selected_tab().read(cx);
        assert_eq!(tab.coordinator_model(cx).as_ref(), "Default");
        assert_eq!(tab.lanes_model(cx).as_ref(), "Default");
        assert_eq!(tab.workers(cx).as_ref(), "Default");
    });
}

/// The `+` appends a new empty tab and selects it (§7.1).
#[gpui_kit::test]
fn the_add_button_appends_and_selects_a_new_tab(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-add", cx);
    })
    .unwrap();

    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.tabs().len(), 2);
        assert_eq!(view.selected_index(), 1, "the new tab is selected");
        assert_eq!(view.selected_tab().read(cx).state(), &TabState::Empty);
        assert_eq!(view.tabs()[0].read(cx).state(), &TabState::Empty);
    });
}

/// Clicking a tab selects it (§7.1).
#[gpui_kit::test]
fn clicking_a_tab_selects_it(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let first = cx.update(|cx| tab_id(&view, 0, cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("tab-add", cx);
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
        window.click("tab-add", cx);
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
        window.click("tab-add", cx);
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
#[gpui_kit::test]
fn the_keyboard_shortcuts_add_and_close_tabs(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
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
        view.update(cx, |view, cx| {
            view.add_tab(window, cx);
        });
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
