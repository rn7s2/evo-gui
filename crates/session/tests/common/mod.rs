//! Shared fixture access for the integration tests.
//!
//! The files under `tests/fixtures/` are captures from a real `evo-swarm serve` with the
//! stub-provider harness of `evo-agent/tests/swarm-serve-e2e.py`; see
//! `tests/capture_fixtures.py` for how they were recorded, and each fixture's own content
//! for what the server actually says.

#![allow(dead_code)]

use std::path::PathBuf;

use serde_json::Value;
use session::{AgentModel, Effect};

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

/// One SSE capture as `(id, event, data)` in the order the server sent them.
///
/// A minimal reader for the capture files only: the product's resumable SSE parser
/// belongs to `swarm_client` and is not this crate's job.
pub fn sse_events(name: &str) -> Vec<(u64, String, Value)> {
    let text = fixture_text(name);
    let mut events = Vec::new();
    let mut id: Option<u64> = None;
    let mut kind: Option<String> = None;
    let mut data: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(kind) = kind.take() {
                let data = data
                    .take()
                    .map(|data| serde_json::from_str(&data).unwrap_or(Value::Null))
                    .unwrap_or(Value::Null);
                events.push((
                    id.take().expect("every captured event has an id"),
                    kind,
                    data,
                ));
            }
            id = None;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("id: ") {
            id = Some(rest.parse().expect("captured ids are integers"));
        } else if let Some(rest) = line.strip_prefix("event: ") {
            kind = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }
    events
}

/// Feed a whole capture into a model, returning the union of the effects.
pub fn apply_capture(model: &mut AgentModel, name: &str) -> Effect {
    let mut effect = Effect::NONE;
    for (id, kind, data) in sse_events(name) {
        effect |= model.apply_event(id, &kind, &data);
    }
    effect
}

/// What one row renders, without the model's own ids — how the two assembly paths are
/// compared.
#[derive(Clone, Debug, PartialEq)]
pub enum RowView {
    User(String),
    Context {
        key: String,
        text: String,
    },
    Assistant {
        markdown: String,
        thinking: String,
        error: Option<String>,
    },
    Tool {
        name: String,
        arguments: String,
        result: Option<(bool, String)>,
    },
    Report {
        done: String,
        evidence: String,
        next: String,
        blocked: String,
        requests: String,
        goal: Option<String>,
        lane: Option<u32>,
    },
    LaneNotice {
        lane: u32,
        text: String,
        tone: session::DimStyle,
    },
    GoalNudge {
        kind: session::GoalNudgeKind,
        objective: String,
        budget: String,
        text: String,
    },
    CommandNote {
        command: String,
        text: String,
    },
    Dim(String),
    RunOutcome {
        outcome: String,
        text: String,
    },
}

impl RowView {
    pub fn of(row: &session::Row) -> RowView {
        match &row.kind {
            session::RowKind::User { text } => RowView::User(text.clone()),
            session::RowKind::Context { key, text } => RowView::Context {
                key: key.clone(),
                text: text.clone(),
            },
            session::RowKind::Assistant {
                markdown,
                thinking,
                error,
                ..
            } => RowView::Assistant {
                markdown: markdown.clone(),
                thinking: thinking.clone(),
                error: error.clone(),
            },
            session::RowKind::Tool {
                name,
                arguments,
                result,
                ..
            } => RowView::Tool {
                name: name.clone(),
                arguments: arguments.clone(),
                result: result.as_ref().map(|r| (r.is_error, r.content.clone())),
            },
            session::RowKind::Report {
                done,
                evidence,
                next,
                blocked,
                requests,
                goal,
                lane,
            } => RowView::Report {
                done: done.clone(),
                evidence: evidence.clone(),
                next: next.clone(),
                blocked: blocked.clone(),
                requests: requests.clone(),
                goal: goal.clone(),
                lane: *lane,
            },
            session::RowKind::LaneNotice { lane, text, tone } => RowView::LaneNotice {
                lane: *lane,
                text: text.clone(),
                tone: *tone,
            },
            session::RowKind::GoalNudge {
                kind,
                objective,
                budget,
                text,
            } => RowView::GoalNudge {
                kind: *kind,
                objective: objective.clone(),
                budget: budget.clone(),
                text: text.clone(),
            },
            session::RowKind::CommandNote { command, text } => RowView::CommandNote {
                command: command.clone(),
                text: text.clone(),
            },
            session::RowKind::Dim { text, .. } => RowView::Dim(text.clone()),
            session::RowKind::RunOutcome { outcome, text } => RowView::RunOutcome {
                outcome: outcome.clone(),
                text: text.clone(),
            },
        }
    }
}

/// The transcript rows of a model: everything the transcript fold carries, i.e. the
/// user turns, assistant messages and tool calls — `output` lines, status rows and a
/// run's outcome are events only and never reach `/transcript`.
pub fn transcript_rows(model: &AgentModel) -> Vec<RowView> {
    model
        .rows()
        .iter()
        .map(RowView::of)
        .filter(|row| !matches!(row, RowView::Dim(_) | RowView::RunOutcome { .. }))
        .collect()
}
