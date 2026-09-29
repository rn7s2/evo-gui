//! The menu bar (§7.1 — one window, but macOS expects a menu), and the actions
//! the menu items invoke.
//!
//! Two kinds of item live here:
//!
//! * the ones the app does itself — Quit, the File menu, Minimize/Zoom, About,
//!   Hide — which declare an action, bind it, and handle it globally;
//! * the editing items, which must reach the focused text field instead of the
//!   app. Those use the action types the composer's `Textarea` already handles,
//!   and [`MenuItem::os_action`] so macOS runs its own cut/copy/paste;
//!   `gpui_base` already binds ⌘C/⌘V/⌘X/⌘A/⌘Z to them, so nothing here has to.
//!
//! Registering a *global* handler for an editing action would be wrong: the
//! handler runs after the focused view's, so it would only ever see the ones the
//! text field did not want.
//!
//! One thing to know about those global handlers: they run while the window is
//! still on the update stack, so a handler cannot borrow it again — `handle.update`
//! answers "window not found". The work that needs the window is therefore handed
//! to [`in_window_later`], which runs at the end of the effect cycle, when it is
//! free again.

use gpui_kit::component::dialog::AlertDialog;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, Window};

use crate::launcher;
use crate::quit;

gpui_kit::actions!(
    evo_desktop,
    [
        /// About Evo Desktop.
        AboutApp,
        /// Hide this app.
        HideApp,
        /// Hide every other app.
        HideOthers,
        /// Show every hidden app.
        ShowAll,
        /// Choose the quit sequence (window close and ⌘Q both end here).
        QuitApp,
        /// Open a new empty tab (⌘T).
        NewTab,
        /// Close the selected tab (⌘W).
        CloseTab,
        /// Minimize the window.
        MinimizeWindow,
        /// Zoom (maximize) the window.
        ZoomWindow,
    ]
);

/// Build the menu bar, bind the shortcuts, and register the handlers.
pub fn install(cx: &mut App) {
    // The shortcuts the titles advertise. macOS draws a menu item's key
    // equivalent from these bindings, and they are what make ⌘T/⌘W/⌘M work when
    // the menu is not in play.
    cx.bind_keys([
        KeyBinding::new("cmd-q", QuitApp, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
    ]);

    cx.on_action(|_: &QuitApp, cx: &mut App| quit::begin(cx));
    cx.on_action(|_: &AboutApp, cx: &mut App| about(cx));
    cx.on_action(|_: &HideApp, cx: &mut App| {
        cx.hide();
        log(cx, "hide");
    });
    cx.on_action(|_: &HideOthers, cx: &mut App| {
        cx.hide_other_apps();
        log(cx, "hide others");
    });
    cx.on_action(|_: &ShowAll, cx: &mut App| {
        cx.unhide_other_apps();
        log(cx, "show all");
    });
    cx.on_action(|_: &NewTab, cx: &mut App| {
        let Some(view) = quit::view(cx) else {
            return;
        };
        in_window_later(cx, move |window, cx| {
            view.update(cx, |view, cx| {
                view.add_tab(window, cx);
            });
        });
    });
    cx.on_action(|_: &CloseTab, cx: &mut App| {
        let Some(view) = quit::view(cx) else {
            return;
        };
        in_window_later(cx, move |window, cx| {
            view.update(cx, |view, cx| view.close_selected_tab(window, cx));
        });
    });
    cx.on_action(|_: &MinimizeWindow, cx: &mut App| {
        in_window_later(cx, |window, _cx| window.minimize_window());
    });
    cx.on_action(|_: &ZoomWindow, cx: &mut App| {
        in_window_later(cx, |window, _cx| window.zoom_window());
    });

    cx.set_menus(menus());
}

/// The menu bar itself: the app menu, File, Edit, Window — the order macOS shows.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Evo Desktop").items([
            MenuItem::action("About Evo Desktop", AboutApp),
            MenuItem::separator(),
            MenuItem::action("Hide Evo Desktop", HideApp),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Evo Desktop", QuitApp),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        // The editing items are the actions the composer's text field handles;
        // `os_action` is what makes macOS run its own cut/copy/paste for them.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
        ]),
    ]
}

/// The editing actions, from the crate that defines them (`gpui_base::input`,
/// re-exported by the component library the composer's `Textarea` comes from).
mod input {
    pub use gpui_kit::base::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
}

/// "About Evo Desktop": what the app is, which build, and where its log is.
fn about(cx: &mut App) {
    let (root, log) = {
        let shell = cx.global::<crate::Shell>();
        (shell.root.clone(), shell.log.clone())
    };
    let version = env!("CARGO_PKG_VERSION");
    log.info(format!("about: evo-desktop {version}"));
    in_window_later(cx, move |window, cx| {
        window.open_alert_dialog(cx, move |alert: AlertDialog, _window, _cx| {
            alert
                .title(format!("Evo Desktop {version}"))
                .description(format!(
                    "A native window onto the evo agent runtime.\n\n\
                     Log: {}\n\
                     State: {}",
                    log.path().display(),
                    root.app_json().display()
                ))
                .ok_text("Close")
        });
    });
}

/// Run `f` with the app's window — the one window the app opens (§7.1) — at the
/// end of this effect cycle.
///
/// An action arrives while the window is still on the update stack, so borrowing
/// it now answers "window not found"; `App::defer` is the way back to it (the
/// same one `Context::defer_in` takes).
fn in_window_later(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    let Some(handle) = launcher::window_of(cx) else {
        log(cx, "no window to run a menu action in");
        return;
    };
    cx.defer(move |cx| {
        if let Err(error) = handle.update(cx, |_root, window, cx| f(window, cx)) {
            log(cx, &format!("the window could not be updated: {error}"));
        }
    });
}

fn log(cx: &App, what: &str) {
    let shell = cx.global::<crate::Shell>();
    shell.log.info(format!("menu: {what}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_bar_has_the_four_menus_in_order() {
        let menus = menus();
        let names: Vec<&str> = menus.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(names, ["Evo Desktop", "File", "Edit", "Window"]);
    }

    #[test]
    fn the_app_menu_ends_with_quit() {
        let menus = menus();
        let app_menu = &menus[0];
        assert!(
            matches!(app_menu.items.last(), Some(MenuItem::Action { .. })),
            "the app menu ends with an item"
        );
        let names: Vec<String> = app_menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [
                "About Evo Desktop",
                "Hide Evo Desktop",
                "Hide Others",
                "Show All",
                "Quit Evo Desktop",
            ]
        );
    }

    #[test]
    fn the_edit_menu_is_the_text_fields_actions() {
        let menus = menus();
        let edit = &menus[2];
        let actions: Vec<String> = edit
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, os_action, .. } => {
                    // The names are namespaced (`input::SelectAll`); the menu
                    // cares only which action it is.
                    let name = action.name().rsplit("::").next().unwrap_or_default();
                    Some(format!("{name}:{}", os_action.is_some()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            ["Undo:true", "Redo:true", "Cut:true", "Copy:true", "Paste:true", "SelectAll:true"],
            "every editing item is an OS action the text field handles"
        );
    }

    #[test]
    fn the_file_menu_offers_new_and_close_tab() {
        let menus = menus();
        let names: Vec<String> = menus[1]
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["New Tab", "Close Tab"]);
    }
}
