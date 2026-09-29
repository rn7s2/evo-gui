//! session — pure per-agent view model (no gpui, no I/O).
//!
//! CONTRACT (shared with the `transcript`, `composer`, `workspace` crates): the public
//! types below are what the UI crates render. Additive changes are fine; renames or
//! removals must be agreed with the coordinator first.
//!
//! Inputs are raw JSON (`serde_json::Value`) exactly as the server sends them, so this
//! crate does not depend on `swarm_client`.

/// Stable domain id of a row within one agent's transcript (never reused within a revision).
pub type RowId = u64;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    /// Bumped every time this row's content changes (UI uses it to know what to re-set).
    pub version: u64,
    pub kind: RowKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowKind {
    /// A user turn (`user-input`, `steering`, or a user message from /transcript).
    User { text: String },
    /// Assistant message. `markdown` is the whole source so far; UI calls
    /// `TextViewState::set_text(markdown)` whenever `version` changes.
    Assistant { markdown: String, thinking: String, streaming: bool, error: Option<String> },
    /// One tool call, completed by its result.
    Tool { call_id: String, name: String, arguments: String, result: Option<ToolResult> },
    /// A lane's `report` event.
    Report { done: String, evidence: String, next: String, blocked: String, requests: String },
    /// `output` lines and status events (compaction, provider-retry, run outcome...).
    Dim { style: DimStyle, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult { pub is_error: bool, pub content: String, pub content_chars: Option<u64> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimStyle { Dim, Notice, Error, Status }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus { Pending, InProgress, Done }

#[derive(Clone, Debug, PartialEq)]
pub struct Todo { pub text: String, pub status: TodoStatus }

/// Coordinator activity as the composer button sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity { Idle, Running, Compacting }

/// Lane list glyphs (§7.3): ● working, ◐ compacting, ○ idle, ◌ starting, ✗ down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneStatus { Working, Compacting, Idle, Starting, Down }
