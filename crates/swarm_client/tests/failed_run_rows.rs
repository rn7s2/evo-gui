//! A real swarm whose model dies mid-stream — `docs/screens/09-bad-run.png`'s run — and
//! the rows the tab makes of it.
//!
//! The stub model sends half a sentence and hangs up without a terminal event, so evo's
//! provider retries four times and the run ends `:error`. The events are folded through
//! `session::AgentModel`, the way a tab folds them, and the rows are asked what they say:
//! once, through the failing message's own row, with no outcome row repeating it — and a
//! rebuild from `/transcript`, which is what a resync does, says the same one line.
//!
//! Run with `CARGO_TARGET_DIR=target/swarm_client cargo test -p swarm_client`.

#![cfg(feature = "test-harness")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use swarm_client::harness::{Harness, HarnessConfig, TempDir};
use swarm_client::{EventStream, StreamConfig, StreamMsg, StreamTarget};

use session::{AgentModel, RowKind};

/// The model that dies: one content block, half a sentence, no terminal event.
const TRUNCATING_MODEL_PY: &str = r####"#!/usr/bin/env python3
"""The model that dies mid-stream (written by failed_run_rows.rs)."""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        def sse(event, data):
            self.wfile.write(("event: %s\ndata: %s\n\n" % (event, json.dumps(data))).encode())
            self.wfile.flush()

        sse("message_start", {"type": "message_start",
                              "message": {"id": "msg_stub", "type": "message",
                                          "role": "assistant", "model": "stub-a",
                                          "content": [], "usage": {"input_tokens": 10,
                                                                   "output_tokens": 0}}})
        sse("content_block_start", {"type": "content_block_start", "index": 0,
                                    "content_block": {"type": "text", "text": ""}})
        sse("content_block_delta", {"type": "content_block_delta", "index": 0,
                                    "delta": {"type": "text_delta",
                                              "text": "half a sentence "}})
        # No content_block_stop, no message_delta, no message_stop: the provider died.
        self.close_connection = True


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    print("stub listening %d" % server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
"####;

fn failing_stub() -> (TempDir, PathBuf) {
    let dir = TempDir::new("failed-run-stub").expect("temp dir");
    let path = dir.join("truncating-model.py");
    std::fs::write(&path, TRUNCATING_MODEL_PY).expect("write the stub");
    (dir, path)
}

/// How many rows say NEEDLE, counting every kind of row that draws words.
fn mentions(model: &AgentModel, needle: &str) -> usize {
    model
        .rows()
        .iter()
        .filter(|row| match &row.kind {
            RowKind::Dim { text, .. } | RowKind::RunOutcome { text, .. } => text.contains(needle),
            RowKind::Assistant {
                markdown, error, ..
            } => markdown.contains(needle) || error.as_deref().is_some_and(|e| e.contains(needle)),
            _ => false,
        })
        .count()
}

#[test]
fn a_real_failed_run_is_said_once() {
    // The harness starts the stub `EVO_STUB_MESSAGES` names; this test's own process, so
    // setting it here reaches nothing else.
    let (_stub_dir, stub) = failing_stub();
    std::env::set_var("EVO_STUB_MESSAGES", &stub);

    let mut harness = Harness::start(HarnessConfig {
        workers: 1,
        ..Default::default()
    })
    .expect("the swarm should come up");
    let client = harness.client().clone();
    let stream = EventStream::start(
        StreamTarget::coordinator(&client, None),
        StreamConfig {
            reconnect: false,
            ..Default::default()
        },
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(StreamMsg::Connected { .. }) = stream.try_recv() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    client.prompt("FAIL: the provider is down").expect("prompt");

    // Fold the run the way a tab does, remembering the order the events arrived in.
    let mut model = AgentModel::new();
    let mut events: Vec<Value> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(150);
    while Instant::now() < deadline {
        match stream.try_recv() {
            Some(StreamMsg::Event { id, kind, data }) => {
                model.apply_event(id.unwrap_or(0) as u64, &kind, &data);
                if kind == "settled" {
                    break;
                }
                events.push(data.clone());
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let failure = model
        .rows()
        .iter()
        .find_map(|row| match &row.kind {
            RowKind::Assistant { error: Some(e), .. } if !e.is_empty() => Some(e.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no failed message at all: {:?}", model.rows()));

    // The order the fold relies on: every run that ended `:error` had a `message-end`
    // carrying that error immediately before it.
    let failed_runs: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, data)| {
            data.get("type").and_then(Value::as_str) == Some("run-end")
                && data.get("outcome").and_then(Value::as_str) == Some("error")
        })
        .map(|(at, _)| at)
        .collect();
    assert!(
        !failed_runs.is_empty(),
        "the stub should have failed a run: {events:?}"
    );
    for at in &failed_runs {
        let before = &events[at - 1];
        assert_eq!(
            before.get("type").and_then(Value::as_str),
            Some("message-end"),
            "run-end at {at} follows {before}"
        );
        assert_eq!(
            before.get("error").and_then(Value::as_str),
            Some(failure.as_str()),
            "and carries the run's own error"
        );
    }

    // One line per failed run — the message's, not the outcome's — and no row repeats it.
    assert_eq!(
        mentions(&model, &failure),
        failed_runs.len(),
        "{:?}",
        model.rows()
    );
    assert!(
        model
            .rows()
            .iter()
            .all(|row| !matches!(row.kind, RowKind::RunOutcome { .. })),
        "the outcome rows would double every one of them: {:?}",
        model.rows()
    );

    // A resync rebuilds the rows from `/transcript` and says the same thing.
    let transcript = client.transcript(Some(50)).expect("GET /transcript");
    let mut rebuilt = AgentModel::new();
    rebuilt.rebuild_from_transcript(&transcript.raw);
    assert_eq!(
        mentions(&rebuilt, &failure),
        failed_runs.len(),
        "{:?}",
        rebuilt.rows()
    );

    let _ = harness.shutdown();
}
