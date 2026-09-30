//! `GET /catalog` as a value (§5.6), and the two ways of reading one.
//!
//! The body is what `evo-swarm catalog --json` prints, what a live server answers
//! and what `model-cache.json` keeps. It is a document, not a type tree: evo adds
//! entries (ops, commands, skills) that no GUI field needs, and evo **drops** any
//! entry that fails to encode, naming it in `warnings`. So this module parses the
//! fields the app acts on and leaves the rest of the document alone.
//!
//! `evo-agent catalog --json` and `evo-swarm catalog --json` print the same body,
//! except that the swarm adds `lanes`: the models a **lane** can register, each
//! with `ok` and the `reason` it cannot. That list is what greys out a lanes
//! chooser option — not a guess from an api set.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The catalog body, opaque but for the readers below.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Catalog {
    body: Value,
}

/// One model, as `/catalog.models` describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    /// The provider keyword, lower-cased.
    pub provider: Option<String>,
    /// What evo calls it in its own pickers (`Claude Opus 5`).
    pub name: Option<String>,
    pub api: Option<String>,
    pub context_window: Option<u64>,
    /// Whether the model can be given a thinking budget.
    pub reasoning: bool,
    /// Whether it takes images.
    pub images: bool,
    /// Whether evo can reach it right now (key present, endpoint alive).
    pub ready: bool,
    /// Why not, when it is not.
    pub reason: Option<String>,
}

/// One model a lane may run, as `/catalog.lanes.models` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneModel {
    pub id: String,
    pub provider: Option<String>,
    /// Whether a lane can register it.
    pub ok: bool,
    /// Why not, when it cannot — evo's own words.
    pub reason: Option<String>,
}

impl LaneModel {
    /// The `ID@PROVIDER` pair, as above.
    pub fn spec(&self) -> String {
        spec_of(&self.id, self.provider.as_deref())
    }
}

/// A model named by id and provider.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModelRef {
    pub id: String,
    pub provider: Option<String>,
}

impl Model {
    /// The `ID@PROVIDER` pair evo identifies this registration by — what a
    /// `--model` / `--lane-model` flag takes (§1).
    pub fn spec(&self) -> String {
        spec_of(&self.id, self.provider.as_deref())
    }
}

impl ModelRef {
    pub fn new(id: impl Into<String>, provider: Option<impl Into<String>>) -> ModelRef {
        ModelRef {
            id: id.into(),
            provider: provider.map(Into::into),
        }
    }

    /// The `ID@PROVIDER` form the launch flags take (§1): `--model`,
    /// `--lane-model`. A bare id means evo's own rule — the first registration.
    pub fn spec(&self) -> String {
        spec_of(&self.id, self.provider.as_deref())
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.spec())
    }
}

/// One model `evo-swarm check --json` judged: the coordinator's or the lanes'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelCheck {
    pub id: Option<String>,
    pub provider: Option<String>,
    pub ok: bool,
    pub reason: Option<String>,
}

/// One problem `evo-swarm check --json` found with a launch (§9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Problem {
    /// evo's own code for it (`model_not_found`, `no_key`, …), which is also what
    /// decides where a click on the line goes.
    pub code: String,
    /// One line, already worded for a person.
    pub message: String,
}

impl Problem {
    /// The line a client shows: the message, folded onto one line — something a
    /// reader can act on, never a paragraph.
    pub fn line(&self) -> String {
        let text = one_line(&self.message);
        if text.is_empty() {
            self.code.clone()
        } else {
            text
        }
    }

    /// What this problem is about, which is what a click on its line opens.
    pub fn target(&self) -> ProblemTarget {
        let code = self.code.to_ascii_lowercase();
        if code.contains("lane_model") {
            ProblemTarget::LaneModel
        } else if code.contains("lane_thinking") || code.contains("thinking") {
            ProblemTarget::Thinking
        } else if code.contains("worker") {
            ProblemTarget::Workers
        } else if code.contains("model") {
            ProblemTarget::Model
        } else {
            // Everything else is about the machine rather than the launch — a binary
            // that is not there, a folder that cannot be written — which is the
            // application's own settings' business.
            ProblemTarget::Other
        }
    }
}

/// What one problem is about (§9): the chooser (or the setting) a click on its line
/// belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemTarget {
    /// The coordinator's model.
    Model,
    /// The lanes' model.
    LaneModel,
    /// The lanes' thinking level.
    Thinking,
    /// The worker count.
    Workers,
    /// Something outside the launch's choices.
    Other,
}

/// The answer to `evo-swarm check --json`: whether the launch would work, and
/// what is wrong when it would not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub ok: bool,
    pub model: Option<ModelCheck>,
    pub lane_model: Option<ModelCheck>,
    pub problems: Vec<Problem>,
}

impl Catalog {
    /// Wrap a body that came off the wire or out of a process.
    pub fn from_json(body: Value) -> Catalog {
        Catalog { body }
    }

    /// An empty catalog: no models known, the choosers stay on Default.
    pub fn empty() -> Catalog {
        Catalog { body: Value::Null }
    }

    pub fn raw(&self) -> &Value {
        &self.body
    }

    /// True when the body names no model at all — the same as no catalog.
    pub fn is_empty(&self) -> bool {
        self.models().is_empty()
    }

    /// Every registered model, in the order evo listed them.
    pub fn models(&self) -> Vec<Model> {
        let Some(list) = self.body.get("models").and_then(Value::as_array) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|m| {
                let id = m.get("id").and_then(Value::as_str)?.to_owned();
                Some(Model {
                    id,
                    provider: string(m, "provider").map(|p| p.to_lowercase()),
                    name: string(m, "name"),
                    api: string(m, "api"),
                    context_window: m.get("context_window").and_then(Value::as_u64),
                    reasoning: m.get("reasoning").and_then(Value::as_bool).unwrap_or(false),
                    images: m.get("images").and_then(Value::as_bool).unwrap_or(false),
                    ready: m.get("ready").and_then(Value::as_bool).unwrap_or(false),
                    reason: string(m, "reason"),
                })
            })
            .collect()
    }

    /// One registration, by the pair evo identifies it with.
    pub fn model(&self, id: &str, provider: Option<&str>) -> Option<Model> {
        self.models()
            .into_iter()
            .find(|m| m.id == id && m.provider.as_deref() == provider)
    }

    /// The models a lane can register — `/catalog.lanes.models`, which only
    /// `evo-swarm catalog --json` (and a swarm server) prints.
    ///
    /// With no `lanes` in the body there is nothing to grey out: every model is
    /// reported `ok`, and `None` says so.
    pub fn lane_models(&self) -> Option<Vec<LaneModel>> {
        let list = self.body.get("lanes")?.get("models")?.as_array()?;
        Some(
            list.iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(Value::as_str)?.to_owned();
                    Some(LaneModel {
                        id,
                        provider: string(m, "provider").map(|p| p.to_lowercase()),
                        ok: m.get("ok").and_then(Value::as_bool).unwrap_or(false),
                        reason: string(m, "reason"),
                    })
                })
                .collect(),
        )
    }

    /// Whether a lane can run one registration: `lanes.models` when the body has
    /// it, else the model's own `ready`.
    pub fn lane_model_ok(&self, id: &str, provider: Option<&str>) -> bool {
        match self.lane_models() {
            Some(lanes) => lanes
                .iter()
                .find(|m| m.id == id && m.provider.as_deref() == provider)
                .is_some_and(|m| m.ok),
            None => self.model(id, provider).is_some_and(|m| m.ready),
        }
    }

    /// Entries evo could not encode, named rather than dropped silently (§5.6).
    /// The thinking levels the server accepts, in its order — `thinking_levels`,
    /// which CONTRACT §5.6 calls authoritative: a client never hard-codes the
    /// ladder (`off low medium high xhigh max` on a full registration).
    ///
    /// A body without the key names none, which is not the same as an empty
    /// ladder: the caller shows no effort control rather than inventing rungs.
    pub fn thinking_levels(&self) -> Vec<String> {
        self.body
            .get("thinking_levels")
            .and_then(Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| level.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn warnings(&self) -> Vec<String> {
        string_array(self.body.get("warnings"))
    }
}

impl CheckReport {
    /// Read `evo-swarm check --json`'s answer.
    pub fn from_json(body: &Value) -> CheckReport {
        CheckReport {
            ok: body.get("ok").and_then(Value::as_bool).unwrap_or(false),
            model: body.get("model").and_then(model_check),
            lane_model: body.get("lane_model").and_then(model_check),
            problems: body
                .get("problems")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|p| Problem {
                            code: string(p, "code").unwrap_or_default(),
                            message: string(p, "message").unwrap_or_default(),
                        })
                        .filter(|p| !p.code.is_empty() || !p.message.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The one line each problem becomes in the empty tab: evo's own words, one
    /// line, with the code kept for the hover.
    pub fn lines(&self) -> Vec<String> {
        self.problems
            .iter()
            .map(
                |problem| match (problem.message.is_empty(), problem.code.is_empty()) {
                    (false, _) => one_line(&problem.message),
                    (true, false) => problem.code.clone(),
                    (true, true) => String::new(),
                },
            )
            .filter(|line| !line.is_empty())
            .collect()
    }
}

fn model_check(value: &Value) -> Option<ModelCheck> {
    Some(ModelCheck {
        id: string(value, "id"),
        provider: string(value, "provider").map(|p| p.to_lowercase()),
        ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
        reason: string(value, "reason"),
    })
}

fn spec_of(id: &str, provider: Option<&str>) -> String {
    match provider.filter(|p| !p.is_empty()) {
        Some(provider) => format!("{id}@{provider}"),
        None => id.to_owned(),
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// A message with its newlines folded to spaces: one line, whatever evo wrote.
fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A catalog body shaped the way §5.6 writes it — the same body the fixtures
    /// in `tests/fixtures/` hold, and the shape `evo-swarm catalog --json`
    /// prints (with `lanes`).
    fn body() -> Value {
        json!({
            "models": [
                {"id": "claude-opus-5", "provider": "Anthropic", "name": "Claude Opus 5",
                 "api": "anthropic-messages", "context_window": 200000,
                 "reasoning": true, "images": true, "ready": true, "reason": null},
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "name": "DeepSeek V4.1 Flash",
                 "api": "ark-chat", "context_window": 936000,
                 "reasoning": false, "images": false, "ready": true, "reason": null},
                {"id": "claude-sonnet-5", "provider": "proxy", "name": "Claude Sonnet 5",
                 "api": "anthropic-oauth-messages", "context_window": 1000000,
                 "reasoning": true, "images": true, "ready": false, "reason": "no credential"}
            ],
            "providers": [
                {"name": "anthropic", "api": "anthropic-messages", "has_key": true,
                 "key_env": "ANTHROPIC_API_KEY"},
                {"name": "proxy", "api": "anthropic-oauth-messages", "has_key": false, "key_env": null}
            ],
            "default_model": {"id": "ark-deepseek-v4.1-flash", "provider": "aiden"},
            "thinking_levels": ["off", "low", "medium", "high", "xhigh"],
            "languages": [{"code": "en", "name": "English"}],
            "ops": [], "commands": [], "skills": [], "tools": [],
            "lanes": {"models": [
                {"id": "claude-opus-5", "provider": "anthropic", "ok": true, "reason": null},
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "ok": true, "reason": null},
                {"id": "claude-sonnet-5", "provider": "proxy", "ok": false,
                 "reason": "api anthropic-oauth-messages is not in a lane"}
            ]},
            "warnings": ["mcp[2] could not be encoded"]
        })
    }

    /// The effort ladder is the server's, in the server's order (CONTRACT §5.6):
    /// the list a client shows is read, never written down here.
    #[test]
    fn the_thinking_levels_are_read_in_the_servers_order() {
        let catalog = Catalog::from_json(body());
        assert_eq!(
            catalog.thinking_levels(),
            ["off", "low", "medium", "high", "xhigh"]
        );
        // A body that names none names none — not an empty list of rungs.
        assert!(Catalog::empty().thinking_levels().is_empty());
    }

    #[test]
    fn models_come_back_in_order_with_their_flags() {
        let catalog = Catalog::from_json(body());
        let models = catalog.models();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "claude-opus-5");
        assert_eq!(models[0].provider.as_deref(), Some("anthropic"));
        assert_eq!(models[0].name.as_deref(), Some("Claude Opus 5"));
        assert_eq!(models[0].context_window, Some(200_000));
        assert!(models[0].reasoning && models[0].images && models[0].ready);
        // An unreachable registration says so.
        assert!(!models[2].ready);
        assert_eq!(models[2].reason.as_deref(), Some("no credential"));
    }

    #[test]
    fn the_lanes_list_decides_what_a_lane_can_run() {
        let catalog = Catalog::from_json(body());
        let lanes = catalog.lane_models().expect("a swarm catalog has lanes");
        assert_eq!(lanes.len(), 3);
        assert!(lanes[0].ok);
        assert!(!lanes[2].ok);
        assert_eq!(
            lanes[2].reason.as_deref(),
            Some("api anthropic-oauth-messages is not in a lane")
        );
        assert!(catalog.lane_model_ok("claude-opus-5", Some("anthropic")));
        assert!(!catalog.lane_model_ok("claude-sonnet-5", Some("proxy")));
        // The provider matters: the same id under another provider is not the
        // same registration.
        assert!(!catalog.lane_model_ok("claude-opus-5", Some("proxy")));
    }

    #[test]
    fn without_a_lanes_list_the_models_own_readiness_answers() {
        let mut bare = body();
        bare.as_object_mut().unwrap().remove("lanes");
        let catalog = Catalog::from_json(bare);
        assert!(catalog.lane_models().is_none());
        assert!(catalog.lane_model_ok("claude-opus-5", Some("anthropic")));
        assert!(!catalog.lane_model_ok("claude-sonnet-5", Some("proxy")));
    }

    #[test]
    fn the_rest_of_the_body_is_read_not_required() {
        let catalog = Catalog::from_json(body());
        assert_eq!(catalog.warnings(), ["mcp[2] could not be encoded"]);
        assert!(!catalog.is_empty());

        // A body missing every optional part is empty, not a crash.
        let bare = Catalog::from_json(json!({"models": []}));
        assert!(bare.is_empty());
        assert_eq!(bare.warnings(), Vec::<String>::new());
        assert!(Catalog::empty().is_empty());
    }

    #[test]
    fn a_model_without_an_id_is_dropped_and_one_without_a_provider_keeps_a_bare_spec() {
        let catalog = Catalog::from_json(json!({"models": [
            {"provider": "aiden"},
            {"id": "bare", "ready": true}
        ]}));
        let models = catalog.models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "bare");
        assert!(catalog.lane_model_ok("bare", None));
    }

    #[test]
    fn a_check_report_reads_its_problems_as_lines() {
        let report = CheckReport::from_json(&json!({
            "ok": false,
            "model": {"id": "m", "provider": "aiden", "ok": true, "reason": null},
            "lane_model": {"id": "l", "provider": "aiden", "ok": false,
                           "reason": "a lane cannot register ark-chat"},
            "problems": [
                {"code": "model_not_found", "message": "no model named nope"},
                {"code": "no_key", "message": "line one\nline two"}
            ]
        }));
        assert!(!report.ok);
        assert_eq!(report.model.as_ref().map(|m| m.ok), Some(true));
        assert_eq!(
            report.lane_model.as_ref().and_then(|m| m.reason.as_deref()),
            Some("a lane cannot register ark-chat")
        );
        assert_eq!(report.problems.len(), 2);
        assert_eq!(report.problems[0].line(), "no model named nope");
        assert_eq!(report.problems[1].line(), "line one line two");
    }

    /// §9: a problem knows which chooser its line belongs to, so a click can open
    /// the right one — and anything that is not about a choice opens the settings.
    #[test]
    fn a_problem_knows_what_it_is_about() {
        let cases = [
            ("model_not_ready", ProblemTarget::Model),
            ("model_not_found", ProblemTarget::Model),
            ("lane_model_not_ready", ProblemTarget::LaneModel),
            ("lane_thinking_unknown", ProblemTarget::Thinking),
            ("workers_out_of_range", ProblemTarget::Workers),
            ("swarm_binary_missing", ProblemTarget::Other),
            ("folder_not_writable", ProblemTarget::Other),
        ];
        for (code, target) in cases {
            assert_eq!(
                Problem {
                    code: code.to_string(),
                    message: "…".to_string()
                }
                .target(),
                target,
                "{code}"
            );
        }
    }

    #[test]
    fn a_check_report_that_says_nothing_is_an_empty_line() {
        let report = CheckReport::from_json(&json!({"ok": true, "problems": []}));
        assert!(report.ok);
        assert!(report.lines().is_empty());
        // A problem with an empty code and message is not a line.
        let blank = CheckReport::from_json(&json!({"problems": [{"code": "", "message": " "}]}));
        assert_eq!(blank.problems[0].line(), "");
    }
}
