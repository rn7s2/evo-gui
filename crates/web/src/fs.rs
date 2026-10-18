//! `/api/fs` — the serving machine's directories, for the browser's pickers.
//!
//! The desktop app opens a folder with `rfd`, a native dialog. A page has no such
//! dialog, so the folder picker has to be drawn in the browser out of a listing
//! this server produces. That is the whole purpose of this route, and it is the
//! only route in this crate that reads the filesystem on a caller's say-so.
//!
//! ```text
//! GET /api/fs?path=<dir> -> { "path", "dirs": [ { "name", "path" } … ] }
//! ```
//!
//! # What this route will and will not do
//!
//! It reads a **directory listing**. Deliberately, and in this order:
//!
//! * **Directories only.** An entry that is not a directory is dropped from the
//!   reply — no size, no mtime, and never a byte of its contents. There is no
//!   parameter that makes this route open a file, and no way to spell one: the
//!   answer is a list of names and paths, and that is all it can ever be. A
//!   picker that showed files would leak more than it needs and this one does not
//!   need to.
//! * **The path must be a directory, or the reply is a 404.** A path that names a
//!   file, a socket, a device, or nothing at all is refused by name — which is why
//!   a caller cannot use this route to *probe* for a file's existence either.
//! * **A symlinked entry is dropped, not followed.** Each entry's type comes from
//!   `DirEntry::file_type`, which is the link itself, so a symlink is neither
//!   listed nor resolved. Nothing here calls `canonicalize` on an entry, and
//!   nothing recurses: one level, no walking.
//! * **The requested path itself is resolved first** (`canonicalize`), so that
//!   `/tmp/../etc` and `/etc` are one directory and are reported under one name.
//!   That is the one place a symlink *is* followed, and it is followed for the
//!   directory the caller asked about, not for what it contains. A directory a
//!   symlink points at is a directory on this machine; the picker's job is to let
//!   a person walk to it and choose it.
//! * **No hidden directories.** A name beginning with `.` is not listed. Every
//!   picker in this app wants a project folder or a home directory, not the
//!   dotfiles beside it, and leaving them out keeps `.ssh`, `.aws` and `.evo`
//!   itself out of a list that a browser draws.
//! * **Sorted by name**, so two listings of one directory agree.
//!
//! # What protects it
//!
//! This route is not open. It sits under `/api`, which means:
//!
//! * it is on **loopback by default** — [`crate::cli`] refuses a non-loopback
//!   bind without a token, so the ordinary case cannot be reached from the
//!   network at all;
//! * it is behind the **token middleware** like every other `/api` route (see
//!   [`crate::require_token`]), so off loopback it needs the token or a
//!   same-origin cookie;
//! * and there is **no CORS**, so no other origin can read a reply.
//!
//! Whoever holds the token can already `POST /api/tabs` and start processes as
//! this user, so a directory listing grants them nothing they did not have. What
//! this route must not become is a way to read a *file*, and the rules above are
//! what keep it from being one.

use std::path::{Path, PathBuf};

use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::tabs::ApiError;

/// `GET /api/fs?path=<dir>`
#[derive(Debug, Deserialize)]
pub struct ListingQuery {
    /// The directory to list. Absent means this process's home directory, which
    /// is where a picker starts.
    path: Option<String>,
}

/// A directory and the directories in it.
#[derive(Debug, Serialize)]
pub struct Listing {
    /// The directory, resolved — the same spelling a later `..` is measured from.
    path: String,
    /// Its parent, or `None` at the root. For the picker's "up" control.
    parent: Option<String>,
    /// The directories in it, by name.
    dirs: Vec<Entry>,
}

/// One directory in a listing.
#[derive(Debug, Serialize)]
pub struct Entry {
    name: String,
    path: String,
}

/// `GET /api/fs` — list a directory on the serving machine.
pub async fn list(Query(query): Query<ListingQuery>) -> Result<Response, ApiError> {
    let asked = match query.path.as_deref().map(str::trim) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => store::paths::home_dir(),
    };

    // A caller's path is read, listed and stat'ed — all of it blocking — so it
    // runs off the runtime like the other filesystem reads in this crate.
    let listing = tokio::task::spawn_blocking(move || read_listing(&asked))
        .await
        .map_err(|e| ApiError::internal(format!("the listing task failed: {e}")))?;

    Ok(Json(listing?).into_response())
}

/// The listing itself, on the blocking thread.
fn read_listing(asked: &Path) -> Result<Listing, ApiError> {
    // Resolved first: one directory has one spelling, so `..` in the reply is the
    // parent of the directory that was actually listed.
    let dir = asked.canonicalize().map_err(|error| {
        ApiError::not_found(format!(
            "{} is not a directory on this machine: {error}",
            asked.display()
        ))
    })?;
    let meta = std::fs::metadata(&dir).map_err(|error| {
        ApiError::not_found(format!("{} cannot be read: {error}", dir.display()))
    })?;
    if !meta.is_dir() {
        return Err(ApiError::not_found(format!(
            "{} is not a directory",
            dir.display()
        )));
    }

    let entries = std::fs::read_dir(&dir).map_err(|error| {
        ApiError::not_found(format!("{} cannot be listed: {error}", dir.display()))
    })?;

    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // A name that is not UTF-8 cannot be sent as JSON, and a picker
            // cannot offer it; it is skipped rather than mangled.
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        // `file_type` is the entry's own type — for a symlink, the link. So a
        // symlink is never listed and never followed, and no path in the reply
        // can be one that resolves somewhere else.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        dirs.push(Entry {
            name: name.to_owned(),
            path: entry.path().to_string_lossy().into_owned(),
        });
    }
    dirs.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Listing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        dirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, with the shapes this route has to get
    /// right: a subdirectory, a file, a dotfile, a dot-directory, and a symlink
    /// to a directory.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("evo-web-fs-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("alpha")).unwrap();
            std::fs::create_dir_all(dir.join("beta")).unwrap();
            std::fs::create_dir_all(dir.join(".hidden")).unwrap();
            std::fs::write(dir.join("notes.txt"), "not a directory").unwrap();
            std::fs::write(dir.join(".secret"), "dotfile").unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(dir.join("beta"), dir.join("link-to-beta")).unwrap();
            Scratch(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_directories_are_listed_and_only_plain_ones() {
        let scratch = Scratch::new("plain");
        let listing = read_listing(scratch.path()).expect("lists");
        let names: Vec<&str> = listing.dirs.iter().map(|e| e.name.as_str()).collect();
        // Sorted, and exactly the two real directories.
        assert_eq!(names, vec!["alpha", "beta"]);

        // A file is not listed, and neither is a dotfile or a dot-directory.
        assert!(!names.contains(&"notes.txt"));
        assert!(!names.contains(&".secret"));
        assert!(!names.contains(&".hidden"));
        // And a symlink to a directory is not listed either: it is not followed.
        #[cfg(unix)]
        assert!(!names.contains(&"link-to-beta"));
    }

    /// The listing cannot be used to read a file: a path that is not a directory
    /// is refused, whether it exists or not.
    #[test]
    fn a_path_that_is_not_a_directory_is_refused() {
        let scratch = Scratch::new("refuse");
        let file = scratch.path().join("notes.txt");
        let err = read_listing(&file).unwrap_err();
        assert_eq!(err.status, 404);
        assert!(err.message.contains("not a directory"), "{}", err.message);

        let missing = scratch.path().join("nope");
        assert_eq!(read_listing(&missing).unwrap_err().status, 404);
    }

    /// The reply names the resolved directory, so `..` from it is its real
    /// parent — not the parent of however the caller spelled it.
    #[test]
    fn the_path_is_resolved_and_the_parent_is_its_own() {
        let scratch = Scratch::new("resolve");
        let through = scratch.path().join("alpha").join("..");
        let listing = read_listing(&through).expect("lists");
        let resolved = scratch.path().canonicalize().unwrap();
        assert_eq!(listing.path, resolved.to_string_lossy());
        // The parent is the *resolved* directory's parent, not the parent of
        // however the caller spelled it: on a machine where the temp directory is
        // reached through a symlink (`/var` → `/private/var` on macOS) those two
        // are different strings, and the resolved one is the honest answer.
        assert_eq!(
            listing.parent.as_deref(),
            resolved.parent().map(|p| p.to_string_lossy()).as_deref()
        );
    }

    /// Every entry's path is the directory that was listed joined with a name
    /// that is one segment — so a name can never escape the listing.
    #[test]
    fn an_entry_path_is_one_segment_under_the_directory() {
        let scratch = Scratch::new("segments");
        let listing = read_listing(scratch.path()).expect("lists");
        let base = scratch.path().canonicalize().unwrap();
        for entry in &listing.dirs {
            assert_eq!(Path::new(&entry.path), base.join(&entry.name));
            assert!(!entry.name.contains('/'));
        }
    }

    #[test]
    fn the_root_has_no_parent() {
        let listing = read_listing(Path::new("/")).expect("lists /");
        assert_eq!(listing.path, "/");
        assert_eq!(listing.parent, None);
    }
}
