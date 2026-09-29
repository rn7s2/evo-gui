//! Fixture sanity: every capture is real server output, complete, and the two forms of
//! each event capture (the raw SSE body and the same events as a JSON array) agree.
//!
//! A truncated fixture would silently weaken every other test in this crate — the
//! reducers would still "pass", just on less of the session — so the captures are
//! checked as data here.

mod common;

use common::{fixture, fixture_path, fixture_text, sse_events};
use serde_json::Value;

/// Every fixture the tests rely on, so a regenerate that loses one is loud.
const REQUIRED: &[&str] = &[
    // readiness, state and its variants
    "health.json",
    "state.json",
    "state-after-new.json",
    "state-final.json",
    "state-two-providers.json",
    "state-with-goal.json",
    "state-with-todos.json",
    // the session's context and the model catalog
    "transcript.json",
    "transcript-limit20.json",
    "transcript-empty.json",
    "registry.json",
    "registry-two-providers.json",
    "journal.json",
    "journal-limit4.json",
    "journal-limit40.json",
    "journal-empty.json",
    // the swarm
    "lanes.json",
    "lanes-lane1-working.json",
    "lanes-final.json",
    "lane1-transcript.json",
    "lane2-transcript.json",
    // the event streams, raw and as data
    "events-coordinator.json",
    "events-coordinator.sse",
    "events-compact.json",
    "events-compact.sse",
    "events-shutdown.json",
    "events-shutdown.sse",
    "events-session-switched.json",
    "events-session-switched.sse",
    "events-user-input.json",
    "events-user-input.sse",
    "lane1-events.json",
    "lane1-events.sse",
    "lane2-events.json",
    "lane2-events.sse",
    // the POST reply envelopes
    "reply-prompt.json",
    "reply-goal.json",
    "reply-new.json",
    "reply-compact.json",
    "reply-delegate-1.json",
    "reply-delegate-2.json",
    "reply-eval-user-input.json",
    "reply-shutdown.json",
    "reply-shutdown-two-providers.json",
];

#[test]
fn every_capture_is_present_and_parses() {
    for name in REQUIRED {
        let path = fixture_path(name);
        assert!(path.exists(), "missing fixture {name} — regenerate: python3 crates/session/tests/capture_fixtures.py");
        let text = fixture_text(name);
        assert!(text.ends_with('\n'), "{name} does not end in a newline: it may be truncated");
        if name.ends_with(".json") {
            let value: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!value.is_null(), "{name} is null");
        }
    }
}

/// The JSON array beside each SSE capture is the same events in the same order: the
/// reducer tests that read the `.sse` files and the ones that read the `.json` arrays are
/// looking at one session, not two.
#[test]
fn the_json_and_sse_event_captures_agree() {
    for stem in ["events-coordinator", "events-compact", "events-shutdown", "events-session-switched", "events-user-input", "lane1-events", "lane2-events"] {
        let from_json: Vec<(u64, String, Value)> = fixture(&format!("{stem}.json"))
            .as_array()
            .unwrap_or_else(|| panic!("{stem}.json is not an array"))
            .iter()
            .map(|event| {
                (
                    event["id"].as_u64().unwrap_or_else(|| panic!("{stem}: event without an id")),
                    event["event"].as_str().unwrap_or_else(|| panic!("{stem}: event without a name")).to_string(),
                    event["data"].clone(),
                )
            })
            .collect();
        let from_sse = sse_events(&format!("{stem}.sse"));
        assert!(!from_json.is_empty(), "{stem}: empty capture");
        assert_eq!(from_json, from_sse, "{stem}: the two captures describe different events");
    }
}
