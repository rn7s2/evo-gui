//! `model-cache.json` — the last `/registry` snapshot (§9.4, §6).
//!
//! The empty tab needs model names before any server exists, so the catalog is
//! cached here: fetch it once with a throwaway `evo-agent serve` in
//! `~/.evo/desktop/probe/`, then refresh it from every live server's `/registry`.
//!
//! The cache also remembers the **kernel api set**: the `apis` a
//! `--no-userspace` probe reports. A lane can only register a model whose API
//! the lane itself knows, so that set is what decides which models the lanes
//! chooser may offer (§9.6's counterpart).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

use crate::paths::{self, Root};
use crate::time;

/// Bumped when the shape of `model-cache.json` changes.
pub const CACHE_VERSION: u32 = 1;

/// One model as `/registry` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    /// Provider keyword, lower-cased (`aiden`, `ark`, …).
    pub provider: Option<String>,
    /// The API this model speaks (`anthropic-messages`, …).
    pub api: Option<String>,
    pub context_window: Option<u64>,
    /// Effort levels the model accepts, weakest first. Empty = no dial.
    pub effort: Vec<String>,
}

impl ModelInfo {
    /// The `--model` / swarm.lisp value. Two registrations of one id under
    /// different providers are distinct models, so the label has to say which.
    pub fn label(&self) -> String {
        match &self.provider {
            Some(p) => format!("{} ({p})", self.id),
            None => self.id.clone(),
        }
    }
}

/// The cached catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ModelCache {
    pub version: u32,
    /// RFC 3339 UTC, when `registry` was fetched.
    pub fetched_at: String,
    /// The kernel's own API set, from a `--no-userspace` probe.
    pub kernel_apis: Vec<String>,
    /// The raw `/registry` body, exactly as the server sent it.
    pub registry: Value,
}

impl Default for ModelCache {
    fn default() -> Self {
        ModelCache {
            version: CACHE_VERSION,
            fetched_at: String::new(),
            kernel_apis: Vec::new(),
            registry: Value::Null,
        }
    }
}

impl ModelCache {
    /// A catalog learned from a `--no-userspace` probe: it names the kernel's
    /// own api set, which is exactly what a lane can register.
    pub fn from_probe(registry: Value) -> ModelCache {
        let apis = string_array(registry.get("apis"));
        ModelCache {
            version: CACHE_VERSION,
            fetched_at: time::now_rfc3339(),
            kernel_apis: apis,
            registry,
        }
    }

    /// Refreshed from a live (userspace) server: the catalog moves forward, the
    /// kernel api set — which only a probe can tell us — is kept.
    pub fn with_live_registry(&self, registry: Value) -> ModelCache {
        ModelCache {
            version: CACHE_VERSION,
            fetched_at: time::now_rfc3339(),
            kernel_apis: self.kernel_apis.clone(),
            registry,
        }
    }

    /// Record the kernel api set without touching the catalog.
    pub fn with_kernel_apis(mut self, apis: Vec<String>) -> ModelCache {
        self.kernel_apis = apis;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.registry.is_null() || self.models().is_empty()
    }

    pub fn fetched_epoch(&self) -> Option<u64> {
        time::parse_rfc3339(&self.fetched_at)
    }

    /// Every registered model, in registry order.
    pub fn models(&self) -> Vec<ModelInfo> {
        let Some(list) = self.registry.get("models").and_then(Value::as_array) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|m| {
                let id = m.get("id").and_then(Value::as_str)?.to_owned();
                Some(ModelInfo {
                    id,
                    provider: m.get("provider").and_then(Value::as_str).map(str::to_owned),
                    api: m.get("api").and_then(Value::as_str).map(str::to_owned),
                    // On the wire evo writes `context_window`: the server's JSON
                    // encoder turns keyword dashes into underscores (verified
                    // against a live `--no-userspace` server's `/registry`).
                    context_window: m.get("context_window").and_then(Value::as_u64),
                    effort: string_array(m.get("effort")),
                })
            })
            .collect()
    }

    /// True once a `--no-userspace` probe has told us the kernel api set.
    pub fn kernel_apis_known(&self) -> bool {
        !self.kernel_apis.is_empty()
    }

    /// Whether a lane can register a model with this id.
    ///
    /// A lane fails to initialize when it cannot register its model, so the
    /// lanes chooser must only offer models whose API a `--no-userspace` lane
    /// has. Before the first probe we have no such set and this answers `true`
    /// rather than hiding the whole catalog — check [`Self::kernel_apis_known`]
    /// when the difference matters.
    pub fn lane_model_available(&self, id: &str) -> bool {
        let registrations: Vec<ModelInfo> = self.models().into_iter().filter(|m| m.id == id).collect();
        if registrations.is_empty() {
            return false;
        }
        if !self.kernel_apis_known() {
            return true;
        }
        registrations.iter().any(|m| self.api_is_kernel(m.api.as_deref()))
    }

    /// Whether one exact registration — the (id, provider) pair evo identifies
    /// a model by — is registerable in a lane. Use this when the chooser knows
    /// which provider it is writing into `swarm.lisp`, since `find-model`
    /// refuses a model/provider mismatch.
    pub fn lane_model_available_for(&self, id: &str, provider: &str) -> bool {
        let Some(m) = self.models().into_iter().find(|m| m.id == id && m.provider.as_deref() == Some(provider))
        else {
            return false;
        };
        !self.kernel_apis_known() || self.api_is_kernel(m.api.as_deref())
    }

    /// The models the lanes chooser may offer.
    pub fn lane_models(&self) -> Vec<ModelInfo> {
        let kernel_known = self.kernel_apis_known();
        self.models()
            .into_iter()
            .filter(|m| !kernel_known || self.api_is_kernel(m.api.as_deref()))
            .collect()
    }

    fn api_is_kernel(&self, api: Option<&str>) -> bool {
        matches!(api, Some(a) if self.kernel_apis.iter().any(|k| k == a))
    }

    /// Read the cache; a missing, truncated or corrupt file yields an empty one
    /// (the corrupt file is kept beside it as `.bak`).
    pub fn load(root: &Root) -> ModelCache {
        let path = root.model_cache();
        match paths::read_json::<ModelCache>(&path) {
            Ok(mut cache) => {
                cache.version = CACHE_VERSION;
                cache
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => ModelCache::default(),
            Err(_) => {
                let _ = paths::quarantine(&path);
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

fn string_array(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => {
            items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect()
        }
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
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

    /// A registry body shaped the way the server really sends it: plist keys
    /// become snake_case JSON keys (`:context-window` → `context_window`), and
    /// `:apis` is the registered api set. Key names and types verified against
    /// a live `evo-agent serve --no-userspace` server, 2026-09-29.
    fn registry() -> Value {
        serde_json::json!({
            "models": [
                {"id": "claude-opus-5", "provider": "anthropic", "api": "anthropic-messages",
                 "context_window": 200000, "max_output": 64000, "vision": true,
                 "thinking_mode": "adaptive", "effort": ["low", "medium", "high", "xhigh", "max"]},
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "api": "ark-chat",
                 "context_window": 936000, "max_output": 32000, "vision": false,
                 "thinking_mode": "effort-only", "effort": null},
                {"id": "claude-sonnet-5", "provider": "proxy", "api": "anthropic-oauth-messages",
                 "context_window": 1000000, "max_output": 128000, "vision": true,
                 "thinking_mode": "adaptive", "effort": ["low", "max"]}
            ],
            "providers": [{"key": "anthropic", "has_api_key": true}],
            "apis": ["anthropic-messages", "ark-chat"],
            "tools": []
        })
    }

    #[test]
    fn lists_models_in_registry_order() {
        let cache = ModelCache::from_probe(registry());
        let models = cache.models();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "claude-opus-5");
        assert_eq!(models[0].provider.as_deref(), Some("anthropic"));
        assert_eq!(models[0].api.as_deref(), Some("anthropic-messages"));
        assert_eq!(models[0].context_window, Some(200_000));
        assert_eq!(models[0].effort, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(models[1].effort, Vec::<String>::new());
        assert_eq!(models[1].label(), "ark-deepseek-v4.1-flash (aiden)");
        assert_eq!(cache.kernel_apis, ["anthropic-messages", "ark-chat"]);
        assert!(cache.kernel_apis_known());
    }

    #[test]
    fn lane_availability_follows_the_kernel_api_set() {
        let cache = ModelCache::from_probe(registry());
        assert!(cache.lane_model_available("claude-opus-5"));
        assert!(cache.lane_model_available("ark-deepseek-v4.1-flash"));
        // An API an extension defines is not in a --no-userspace lane.
        assert!(!cache.lane_model_available("claude-sonnet-5"));
        assert!(!cache.lane_model_available("nope"));
        assert!(!cache.lane_model_available_for("claude-sonnet-5", "proxy"));
        // …while a model with no provider named is checked by its own api.
        assert!(cache.models().iter().any(|m| m.id == "claude-sonnet-5"));

        // Only the kernel's own API: the OAuth model drops out.
        let kernel_only = ModelCache::from_probe(registry()).with_kernel_apis(vec!["anthropic-messages".into()]);
        assert!(kernel_only.lane_model_available("claude-opus-5"));
        assert!(!kernel_only.lane_model_available("ark-deepseek-v4.1-flash"));
        assert_eq!(kernel_only.lane_models().len(), 1);
    }

    #[test]
    fn unknown_kernel_set_does_not_hide_the_catalog() {
        let cache = ModelCache::default().with_live_registry(registry());
        assert!(!cache.kernel_apis_known());
        assert!(cache.lane_model_available("claude-sonnet-5"));
        assert!(!cache.lane_model_available("never-registered"));
        assert_eq!(cache.lane_models().len(), 3);
    }

    #[test]
    fn a_live_refresh_keeps_the_probed_api_set() {
        let probed = ModelCache::from_probe(registry());
        let live = probed.with_live_registry(serde_json::json!({"models": [], "apis": ["anthropic-messages", "mine"]}));
        assert_eq!(live.kernel_apis, probed.kernel_apis);
        assert_eq!(live.registry["apis"], serde_json::json!(["anthropic-messages", "mine"]));
        assert!(live.fetched_epoch().is_some());
    }

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let root = temp_root("disk");
        let cache = ModelCache::from_probe(registry());
        cache.save(&root).unwrap();
        assert_eq!(ModelCache::load(&root), cache);
        assert!(ModelCache::load(&root).fetched_epoch().is_some());

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
        assert_eq!(ModelCache::default().fetched_epoch(), None);
    }
}
