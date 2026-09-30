//! `model-cache.json` — the last catalog, for the empty tab's choosers (§6, §9).
//!
//! The empty tab needs model names before any server exists, so the catalog is
//! fetched once with `evo-swarm catalog --json` — a process, not a throwaway
//! server in a scratch directory — and kept here. Every launch re-fetches it on a
//! thread of its own; a live server's own `GET /catalog` can refresh it too.
//!
//! The body is stored whole ([`Catalog`]): evo owns its shape, drops entries it
//! cannot encode, and names them in `warnings`, and none of that is this file's
//! business to re-encode.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::path::Path;

use crate::catalog::{Catalog, Model};
use crate::paths::{self, Root};
use crate::time;

/// Bumped when the shape of `model-cache.json` changes.
pub const CACHE_VERSION: u32 = 2;

/// The cached catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ModelCache {
    pub version: u32,
    /// RFC 3339 UTC, when the catalog was fetched.
    pub fetched_at: String,
    /// Which binary answered (`evo-swarm`), so a stale cache from the other one
    /// is recognisable.
    pub program: String,
    /// The `catalog --json` body, exactly as the binary printed it.
    pub catalog: Catalog,
}

impl Default for ModelCache {
    fn default() -> Self {
        ModelCache {
            version: CACHE_VERSION,
            fetched_at: String::new(),
            program: String::new(),
            catalog: Catalog::empty(),
        }
    }
}

impl ModelCache {
    /// A catalog read from `evo-swarm catalog --json` (or a live `GET /catalog`).
    pub fn from_catalog(program: &str, body: Value) -> ModelCache {
        ModelCache {
            version: CACHE_VERSION,
            fetched_at: time::now_rfc3339(),
            program: program.to_owned(),
            catalog: Catalog::from_json(body),
        }
    }

    /// True when nothing usable is cached.
    pub fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }

    pub fn fetched_epoch(&self) -> Option<u64> {
        time::parse_rfc3339(&self.fetched_at)
    }

    /// The cached body, for the choosers.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The raw body, for a caller that hands it on as JSON.
    pub fn raw(&self) -> &Value {
        self.catalog.raw()
    }

    /// Every registered model.
    pub fn models(&self) -> Vec<Model> {
        self.catalog.models()
    }

    /// Entries evo could not encode (§5.6).
    pub fn warnings(&self) -> Vec<String> {
        self.catalog.warnings()
    }

    /// Read the cache; a missing, truncated or corrupt file yields an empty one
    /// (the corrupt file is kept beside it as `.bak`).
    pub fn load(root: &Root) -> ModelCache {
        ModelCache::load_at(&root.model_cache())
    }

    /// [`Self::load`] for a path handed in, which is what a test uses.
    pub fn load_at(path: &Path) -> ModelCache {
        match paths::read_json::<ModelCache>(path) {
            Ok(mut cache) => {
                cache.version = CACHE_VERSION;
                cache
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => ModelCache::default(),
            Err(_) => {
                let _ = paths::quarantine(path);
                ModelCache::default()
            }
        }
    }

    pub fn save(&self, root: &Root) -> io::Result<()> {
        root.ensure()?;
        let path = root.model_cache();
        paths::backup(&path)?;
        paths::write_json(&path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-mc-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    /// The body `evo-swarm catalog --json` prints (§5.6), with the lanes list.
    fn catalog() -> Value {
        serde_json::json!({
            "models": [
                {"id": "claude-opus-5", "provider": "anthropic", "name": "Claude Opus 5",
                 "api": "anthropic-messages", "context_window": 200000,
                 "reasoning": true, "images": true, "ready": true, "reason": null},
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "name": "DeepSeek V4.1",
                 "api": "ark-chat", "context_window": 936000,
                 "reasoning": false, "images": false, "ready": true, "reason": null}
            ],
            "providers": [{"name": "anthropic", "api": "anthropic-messages", "has_key": true,
                           "key_env": "ANTHROPIC_API_KEY"}],
            "default_model": {"id": "ark-deepseek-v4.1-flash", "provider": "aiden"},
            "thinking_levels": ["off", "low", "medium", "high", "xhigh"],
            "languages": [{"code": "en", "name": "English"}],
            "lanes": {"models": [
                {"id": "claude-opus-5", "provider": "anthropic", "ok": true, "reason": null},
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "ok": false,
                 "reason": "api ark-chat is not in a lane"}
            ]},
            "warnings": []
        })
    }

    #[test]
    fn a_cached_catalog_reads_back_as_models_and_lanes() {
        let cache = ModelCache::from_catalog("evo-swarm", catalog());
        assert!(!cache.is_empty());
        assert_eq!(cache.models().len(), 2);
        assert_eq!(cache.program, "evo-swarm");
        assert!(cache.fetched_epoch().is_some());
        let lanes = cache
            .catalog()
            .lane_models()
            .expect("the swarm catalog has lanes");
        assert_eq!(lanes.len(), 2);
        assert!(cache
            .catalog()
            .lane_model_ok("claude-opus-5", Some("anthropic")));
        assert!(!cache
            .catalog()
            .lane_model_ok("ark-deepseek-v4.1-flash", Some("aiden")));
    }

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let root = temp_root("disk");
        let cache = ModelCache::from_catalog("evo-swarm", catalog());
        cache.save(&root).unwrap();
        let loaded = ModelCache::load(&root);
        assert_eq!(loaded, cache);
        assert_eq!(loaded.raw(), &catalog());
        assert!(loaded.fetched_epoch().is_some());

        fs::write(root.model_cache(), "}{").unwrap();
        let empty = ModelCache::load(&root);
        assert!(empty.is_empty());
        assert!(paths::backup_path(&root.model_cache()).exists());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn an_empty_cache_is_empty() {
        assert!(ModelCache::default().is_empty());
        assert!(ModelCache::default().models().is_empty());
        assert!(ModelCache::default().catalog().lane_models().is_none());
        assert_eq!(ModelCache::default().fetched_epoch(), None);
        // A file that is not there is an empty cache, not an error.
        let root = temp_root("missing");
        assert!(ModelCache::load(&root).is_empty());
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn a_cache_from_a_binary_without_lanes_still_answers() {
        let mut body = catalog();
        body.as_object_mut().unwrap().remove("lanes");
        let cache = ModelCache::from_catalog("evo-agent", body);
        assert!(cache.catalog().lane_models().is_none());
        assert!(cache
            .catalog()
            .lane_model_ok("claude-opus-5", Some("anthropic")));
        assert!(!cache.catalog().lane_model_ok("never-registered", None));
    }
}
