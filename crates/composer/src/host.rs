//! The host's file system, where this platform has one.
//!
//! Two things this crate asks of the machine it runs on: the first bytes of a file
//! an attachment names (to tell a picture from a file by its magic number), and the
//! bytes of an image file (to make its tile's thumbnail). Both are the file system,
//! and a browser has none: a path names nothing there, and a file a reader picks or
//! drops arrives as bytes through the web platform rather than as a path.
//!
//! Behind a seam rather than a `std::fs` call at each site, because that is what the
//! platform boundary is: `std::fs` *compiles* for `wasm32-unknown-unknown` — its
//! backend there is a set of stubs returning `Err` — so a call left in the UI would
//! read as though it could open a file. One `cfg` here, and each call site says what
//! it means.

use std::path::Path;

/// The first `head.len()` bytes of the file at `path`, and how many there were.
///
/// A file that cannot be opened, or is shorter than `head`, gives what it has: the
/// caller is sniffing a magic number, and half of one is not one.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn read_head(path: &Path, head: &mut [u8]) -> Option<usize> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    file.read(head).ok()
}

/// Nothing in a browser: there is no file to open at a path.
#[cfg(target_family = "wasm")]
pub(crate) fn read_head(path: &Path, head: &mut [u8]) -> Option<usize> {
    let _ = (path, head);
    None
}

/// The whole of the file at `path`.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn read_file(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Nothing in a browser: there is no file to read at a path.
#[cfg(target_family = "wasm")]
pub(crate) fn read_file(path: &Path) -> Option<Vec<u8>> {
    let _ = path;
    None
}
