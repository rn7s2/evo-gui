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
use std::path::PathBuf;
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

/// A tab whose swarm came up draws the tab page: the real transcript view and
/// the real composer, with no todo panel while that agent has no todos (§7.3).
#[gpui_kit::test]
fn a_running_tab_shows_the_transcript_and_the_composer(cx: &mut TestAppContext) {
    let (handle, view) = open_workspace(cx);
    let folder = PathBuf::from("/tmp/evo-desktop-a-running-tab");

    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.selected_tab().update(cx, |tab, cx| {
                tab.begin_boot(folder.clone(), cx);
                tab.mark_running(cx);
            });
        });
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("tab-page").visible());
        assert!(window.find("transcript-column").visible());
        assert!(window.find("composer-column").visible());
        assert!(window.find(composer::READOUT_ID).visible());
        assert_eq!(
            window.find(composer::BUTTON_ID).label(),
            Some("Send"),
            "an idle coordinator offers Send"
        );
        assert!(
            window.try_find("todo-panel").is_none(),
            "an agent with no todos shows no panel"
        );
    })
    .unwrap();

    cx.update(|cx| {
        let tab = view.read(cx).selected_tab().read(cx);
        assert_eq!(
            tab.state(),
            &TabState::Running { folder },
            "the tab stays on the page it moved to"
        );
        assert!(tab.transcript().read(cx).rows(cx).is_empty());
    });
}
