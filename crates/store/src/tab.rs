//! One tab's own descriptor: `tabs/<id>/tab.json` (§6).
//!
//! This is what a tab needs to exist again after a relaunch: where it runs,
//! which session it resumed, which swarm it is, and the choices it was created
//! with. The bearer token is *not* here — the server owns that file (§2 rule 4).

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::{self, Root, TabId};

/// The models a tab was created with. `None` means **Default** — nothing is
/// passed to the server, and the lanes model block is not written (§9.6).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct TabModels {
    /// Coordinator model id (`--model`).
    pub coordinator: Option<String>,
    /// Lanes model id (written into the project's `.evo/swarm.lisp`).
    pub lanes: Option<String>,
}

impl TabModels {
    pub fn is_default(&self) -> bool {
        self.coordinator.is_none() && self.lanes.is_none()
    }
}

/// `tabs/<id>/tab.json`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct TabState {
    /// The folder the swarm runs in (the tab's cwd).
    pub folder: PathBuf,
    /// The coordinator session path, when the tab resumed one (`--resume`).
    pub session: Option<PathBuf>,
    /// The swarm id evo assigned, once known (`GET /lanes` → `swarm.id`).
    pub swarm_id: Option<String>,
    /// The models chosen when the tab was created.
    pub models: TabModels,
    /// Worker count, when the user chose one. `None` = evo's own default.
    pub workers: Option<u32>,
}

impl TabState {
    pub fn new(folder: impl Into<PathBuf>) -> TabState {
        TabState {
            folder: folder.into(),
            ..TabState::default()
        }
    }

    /// The folder's name, for a tab label or a history row.
    pub fn folder_name(&self) -> String {
        folder_label(&self.folder)
    }

    pub fn load(root: &Root, id: &TabId) -> TabState {
        let path = root.tab_json(id);
        match paths::read_json::<TabState>(&path) {
            Ok(state) => state,
            Err(e) if e.kind() == io::ErrorKind::NotFound => TabState::default(),
            Err(_) => {
                let _ = paths::quarantine(&path);
                TabState::default()
            }
        }
    }

    pub fn save(&self, root: &Root, id: &TabId) -> io::Result<()> {
        root.ensure_tab_dir(id)?;
        let path = root.tab_json(id);
        paths::backup(&path)?;
        paths::write_json(&path, self)
    }
}

/// The last path component of `folder`, or the whole path when there is none
/// (`/` → `/`). Trailing separators are ignored.
pub fn folder_label(folder: &Path) -> String {
    let trimmed = folder.to_string_lossy();
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match Path::new(trimmed).file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-tab-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    #[test]
    fn round_trips() {
        let root = temp_root("roundtrip");
        let id = TabId::new();
        let tab = TabState {
            folder: PathBuf::from("/Users/x/coding/foo"),
            session: Some(PathBuf::from("/Users/x/.evo/sessions/a/1.sexp")),
            swarm_id: Some("20260929T051622-d540".into()),
            models: TabModels {
                coordinator: Some("m1".into()),
                lanes: Some("m2".into()),
            },
            workers: Some(3),
        };
        tab.save(&root, &id).unwrap();
        assert_eq!(TabState::load(&root, &id), tab);
        // No token path is ever written into a tab descriptor.
        let raw = fs::read_to_string(root.tab_json(&id)).unwrap();
        assert!(!raw.contains("token"), "{raw}");
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn missing_and_corrupt_are_defaults() {
        let root = temp_root("corrupt");
        let id = TabId::new();
        assert_eq!(TabState::load(&root, &id), TabState::default());

        root.ensure_tab_dir(&id).unwrap();
        fs::write(root.tab_json(&id), "{ not json").unwrap();
        assert_eq!(TabState::load(&root, &id), TabState::default());
        assert!(paths::backup_path(&root.tab_json(&id)).exists());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn labels_are_the_folder_name() {
        assert_eq!(folder_label(Path::new("/Users/x/coding/foo")), "foo");
        assert_eq!(folder_label(Path::new("/Users/x/coding/foo/")), "foo");
        assert_eq!(folder_label(Path::new("/")), "/");
    }
}
