//! Seeding the cache figure from the journal.
//!
//! The cache segment is not in `/state`: the coordinator's `340-cache-stats.lisp`
//! extension persists `{:input, :cache-read, :cache-write}` running totals as a journal
//! `:custom` entry under the key `cache-stats` (invisible to the model, survives restart
//! and compaction). The GUI seeds its own totals once per tab from the newest such entry
//! and then folds each `message-end`'s usage, exactly as the TUI's hook does.
//!
//! The walk is deliberately bounded: `GET /journal?limit=N` only, trying 20, then 100,
//! then 400, growing the limit *only* while the entry is not found — so a long session
//! is never re-sent in full. Without the extension the entry never appears and the
//! segment stays hidden, the same rule the TUI applies.

use serde_json::Value;

use crate::readout::CacheTotals;

/// The limits to try, in order, for `GET /journal?limit=N`. Start at the smallest that
/// covers a recent entry and grow only when it is missing.
const SEED_LIMITS: [u64; 3] = [20, 100, 400];

/// The `:custom` key the cache totals persist under (`340-cache-stats.lisp`).
const CACHE_STATS_KEY: &str = "cache-stats";

/// The limits to ask for, in order: `20`, `100`, `400`.
pub fn cache_seed_limits() -> &'static [u64] {
    &SEED_LIMITS
}

/// The next limit to ask for after `limit` came back without the entry, or `None` when
/// there is nothing left to try — "not found, grow limit" has run out of limits, so the
/// segment stays hidden (the extension is not installed).
pub fn cache_seed_next_limit(limit: u64) -> Option<u64> {
    let position = SEED_LIMITS.iter().position(|l| *l == limit)?;
    SEED_LIMITS.get(position + 1).copied()
}

/// The newest `cache-stats` entry in a `GET /journal` body, or `None` when the reply
/// holds no such entry (grow the limit and ask again) or the entry is unusable.
///
/// `entries` is the root→leaf path, so the newest entry is the **last** match: a journal
/// that was resumed or rewound can hold several.
pub fn cache_stats_from_journal(journal: &Value) -> Option<CacheTotals> {
    let entries = journal.get("entries")?.as_array()?;
    let mut totals = None;
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("key").and_then(Value::as_str) != Some(CACHE_STATS_KEY)
        {
            continue;
        }
        totals = Some(entry.get("data").cloned().unwrap_or(Value::Null));
    }
    let data = totals?;
    if !data.is_object() {
        return None;
    }
    let mut totals = CacheTotals::default();
    totals.fold(&data);
    Some(totals)
}
