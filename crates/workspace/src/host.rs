//! What this window cannot do for itself, because the machine it draws on is not
//! the machine the work happens on.
//!
//! Three things every desktop window does by reaching out of the process: it puts
//! a system **dialog** on the screen, it reads a **file**, and it **runs a
//! binary** to get a document out of it. A browser can do none of them — there is
//! no panel to open, its filesystem is the server's rather than the page's, and
//! there is no child process at all — so each is behind a seam here, answered by
//! target.
//!
//! ```text
//! desktop  dialog: rfd's panel     read: std::fs         run: store::cli, a real child
//! browser  dialog: (not wired)     read: (not wired)     run: (not wired) → the axum server
//! ```
//!
//! ## The browser side today
//!
//! **One of these is now a real answer.** A folder is how a session starts, and a
//! browser has no dialog to ask for one — so [`ENTERS_FOLDER`] tells the empty tab
//! to draw a text field for the path instead of a card that would do nothing, and
//! that field is the browser's whole way in. The path names a folder on the machine
//! that served the page, which is the machine `POST /api/tabs` opens the tab on.
//!
//! The rest are still stubs, and honestly so. [`read_to_string`] answers `Err`, so
//! a note that cannot be read is the same "write it" path a first run takes, not a
//! panic and not a false "already right". [`run_json`] answers
//! [`CliError::NotFound`], which is what the desktop says about a binary that could
//! not be started — and in a browser that is the literal truth, since the binary is
//! on the serving machine and nothing here can reach it. Each caller already renders
//! that answer (a problem line under the cards, "… could not be run").
//!
//! None of it invents an HTTP client: a browser's reads go through the `evo-web`
//! axum server the way a tab's session does (`crates/web`), and wiring them is a
//! change to this file rather than to the UI that calls it.

use std::path::PathBuf;

use store::cli::CliError;

/// Ask the person for a folder, starting in `start` when the caller knows a good
/// one. `None` is a dialog that was cancelled.
///
/// The desktop answer is `rfd`'s async dialog: the caller neither blocks on it
/// nor waits for it. A browser has no such dialog — see [`ENTERS_FOLDER`].
#[cfg(not(target_family = "wasm"))]
pub async fn pick_folder(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    if let Some(folder) = start {
        dialog = dialog.set_directory(folder);
    }
    let picked = dialog.pick_folder().await?;
    Some(picked.path().to_path_buf())
}

/// Whether this build asks for a folder by **typing its path** rather than by
/// opening a system dialog.
///
/// True in a browser, and it is not a preference: a page has no file dialog to
/// open, and — the part that matters — a path typed there is not a path *here*.
/// The folder a browser session runs in is on the machine that served the page,
/// which is the machine `POST /api/tabs` opens the tab on. So the empty tab draws
/// a field for it instead of a card that would do nothing.
pub const ENTERS_FOLDER: bool = cfg!(target_family = "wasm");

/// The note under the folder field in a browser: whose machine the path names.
///
/// A person who types `/Users/me/coding/foo` into a page is naming a folder on the
/// serving machine, and one that is not there is refused by that machine rather
/// than by this one — which is worth saying before they try, not after.
pub const FOLDER_HINT: &str = "An absolute path on the machine that served this page.";

/// The placeholder a browser's folder field shows: a path, so what is expected of
/// it is visible before anything is typed.
pub const FOLDER_PLACEHOLDER: &str = "/Users/you/coding/foo";

/// What a file holds, or why it could not be read.
///
/// The desktop answer is this machine's own filesystem ([`std::fs`]). The browser
/// has no filesystem to read: a path is the *serving* machine's, and the file
/// behind it arrives over the server. Until that is wired, every read is an error
/// — which is the answer a file that is not there gives, and the one callers here
/// already handle.
#[cfg(not(target_family = "wasm"))]
pub fn read_to_string(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// [`read_to_string`] where there is no filesystem to read: a browser.
#[cfg(target_family = "wasm")]
pub fn read_to_string(path: &std::path::Path) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "{} is on the serving machine, not in this page",
            path.display()
        ),
    ))
}

/// The wall clock, in epoch milliseconds — the step clock's own reading, and the
/// one place a target difference is allowed to show.
///
/// `std::time` is unimplemented on `wasm32-unknown-unknown`: `SystemTime::now()`
/// **panics** there rather than returning an error (measured — see the panic in
/// `docs/` and the note in `launcher_data.rs`). A page has a clock, though: the
/// browser's own, which is what `Date.now()` reads. So the reading is taken per
/// target, and callers get the same number in the same unit either way.
#[cfg(not(target_family = "wasm"))]
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// [`now_millis`] from the page's own clock: `Date.now()` is already milliseconds
/// since the epoch, which is the unit this answers in.
#[cfg(target_family = "wasm")]
pub fn now_millis() -> u64 {
    js_sys::Date::now() as u64
}

/// The same path, as the machine spells it: symlinks followed, `.` and `..`
/// resolved, a trailing slash dropped.
///
/// The desktop answer is [`std::fs::canonicalize`]. A browser has no filesystem
/// to resolve against, and the paths it holds are the *serving* machine's — so
/// this returns the path as written, which is what the desktop's own fallback
/// does for a path that cannot be resolved. Two spellings of one path then
/// compare unequal on wasm; the comparison they feed is a display's, not a
/// decision's.
#[cfg(not(target_family = "wasm"))]
pub fn canonicalize(path: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// [`canonicalize`] where there is no filesystem: a browser.
#[cfg(target_family = "wasm")]
pub fn canonicalize(path: &std::path::Path) -> std::io::Result<PathBuf> {
    Ok(PathBuf::from(
        path.to_string_lossy().trim_end_matches('/'),
    ))
}

/// Run `bin args…` and read stdout as one JSON document (§9).
///
/// The desktop answer is [`store::cli::run_json`]'s bounded run: a real child, in
/// its own process group, stopped if it never answers (§9.1). A browser has no
/// child to run — see the module docs for what it answers instead.
#[cfg(not(target_family = "wasm"))]
pub fn run_json(bin: &std::path::Path, args: &[String]) -> Result<serde_json::Value, CliError> {
    store::cli::run_json(bin, args)
}

/// [`run_json`] where there is no process to run: a browser.
#[cfg(target_family = "wasm")]
pub fn run_json(bin: &std::path::Path, args: &[String]) -> Result<serde_json::Value, CliError> {
    let _ = args;
    Err(not_hosted(bin))
}

/// [`run_json`], for the one read that is read on a non-zero exit too
/// (`evo-swarm check --json` prints its problems and exits 1).
#[cfg(not(target_family = "wasm"))]
pub fn run_json_reporting(
    bin: &std::path::Path,
    args: &[String],
) -> Result<serde_json::Value, CliError> {
    store::cli::run_json_reporting(bin, args)
}

/// [`run_json_reporting`] where there is no process to run: a browser.
#[cfg(target_family = "wasm")]
pub fn run_json_reporting(
    bin: &std::path::Path,
    args: &[String],
) -> Result<serde_json::Value, CliError> {
    let _ = args;
    Err(not_hosted(bin))
}

/// The answer a read gives where there is nothing to run it on.
///
/// [`CliError::NotFound`] is the desktop's own "this binary could not be
/// started", which is exactly what happened: it is on the serving machine and no
/// process here can reach it. The `source` is what the hover shows, so it says
/// the whole of it.
#[cfg(target_family = "wasm")]
fn not_hosted(bin: &std::path::Path) -> CliError {
    CliError::NotFound {
        bin: bin.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a browser has no process to run it in: the binary is on the serving \
             machine, and the page reaches it through the server",
        ),
    }
}

