//! Shared fixture access for the integration tests.
//!
//! The files under `tests/fixtures/` are the wire as CONTRACT §4/§5 describes it: a
//! `/snapshot` body per topic, a page of older items, a stream of ops, and the two
//! catalog shapes the choosers read. They are hand-written to the contract rather than
//! captured, because the protocol is the contract now — a capture would only echo it.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub fn fixture_text(name: &str) -> String {
    let path = fixture_path(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("fixture {} is unreadable: {error}", path.display()))
}

/// One fixture as JSON.
pub fn fixture(name: &str) -> Value {
    let text = fixture_text(name);
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("fixture {name} is not JSON: {error}"))
}

/// The per-topic body of a whole `/snapshot` reply for one topic.
pub fn topic_body(snapshot: &Value, topic: &str) -> Value {
    snapshot["topics"][topic].clone()
}

/// A captured stream as `(op, topic, data)` in the order the server sent them. A frame
/// without a topic is the stream's own (`hello`, `stream.reset`).
pub fn ops(name: &str) -> Vec<(String, Option<String>, Value)> {
    fixture(name)
        .as_array()
        .unwrap_or_else(|| panic!("{name} is not an array of frames"))
        .iter()
        .map(|frame| {
            (
                frame["op"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: a frame without an op"))
                    .to_string(),
                frame["topic"].as_str().map(str::to_string),
                frame["data"].clone(),
            )
        })
        .collect()
}
