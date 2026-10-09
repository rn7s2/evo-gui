//! §13: the Settings panel, from the app's side.
//!
//! The panel itself is the `settings` crate: two binary paths, the theme, Save and
//! Cancel. What is the app's is everything around it — open it over the window,
//! persist what Save hands over into `app.json`, keep the [`Shell`] in step, and
//! give the window's *next* tab the new binaries. A tab that is already running
//! keeps what it started with, which is the panel's own note.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    div, px, App, AppContext as _, DismissEvent, Entity, InteractiveElement as _,
    ParentElement as _, Styled as _, TestSupportExt as _, Window,
};
use settings::{SettingsEvent, SettingsPanel, SettingsValues, PANEL_SIZE};
use store::app_state::AppState;

use crate::Shell;

/// The kit `Dialog`'s own content padding, each side: `gpui-component`'s `dialog.rs`
/// puts 16 pt between its border and the view it is given, and the dialog's width has
/// to leave room for it around the panel.
const DIALOG_PADDING: f32 = 16.;

/// The element the panel is drawn in: the dialog's content box, inside its padding and
/// border. Its id is the app's, so a test can assert the panel — and the controls on its
/// right edge — are inside the box they are drawn in rather than clipped by it.
pub const DIALOG_CONTENT_ID: &str = "settings-dialog-content";

/// Open the panel over the app's window — the Settings… menu item's half.
///
/// Returns the panel, which is what a test drives; the menu ignores it.
pub fn open(window: &mut Window, cx: &mut App) -> Entity<SettingsPanel> {
    let (values, log) = {
        let shell = cx.global::<Shell>();
        (
            SettingsValues::from_state(&shell.binaries, shell.theme, shell.terminal_font.clone()),
            shell.log.clone(),
        )
    };
    log.info(format!("settings: {}", opened(&values)));

    // `embedded`: the dialog draws the frame — the border, the fill and the padding —
    // and the panel takes the width it is given. So the dialog has to have room for the
    // panel's own 560 pt: the kit pads its content by [`DIALOG_PADDING`] on each side
    // (`gpui-component`'s `dialog.rs`), and `PANEL_SIZE.0` alone left the panel 32 pt
    // wider than the box it was drawn in — the right-hand controls clipped.
    let panel = cx.new(|cx| SettingsPanel::new(values, window, cx).embedded(true));
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
            // The frame is the dialog's: the panel is embedded, and this is the room
            // around it — `PANEL_SIZE.0` of content plus [`DIALOG_PADDING`] each side,
            // which is what the kit leaves between its border and the view it is
            // given. A dialog only `PANEL_SIZE.0` wide left the panel wider than its
            // box, and the right-hand controls were clipped by the edge.
            .close_button(false)
            // A press beside the panel lands on the dialog's backdrop, which the
            // kit draws invisible (and which closes the dialog by default). Every
            // gesture *around* the panel — a stray click, a drag that ends there —
            // would then throw away the paths somebody typed, without a word, and
            // Save and Cancel are the two answers this panel offers (§13). The
            // escape key still cancels, which is the deliberate way out.
            .overlay_closable(false)
            .w(px(PANEL_SIZE.0 + 2. * DIALOG_PADDING))
            .content(move |content, _window, _cx| {
                content.child(
                    div()
                        .id(DIALOG_CONTENT_ID)
                        .test_support()
                        .w_full()
                        .child(panel.clone()),
                )
            })
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
    state.terminal_font = values.terminal_font.clone();
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
        shell.terminal_font = values.terminal_font.clone();
    }

    // Read the new binaries' versions again: the About dialog's second line and the
    // empty tab's "evo-swarm not found" line both follow what was just saved (§9.4).
    crate::about::start(cx);

    // A different `evo-swarm` has a different catalog (§9.4): read it again in the
    // background, exactly as a launch does.
    crate::startup::refresh_catalog(cx);

    // The next tab this window opens starts with them. A running tab keeps the
    // binaries it started with, and the panel says so in its own note.
    if let Some(view) = crate::quit::view(cx) {
        let env = Arc::new(crate::launch_env(cx));
        view.update(cx, |view, cx| view.set_launch_env(env, cx));
    }
}

/// The one line both halves log: what was chosen, in the app's own words.
fn opened(values: &SettingsValues) -> String {
    format!(
        "evo-swarm {}, evo-agent {}, theme {}, terminal font {}",
        values.evo_swarm.display(),
        values.evo_agent.display(),
        match values.theme {
            store::app_state::Theme::System => "system",
            store::app_state::Theme::Light => "light",
            store::app_state::Theme::Dark => "dark",
        },
        values.terminal_font,
    )
}
