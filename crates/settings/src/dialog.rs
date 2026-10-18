//! Asking the person for a file: the one place this app opens a system dialog.
//!
//! A file dialog is a thing a *desktop* has. `rfd`'s async dialog puts a real
//! panel on the screen — AppKit's on macOS — and hands back a path; a browser has
//! no such panel and no path to hand back, and `rfd`'s own web backend cannot pick
//! a folder at all (its `pick_folder` does not exist for `wasm32`, and its
//! `FileHandle` has no `path`). So the dialog is behind this seam, and each target
//! answers it its own way.
//!
//! ```text
//! desktop  choose a binary → rfd's panel → the path → the row's field
//! browser  (not wired yet) → the axum server's folder listing, when it exists
//! ```
//!
//! ## What the browser side is today
//!
//! **A stub, and honestly so.** [`pick_file`] answers `None`, which is exactly
//! what a cancelled dialog answers, so the panel's own behaviour is unchanged and
//! correct: the path stays as it was and nothing is checked. The button is inert
//! until the frontend asks the *server* for a listing — the same shape every other
//! host-only read in this port takes (`crates/workspace`'s folder picker, and the
//! empty tab's catalog and check probes) — and nothing here invents an HTTP
//! client. The seam is what makes that a change to this file rather than to the
//! panel.

use std::path::PathBuf;

/// Ask for a file, starting in `start` when the caller knows a good directory.
///
/// The dialog is `rfd`'s async one: the caller neither blocks on it nor waits for
/// it. `None` is a dialog that was cancelled — or, on a target with no dialog, one
/// that was never opened.
#[cfg(not(target_family = "wasm"))]
pub async fn pick_file(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    // Open where the file is, so a sibling is one click away.
    if let Some(folder) = start {
        dialog = dialog.set_directory(folder);
    }
    let picked = dialog.pick_file().await?;
    Some(picked.path().to_path_buf())
}

/// [`pick_file`] where there is no system dialog: a browser.
///
/// Not wired to anything yet — see the module docs. `None` is the answer a
/// cancelled dialog gives, so a caller needs no branch for it.
#[cfg(target_family = "wasm")]
pub async fn pick_file(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    let _ = (title, start);
    None
}
