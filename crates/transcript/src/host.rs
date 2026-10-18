//! The host's file system, where this platform has one.
//!
//! Two things this crate asks of the machine it runs on: the bytes of a picture a
//! row points at on disk, and whether a path a message names is really there (a
//! path is only linked when it resolves to a file). A browser has no file system —
//! a path names nothing there, and a file a reader picks or drops arrives as bytes
//! through the web platform, not as a path — so both answers are behind this seam
//! and the web build answers with nothing.
//!
//! It is a seam rather than a `std::fs` call at each site because that is what the
//! platform boundary is: `std::fs` *compiles* for `wasm32-unknown-unknown` — its
//! backend there is a set of stubs that return `Err` — so a call left in the UI
//! would look like it might read a file, and would keep looking that way until the
//! standard library turns those stubs into the compile error it says it wants them
//! to be. One `cfg` here, and every call site says what it means.

use std::path::{Path, PathBuf};

/// The bytes of the file at `path`, when it is a file no larger than `max_bytes`.
///
/// The size is checked against the file's own metadata before a byte is read, so a
/// path that names something enormous is refused rather than loaded.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn read_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    std::fs::read(path).ok()
}

/// Nothing in a browser: there is no file system to read a path from.
#[cfg(target_family = "wasm")]
pub(crate) fn read_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let _ = (path, max_bytes);
    None
}

/// Whether `path` names something that is there.
///
/// The one look at the file system a linkified row makes: a path is only linked when
/// it resolves to a file, so a token that reads like one but is not stays the words
/// the reader wrote.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn exists(path: &Path) -> bool {
    path.exists()
}

/// Nothing in a browser: no file system, and a path in a message is not a link there.
#[cfg(target_family = "wasm")]
pub(crate) fn exists(path: &Path) -> bool {
    let _ = path;
    false
}

/// The reader's home directory, for a `~/…` path.
///
/// Empty when the platform does not name one — the same answer this has always given
/// a process started without `HOME` — and a browser names none: it has no home
/// directory, so `~/…` is a path nothing resolves.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Nothing in a browser: no home directory to name.
#[cfg(target_family = "wasm")]
pub(crate) fn home_dir() -> PathBuf {
    PathBuf::new()
}
