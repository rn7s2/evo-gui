//! Unsaved Settings edits, and the quit (§13, §9.8).
//!
//! Settings is edited in a tab — the global file and a project's own `.evo/` —
//! and a tab belongs to the window. The app owns the quit (§9.8), so this is the
//! one place the two meet: before the quit marks itself as begun — before
//! `app.json` is written and before a single swarm is told to stop — the app asks
//! the window what its Settings editors are doing.
//!
//! ```text
//!   ⌘Q held / Quit / the window's ×
//!        └─▶ quit::begin_with ──▶ settings_guard::hold
//!                                   ├─ a write in flight ─▶ held: "Saving settings…"
//!                                   ├─ nothing dirty ─────▶ the quit runs
//!                                   └─ dirty ─▶ [Discard and Quit] [Keep Editing]
//!                                                 ├─ Discard ─▶ quit::proceed
//!                                                 └─ Keep    ─▶ the quit is dropped
//! ```
//!
//! **A write in flight is not a question.** The draft behind it must not be put
//! back or thrown away under it, and the write itself cannot be called off, so
//! the quit simply waits: the person is put in front of the editor that is
//! writing and told what is going on, and they quit again when it is done.
//! Nothing is remembered for them — no pending quit runs itself later.
//!
//! Both keyboard answers to the question are the safe one — Enter and Escape keep
//! editing, and only the explicit click on "Discard and Quit" throws anything
//! away. Nothing here saves: a draft that is not a valid file is not written
//! behind the person's back, and "Keep Editing" neither stops a swarm nor touches
//! a draft.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, ClickEvent, Entity, Global, ParentElement as _, Window};

use workspace::WorkspaceView;

use crate::launcher;

/// The "Keep Editing" answer, by element id, so a test can click it.
pub const KEEP_EDITING_ID: &str = "quit-settings-keep-editing";

/// The "Discard and Quit" answer.
pub const DISCARD_ID: &str = "quit-settings-discard";

const TITLE: &str = "Quit with unsaved Settings changes?";
const MESSAGE: &str = "Settings has unsaved edits. Quitting now throws them away.";
const KEEP_EDITING: &str = "Keep Editing";
const DISCARD: &str = "Discard and Quit";

/// What a quit during a write says (§13): the write is not a question, so there
/// is nothing to answer — only something to say.
const SAVING_TITLE: &str = "Saving settings…";
const SAVING_MESSAGE: &str = "Quit again once the write is done.";

/// The saving toast's id: a second ⌘Q while the same write is going replaces what
/// is on screen rather than stacking another toast.
struct Saving;

/// The one line the log gets for a quit held back by a write (§13).
pub const SAVING_HELD_LOG: &str = "quit held: a settings write is in flight";

/// Whether the quit guard's question is on screen (§13).
///
/// The quit it held back is still waiting on an answer, and nothing of the quit
/// has happened — so a second ⌘Q, or the × again, must not stack a second alert:
/// while this is set, a quit asked for is answered by the dialog already up.
/// Cleared by every way out of that dialog.
struct Asking(bool);
impl Global for Asking {}

/// Whether the guard is holding a quit back with its dialog (§9.8).
pub fn asking(cx: &App) -> bool {
    cx.has_global::<Asking>() && cx.global::<Asking>().0
}

fn set_asking(asking: bool, cx: &mut App) {
    cx.set_global(Asking(asking));
}

/// A quit was asked for: `true` means it is held back — either by the question
/// about unsaved edits, or by a write that is still going (§9.8, §13).
///
/// Called before anything of the quit has happened — no `app.json`, no
/// `shutdown` — so a quit answered "Keep Editing", or one that waits for a write,
/// leaves the session exactly as it was.
pub fn hold(cx: &mut App) -> bool {
    let Some(view) = crate::quit::view(cx) else {
        // No window, nothing on screen that could be holding a draft.
        return false;
    };
    if asking(cx) {
        // Already asking: the dialog that is up is the answer to this too.
        return true;
    }
    // A write in flight comes first, and it is asked about by nobody: it cannot
    // be called off, and the draft behind it must not be offered up for discarding.
    if view.read(cx).has_saving_settings(cx) {
        wait_for_the_write(cx, view);
        return true;
    }
    if !view.read(cx).has_dirty_settings(cx) {
        return false;
    }
    let Some(window) = launcher::window_of(cx) else {
        return false;
    };
    set_asking(true, cx);
    // The window is on the update stack when this is asked from its close hook
    // (and from a menu action, which is the same shape): the dialog goes up at
    // the end of the effect cycle, when it is free again.
    cx.defer(move |cx| {
        let _ = window.update(cx, |_root, window, cx| ask(window, cx, view));
    });
    true
}

/// A write is in flight (§13): the quit waits for it rather than asking anything.
///
/// The person is put in front of the editor that is writing — whose own status
/// line says what it is doing — and a toast says why the quit did not happen.
/// Nothing is remembered for them: they quit again when the write is done.
fn wait_for_the_write(cx: &mut App, view: Entity<WorkspaceView>) {
    cx.global::<crate::Shell>().log.info(SAVING_HELD_LOG);
    let Some(window) = launcher::window_of(cx) else {
        return;
    };
    cx.defer(move |cx| {
        let _ = window.update(cx, |_root, window, cx| {
            view.update(cx, |view, cx| {
                view.focus_dirty_settings(window, cx);
            });
            window.push_notification(
                Notification::info(SAVING_MESSAGE)
                    .title(SAVING_TITLE)
                    .id::<Saving>(),
                cx,
            );
        });
    });
}

/// Put the question on screen (§13).
fn ask(window: &mut Window, cx: &mut App, view: Entity<WorkspaceView>) {
    window.open_alert_dialog(cx, move |alert, _window, _cx| {
        let keep = {
            let view = view.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| keep_editing(window, cx, &view)
        };
        let discard = {
            let view = view.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                discard_and_quit(window, cx, &view)
            }
        };
        alert
            .title(TITLE)
            .description(MESSAGE)
            // The buttons are the app's rather than the dialog's own OK/Cancel:
            // with those, Enter (`Confirm`) and Escape (`Cancel`) are both wired
            // to the destructive answer. Here both keyboard ways out land on
            // "Keep Editing" — one for either — and only the click discards.
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new(DISCARD_ID)
                            .label(DISCARD)
                            .danger()
                            .on_click(discard),
                    )
                    .child(
                        Button::new(KEEP_EDITING_ID)
                            .label(KEEP_EDITING)
                            .primary()
                            .on_click(keep),
                    ),
            )
            .on_ok({
                let view = view.clone();
                move |_, window, cx| {
                    keep_editing(window, cx, &view);
                    true
                }
            })
            .on_cancel({
                let view = view.clone();
                move |_, window, cx| {
                    keep_editing(window, cx, &view);
                    true
                }
            })
            // The dialog can also go away without an answer (the window is
            // closed under it, say). Whatever the route, the guard is no longer
            // asking, so the next ⌘Q asks again rather than doing nothing.
            .on_close(|_, _, cx| set_asking(false, cx))
    });
}

/// "Keep Editing": the quit is dropped, and the person is shown what is dirty.
fn keep_editing(window: &mut Window, cx: &mut App, view: &Entity<WorkspaceView>) {
    set_asking(false, cx);
    window.close_dialog(cx);
    // Nothing about the session changes — the swarms keep running and the drafts
    // keep their edits — but the edits are what the person came back for, so the
    // tab holding them is what they are shown.
    view.update(cx, |view, cx| view.focus_dirty_settings(window, cx));
}

/// "Discard and Quit": the drafts go, and the quit they held back runs.
fn discard_and_quit(window: &mut Window, cx: &mut App, view: &Entity<WorkspaceView>) {
    set_asking(false, cx);
    window.close_dialog(cx);
    // A write may have started while the question was up. Discarding is not what
    // the person is offered then: the draft behind a write is frozen, and the
    // write itself cannot be called off, so this answer waits exactly as a quit
    // asked during the write does.
    let saving = view.read(cx).has_saving_settings(cx);
    if saving {
        wait_for_the_write(cx, view.clone());
        return;
    }
    // Putting a draft back is an input's own work, and an input lives in a
    // window: the window the question was asked in is the one to hand over.
    view.update(cx, |view, cx| view.discard_settings_changes(window, cx));
    // Past the guard on purpose: the answer *is* "discard", so a window that
    // still reports a draft afterwards — a file that could not be put back — must
    // not put the same question up again.
    crate::quit::proceed(cx);
}
