//! The §7.3 status readout, checked against a captured session and against the TUI's
//! own formatting rules (`src/tui/tui.lisp`'s `fmt-ktokens` / `context-label` /
//! `goal-label`, `extensions/340-cache-stats.lisp`'s `cache-stats-label`).
//!
//! The synthesized cases say so: they exist because no captured session reaches every
//! branch (no session has a model id under two providers *and* a budgeted goal *and* a
//! half-way k figure). Every value the rules turn on is checked against Common Lisp's
//! own `round`.

mod common;

use common::{fixture, sse_events};
use serde_json::json;
use session::{cache_seed_limits, cache_seed_next_limit, cache_stats_from_journal, AgentModel, CacheTotals, Readout};

#[test]
fn the_captured_session_reads_as_the_tui_would_draw_it() {
    let mut readout = Readout::new();
    readout.apply_registry(&fixture("registry.json"));
    readout.apply_state(&fixture("state-final.json"));

    // The journal's `cache-stats` entry is all input, no cache: the segment stays hidden,
    // the same rule the extension applies.
    let totals = cache_stats_from_journal(&fixture("journal.json")).expect("the capture has one");
    assert_eq!(totals, CacheTotals { input: 100, cache_read: 0, cache_write: 0 });
    readout.set_cache_totals(totals);

    assert_eq!(readout.model_label().as_deref(), Some("stub-a"), "one provider, bare id");
    assert_eq!(readout.context_label(), "ctx 0k/200k (0%)");
    assert_eq!(readout.cache_label(), None);
    assert_eq!(readout.goal_label().as_deref(), Some("goal g-4cb9 (complete) 0k"));
    assert_eq!(
        readout.text(),
        "stub-a · medium · ctx 0k/200k (0%) · goal g-4cb9 (complete) 0k"
    );
}

#[test]
fn a_model_id_under_two_providers_names_the_live_one() {
    // The second capture registers `stub-a` under `:stub` and `:stub2`, which is what
    // makes the bare id ambiguous.
    let registry = fixture("registry-two-providers.json");
    let providers: Vec<String> = registry["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|model| model["id"] == "stub-a")
        .map(|model| model["provider"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(providers, vec!["stub", "stub2"]);

    let mut readout = Readout::new();
    readout.apply_registry(&registry);
    readout.apply_state(&fixture("state-two-providers.json"));
    assert_eq!(readout.model_label().as_deref(), Some("stub-a (stub)"));
    assert!(
        readout.text().starts_with("stub-a (stub) · medium · ctx 0k/200k (0%)"),
        "line: {}",
        readout.text()
    );
}

/// `(format nil "~dk" (round n 1000))` — CL `round` goes half to even.
#[test]
fn k_figures_round_half_to_even() {
    let cases = [(0u64, "0k"), (499, "0k"), (500, "0k"), (999, "1k"), (1500, "2k"), (2500, "2k"), (3500, "4k"), (48211, "48k"), (936000, "936k")];
    for (tokens, expected) in cases {
        let mut readout = Readout::new();
        readout.apply_state(&json!({ "model": "m", "context_tokens": tokens, "context_window": 4000000 }));
        assert_eq!(readout.context_label(), format!("ctx {expected}/4000k (0%)"), "tokens {tokens}");
    }
}

#[test]
fn the_context_segment_shows_the_spec_line() {
    // docs/PROMPT.md §7.3's own example figure: 48211 of 936000 is 5%, 48k of 936k.
    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "ark-deepseek-v4.1-flash", "thinking": "max", "context_tokens": 48211, "context_window": 936000 }));
    assert_eq!(readout.context_label(), "ctx 48k/936k (5%)");
    assert_eq!(readout.text(), "ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%)");

    // No window: the count alone.
    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "m", "context_tokens": 48211, "context_window": null }));
    assert_eq!(readout.context_label(), "ctx 48k");

    // A usage past the window never reads more than 100%.
    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "m", "context_tokens": 5000, "context_window": 1000 }));
    assert_eq!(readout.context_label(), "ctx 5k/1k (100%)");
}

#[test]
fn a_missing_model_or_thinking_is_hidden() {
    let mut readout = Readout::new();
    // No model at all, no thinking: the line is the context figure alone.
    readout.apply_state(&json!({ "model": null, "thinking": null, "context_tokens": 48000, "context_window": 200000 }));
    assert_eq!(readout.text(), "ctx 48k/200k (24%)");

    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "m", "thinking": "", "context_tokens": 0, "context_window": null }));
    assert_eq!(readout.text(), "m · ctx 0k");

    // The effort level is lower-cased, as `tui-thinking-label` does.
    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "m", "thinking": "XHigh", "context_tokens": 0, "context_window": null }));
    assert_eq!(readout.text(), "m · xhigh · ctx 0k");
}

#[test]
fn the_cache_segment_needs_cache_activity() {
    let mut readout = Readout::new();
    for (totals, expected) in [
        (CacheTotals { input: 0, cache_read: 0, cache_write: 0 }, None),
        // Input alone is not cache activity.
        (CacheTotals { input: 100, cache_read: 0, cache_write: 0 }, None),
        // The spec's example: 97% of 10000.
        (CacheTotals { input: 0, cache_read: 9700, cache_write: 300 }, Some("97% cached")),
        // A write with no read is cache activity too, and reads as 0%.
        (CacheTotals { input: 95, cache_read: 0, cache_write: 5 }, Some("0% cached")),
        (CacheTotals { input: 0, cache_read: 5, cache_write: 5 }, Some("50% cached")),
    ] {
        readout.set_cache_totals(totals);
        readout.apply_state(&json!({ "model": "m", "context_tokens": 0, "context_window": null }));
        assert_eq!(readout.cache_label().as_deref(), expected, "totals {totals:?}");
    }
}

#[test]
fn the_goal_segment_follows_the_tui_rule() {
    // docs/PROMPT.md §7.3's own example: `goal a1b2c3d4 (active) 12k/50k`.
    let mut readout = Readout::new();
    readout.apply_state(&json!({
        "model": "m",
        "goal": { "goal_id": "a1b2c3d4", "objective": "ship", "status": "active",
                  "token_budget": 50000, "tokens_used": 4000, "tokens_used_live": 8000 }
    }));
    assert_eq!(readout.goal_label().as_deref(), Some("goal a1b2c3d4 (active) 12k/50k"));
    assert_eq!(readout.text(), "m · ctx 0k · goal a1b2c3d4 (active) 12k/50k");

    // The budget half appears only when the goal has one — and a budget of zero is still
    // a budget: Lisp's `(and budget …)` is false only for NIL, never for 0.
    for (goal, expected) in [
        (json!({ "goal_id": "g", "status": "active", "token_budget": null, "tokens_used": 0, "tokens_used_live": 0 }),
         "goal g (active) 0k"),
        (json!({ "goal_id": "g", "status": "active", "token_budget": 0, "tokens_used": 0, "tokens_used_live": 0 }),
         "goal g (active) 0k/0k"),
        (json!({ "goal_id": "g", "status": "paused", "token_budget": 1500, "tokens_used": 500, "tokens_used_live": 1000 }),
         "goal g (paused) 2k/2k"),
    ] {
        let mut readout = Readout::new();
        readout.apply_state(&json!({ "model": "m", "goal": goal }));
        assert_eq!(readout.goal_label().as_deref(), Some(expected), "goal {goal}");
    }

    // No goal: the segment is gone.
    let mut readout = Readout::new();
    readout.apply_state(&json!({ "model": "m", "goal": null }));
    assert_eq!(readout.text(), "m · ctx 0k");
}

/// §7.3: "on every `message-end`, let `context_tokens = usage.input + usage.output +
/// usage.cache_read + usage.cache_write` and fold the cache totals … so the line moves
/// with the run instead of jumping at `settled`".
#[test]
fn a_message_end_moves_the_line_with_the_run() {
    let mut readout = Readout::new();
    readout.apply_state(&json!({
        "model": "ark-deepseek-v4.1-flash", "thinking": "max", "context_tokens": 0,
        "context_window": 936000,
        "goal": { "goal_id": "a1b2c3d4", "status": "active", "token_budget": 50000, "tokens_used": 0, "tokens_used_live": 0 }
    }));
    assert_eq!(readout.text(), "ark-deepseek-v4.1-flash · max · ctx 0k/936k (0%) · goal a1b2c3d4 (active) 0k/50k");

    assert!(readout.fold_message_end(&json!({
        "usage": { "input": 48211, "output": 100, "cache_read": 9700, "cache_write": 200 }
    })));
    assert_eq!(readout.context_tokens(), 58211);
    assert_eq!(readout.cache_totals(), CacheTotals { input: 48211, cache_read: 9700, cache_write: 200 });
    assert_eq!(
        readout.text(),
        "ark-deepseek-v4.1-flash · max · ctx 58k/936k (6%) · 17% cached · goal a1b2c3d4 (active) 58k/50k"
    );

    // All-zero usage (an aborted or failed request) moves nothing.
    let before = readout.text();
    assert!(!readout.fold_message_end(&json!({
        "usage": { "input": 0, "output": 0, "cache_read": 0, "cache_write": 0 }
    })));
    assert_eq!(readout.text(), before);

    // The next /state re-anchors: the kernel's own live count replaces what we folded.
    readout.apply_state(&json!({
        "model": "ark-deepseek-v4.1-flash", "thinking": "max", "context_tokens": 58211,
        "context_window": 936000,
        "goal": { "goal_id": "a1b2c3d4", "status": "complete", "token_budget": 50000, "tokens_used": 58211, "tokens_used_live": 0 }
    }));
    assert_eq!(
        readout.text(),
        "ark-deepseek-v4.1-flash · max · ctx 58k/936k (6%) · 17% cached · goal a1b2c3d4 (complete) 58k/50k"
    );
}

#[test]
fn segments_are_the_five_the_spec_names_in_order() {
    let mut readout = Readout::new();
    readout.apply_registry(&json!({ "models": [ { "id": "m", "provider": "stub" }, { "id": "m", "provider": "proxy" } ] }));
    readout.apply_state(&json!({
        "model": "m", "provider": "proxy", "thinking": "high", "context_tokens": 48211,
        "context_window": 936000,
        "goal": { "goal_id": "g", "status": "active", "token_budget": 50000, "tokens_used": 12000, "tokens_used_live": 0 }
    }));
    readout.set_cache_totals(CacheTotals { input: 300, cache_read: 9700, cache_write: 0 });
    assert_eq!(
        readout.segments(),
        vec![
            "m (proxy)",
            "high",
            "ctx 48k/936k (5%)",
            "97% cached",
            "goal g (active) 12k/50k",
        ]
    );
}

/// The journal walk: the seeded totals, and the limits to grow through when the entry is
/// not in the reply yet.
#[test]
fn the_cache_seed_comes_from_the_newest_journal_entry() {
    assert_eq!(cache_seed_limits(), &[20, 100, 400]);
    assert_eq!(cache_seed_next_limit(20), Some(100));
    assert_eq!(cache_seed_next_limit(100), Some(400));
    assert_eq!(cache_seed_next_limit(400), None, "run out of limits: no extension installed");
    assert_eq!(cache_seed_next_limit(7), None);

    // The capture's last entry: ten folds of the same session, `input` only.
    let totals = cache_stats_from_journal(&fixture("journal.json")).unwrap();
    assert_eq!(totals, CacheTotals { input: 100, cache_read: 0, cache_write: 0 });

    // The newest entry wins, not the first: a session that was resumed holds several.
    let journal = json!({ "entries": [
        { "type": "custom", "key": "cache-stats", "data": { "input": 10, "cache_read": 1, "cache_write": 2 } },
        { "type": "message", "message": { "role": "user" } },
        { "type": "custom", "key": "cache-stats", "data": { "input": 100, "cache_read": 9700, "cache_write": 200 } },
    ]});
    assert_eq!(
        cache_stats_from_journal(&journal),
        Some(CacheTotals { input: 100, cache_read: 9700, cache_write: 200 })
    );

    // A session that never used the extension: the load entry only.
    assert_eq!(cache_stats_from_journal(&fixture("journal-empty.json")), None);
    // …and an entry with no usable data is not a seed either.
    assert_eq!(
        cache_stats_from_journal(&json!({ "entries": [ { "type": "custom", "key": "cache-stats" } ] })),
        None
    );
    assert_eq!(cache_stats_from_journal(&json!({ "entries": [] })), None);
    assert_eq!(cache_stats_from_journal(&json!({})), None);
}

/// The seeded totals ride the same readout the UI renders: a session whose journal says
/// 97% shows it before any event arrives.
#[test]
fn a_seeded_cache_figure_shows_up_in_the_line() {
    let mut model = AgentModel::new();
    model.readout_mut().set_cache_totals(CacheTotals { input: 300, cache_read: 9700, cache_write: 0 });
    model.apply_state(&fixture("state-final.json"));
    assert!(model.readout().text().contains("97% cached"), "line: {}", model.readout().text());
}

/// The whole capture, one event at a time: the readout the UI ends up with is the one the
/// same session's `/state` describes (a live fold never disagrees with a resync on the
/// figures both of them carry).
#[test]
fn folding_the_capture_agrees_with_the_state_the_server_reports() {
    let mut model = AgentModel::new();
    model.readout_mut().set_cache_totals(cache_stats_from_journal(&fixture("journal.json")).unwrap());
    for (id, kind, data) in sse_events("events-coordinator.sse") {
        model.apply_event(id, &kind, &data);
    }
    let state = fixture("state-final.json");
    let folded = model.readout();
    assert_eq!(folded.context_tokens(), state["context_tokens"].as_u64().unwrap());
    assert_eq!(
        folded.cache_totals(),
        CacheTotals { input: 100, cache_read: 0, cache_write: 0 },
        "the folded totals are the journal's own running totals"
    );
    assert_eq!(folded.goal_label().as_deref(), Some("goal g-4cb9 (complete) 0k"));
}
