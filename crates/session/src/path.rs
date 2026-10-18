//! The path questions this UI asks, answered the same way on every target.
//!
//! There is exactly one of them that is not obvious: **`Path::is_absolute` is
//! `false` for every path on `wasm32-unknown-unknown`**, `/Users/you/foo`
//! included. That is not a guess. On that target `std` compiles its path support
//! from `sys/path/unsupported_backslash.rs` (`sys/path/mod.rs` selects it for
//! everything but Windows/SGX/UEFI/Cygwin), where:
//!
//! ```ignore
//! pub fn parse_prefix(_: &OsStr) -> Option<Prefix<'_>> { None }
//! pub(crate) fn is_absolute(path: &Path) -> bool {
//!     path.has_root() && path.prefix().is_some()
//! }
//! ```
//!
//! `parse_prefix` returns `None` for every input, so the second half of that
//! conjunction can never hold. Measured on the shipped bundle: `is_absolute()`
//! is `false` for `/Users/you/coding/foo`, while `has_root()` is `true`.
//!
//! A browser build that asked `is_absolute` therefore refused every path a person
//! could type, drew no link for a path a transcript named, and resolved no
//! attachment — silently, because `false` is a perfectly ordinary answer.
//!
//! [`is_absolute`] is the question those call sites meant. [`Path::has_root`] is
//! the honest spelling and works everywhere: it is `components().has_root()`, and
//! `components()` is not target-specific. On a unix the two agree exactly (a
//! rooted path *is* an absolute one); on Windows `has_root` alone would also
//! accept a drive-relative `\foo`, which this app never sees — the work happens on
//! the unix machine that serves it.

use std::path::Path;

/// Whether `path` names a place from the root, on every target.
///
/// The replacement for `Path::is_absolute` in this workspace: see the module note
/// for why the std method cannot answer on wasm.
pub fn is_absolute(path: &Path) -> bool {
    path.has_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_rooted_path_is_absolute() {
        assert!(is_absolute(Path::new("/Users/you/coding/foo")));
        assert!(is_absolute(Path::new("/tmp/a.png")));
        assert!(is_absolute(Path::new("/")));
    }

    #[test]
    fn a_relative_path_is_not() {
        assert!(!is_absolute(Path::new("relative/x")));
        assert!(!is_absolute(Path::new("foo.png")));
        assert!(!is_absolute(Path::new("./a/b")));
    }

    #[test]
    fn it_agrees_with_std_on_a_unix() {
        // The two are the same question here, which is why this helper can stand
        // in for the std method at every call site on the platforms that build
        // natively. On wasm only this one answers at all.
        for p in ["/a", "/a/b", "a", "./a", "..", "/"] {
            let path = PathBuf::from(p);
            assert_eq!(is_absolute(&path), path.is_absolute(), "{p}");
        }
    }

    #[test]
    fn a_home_shortcut_is_not_absolute() {
        // `~/notes` is a relative path until something expands the `~`.
        assert!(!is_absolute(Path::new("~/notes")));
    }
}
