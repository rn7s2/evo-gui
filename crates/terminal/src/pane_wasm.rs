//! The terminal pane, in a build that has no terminal to put in it.
//!
//! The desktop pane ([`crate::pane`]) is a pty, the user's shell on it, and
//! `alacritty_terminal`'s grid parsing what the shell prints. A browser has none
//! of those: no pseudo-terminal, no child process to put on one, no thread to
//! drain it with — and `alacritty_terminal` does not even *build* for
//! `wasm32-unknown-unknown` (see `Cargo.toml`). So the grid goes with the shell
//! it was there to draw.
//!
//! What is kept is the pane's **surface**: the same [`TerminalPane`] type, built
//! the same way (`new`), with the same key context and the same
//! [`TerminalEvent`], so `crates/workspace`'s terminal column compiles against
//! one name on both targets and its own layout, header and split handle are
//! unchanged. Only the body differs, and it says plainly what is not here.
//!
//! This is a **stub with a seam's shape, not a wired one**: a terminal in the
//! browser is the serving machine's, reached through the axum server
//! (`crates/web`) the way a tab's session is, and nothing here talks to it yet.
//! When it does, it replaces this file's body — the type, its constructor and
//! its event stay as they are.

use std::path::PathBuf;

use gpui_kit::component::{ActiveTheme as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, App, Context, EventEmitter, FocusHandle, Focusable, Render, SharedString, Window,
};

use crate::KEY_CONTEXT;

/// What a pane tells its owner.
///
/// The same two variants the desktop pane has, so a host that subscribes to
/// [`TerminalEvent::Exited`] compiles unchanged. Nothing emits it here: there is
/// no shell to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The shell ended (`exit`, `^D`, or killed): there is nothing left to type
    /// into, and the owner should put the pane away.
    Exited,
}

/// The terminal pane, on a target with no terminal behind it.
///
/// It holds the pane's own key context and focus handle — so the tab's focus
/// bookkeeping (`TerminalPane::focus_handle`, `toggle_terminal`) behaves — and
/// nothing else.
pub struct TerminalPane {
    /// The keyboard's handle on the pane. Kept so the tab can focus the pane and
    /// ask whether it holds the keyboard, which is what opening and closing the
    /// terminal column does.
    focus: FocusHandle,
}

impl EventEmitter<TerminalEvent> for TerminalPane {}

impl TerminalPane {
    /// The pane, in a build with no shell to start.
    ///
    /// `working_dir` is taken and dropped: the desktop pane starts a shell in it,
    /// and this one has no shell. The signature is the desktop's on purpose — the
    /// tab calls it the same way on both targets.
    pub fn new(working_dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = working_dir;
        crate::bind_keys(cx);
        TerminalPane {
            focus: cx.focus_handle(),
        }
    }

    /// Resize the terminal to `cols` cells across and `rows` down.
    ///
    /// A no-op here, and deliberately kept: the desktop pane resizes its grid and
    /// its pty, and a caller that knows the pane's size (the tab page's measure)
    /// calls this on both targets. There is nothing behind this one to resize.
    pub fn set_size(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let _ = (cols, rows);
        let _ = cx;
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("terminal-pane")
            .size_full()
            .bg(theme.background)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(
                div()
                    .text_color(theme.foreground)
                    .font_semibold()
                    .child(SharedString::from(
                        "The terminal is only available in the desktop app",
                    )),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .min_w_0()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(
                        "A browser has no shell to run: the terminal opens a process on \
                         the machine the desktop app is on.",
                    )),
            )
    }
}
