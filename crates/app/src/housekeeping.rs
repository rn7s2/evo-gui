//! §6 layout hygiene: the `tabs/<id>/` directories the app is done with.
//!
//! Starting a swarm mints a tab directory holding its ready file, its
//! `swarm.log` and its lanes' logs. Those are evidence — §9.7 shows the log's
//! tail when a boot fails — so a directory is never removed while it might still
//! be wanted: not one `app.json` names as an open tab, and not one touched inside
//! [`TAB_DIR_TTL`]. What is older than that belongs to a swarm nobody has looked
//! at for a week, and is removed. The walk happens at startup, on a thread of
//! its own.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use store::paths::Root;

/// How long an untouched tab directory is kept.
pub const TAB_DIR_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What one prune did, for the log line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The tab directories removed, by id.
    pub removed: Vec<String>,
    /// How many were kept: young enough, spoken for, or not a directory.
    pub kept: usize,
    /// How many could not be removed.
    pub failed: usize,
}

/// Remove the tab directories last touched longer ago than `ttl`.
///
/// `keep` is what `app.json` has open — a tab that is coming back keeps its
/// directory even if it is old. A missing `tabs/` is not an error: there is
/// nothing to prune yet.
pub fn prune_tab_dirs(
    root: &Root,
    keep: &[String],
    ttl: Duration,
    now: SystemTime,
) -> io::Result<Pruned> {
    let entries = match fs::read_dir(root.tabs_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Pruned::default()),
        Err(error) => return Err(error),
    };

    let mut pruned = Pruned::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            pruned.kept += 1;
            continue;
        }
        let Some(id) = file_name(&path) else {
            pruned.kept += 1;
            continue;
        };
        if keep.iter().any(|open| open == &id) {
            pruned.kept += 1;
            continue;
        }
        let old = last_touched(&path)
            .and_then(|touched| now.duration_since(touched).ok())
            .is_some_and(|age| age > ttl);
        if !old {
            // Either it is recent, or its times cannot be read: a directory the
            // app cannot date is one it does not throw away.
            pruned.kept += 1;
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => pruned.removed.push(id),
            Err(_) => pruned.failed += 1,
        }
    }
    Ok(pruned)
}

/// The newest of the directory's own time and its entries'.
///
/// A swarm appends to `swarm.log` as it talks, which does not change the
/// directory's own time — so the files are what say when the tab was last alive.
fn last_touched(dir: &Path) -> Option<SystemTime> {
    let mut newest = fs::metadata(dir).and_then(|meta| meta.modified()).ok();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
            newest = Some(match newest {
                Some(newest) if newest > modified => newest,
                _ => modified,
            });
        }
    }
    newest
}

fn file_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    /// A tab directory whose newest file is `age` old — the shape a swarm leaves.
    fn tab_dir(root: &Root, id: &str, age: Duration, now: SystemTime) -> String {
        let dir = root
            .ensure_tab_dir(&store::paths::TabId::parse(id).expect("a safe id"))
            .unwrap();
        for name in ["swarm.log", "tab.json"] {
            let file = File::create(dir.join(name)).unwrap();
            file.set_modified(now - age).unwrap();
        }
        // The directory itself too, so the test does not depend on what creating
        // a file does to its parent's time.
        File::open(&dir).unwrap().set_modified(now - age).unwrap();
        id.to_owned()
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    #[test]
    fn a_directory_older_than_the_ttl_is_removed() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-old-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let now = now();
        tab_dir(&root, "ancient", TAB_DIR_TTL * 2, now);
        tab_dir(&root, "recent", Duration::from_secs(60), now);

        let pruned = prune_tab_dirs(&root, &[], TAB_DIR_TTL, now).unwrap();
        assert_eq!(pruned.removed, ["ancient"]);
        assert_eq!(pruned.kept, 1);
        assert_eq!(pruned.failed, 0);
        assert!(!root
            .tab_dir(&store::paths::TabId::parse("ancient").unwrap())
            .exists());
        assert!(root
            .tab_dir(&store::paths::TabId::parse("recent").unwrap())
            .exists());
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn a_directory_touched_inside_the_ttl_is_kept() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-young-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let now = now();
        // Six days: inside the week, however old the directory's own time is.
        tab_dir(
            &root,
            "yesterday-ish",
            Duration::from_secs(6 * 24 * 60 * 60),
            now,
        );

        let pruned = prune_tab_dirs(&root, &[], TAB_DIR_TTL, now).unwrap();
        assert!(pruned.removed.is_empty(), "{pruned:?}");
        assert_eq!(pruned.kept, 1);
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn a_tab_app_json_has_open_is_kept_however_old() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-open-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let now = now();
        tab_dir(&root, "open-tab", TAB_DIR_TTL * 10, now);
        tab_dir(&root, "closed-tab", TAB_DIR_TTL * 10, now);

        let pruned = prune_tab_dirs(&root, &["open-tab".to_owned()], TAB_DIR_TTL, now).unwrap();
        assert_eq!(pruned.removed, ["closed-tab"]);
        assert_eq!(pruned.kept, 1, "the open one, old as it is");
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn the_newest_file_decides_the_age() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-newest-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let now = now();
        let id = tab_dir(&root, "writing-now", TAB_DIR_TTL * 4, now);
        // A swarm that is still writing: the log is new even though `tab.json`
        // (and the directory) is old.
        let dir = root.tab_dir(&store::paths::TabId::parse(&id).unwrap());
        File::open(dir.join("swarm.log"))
            .unwrap()
            .set_modified(now)
            .unwrap();

        let pruned = prune_tab_dirs(&root, &[], TAB_DIR_TTL, now).unwrap();
        assert!(
            pruned.removed.is_empty(),
            "the log is the tab's pulse: {pruned:?}"
        );
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn a_missing_tabs_directory_is_nothing_to_do() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-none-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let pruned = prune_tab_dirs(&root, &[], TAB_DIR_TTL, now()).unwrap();
        assert_eq!(pruned, Pruned::default());
    }

    #[test]
    fn files_beside_the_tab_directories_are_left_alone() {
        let root =
            Root::at(std::env::temp_dir().join(format!("evo-prune-file-{}", std::process::id())));
        let _ = fs::remove_dir_all(root.path());
        let now = now();
        root.ensure().unwrap();
        fs::create_dir_all(root.tabs_dir()).unwrap();
        let stray = root.tabs_dir().join("notes.txt");
        File::create(&stray)
            .unwrap()
            .set_modified(now - TAB_DIR_TTL * 3)
            .unwrap();

        let pruned = prune_tab_dirs(&root, &[], TAB_DIR_TTL, now).unwrap();
        assert!(pruned.removed.is_empty(), "{pruned:?}");
        assert!(
            stray.exists(),
            "only tab directories are the app's to remove"
        );
        let _ = fs::remove_dir_all(root.path());
    }
}
