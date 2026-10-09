//! terminal — a shell in a pane of the window.
//!
//! The pane is a real terminal, not an imitation of one: a pseudo-terminal with
//! the user's own shell on it ([`portable_pty`]), and `alacritty_terminal`'s
//! `Term` parsing the escape stream into a grid. What the shell prints is the
//! shell's own drawing — programs see a terminal and behave as they do in one
//! (`ls` colours its columns, a pager takes the screen, a shell's line editor
//! keeps its own history) — and the pane's job is to put that grid on the screen
//! and the keyboard into it.
//!
//! ```text
//!   pty master ──▶ reader thread ──▶ Term ──┐
//!        ▲         (parses bytes)      │   │ one lock
//!        │                             ▼   ▼
//!   keyboard ─────────────────────▶ TerminalPane ──▶ gpui
//!        ▲                             │
//!        └── the grid's own answers ◀──┘
//! ```
//!
//! ## Using it
//!
//! [`TerminalPane`] is an ordinary gpui view: build it with the directory the
//! shell should start in, put it wherever the window wants a terminal, and size
//! it with [`TerminalPane::set_size`] if the caller is the one who knows how big
//! it is. Left to itself it measures its own box and resizes the pty to match,
//! which is what a pane inside a split does.
//!
//! ```no_run
//! # use std::path::PathBuf;
//! # use gpui_kit::{Context, Window, div, prelude::*};
//! # use terminal::TerminalPane;
//! fn terminal(cx: &mut Context<SomeView>, window: &mut Window) -> impl IntoElement {
//!     let pane = cx.new(|cx| TerminalPane::new(PathBuf::from("/tmp"), window, cx));
//!     div().size_full().child(pane)
//! }
//! # struct SomeView;
//! # impl Render for SomeView {
//! #     fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement { div() }
//! # }
//! ```
//!
//! The view carries a `"Terminal"` key context, so key bindings that must only
//! apply in a terminal — or must not — have somewhere to say so.
//!
//! ## What the shell is started with
//!
//! `$SHELL`, else `/bin/zsh`, in the directory the caller gives, with
//! `TERM=xterm-256color`. It inherits this process's environment, which is the
//! login environment the app adopted at start-up (see the app's `login_env`), so
//! the terminal starts with the `PATH` a terminal window on this machine would
//! have.

mod keys;
mod palette;
mod pane;
mod pty;

use gpui_kit::{App, Global, KeyBinding, SharedString};

pub use pane::{TerminalEvent, TerminalPane};

/// The key context every pane carries.
pub const KEY_CONTEXT: &str = "Terminal";

gpui_kit::actions!(
    terminal,
    [
        /// Tab, sent to the shell (its completion) rather than moving the focus.
        SendTab,
        /// Shift-Tab, sent to the shell rather than moving the focus back.
        SendBackTab,
        /// Ctrl-C, sent to the shell as an interrupt. On macOS it already is one;
        /// elsewhere the kit binds it to Copy, which a terminal must not take.
        SendInterrupt,
    ]
);

/// The font every terminal draws with: the app's "Terminal Font" settings.
///
/// One global for the app, so a Save in Settings is seen by every open pane on
/// its next frame. Absent, a pane draws with the theme's own monospace.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFont {
    pub family: SharedString,
    pub size: f32,
}

impl Global for TerminalFont {}

impl TerminalFont {
    /// Make this the font every terminal draws with, and redraw the windows.
    pub fn set(self, cx: &mut App) {
        if cx.try_global::<TerminalFont>() == Some(&self) {
            return;
        }
        cx.set_global(self);
        cx.refresh_windows();
    }
}

/// Set once the terminal's keys are in the keymap.
struct KeysBound;
impl Global for KeysBound {}

/// Bind the keys a terminal must keep from the window: Tab and Shift-Tab, which
/// the kit's root otherwise spends on moving the focus, and Ctrl-C. Bound in the
/// terminal's own context, which is deeper than the root's, so these win only
/// while a terminal has the keyboard. Idempotent.
pub fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", SendBackTab, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-c", SendInterrupt, Some(KEY_CONTEXT)),
    ]);
}
