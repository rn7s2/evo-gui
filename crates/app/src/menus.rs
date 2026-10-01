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

use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, Window};
use store::app_state::AppState;
use transcript::TranscriptZoom;

use crate::launcher;
use crate::quit;
use crate::Shell;

gpui_kit::actions!(
    evo_desktop,
    [
        /// About Evo Desktop.
        AboutApp,
        /// Settings… (⌘,): the two binaries this app spawns, and the theme.
        SettingsApp,
        /// Hide this app.
        HideApp,
        /// Hide every other app.
        HideOthers,
        /// Show every hidden app.
        ShowAll,
        /// Quit now: what the menu item does, with no hold in front of it (§9.8).
        /// ⌘Q is [`workspace::HoldToQuit`] and ends in the same quit.
        QuitApp,
        /// Open a new empty tab (⌘T).
        NewTab,
        /// Close the selected tab (⌘W).
        CloseTab,
        /// Minimize the window.
        MinimizeWindow,
        /// Zoom (maximize) the window.
        ZoomWindow,
        /// Draw the transcripts' text larger (⌘=, ⌘+): the View menu (§7.2).
        ZoomIn,
        /// Draw the transcripts' text smaller (⌘−).
        ZoomOut,
        /// Back to the design's own size (⌘0), as macOS's View menu words it.
        ActualSize,
    ]
);

/// Build the menu bar, bind the shortcuts, and register the handlers.
pub fn install(cx: &mut App) {
    // The shortcuts the titles advertise. macOS draws a menu item's key
    // equivalent from these bindings, and they are what make ⌘T/⌘W/⌘M work when
    // the menu is not in play.
    //
    // ⌘Q is the exception, and it is bound to the *hold*: a menu item that carries
    // a key equivalent is fired by AppKit before any window sees the key, and a
    // hold is a key-down and a key-up. So the Quit item shows no shortcut of its
    // own — it is the deliberate path, and it quits the moment it is chosen — and
    // ⌘Q goes to the window, which is what can tell a tap from a hold (§9.8).
    cx.bind_keys([
        KeyBinding::new("cmd-q", workspace::HoldToQuit, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("cmd-,", SettingsApp, None),
        // The View menu's three. macOS fires the menu item's own key equivalent
        // before any window sees the key, and a menu item shows the *first*
        // binding its action has, so ⌘+ is bound first: that is the equivalent
        // the View menu draws (⇧ is implied by a key that needs it). The other
        // two are the same action by the keystrokes a keyboard actually sends —
        // AppKit reports ⌘⇧= as `cmd-+` — and what a non-macOS keymap asks for.
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd-shift-=", ZoomIn, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ActualSize, None),
    ]);
    // The Window menu's tab items are the workspace's actions, and their key
    // equivalents come from the same place: the app's keymap, read once, when
    // the menu bar is built below. A window binds them too, but the menu bar
    // outlives every window's keyboard and is built before the first window
    // exists, so the bindings have to be here first (workspace::bind_tab_keys).
    workspace::bind_tab_keys(cx);

    cx.on_action(|_: &QuitApp, cx: &mut App| quit::begin(cx));
    cx.on_action(|_: &AboutApp, cx: &mut App| open_about(cx));
    cx.on_action(|_: &SettingsApp, cx: &mut App| open_settings(cx));
    // The workspace's own Settings action: the empty tab's "evo-swarm not found" line
    // (§9.7) is drawn there and the panel is here, so the tab asks through this action
    // and the app answers with the same handler the menu item uses.
    cx.on_action(|_: &workspace::OpenSettings, cx: &mut App| open_settings(cx));
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
    // The View menu's three (§7.2): the reader's zoom, live and remembered.
    cx.on_action(|_: &ZoomIn, cx: &mut App| {
        let zoom = TranscriptZoom::get(cx).zoom_in();
        set_zoom(cx, zoom);
    });
    cx.on_action(|_: &ZoomOut, cx: &mut App| {
        let zoom = TranscriptZoom::get(cx).zoom_out();
        set_zoom(cx, zoom);
    });
    cx.on_action(|_: &ActualSize, cx: &mut App| {
        set_zoom(cx, TranscriptZoom::default());
    });

    // What `app.json` remembered, before any transcript draws a row: the actions
    // above go on to change it, and every open transcript follows. A host that
    // installs the menu bar without a [`Shell`] — a test — opens at the design's
    // own size.
    if let Some(stored) = cx.try_global::<Shell>().map(|shell| shell.zoom) {
        TranscriptZoom::clamped(stored).set(cx);
    }

    cx.set_menus(menus());
}

/// What a View-menu item does: the reader's zoom, and `app.json` (§7.2).
///
/// The global is what every open transcript reads — a view that is showing rows
/// observes it and measures them again — and `app.json` is what the next launch
/// opens with. The [`Shell`] keeps the number too, so the panel-less half of the
/// app (the quit path, a second menu action) sees the same one.
fn set_zoom(cx: &mut App, zoom: TranscriptZoom) {
    if zoom == TranscriptZoom::get(cx) {
        // A step at the top or the bottom of the range: nothing to draw differently
        // and nothing to write.
        return;
    }
    zoom.set(cx);
    // A host that installed the menu bar without a [`Shell`] — a test — has no
    // `app.json` to remember the size in; the global is still what this window
    // draws its transcripts at.
    let Some((root, log)) = cx
        .try_global::<Shell>()
        .map(|shell| (shell.root.clone(), shell.log.clone()))
    else {
        return;
    };
    cx.global_mut::<Shell>().zoom = zoom.0;

    let mut state = AppState::load(&root);
    state.zoom = zoom.0;
    match state.save(&root) {
        Ok(()) => log.info(format!("zoom: {:.0}%", zoom.0 * 100.)),
        Err(error) => log.error(format!(
            "could not save {}: {error}",
            root.app_json().display()
        )),
    }
}

/// The menu bar itself: the app menu, File, Edit, View, Window — the order macOS
/// shows.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Evo Desktop").items([
            MenuItem::action("About Evo Desktop", AboutApp),
            MenuItem::action("Settings…", SettingsApp),
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
        // The View menu: macOS's own three, in its own words, and they are the
        // transcript's text size rather than the window's — its key equivalents
        // are the bindings above, and its Zoom In is ⌘+ as every Mac app's is.
        Menu::new("View").items([
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::separator(),
            MenuItem::action("Actual Size", ActualSize),
        ]),
        Menu::new("Window").items([
            // The window's tabs. The keys (⌃⇥, ⌃⇧⇥, ⌘9) are the workspace's own
            // bindings — macOS draws them from there — so the items only name the
            // actions, and the tab strip handles them wherever focus is.
            MenuItem::action("Select Next Tab", workspace::SelectNextTab),
            MenuItem::action("Select Previous Tab", workspace::SelectPreviousTab),
            MenuItem::action("Select Last Tab", workspace::SelectLastTab),
            MenuItem::separator(),
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

/// "About Evo Desktop": the app, its build, the binaries it spawns, and where its
/// state and log live (§7.1). The dialog itself is `about`'s; this is the menu
/// item's half of it.
pub fn open_about(cx: &mut App) {
    let log = cx.global::<crate::Shell>().log.clone();
    log.info(format!("about: evo-desktop {}", env!("CARGO_PKG_VERSION")));
    in_window_later(cx, crate::about::open);
}

/// "Settings…" (⌘,): the two binaries the app spawns and the theme (§13). The
/// panel itself is `crates/settings`; this is the menu item's half of it, and Save
/// is what persists — the panel owns the dialog.
pub fn open_settings(cx: &mut App) {
    log(cx, "settings");
    in_window_later(cx, |window, cx| {
        crate::settings::open(window, cx);
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
    use gpui_kit::TestAppContext;

    /// A temp app root, as the settings tests make one.
    fn temp_root(name: &str) -> store::paths::Root {
        let dir =
            std::env::temp_dir().join(format!("evo-desktop-menus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        store::paths::Root::at(dir)
    }

    #[test]
    fn the_menu_bar_has_the_five_menus_in_order() {
        let menus = menus();
        let names: Vec<&str> = menus.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(
            names,
            ["Evo Desktop", "File", "Edit", "View", "Window"],
            "the View menu sits between the editing items and the window's, as it does in every Mac app"
        );
    }

    /// §7.2: the transcript's own zoom, in macOS's own words — and the separator
    /// between the two steps and the way back is Apple's placement.
    #[test]
    fn the_view_menu_is_macoss_own_three() {
        let menus = menus();
        let view = &menus[3];
        assert_eq!(view.name.as_ref(), "View");
        let items: Vec<String> = view
            .items
            .iter()
            .map(|item| match item {
                MenuItem::Action { name, .. } => name.to_string(),
                MenuItem::Separator => "—".to_string(),
                _ => "?".to_string(),
            })
            .collect();
        assert_eq!(items, ["Zoom In", "Zoom Out", "—", "Actual Size"]);

        let actions: Vec<&str> = view
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.name()),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            [
                "evo_desktop::ZoomIn",
                "evo_desktop::ZoomOut",
                "evo_desktop::ActualSize"
            ]
        );
    }

    /// The keys the View menu draws: ⌘+ first, which is the equivalent macOS shows
    /// for Zoom In, and the two other ways a keyboard can ask for the same thing.
    /// ⌘0 is the app's own — the tab strip stopped at ⌘8 — and none of the three
    /// is a key something else already had.
    #[gpui_kit::test]
    fn the_zoom_keys_are_bound_and_collide_with_nothing(cx: &mut TestAppContext) {
        cx.update(install);
        let keymap = cx.update(|cx| cx.key_bindings());
        let keymap = keymap.borrow();
        let keys = |action: &dyn gpui_kit::Action| -> Vec<String> {
            keymap
                .bindings_for_action(action)
                .flat_map(|binding| binding.keystrokes())
                .map(|keystroke| keystroke.to_string())
                .collect()
        };

        // The rendered form is what macOS draws beside the item: ⇧ is implied by
        // the "+" a keyboard needs shift for, so Zoom In reads ⌘+ — Apple's own.
        assert_eq!(keys(&ZoomIn), ["⌘+", "⌘⇧=", "⌘="]);
        assert_eq!(keys(&ZoomOut), ["⌘-"]);
        assert_eq!(keys(&ActualSize), ["⌘0"]);

        // Nothing else in the keymap answers to them: every binding the zoom keys
        // reach belongs to a zoom action.
        let zoom = [
            "evo_desktop::ZoomIn",
            "evo_desktop::ZoomOut",
            "evo_desktop::ActualSize",
        ];
        for key in ["cmd-+", "cmd-shift-=", "cmd-=", "cmd--", "cmd-0"] {
            let keystroke = gpui_kit::Keystroke::parse(key).expect("a keystroke");
            let (bindings, _) = keymap.bindings_for_input(&[keystroke], &[]);
            let owners: Vec<&str> = bindings
                .iter()
                .map(|binding| binding.action().name())
                .collect();
            assert!(!owners.is_empty(), "{key} is bound to nothing");
            assert!(
                owners.iter().all(|name| zoom.contains(name)),
                "{key} is also bound to {owners:?}"
            );
        }
    }

    /// §7.2: a launch opens the transcripts at the size the reader last left them
    /// at, and a file nothing has zoomed opens at the design's own.
    #[gpui_kit::test]
    fn the_stored_zoom_is_the_one_the_transcripts_open_at(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let root = temp_root("zoom-install");
        let state = AppState {
            zoom: 1.4,
            ..AppState::default()
        };
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root).zoom, 1.4);

        cx.update(|cx| {
            let log = crate::AppLog::open(&root);
            crate::Shell::new(
                root.clone(),
                log,
                state,
                store::model_cache::ModelCache::default(),
            )
            .install(cx);
            install(cx);
            assert_eq!(
                TranscriptZoom::get(cx),
                TranscriptZoom(1.4),
                "the reader's own size"
            );
        });

        // A file that never had one: the design's.
        std::fs::remove_file(root.app_json()).unwrap();
        cx.update(|cx| {
            cx.set_global(crate::Shell::new(
                root.clone(),
                crate::AppLog::open(&root),
                AppState::default(),
                store::model_cache::ModelCache::default(),
            ));
            install(cx);
            assert_eq!(TranscriptZoom::get(cx), TranscriptZoom::default());
        });
        let _ = std::fs::remove_dir_all(root.path());
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
                "Settings…",
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
                MenuItem::Action {
                    action, os_action, ..
                } => {
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
            [
                "Undo:true",
                "Redo:true",
                "Cut:true",
                "Copy:true",
                "Paste:true",
                "SelectAll:true"
            ],
            "every editing item is an OS action the text field handles"
        );
    }

    #[test]
    fn the_window_menu_offers_the_tabs_and_the_window() {
        let menus = menus();
        let window = menus.last().expect("the Window menu");
        assert_eq!(window.name.as_ref(), "Window");
        assert_eq!(
            names(window),
            [
                "Select Next Tab",
                "Select Previous Tab",
                "Select Last Tab",
                "Minimize",
                "Zoom",
            ],
            "the tab items come first, in the order the keys walk them"
        );
        // They are the workspace's actions, not copies of them: the workspace binds
        // ⌃⇥, ⌃⇧⇥ and ⌘9, and the menu item is what carries the key equivalent.
        let actions: Vec<&str> = window
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.name()),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            [
                "workspace::SelectNextTab",
                "workspace::SelectPreviousTab",
                "workspace::SelectLastTab",
                "evo_desktop::MinimizeWindow",
                "evo_desktop::ZoomWindow",
            ]
        );
    }

    /// The names of a menu's items, separators left out.
    fn names(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect()
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

    /// §9.8: ⌘Q is the *window's* key — a hold — and not the Quit item's. A menu
    /// item's key equivalent is fired by AppKit before any window sees the key,
    /// and a hold is both halves of the keystroke, so the item carries none: the
    /// keymap is where the shortcut is, and this is what keeps it that way.
    #[gpui_kit::test]
    fn the_quit_item_has_no_key_of_its_own(cx: &mut gpui_kit::TestAppContext) {
        cx.update(install);
        let keymap = cx.update(|cx| cx.key_bindings());
        let keymap = keymap.borrow();

        // Exactly what the menu bar draws for the item: nothing.
        let quit: Vec<String> = keymap
            .bindings_for_action(&QuitApp)
            .flat_map(|binding| binding.keystrokes())
            .map(|keystroke| keystroke.to_string())
            .collect();
        assert!(
            quit.is_empty(),
            "the Quit item must show no shortcut, or macOS quits on the key-down: {quit:?}"
        );

        let hold: Vec<(String, bool)> = keymap
            .bindings_for_action(&workspace::HoldToQuit)
            .flat_map(|binding| binding.keystrokes())
            .map(|keystroke| (keystroke.key().to_owned(), keystroke.modifiers().platform))
            .collect();
        assert_eq!(
            hold,
            [("q".to_owned(), true)],
            "and ⌘Q is the window's own hold"
        );
    }
}
