//! The §7.3 status readout — the TUI's status line, segment for segment.
//!
//! The TUI builds the line from ordered segments (`src/tui/tui.lisp`: `:model` 100,
//! `:thinking` 200, `:context` 300, `:goal` 400, plus `extensions/340-cache-stats.lisp`'s
//! `:cache-stats` at 350) and joins them with `" · "`. This module reproduces the same
//! numbers, the same units and the same hiding rules, so the same session reads the same
//! in both frontends:
//!
//! ```text
//! ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%) · 97% cached · goal a1b2c3d4 (active) 12k/50k
//! ```
//!
//! `/state` is the seed and the resync; between resyncs each `message-end`'s `usage`
//! re-anchors the context figure and folds the cache totals, so the line moves with the
//! run instead of jumping at `settled`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Between adjacent segments, exactly the TUI's separator.
const SEPARATOR: &str = " · ";

/// `(round n 1000)` in Common Lisp, which rounds half to **even**: 1500 → 2, 2500 → 2,
/// 3500 → 4. Integer arithmetic only, so the result is exact for any token count.
pub fn round_div_half_even(n: u64, d: u64) -> u64 {
    debug_assert!(d > 0);
    let quotient = n / d;
    let remainder = n % d;
    match (remainder * 2).cmp(&d) {
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Less => quotient,
        // An exact half goes to the even quotient, as CL `round` does.
        std::cmp::Ordering::Equal => quotient + (quotient & 1),
    }
}

/// `(format nil "~dk" (round n 1000))` — the TUI's `fmt-ktokens`.
pub fn k_tokens(n: u64) -> String {
    format!("{}k", round_div_half_even(n, 1000))
}

/// The session prompt-cache totals, as `extensions/340-cache-stats.lisp` accumulates them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheTotals {
    /// `:input` — normalized usage, which *excludes* cached tokens.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl CacheTotals {
    /// Fold one normalized usage plist (a `message-end`'s `usage`, or a journal
    /// entry's `data`) into the totals, as `cache-stats-record` does: missing keys and
    /// an absent usage read as zero.
    pub fn fold(&mut self, usage: &Value) {
        self.input += u64_field(usage, "input");
        self.cache_read += u64_field(usage, "cache_read");
        self.cache_write += u64_field(usage, "cache_write");
    }

    /// `round(100 · cache_read / (input + cache_read + cache_write))`, or `None` while
    /// no provider has reported cache activity — when the segment stays hidden rather
    /// than showing a noise "0% cached".
    pub fn cached_percent(&self) -> Option<u64> {
        let total = self.input + self.cache_read + self.cache_write;
        if self.cache_read + self.cache_write == 0 {
            None
        } else {
            Some(round_div_half_even(self.cache_read * 100, total))
        }
    }
}

/// The session goal, as `/state.goal` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalState {
    pub id: String,
    pub objective: String,
    pub status: String,
    /// `token_budget` — `None` when the goal has no budget, which is what hides
    /// the `/<budget>` half of the segment.
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    /// The kernel's live count for the goal (`goal-tokens-used`); `0` when `/state`
    /// does not carry it.
    pub tokens_used_live: u64,
}

impl GoalState {
    /// `/state.goal`, or `None` when there is no goal.
    pub fn from_json(value: &Value) -> Option<GoalState> {
        if !value.is_object() {
            return None;
        }
        Some(GoalState {
            id: string_field(value, "goal_id").unwrap_or_default(),
            objective: string_field(value, "objective").unwrap_or_default(),
            status: string_field(value, "status").unwrap_or_default(),
            token_budget: optional_u64(value, "token_budget"),
            tokens_used: u64_field(value, "tokens_used"),
            tokens_used_live: u64_field(value, "tokens_used_live"),
        })
    }

    /// `goal-label` (`src/tui/tui.lisp`): `goal <id> (<status>) <tokens>[/<budget>]`,
    /// `tokens = tokens_used + tokens_used_live + tokens folded since the last /state`,
    /// the budget half only when the goal has one.
    fn label(&self, run_tokens: u64) -> String {
        let used = self.tokens_used + self.tokens_used_live + run_tokens;
        match self.token_budget {
            Some(budget) => format!("goal {} ({}) {}/{}", self.id, self.status, k_tokens(used), k_tokens(budget)),
            None => format!("goal {} ({}) {}", self.id, self.status, k_tokens(used)),
        }
    }
}

/// The status readout: segments (§7.3) in the TUI's order, or the joined line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readout {
    model: Option<String>,
    provider: Option<String>,
    /// Model id → the providers it is registered under (`/registry.models`). More than
    /// one makes the bare id ambiguous, so the label names the live provider.
    providers_by_model: BTreeMap<String, BTreeSet<String>>,
    thinking: Option<String>,
    context_tokens: u64,
    context_window: Option<u64>,
    cache: CacheTotals,
    goal: Option<GoalState>,
    /// Usage folded since the last `/state`, the GUI's stand-in for the TUI's
    /// `tui-goal-run-tokens`; reset whenever `/state` seeds the readout again.
    goal_run_tokens: u64,
}

impl Readout {
    pub fn new() -> Readout {
        Readout::default()
    }

    /// Seed or resync from `GET /state`: model, thinking, context, goal. Todo lists and
    /// activity ride the same reply and are read by [`AgentModel::apply_state`].
    ///
    /// [`AgentModel::apply_state`]: crate::AgentModel::apply_state
    pub fn apply_state(&mut self, state: &Value) {
        self.model = string_field(state, "model").filter(|s| !s.is_empty());
        self.provider = string_field(state, "provider").filter(|s| !s.is_empty());
        self.thinking = string_field(state, "thinking").filter(|s| !s.is_empty());
        self.context_tokens = u64_field(state, "context_tokens");
        // A window of 0 is still a window in Lisp's truthiness, and the TUI prints it as
        // one (`context-label`): only a null/missing window hides the `/<window>` half.
        self.context_window = optional_u64(state, "context_window");
        self.goal = GoalState::from_json(&state["goal"]);
        // The kernel's goal token count in this reply already covers what we folded,
        // so folding on top of it would double-count.
        self.goal_run_tokens = 0;
    }

    /// Note the model catalog (`GET /registry`), so the model segment can tell an
    /// unambiguous id from one registered under several providers.
    pub fn apply_registry(&mut self, registry: &Value) {
        let mut providers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(models) = registry.get("models").and_then(Value::as_array) {
            for model in models {
                let (Some(id), Some(provider)) = (string_field(model, "id"), string_field(model, "provider"))
                else {
                    continue;
                };
                providers.entry(id).or_default().insert(provider);
            }
        }
        self.providers_by_model = providers;
    }

    /// The journal's `cache-stats` totals are the seed for the cache segment; from then
    /// on every `message-end` keeps it live.
    pub fn set_cache_totals(&mut self, totals: CacheTotals) {
        self.cache = totals;
    }

    pub fn cache_totals(&self) -> CacheTotals {
        self.cache
    }

    /// Fold one `message-end`: `context_tokens = input + output + cache_read +
    /// cache_write` (the count the next request would send) and the cache totals, as
    /// `usage-total-tokens` and the cache-stats hook do. A usage of all zeros changes
    /// nothing, exactly as in the TUI.
    pub fn fold_message_end(&mut self, data: &Value) -> bool {
        let usage = match data.get("usage") {
            Some(usage) if usage.is_object() => usage,
            _ => return false,
        };
        let total = u64_field(usage, "input")
            + u64_field(usage, "output")
            + u64_field(usage, "cache_read")
            + u64_field(usage, "cache_write");
        if total == 0 {
            return false;
        }
        self.context_tokens = total;
        self.cache.fold(usage);
        self.goal_run_tokens += total;
        true
    }

    pub fn goal(&self) -> Option<&GoalState> {
        self.goal.as_ref()
    }

    pub fn context_tokens(&self) -> u64 {
        self.context_tokens
    }

    pub fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// The model id as `/state` reports it (bare, without the provider), for a tooltip.
    pub fn model_id(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The provider the session is running on, as `/state` reports it, for a tooltip.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// The effort level as `/state` reports it (not lower-cased; the segment is).
    pub fn thinking(&self) -> Option<&str> {
        self.thinking.as_deref()
    }

    /// `tui-model-label`: the bare id, or `id (provider)` when that id is registered
    /// under more than one provider — the bare id no longer names an endpoint.
    pub fn model_label(&self) -> Option<String> {
        let id = self.model.as_deref()?;
        let ambiguous = self
            .providers_by_model
            .get(id)
            .is_some_and(|providers| providers.len() > 1);
        match (ambiguous, self.provider.as_deref()) {
            (true, Some(provider)) => Some(format!("{} ({})", id, provider.to_lowercase())),
            _ => Some(id.to_string()),
        }
    }

    /// `context-label`: `ctx 48k/936k (5%)`, or `ctx 48k` without a window.
    pub fn context_label(&self) -> String {
        match self.context_window {
            Some(window) => {
                let percent = round_div_half_even(self.context_tokens * 100, window.max(1)).min(100);
                format!(
                    "ctx {}/{} ({}%)",
                    k_tokens(self.context_tokens),
                    k_tokens(window),
                    percent
                )
            }
            None => format!("ctx {}", k_tokens(self.context_tokens)),
        }
    }

    /// `cache-stats-label`: `97% cached`, hidden until some provider reports cache
    /// activity.
    pub fn cache_label(&self) -> Option<String> {
        self.cache
            .cached_percent()
            .map(|percent| format!("{}% cached", percent))
    }

    /// `goal-label`, with the tokens folded since the last `/state` on top of the
    /// kernel's count.
    pub fn goal_label(&self) -> Option<String> {
        self.goal.as_ref().map(|goal| goal.label(self.goal_run_tokens))
    }

    /// The segments the line is made of, in the TUI's order, each already final and
    /// hidden when empty — `["stub-a", "medium", "ctx 15k/200k (0%)", …]`.
    pub fn segments(&self) -> Vec<String> {
        let mut segments = Vec::with_capacity(5);
        if let Some(model) = self.model_label() {
            segments.push(model);
        }
        if let Some(thinking) = self.thinking.as_deref() {
            segments.push(thinking.to_lowercase());
        }
        segments.push(self.context_label());
        if let Some(cache) = self.cache_label() {
            segments.push(cache);
        }
        if let Some(goal) = self.goal_label() {
            segments.push(goal);
        }
        segments
    }

    /// The whole line: the segments joined with `" · "`, which is what the composer's
    /// status row renders (dim, truncated with an ellipsis, never wrapping the button).
    pub fn text(&self) -> String {
        self.segments().join(SEPARATOR)
    }
}

/// A JSON field as a string; null, a missing key and a non-string all read as `None`.
pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A JSON field as a `u64`; null, a missing key and a non-number read as 0.
pub(crate) fn u64_field(value: &Value, key: &str) -> u64 {
    optional_u64(value, key).unwrap_or(0)
}

/// A JSON field as a `u64`, `None` when it is absent or null.
pub(crate) fn optional_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}
