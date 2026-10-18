//! evo-webapp: the browser frontend — the same gpui UI codebase, compiled to wasm.
//!
//! The desktop app (`crates/app`) and this crate are the two *shells* around one
//! shared UI. Everything the window shows lives in `workspace`, `transcript`,
//! `composer`, `agent_list`, `settings` and `widgets`; neither shell draws a row
//! of it. What differs is the process around the window, and there is almost
//! none here: a browser tab has no single instance to take, no window bounds to
//! remember, no child process to spawn and no menu bar. The page owns the first
//! four, and the last does not exist.
//!
//! ```text
//!   page ──▶ start()            (wasm-bindgen entry point)
//!              ├── web_init: panic hook + logging into the browser console
//!              ├── application(): gpui's web platform (WebGPU, WebGL2 fallback)
//!              ├── gpui_kit::init(cx): the component library
//!              └── one window: WorkspaceView, the same view the app opens
//! ```
//!
//! # Where the servers are
//!
//! A browser cannot spawn `evo-swarm`, so the window does not talk to a local
//! child the way the desktop app's tabs do. It talks to the **axum server**
//! (`crates/web`, the `evo-web` binary) that serves this very bundle: that
//! server runs the evo binaries on the machine it is on, and exposes them under
//! `/api/tabs/...`, adding the session's bearer token server-side so it never
//! reaches the page. [`transport`] is the one place that decides which of the
//! two a build talks to.
//!
//! The UI code above this crate is unaware of the difference: a tab is a tab.

#![cfg_attr(target_family = "wasm", allow(clippy::arc_with_non_send_sync))]

pub mod transport;

// The empty tab's catalog and history, fetched from the server that served this
// page. A browser has no child process to run them with, so the two documents come
// over `fetch` — through `swarm_client`'s browser client, which is the same one a
// tab's session uses.
#[cfg(target_family = "wasm")]
mod launcher;

#[cfg(target_family = "wasm")]
mod shell;

#[cfg(target_family = "wasm")]
pub use shell::start;

/// Where the frontend's own resources live, relative to the page.
///
/// The bundle is served by `evo-web` from the same origin as the API, so every
/// URL the frontend builds is a path on that origin — which is also why nothing
/// here needs a host, a port or a token.
pub const API_ROOT: &str = "/api";
