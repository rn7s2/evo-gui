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

pub use pane::TerminalPane;
