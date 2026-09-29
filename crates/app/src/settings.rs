//! §13: the Settings panel, from the app's side.
//!
//! The panel itself is the `settings` crate: two binary paths, the theme, Save and
//! Cancel. What is the app's is everything around it — open it over the window,
//! persist what Save hands over into `app.json`, keep the [`Shell`] in step, and
//! give the window's *next* tab the new binaries. A tab that is already running
//! keeps what it started with, which is the panel's own note.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{px, App, AppContext as _, DismissEvent, Entity, ParentElement as _, Window};
use settings::{SettingsEvent, SettingsPanel, SettingsValues, PANEL_SIZE};
use store::app_state::AppState;

use crate::Shell;

/// Open the panel over the app's window — the Settings… menu item's half.
///
/// Returns the panel, which is what a test drives; the menu ignores it.
pub fn open(window: &mut Window, cx: &mut App) -> Entity<SettingsPanel> {
    let (values, log) = {
        let shell = cx.global::<Shell>();
        (
            SettingsValues::from_state(&shell.binaries, shell.theme),
            shell.log.clone(),
        )
    };
    log.info(format!("settings: {}", opened(&values)));

    let panel = cx.new(|cx| SettingsPanel::new(values, window, cx));
    // Save persists what it hands over; Cancel (and Escape) emit nothing but their
    // own dismissal, so the app has nothing to undo.
    let saved = cx.subscribe(&panel, |_, event: &SettingsEvent, cx| match event {
        SettingsEvent::Saved(values) => apply(values, cx),
    });
    // Save and Cancel both end in the panel's own `DismissEvent`: the panel is the
    // content, the dialog is the app's, so closing it is the app's too. Cancelling
    // happens inside the panel's update, so the close waits for the end of the
    // effect cycle — the window is on the update stack until then (as it is for a
    // menu action).
    let dismissed = cx.subscribe(&panel, |_, _: &DismissEvent, cx| {
        let Some(window) = crate::launcher::window_of(cx) else {
            return;
        };
        cx.defer(move |cx| {
            let _ = window.update(cx, |_root, window, cx| window.close_dialog(cx));
        });
    });
    cx.global_mut::<Shell>().subscriptions.push(saved);
    cx.global_mut::<Shell>().subscriptions.push(dismissed);

    let framed = panel.clone();
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let panel = framed.clone();
        dialog
            // The panel is its own chrome — a border, a background, its own Save and
            // Cancel — so the dialog contributes the overlay and nothing else.
            .close_button(false)
            .w(px(PANEL_SIZE.0))
            .content(move |content, _window, _cx| content.child(panel.clone()))
    });
    panel
}

/// What Save means: `app.json`, the [`Shell`], and the window's next tab (§13).
pub fn apply(values: &SettingsValues, cx: &mut App) {
    let (root, log) = {
        let shell = cx.global::<Shell>();
        (shell.root.clone(), shell.log.clone())
    };

    // Loaded first: what Save does not touch — the window's bounds, the tab set,
    // the recents — is written back as it was.
    let mut state = AppState::load(&root);
    state.binaries = values.binaries();
    state.theme = values.theme;
    match state.save(&root) {
        Ok(()) => log.info(format!("settings: saved {}", opened(values))),
        Err(error) => log.error(format!(
            "could not save {}: {error}",
            root.app_json().display()
        )),
    }

    {
        let shell = cx.global_mut::<Shell>();
        shell.binaries = values.binaries();
        shell.theme = values.theme;
    }

    // The next tab this window opens starts with them. A running tab keeps the
    // binaries it started with, and the panel says so in its own note.
    if let Some(view) = crate::quit::view(cx) {
        let config = Arc::new(crate::swarm_config(cx));
        view.update(cx, |view, cx| view.set_swarm_config(config, cx));
    }
}

/// The one line both halves log: what was chosen, in the app's own words.
fn opened(values: &SettingsValues) -> String {
    format!(
        "evo-swarm {}, evo-agent {}, theme {}",
        values.evo_swarm.display(),
        values.evo_agent.display(),
        match values.theme {
            store::app_state::Theme::System => "system",
            store::app_state::Theme::Light => "light",
            store::app_state::Theme::Dark => "dark",
        }
    )
}
