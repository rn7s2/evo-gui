//! The serve protocol's wire types, exactly as CONTRACT.md describes them.
//!
//! One source of truth for the bytes on the wire, used by every layer above:
//!
//! * §1 — the **ready file** a serving child writes once it is listening
//!   ([`ReadyFile`]). Its `epoch` identifies one process lifetime; a cursor from
//!   another epoch can never be resumed, which is how a restart is detected
//!   without anyone watching a pid.
//! * §4.1 — **items**, the ordered transcript objects ([`Item`], [`ItemKind`]).
//!   Ids are the journal entry ids, so they are stable across refetch and
//!   restart. An unknown `kind` decodes as [`ItemKind::Other`] rather than
//!   failing the whole read: a newer server must not break an older client.
//! * §4.2/§4.3 — **topic state** ([`SessionState`], [`SwarmState`], [`LaneInfo`]).
//! * §5.2 — the **snapshot** response; §5.3 — the **ops** of the op stream;
//!   §5.5 — the `POST /ops` request/reply envelope and its error codes;
//!   §5.6 — the **catalog**; §2 — the `sessions --json` and `check --json` bodies.
//!
//! Beside the types, [`TopicMirror`] is the client half of the view model: an
//! ordered list of items plus one state object, mutated only by [`TopicMirror::apply`].
//! A snapshot seeds a mirror; the op stream then carries it forward, and a
//! `topic.reset`/`stream.reset` tells the owner to re-seed it. The evo-agent side
//! projects a journal to items and publishes the same ops it derives — the two
//! paths agree by construction, which is what makes a mirror trustworthy.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The topic that holds the coordinator's (or a lone agent's) items and state.
pub const TOPIC_SESSION: &str = "session";
/// The topic evo-swarm publishes its configuration and lane list under.
pub const TOPIC_SWARM: &str = "swarm";
/// `lane:*` in a request means "every lane topic".
pub const TOPIC_LANE_WILDCARD: &str = "lane:*";

/// The topic name of lane `n` (§4.3).
pub fn lane_topic(n: u32) -> String {
    format!("lane:{n}")
}

/// The lane number a `lane:N` topic names, if it is one.
pub fn lane_number(topic: &str) -> Option<u32> {
    topic.strip_prefix("lane:")?.parse().ok()
}

// ---------------------------------------------------------------------------
// §1 the ready file
// ---------------------------------------------------------------------------

/// `<tabdir>/ready.json`, written atomically (tmp + rename, mode 0600) by the
/// serving child once it is listening, rewritten after every supervisor restart
/// and deleted on a clean shutdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadyFile {
    /// One per process lifetime; every cursor is `<epoch>.<seq>`.
    pub epoch: String,
    pub pid: u32,
    #[serde(default)]
    pub supervisor_pid: Option<u32>,
    pub port: u16,
    /// `http://127.0.0.1:<port>/`.
    pub url: String,
    /// 64 hex characters. Never logged, never rendered.
    pub token: String,
    pub session: SessionRef,
    /// `evo-agent` or `evo-swarm`.
    pub program: String,
    pub version: String,
    #[serde(default)]
    pub restarts: u32,
}

/// The session a server is serving, as recorded in the ready file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// §4.1 items
// ---------------------------------------------------------------------------

/// One thing a frontend draws: an ordered object with a stable id.
///
/// `id`, `kind` and `ts` are common to every item; the rest is [`ItemKind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// Epoch milliseconds.
    #[serde(default)]
    pub ts: i64,
    #[serde(flatten)]
    pub kind: ItemKind,
}

/// The kind-specific fields of an [`Item`].
///
/// Fields are `Option`/defaulted throughout: this is a wire format read from a
/// server that may be *newer* than the client, and a missing field must not cost
/// the whole frame. Enums that carry values are plain strings on the wire
/// (`after_run`, `lane_report`) and unknown values decode as `Other`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemKind {
    User {
        #[serde(default)]
        text: String,
        #[serde(default)]
        images: Vec<ImageRef>,
        #[serde(default)]
        status: UserStatus,
        #[serde(default)]
        queue: QueueMode,
    },
    Assistant {
        #[serde(default)]
        text: String,
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        status: AssistantStatus,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    Tool {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        status: ToolStatus,
        #[serde(default)]
        result: Option<ToolResult>,
        /// The id of the assistant item this call belongs to.
        #[serde(default)]
        parent: Option<String>,
    },
    LaneReport {
        #[serde(default)]
        lane: Option<u32>,
        #[serde(default)]
        done: Option<String>,
        #[serde(default)]
        evidence: Option<String>,
        #[serde(default, rename = "next")]
        next_step: Option<String>,
        #[serde(default)]
        blocked: Option<String>,
        #[serde(default)]
        requests: Option<String>,
        #[serde(default)]
        goal: Option<String>,
    },
    LaneEvent {
        #[serde(default)]
        lane: Option<u32>,
        #[serde(default)]
        event: LaneEventKind,
        #[serde(default)]
        outcome: Option<String>,
        #[serde(default)]
        goal_status: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        severity: Option<String>,
    },
    Goal {
        #[serde(default)]
        event: GoalEventKind,
        #[serde(default)]
        goal_id: Option<String>,
        #[serde(default)]
        objective: Option<String>,
        #[serde(default)]
        budget: Option<Value>,
        #[serde(default)]
        tokens: Option<u64>,
    },
    Context {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        text: String,
    },
    CommandNote {
        #[serde(default)]
        command: String,
        #[serde(default)]
        text: String,
    },
    HumanAction {
        #[serde(default)]
        action: String,
        #[serde(default)]
        lanes: Vec<u32>,
    },
    Notice {
        #[serde(default)]
        severity: NoticeSeverity,
        #[serde(default)]
        text: String,
        #[serde(default)]
        source: Option<String>,
        /// Ephemeral notices (`false`) live only in the live view.
        #[serde(default)]
        durable: bool,
    },
    RunOutcome {
        #[serde(default)]
        outcome: RunOutcomeKind,
        #[serde(default)]
        error: Option<String>,
    },
    Compaction {
        #[serde(default)]
        summary: String,
        #[serde(default)]
        tokens_before: Option<u64>,
        #[serde(default)]
        tokens_after: Option<u64>,
        #[serde(default)]
        manual: bool,
    },
    Recovery {
        #[serde(default)]
        status: String,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default)]
        reason: Option<String>,
    },
    ProviderRetry {
        #[serde(default)]
        attempt: u32,
        #[serde(default, rename = "max")]
        max_attempts: u32,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        reason: String,
    },
    /// A kind this client does not know. Tolerated, never fatal (§4.1).
    #[serde(other)]
    Other,
}

impl ItemKind {
    /// The `kind` string of the wire, `""` for [`ItemKind::Other`].
    pub fn name(&self) -> &'static str {
        match self {
            ItemKind::User { .. } => "user",
            ItemKind::Assistant { .. } => "assistant",
            ItemKind::Tool { .. } => "tool",
            ItemKind::LaneReport { .. } => "lane_report",
            ItemKind::LaneEvent { .. } => "lane_event",
            ItemKind::Goal { .. } => "goal",
            ItemKind::Context { .. } => "context",
            ItemKind::CommandNote { .. } => "command_note",
            ItemKind::HumanAction { .. } => "human_action",
            ItemKind::Notice { .. } => "notice",
            ItemKind::RunOutcome { .. } => "run_outcome",
            ItemKind::Compaction { .. } => "compaction",
            ItemKind::Recovery { .. } => "recovery",
            ItemKind::ProviderRetry { .. } => "provider_retry",
            ItemKind::Other => "",
        }
    }
}

impl Item {
    /// The wire `kind` string, read from the raw JSON when the typed kind is
    /// [`ItemKind::Other`].
    pub fn kind_name(&self) -> &str {
        self.kind.name()
    }
}

/// An image attached to a user item. `href` is `/media/<id>/<n>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub href: String,
}

/// `queued` while it waits in the queue, `sent` once the run took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    #[default]
    Queued,
    Sent,
    Cancelled,
    #[serde(other)]
    Other,
}

/// `now` = steer the run in flight, `after_run` = queue it for the next turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueMode {
    #[default]
    Now,
    AfterRun,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantStatus {
    #[default]
    Streaming,
    Final,
    Error,
    Aborted,
    Length,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    #[default]
    Running,
    Ok,
    Error,
    Blocked,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    #[default]
    Info,
    Warn,
    Error,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcomeKind {
    #[default]
    Aborted,
    Error,
    Length,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneEventKind {
    #[default]
    RunEnded,
    Error,
    FailedToStart,
    InitFailed,
    Crashed,
    Down,
    Restarted,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalEventKind {
    #[default]
    Created,
    Continue,
    Wrapup,
    ObjectiveUpdated,
    Paused,
    Resumed,
    Complete,
    BudgetLimited,
    #[serde(other)]
    Other,
}

/// A tool result, truncated to 4 KiB in snapshots and ops (§4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub chars: u64,
    #[serde(default)]
    pub truncated: bool,
}

/// Token counts of one assistant turn (`cache_read`/`cache_write` are the
/// provider's cache numbers, zero when it has none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
}

// ---------------------------------------------------------------------------
// §4.2/§4.3 topic state
// ---------------------------------------------------------------------------

/// Topic `session` (and each `lane:N`) state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub status: AgentStatus,
    #[serde(default)]
    pub task: Option<TaskInfo>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub context: Option<ContextUsage>,
    #[serde(default)]
    pub goal: Option<GoalState>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    #[serde(default)]
    pub queue: Vec<String>,
    #[serde(default)]
    pub jobs: Vec<JobInfo>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub session: Option<SessionInfo>,
}

/// `waiting` = a coordinator that has settled but is held while its lanes work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Idle,
    Running,
    Compacting,
    Waiting,
    #[serde(other)]
    Other,
}

/// The work in flight: a run or a compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInfo {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub turn: Option<u64>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub step_started_at: Option<i64>,
}

/// `{"id","provider"}` — the model topic state and the catalog agree on this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub window: u64,
    /// `usage` when the provider reported it, `estimate` otherwise.
    #[serde(default)]
    pub source: Option<String>,
}

/// A goal as the state topic reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalState {
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub budget: Option<Value>,
    #[serde(default)]
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Done,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// One status-line segment, from the core's segment registry, so the TUI's
/// status line and the GUI's readout cannot drift apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub side: SegmentSide,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentSide {
    #[default]
    Left,
    Right,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub leaf: Option<String>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// Topic `swarm`: evo-swarm only (§4.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwarmState {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub workers: Option<u32>,
    #[serde(default)]
    pub status: SwarmStatus,
    #[serde(default)]
    pub config: SwarmConfig,
    #[serde(default)]
    pub lanes: Vec<LaneInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwarmStatus {
    #[serde(default)]
    pub busy: u32,
    #[serde(default)]
    pub waiting_on_lanes: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SwarmConfig {
    #[serde(default)]
    pub lane_model: Option<ModelRef>,
    #[serde(default)]
    pub lane_thinking: Option<String>,
}

/// One lane, as the swarm topic reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneInfo {
    #[serde(default)]
    pub n: u32,
    #[serde(default)]
    pub state: LaneState,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub task_started_at: Option<i64>,
    #[serde(default)]
    pub step_started_at: Option<i64>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub context: Option<ContextUsage>,
    #[serde(default)]
    pub goal: Option<GoalState>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    #[serde(default)]
    pub reports: u32,
    #[serde(default)]
    pub last_item: Option<LastItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    #[default]
    Starting,
    Idle,
    Working,
    Compacting,
    Down,
    Stopped,
    #[serde(other)]
    Other,
}

/// A one-line summary of a lane's newest item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastItem {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub summary: String,
}

// ---------------------------------------------------------------------------
// §5.2 snapshot, §5.3 ops
// ---------------------------------------------------------------------------

/// `GET /snapshot?topics=…&items=N` — atomic across the topics requested:
/// every op with `seq` ≤ this `seq` is reflected, none after.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub epoch: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub topics: HashMap<String, TopicSnapshot>,
}

/// One topic inside a [`Snapshot`]. `items` is present only for item-bearing
/// topics and holds the newest N.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicSnapshot {
    #[serde(default)]
    pub state: Value,
    #[serde(default)]
    pub items: Vec<Value>,
    /// Older items exist on the server (`GET /items?before=`).
    #[serde(default)]
    pub has_more: bool,
}

/// An op of the stream (§5.3). `seq`, `ts` and `topic` are the envelope; the
/// payload is [`OpKind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub ts: Option<i64>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(flatten)]
    pub kind: OpKind,
}

/// The payloads of the op stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpKind {
    /// Always the first frame of a stream: where this stream starts. The `seq`
    /// of a hello is the envelope's `seq`; `epoch` is the process that owns it.
    #[serde(rename = "hello")]
    Hello { epoch: String },
    #[serde(rename = "item.add")]
    ItemAdd {
        item: Value,
        /// The id the new item follows, or null for "at the end".
        #[serde(default)]
        after: Option<String>,
    },
    /// A text field grew. Appends for one item are coalesced over ≤ 50 ms.
    #[serde(rename = "item.append")]
    ItemAppend {
        id: String,
        /// `text` or `thinking`.
        field: String,
        text: String,
    },
    /// A JSON merge patch on one item; arrays are replaced whole.
    #[serde(rename = "item.patch")]
    ItemPatch { id: String, patch: Value },
    #[serde(rename = "item.remove")]
    ItemRemove { id: String },
    /// A JSON merge patch on the topic's state.
    #[serde(rename = "state.patch")]
    StatePatch { patch: Value },
    /// Re-snapshot this one topic.
    #[serde(rename = "topic.reset")]
    TopicReset { reason: TopicResetReason },
    /// Re-snapshot everything and reconnect with the new cursor.
    #[serde(rename = "stream.reset")]
    StreamReset { reason: StreamResetReason },
    /// An op this client does not know. Skipped, never fatal.
    #[serde(other)]
    Unknown,
}

impl OpKind {
    /// The `op` string of the wire, `""` for [`OpKind::Unknown`].
    pub fn name(&self) -> &'static str {
        match self {
            OpKind::Hello { .. } => "hello",
            OpKind::ItemAdd { .. } => "item.add",
            OpKind::ItemAppend { .. } => "item.append",
            OpKind::ItemPatch { .. } => "item.patch",
            OpKind::ItemRemove { .. } => "item.remove",
            OpKind::StatePatch { .. } => "state.patch",
            OpKind::TopicReset { .. } => "topic.reset",
            OpKind::StreamReset { .. } => "stream.reset",
            OpKind::Unknown => "",
        }
    }
}

/// Why one topic must be re-snapshotted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicResetReason {
    #[default]
    SessionSwitched,
    LeafMoved,
    LaneRestarted,
    SwarmSwitched,
    #[serde(other)]
    Other,
}

/// Why the stream itself must be re-read from a new cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamResetReason {
    #[default]
    Restarted,
    CursorTooOld,
    CursorUnknown,
    #[serde(other)]
    Other,
}

/// `<epoch>.<seq>`, the cursor a stream resumes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor {
    pub epoch: String,
    pub seq: u64,
}

impl Cursor {
    pub fn new(epoch: impl Into<String>, seq: u64) -> Cursor {
        Cursor {
            epoch: epoch.into(),
            seq,
        }
    }

    /// `"<epoch>.<seq>"` for the `since` query parameter.
    pub fn encode(&self) -> String {
        format!("{}.{}", self.epoch, self.seq)
    }

    /// Parse a cursor, rejecting anything that is not `epoch.seq`.
    pub fn decode(text: &str) -> Option<Cursor> {
        let (epoch, seq) = text.rsplit_once('.')?;
        if epoch.is_empty() {
            return None;
        }
        Some(Cursor::new(epoch, seq.parse().ok()?))
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode())
    }
}

// ---------------------------------------------------------------------------
// §5.5 POST /ops
// ---------------------------------------------------------------------------

/// The request body of `POST /ops`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpRequest {
    /// A client-generated uuid; a retried rid must not act twice.
    pub rid: String,
    pub op: String,
    #[serde(default)]
    pub args: Value,
}

impl OpRequest {
    pub fn new(rid: impl Into<String>, op: impl Into<String>, args: Value) -> OpRequest {
        let args = if args.is_null() {
            Value::Object(Map::new())
        } else {
            args
        };
        OpRequest {
            rid: rid.into(),
            op: op.into(),
            args,
        }
    }
}

/// The reply to an [`OpRequest`]. Always HTTP 200 when answered: a client
/// branches on `ok`/`error.code` alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpReply {
    pub rid: String,
    pub ok: bool,
    /// The op-log position at which this operation's effects are visible.
    #[serde(default)]
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<OpError>,
}

impl OpReply {
    /// The error, whether the server said `ok:false` or sent an `error` object.
    pub fn error(&self) -> Option<&OpError> {
        self.error.as_ref()
    }

    /// The error code, if any.
    pub fn code(&self) -> Option<ErrorCode> {
        self.error.as_ref().map(|e| e.code)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub detail: Value,
}

/// The refusal codes of §5.5. Unknown codes decode as [`ErrorCode::Unknown`],
/// so a newer server cannot turn a clean refusal into a parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Busy,
    NotQuiescent,
    NoTask,
    GoalState,
    AlreadySent,
    UnknownOp,
    InvalidArgs,
    NotFound,
    ModelNotReady,
    OpFailed,
    ShuttingDown,
    Unknown,
}

impl ErrorCode {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Busy => "busy",
            ErrorCode::NotQuiescent => "not_quiescent",
            ErrorCode::NoTask => "no_task",
            ErrorCode::GoalState => "goal_state",
            ErrorCode::AlreadySent => "already_sent",
            ErrorCode::UnknownOp => "unknown_op",
            ErrorCode::InvalidArgs => "invalid_args",
            ErrorCode::NotFound => "not_found",
            ErrorCode::ModelNotReady => "model_not_ready",
            ErrorCode::OpFailed => "op_failed",
            ErrorCode::ShuttingDown => "shutting_down",
            ErrorCode::Unknown => "unknown",
        }
    }

    fn parse(text: &str) -> ErrorCode {
        match text {
            "busy" => ErrorCode::Busy,
            "not_quiescent" => ErrorCode::NotQuiescent,
            "no_task" => ErrorCode::NoTask,
            "goal_state" => ErrorCode::GoalState,
            "already_sent" => ErrorCode::AlreadySent,
            "unknown_op" => ErrorCode::UnknownOp,
            "invalid_args" => ErrorCode::InvalidArgs,
            "not_found" => ErrorCode::NotFound,
            "model_not_ready" => ErrorCode::ModelNotReady,
            "op_failed" => ErrorCode::OpFailed,
            "shutting_down" => ErrorCode::ShuttingDown,
            _ => ErrorCode::Unknown,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ErrorCode, D::Error> {
        let text = String::deserialize(d)?;
        Ok(ErrorCode::parse(&text))
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result of `input.send`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSendResult {
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub queued: bool,
    /// `"model_not_ready"` when the input was taken but cannot run yet.
    #[serde(default)]
    pub blocked: Option<String>,
}

/// The result of `run.interrupt`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterruptResult {
    /// `"session"`, `"swarm"`, `"lane:2"`, …
    #[serde(default)]
    pub interrupted: Vec<String>,
}

// ---------------------------------------------------------------------------
// §5.6 catalog, §2 sessions and check
// ---------------------------------------------------------------------------

/// `GET /catalog` — the live registry. Never contains a key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub providers: Vec<ProviderInfo>,
    #[serde(default)]
    pub default_model: Option<ModelRef>,
    #[serde(default)]
    pub thinking_levels: Vec<String>,
    #[serde(default)]
    pub languages: Vec<LanguageInfo>,
    #[serde(default)]
    pub ops: Vec<OpSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub skills: Vec<NamedSpec>,
    #[serde(default)]
    pub tools: Vec<NamedSpec>,
    /// evo-swarm only: the models a lane can run, and why not otherwise.
    #[serde(default)]
    pub lanes: Option<LaneCatalog>,
    /// Entries that failed to encode, named rather than fatal.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub has_key: bool,
    /// The environment variable a key would come from, if any.
    #[serde(default)]
    pub key_env: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LanguageInfo {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    #[serde(default)]
    pub name: String,
    /// The op's argument JSON schema.
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub precondition: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub args_hint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NamedSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneCatalog {
    #[serde(default)]
    pub models: Vec<LaneModel>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneModel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `evo-agent sessions --json` (§2), newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionsBody {
    #[serde(default)]
    pub sessions: Vec<SessionInfoRow>,
}

/// One row of the session index.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionInfoRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub swarm_id: Option<String>,
    /// The first user text, ≤ 80 chars, one line.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub entries: Option<u64>,
}

/// `evo-swarm check --json` (§2) — the answer a launcher's chooser needs before
/// it spawns anything.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckBody {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub model: ModelCheck,
    #[serde(default)]
    pub lane_model: Option<ModelCheck>,
    #[serde(default)]
    pub problems: Vec<Problem>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCheck {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// Whatever else the model entry carried; not interpreted here.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

// ---------------------------------------------------------------------------
// The mirror
// ---------------------------------------------------------------------------

/// What [`TopicMirror::apply`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The mirror changed.
    Changed,
    /// Nothing to do: an op for another topic, a hello, an append to an item
    /// this mirror does not have, an op kind this client does not know.
    Ignored,
    /// Re-snapshot this topic (`topic.reset`).
    TopicReset(TopicResetReason),
    /// Re-snapshot everything and reconnect (`stream.reset`).
    StreamReset(StreamResetReason),
}

impl Applied {
    /// Did the mirror change?
    pub fn changed(self) -> bool {
        matches!(self, Applied::Changed)
    }
}

/// One topic's items and state, carried forward by the op stream.
///
/// Items are raw JSON objects in the order the server published them; the
/// newest N are kept ([`TopicMirror::set_capacity`]), because whatever falls off
/// the front is still on the server (`GET /items?before=`), and a client that
/// falls behind re-snapshots rather than replaying.
#[derive(Debug, Clone)]
pub struct TopicMirror {
    topic: String,
    state: Value,
    items: Vec<Value>,
    index: HashMap<String, usize>,
    capacity: usize,
    has_more: bool,
}

/// How many items a mirror keeps by default. A client shows these and pages
/// older ones from the server.
pub const DEFAULT_MIRROR_CAPACITY: usize = 500;

impl TopicMirror {
    /// An empty mirror for `topic`, with state `{}`.
    pub fn new(topic: impl Into<String>) -> TopicMirror {
        TopicMirror {
            topic: topic.into(),
            state: Value::Object(Map::new()),
            items: Vec::new(),
            index: HashMap::new(),
            capacity: DEFAULT_MIRROR_CAPACITY,
            has_more: false,
        }
    }

    /// A mirror seeded from a snapshot's topic entry.
    pub fn from_snapshot(topic: impl Into<String>, snapshot: &TopicSnapshot) -> TopicMirror {
        let mut mirror = TopicMirror::new(topic);
        mirror.has_more = snapshot.has_more;
        mirror.state = normalise_state(&snapshot.state);
        for item in &snapshot.items {
            mirror.push_back(item.clone());
        }
        mirror
    }

    /// The topic this mirror holds.
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// The topic state; always a JSON object.
    pub fn state(&self) -> &Value {
        &self.state
    }

    /// The state decoded as a typed topic state.
    pub fn state_as<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.state.clone())
    }

    /// The items, oldest first.
    pub fn items(&self) -> &[Value] {
        &self.items
    }

    /// The newest `n` items, oldest first.
    pub fn recent(&self, n: usize) -> &[Value] {
        let start = self.items.len().saturating_sub(n);
        &self.items[start..]
    }

    /// One item by id.
    pub fn item(&self, id: &str) -> Option<&Value> {
        self.index.get(id).map(|i| &self.items[*i])
    }

    /// An item decoded as a typed [`Item`].
    pub fn typed_item(&self, id: &str) -> Option<Item> {
        serde_json::from_value(self.item(id)?.clone()).ok()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether the server has items older than the ones held here.
    pub fn has_more(&self) -> bool {
        self.has_more
    }

    /// Replace the kept window with a fresh snapshot's worth of items.
    pub fn reseed(&mut self, snapshot: &TopicSnapshot) {
        self.items.clear();
        self.index.clear();
        self.has_more = snapshot.has_more;
        self.state = normalise_state(&snapshot.state);
        for item in &snapshot.items {
            self.push_back(item.clone());
        }
    }

    /// How many items to keep; the oldest beyond that are dropped.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.trim();
    }

    /// Apply one op, untyped. Returns [`Applied::Ignored`] if the bytes are not
    /// an op at all — a client must never die on a frame it cannot read.
    pub fn apply_value(&mut self, value: &Value) -> Applied {
        match serde_json::from_value::<Op>(value.clone()) {
            Ok(op) => self.apply(&op),
            Err(_) => Applied::Ignored,
        }
    }

    /// Apply one op to this mirror.
    pub fn apply(&mut self, op: &Op) -> Applied {
        // Every op but hello/stream.reset names its topic; an op for another
        // topic is not this mirror's business.
        if let Some(topic) = &op.topic {
            if topic != &self.topic {
                return Applied::Ignored;
            }
        } else if !matches!(op.kind, OpKind::StreamReset { .. } | OpKind::Hello { .. }) {
            return Applied::Ignored;
        }

        match &op.kind {
            OpKind::Hello { .. } => Applied::Ignored,
            OpKind::StreamReset { reason } => Applied::StreamReset(*reason),
            OpKind::TopicReset { reason } => Applied::TopicReset(*reason),
            OpKind::StatePatch { patch } => self.patch_state(patch),
            OpKind::ItemAdd { item, after } => self.add_item(item, after.as_deref()),
            OpKind::ItemAppend { id, field, text } => self.append_item(id, field, text),
            OpKind::ItemPatch { id, patch } => self.patch_item(id, patch),
            OpKind::ItemRemove { id } => self.remove_item(id),
            OpKind::Unknown => Applied::Ignored,
        }
    }

    fn patch_state(&mut self, patch: &Value) -> Applied {
        if let Value::Object(entries) = patch {
            if entries.is_empty() {
                return Applied::Ignored;
            }
        }
        merge_patch(&mut self.state, patch);
        self.state = normalise_state(&self.state);
        Applied::Changed
    }

    fn add_item(&mut self, item: &Value, after: Option<&str>) -> Applied {
        let Some(id) = item_id(item).map(str::to_string) else {
            return Applied::Ignored;
        };
        if let Some(pos) = self.index.get(&id).copied() {
            // A re-published item (a reconnect landing mid-stream): replace in
            // place, keeping the order the client already has.
            self.items[pos] = item.clone();
            return Applied::Changed;
        }
        let at = after
            .and_then(|after| self.index.get(after).map(|i| i + 1))
            .unwrap_or(self.items.len());
        self.insert(at, item.clone(), id);
        Applied::Changed
    }

    fn append_item(&mut self, id: &str, field: &str, text: &str) -> Applied {
        let Some(pos) = self.index.get(id).copied() else {
            return Applied::Ignored;
        };
        let Some(object) = self.items[pos].as_object_mut() else {
            return Applied::Ignored;
        };
        match object.get_mut(field) {
            Some(Value::String(existing)) => {
                existing.push_str(text);
                Applied::Changed
            }
            Some(slot) => {
                *slot = Value::String(text.to_string());
                Applied::Changed
            }
            None => {
                object.insert(field.to_string(), Value::String(text.to_string()));
                Applied::Changed
            }
        }
    }

    fn patch_item(&mut self, id: &str, patch: &Value) -> Applied {
        let Some(pos) = self.index.get(id).copied() else {
            return Applied::Ignored;
        };
        merge_patch(&mut self.items[pos], patch);
        Applied::Changed
    }

    fn remove_item(&mut self, id: &str) -> Applied {
        let Some(pos) = self.index.get(id).copied() else {
            return Applied::Ignored;
        };
        self.items.remove(pos);
        // Everything after the hole shifted by one.
        for (offset, item) in self.items[pos..].iter().enumerate() {
            if let Some(id) = item_id(item) {
                self.index.insert(id.to_string(), pos + offset);
            }
        }
        self.index.remove(id);
        Applied::Changed
    }

    fn push_back(&mut self, item: Value) {
        if let Some(id) = item_id(&item).map(str::to_string) {
            let at = self.items.len();
            self.insert(at, item, id);
        }
    }

    fn insert(&mut self, at: usize, item: Value, id: String) {
        self.items.insert(at, item);
        for (offset, item) in self.items[at..].iter().enumerate() {
            if let Some(id) = item_id(item) {
                self.index.insert(id.to_string(), at + offset);
            }
        }
        self.index.insert(id, at);
        self.trim();
    }

    fn trim(&mut self) {
        while self.items.len() > self.capacity {
            let dropped = self.items.remove(0);
            if let Some(id) = item_id(&dropped) {
                self.index.remove(id);
            }
            self.has_more = true;
            for (offset, item) in self.items.iter().enumerate() {
                if let Some(id) = item_id(item) {
                    self.index.insert(id.to_string(), offset);
                }
            }
        }
    }
}

impl Default for TopicMirror {
    fn default() -> Self {
        TopicMirror::new(TOPIC_SESSION)
    }
}

/// The `id` of an item object, if it has a string one.
pub fn item_id(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

/// State is an object on the wire; a `null`/missing one becomes `{}` so that
/// patches and readers need no special case.
fn normalise_state(state: &Value) -> Value {
    match state {
        Value::Object(_) => state.clone(),
        _ => Value::Object(Map::new()),
    }
}

/// RFC 7386 JSON merge patch: objects merge key by key, `null` deletes, and
/// everything else — arrays included — is replaced whole. That last rule is what
/// makes `state.patch` on a `todos`/`lanes` array mean "the whole new list".
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(entries) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let object = target.as_object_mut().expect("just made an object");
            for (key, value) in entries {
                if value.is_null() {
                    object.remove(key);
                } else {
                    merge_patch(object.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        other => *target = other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(value: Value) -> Op {
        serde_json::from_value(value).expect("an op")
    }

    fn mirror_with(items: Value) -> TopicMirror {
        let mut mirror = TopicMirror::new(TOPIC_SESSION);
        for item in items.as_array().unwrap() {
            assert_eq!(
                mirror.apply(&op(json!({"op": "item.add", "topic": TOPIC_SESSION, "seq": 1, "item": item, "after": Value::Null}))),
                Applied::Changed
            );
        }
        mirror
    }

    #[test]
    fn ready_file_decodes_every_field() {
        let ready: ReadyFile = serde_json::from_value(json!({
            "epoch": "7f3a91c2",
            "pid": 123,
            "supervisor_pid": 120,
            "port": 53701,
            "url": "http://127.0.0.1:53701/",
            "token": "ab".repeat(32),
            "session": {"id": "20260930T101010-abc", "path": "/abs/journal.sexp"},
            "program": "evo-swarm",
            "version": "1.2.3",
            "restarts": 0
        }))
        .unwrap();
        assert_eq!(ready.port, 53701);
        assert_eq!(ready.supervisor_pid, Some(120));
        assert_eq!(ready.program, "evo-swarm");
    }

    #[test]
    fn items_decode_by_kind_and_tolerate_unknown_ones() {
        let user: Item = serde_json::from_value(json!({
            "id": "e_1", "kind": "user", "ts": 17, "text": "hi",
            "images": [{"name": "a.png", "media_type": "image/png", "bytes": 3, "href": "/media/m1/0"}],
            "status": "queued", "queue": "after_run"
        }))
        .unwrap();
        match &user.kind {
            ItemKind::User {
                status,
                queue,
                images,
                ..
            } => {
                assert_eq!(*status, UserStatus::Queued);
                assert_eq!(*queue, QueueMode::AfterRun);
                assert_eq!(images[0].href, "/media/m1/0");
            }
            other => panic!("{other:?}"),
        }

        let tool: Item = serde_json::from_value(json!({
            "id": "t_c1", "kind": "tool", "ts": 18, "call_id": "c1", "name": "bash",
            "args": {"command": "ls"}, "status": "ok",
            "result": {"text": "a", "chars": 4000, "truncated": true}, "parent": "e_2"
        }))
        .unwrap();
        match &tool.kind {
            ItemKind::Tool { status, result, .. } => {
                assert_eq!(*status, ToolStatus::Ok);
                assert!(result.as_ref().unwrap().truncated);
            }
            other => panic!("{other:?}"),
        }

        let report: Item = serde_json::from_value(json!({
            "id": "e_3", "kind": "lane_report", "ts": 19, "lane": 3, "done": "d",
            "evidence": "e", "next": "n", "blocked": "b", "requests": "r", "goal": "active"
        }))
        .unwrap();
        match &report.kind {
            ItemKind::LaneReport {
                lane, next_step, ..
            } => {
                assert_eq!(*lane, Some(3));
                assert_eq!(next_step.as_deref(), Some("n"));
            }
            other => panic!("{other:?}"),
        }

        // A kind from a newer server: the item still decodes.
        let unknown: Item =
            serde_json::from_value(json!({"id": "e_9", "kind": "hologram", "ts": 20, "x": 1}))
                .unwrap();
        assert_eq!(unknown.kind, ItemKind::Other);
        assert_eq!(unknown.kind_name(), "");
        assert_eq!(unknown.id, "e_9");
    }

    #[test]
    fn items_round_trip_through_their_flattened_kind() {
        let item = Item {
            id: "e_1".into(),
            ts: 5,
            kind: ItemKind::Notice {
                severity: NoticeSeverity::Warn,
                text: "careful".into(),
                source: Some("swarm".into()),
                durable: true,
            },
        };
        let text = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&text).unwrap();
        assert_eq!(back, item);
        assert!(text.contains("\"kind\":\"notice\""));
        assert!(text.contains("\"severity\":\"warn\""));
    }

    #[test]
    fn topic_state_decodes_and_an_unknown_status_is_tolerated() {
        let state: SessionState = serde_json::from_value(json!({
            "status": "waiting",
            "task": {"id": "t1", "kind": "run", "turn": 3, "started_at": 1, "step_started_at": 2},
            "model": {"id": "m", "provider": "p", "ready": true, "reason": null},
            "thinking": "high", "language": "en",
            "context": {"tokens": 146000, "window": 1000000, "source": "usage"},
            "goal": null,
            "todos": [{"text": "a", "status": "in_progress"}],
            "queue": ["e_1"],
            "jobs": [{"id": "j", "name": "n", "status": "running", "started_at": 3}],
            "segments": [{"name": "model", "order": 100, "side": "left", "text": "x", "data": {}}],
            "session": {"id": "s", "path": "/p", "leaf": "e_2", "program": "evo-swarm", "started_at": 4}
        }))
        .unwrap();
        assert_eq!(state.status, AgentStatus::Waiting);
        assert_eq!(state.todos[0].status, TodoStatus::InProgress);
        assert_eq!(state.context.unwrap().tokens, 146000);
        assert_eq!(state.segments[0].side, SegmentSide::Left);

        let future: SessionState = serde_json::from_value(json!({"status": "teaching"})).unwrap();
        assert_eq!(future.status, AgentStatus::Other);
    }

    #[test]
    fn swarm_state_decodes_lanes() {
        let swarm: SwarmState = serde_json::from_value(json!({
            "id": "20260929T101010-90a1", "workers": 6,
            "status": {"busy": 2, "waiting_on_lanes": true},
            "config": {"lane_model": {"id": "m", "provider": "p"}, "lane_thinking": "medium"},
            "lanes": [{"n": 1, "state": "working", "task": "do it", "task_started_at": 1,
                       "step_started_at": 2, "restarts": 0, "pid": 123, "worktree": "/abs",
                       "branch": "b", "model": {"id": "m", "provider": "p"}, "context": {},
                       "goal": null, "todos": [], "reports": 3,
                       "last_item": {"kind": "tool", "summary": "bash"}}]
        }))
        .unwrap();
        assert_eq!(swarm.workers, Some(6));
        assert!(swarm.status.waiting_on_lanes);
        assert_eq!(swarm.lanes[0].state, LaneState::Working);
        assert_eq!(swarm.lanes[0].reports, 3);
        assert_eq!(swarm.lanes[0].last_item.as_ref().unwrap().kind, "tool");
        assert_eq!(swarm.config.lane_thinking.as_deref(), Some("medium"));
    }

    #[test]
    fn snapshot_decodes_topics_and_a_missing_items_list() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "epoch": "7f3a", "seq": 42,
            "topics": {
                "session": {"state": {"status": "idle"}, "items": [{"id": "e_1", "kind": "user", "ts": 1}], "has_more": false},
                "swarm": {"state": {"id": "s1", "lanes": []}}
            }
        }))
        .unwrap();
        assert_eq!(snapshot.seq, 42);
        assert_eq!(snapshot.topics["session"].items.len(), 1);
        assert_eq!(snapshot.topics["swarm"].items.len(), 0);
        assert!(snapshot.topics["swarm"].state.is_object());
    }

    #[test]
    fn ops_decode_and_an_unknown_op_is_not_fatal() {
        let add = op(
            json!({"seq": 3, "ts": 10, "topic": "session", "op": "item.add",
                            "item": {"id": "e_1", "kind": "user", "ts": 1}, "after": null}),
        );
        assert_eq!(add.seq, 3);
        assert_eq!(add.topic.as_deref(), Some("session"));
        assert!(matches!(add.kind, OpKind::ItemAdd { after: None, .. }));

        let unknown = op(json!({"seq": 4, "op": "item.teleport", "id": "e_1"}));
        assert_eq!(unknown.kind, OpKind::Unknown);
        assert_eq!(unknown.kind.name(), "");

        // Ops round-trip, which is what lets the test harness serve them.
        let text = serde_json::to_string(&add).unwrap();
        assert_eq!(serde_json::from_str::<Op>(&text).unwrap(), add);

        let reset = op(json!({"op": "stream.reset", "reason": "cursor_too_old"}));
        assert!(matches!(
            reset.kind,
            OpKind::StreamReset {
                reason: StreamResetReason::CursorTooOld
            }
        ));
    }

    #[test]
    fn cursor_round_trips_and_rejects_junk() {
        let cursor = Cursor::new("7f3a91c2", 1042);
        assert_eq!(cursor.encode(), "7f3a91c2.1042");
        assert_eq!(Cursor::decode("7f3a91c2.1042"), Some(cursor));
        assert_eq!(Cursor::decode("7f3a91c2"), None);
        assert_eq!(Cursor::decode(".7"), None);
        assert_eq!(Cursor::decode("7f3a.x"), None);
    }

    #[test]
    fn merge_patch_replaces_arrays_and_deletes_with_null() {
        let mut target = json!({"a": {"b": 1, "c": 2}, "list": [1, 2, 3], "gone": 5});
        merge_patch(
            &mut target,
            &json!({"a": {"b": null, "d": 3}, "list": [9], "gone": null}),
        );
        assert_eq!(target, json!({"a": {"c": 2, "d": 3}, "list": [9]}));

        // A non-object patch replaces the whole target.
        let mut target = json!({"a": 1});
        merge_patch(&mut target, &json!([1, 2]));
        assert_eq!(target, json!([1, 2]));
    }

    #[test]
    fn mirror_seeds_from_a_snapshot_and_patches_its_state() {
        let snapshot = TopicSnapshot {
            state: json!({"status": "idle", "queue": ["e_1"]}),
            items: vec![json!({"id": "e_1", "kind": "user", "ts": 1})],
            has_more: true,
        };
        let mut mirror = TopicMirror::from_snapshot(TOPIC_SESSION, &snapshot);
        assert_eq!(mirror.len(), 1);
        assert!(mirror.has_more());
        let state: SessionState = mirror.state_as().unwrap();
        assert_eq!(state.status, AgentStatus::Idle);

        let applied = mirror.apply(&op(
            json!({"op": "state.patch", "topic": "session", "seq": 5,
            "patch": {"status": "running", "queue": [], "segments": [{"name": "x", "text": "1"}]}}),
        ));
        assert_eq!(applied, Applied::Changed);
        let state: SessionState = mirror.state_as().unwrap();
        assert_eq!(state.status, AgentStatus::Running);
        assert!(state.queue.is_empty());
        assert_eq!(state.segments.len(), 1);
    }

    #[test]
    fn mirror_orders_items_by_their_after_pointer() {
        let mut mirror = TopicMirror::new(TOPIC_SESSION);
        for (item, after) in [
            (json!({"id": "e_1", "kind": "user", "ts": 1}), Value::Null),
            (json!({"id": "e_3", "kind": "user", "ts": 3}), Value::Null),
        ] {
            assert_eq!(
                mirror.apply(&op(json!({"op": "item.add", "topic": "session", "seq": 1,
                    "item": item, "after": after}))),
                Applied::Changed
            );
        }
        // Between e_1 and e_3.
        assert_eq!(
            mirror.apply(&op(json!({"op": "item.add", "topic": "session", "seq": 2,
                "item": {"id": "e_2", "kind": "user", "ts": 2}, "after": "e_1"}))),
            Applied::Changed
        );
        let ids: Vec<&str> = mirror.items().iter().map(|i| item_id(i).unwrap()).collect();
        assert_eq!(ids, ["e_1", "e_2", "e_3"]);
        assert_eq!(mirror.item("e_2").unwrap()["ts"], 2);
        assert_eq!(mirror.recent(2).len(), 2);
    }

    #[test]
    fn mirror_appends_text_and_patches_and_removes_items() {
        let mut mirror = mirror_with(json!([
            {"id": "e_1", "kind": "assistant", "ts": 1, "text": "He", "thinking": ""}
        ]));
        assert_eq!(
            mirror.apply(&op(
                json!({"op": "item.append", "topic": "session", "seq": 2,
                "id": "e_1", "field": "text", "text": "llo"})
            )),
            Applied::Changed
        );
        assert_eq!(
            mirror.apply(&op(
                json!({"op": "item.append", "topic": "session", "seq": 3,
                "id": "e_1", "field": "thinking", "text": "hm"})
            )),
            Applied::Changed
        );
        assert_eq!(mirror.item("e_1").unwrap()["text"], "Hello");
        assert_eq!(mirror.item("e_1").unwrap()["thinking"], "hm");

        assert_eq!(
            mirror.apply(&op(
                json!({"op": "item.patch", "topic": "session", "seq": 4,
                "id": "e_1", "patch": {"status": "final", "usage": {"input": 3}}})
            )),
            Applied::Changed
        );
        assert_eq!(mirror.item("e_1").unwrap()["status"], "final");
        assert_eq!(mirror.item("e_1").unwrap()["usage"]["input"], 3);

        assert_eq!(
            mirror.apply(&op(
                json!({"op": "item.remove", "topic": "session", "seq": 5, "id": "e_1"})
            )),
            Applied::Changed
        );
        assert!(mirror.is_empty());
        // Removing it twice does nothing.
        assert_eq!(
            mirror.apply(&op(
                json!({"op": "item.remove", "topic": "session", "seq": 6, "id": "e_1"})
            )),
            Applied::Ignored
        );
    }

    #[test]
    fn mirror_keeps_its_index_honest_across_inserts_and_removals() {
        let mut mirror = mirror_with(json!([
            {"id": "a", "kind": "user", "ts": 1},
            {"id": "b", "kind": "user", "ts": 2},
            {"id": "c", "kind": "user", "ts": 3}
        ]));
        mirror.apply(&op(
            json!({"op": "item.remove", "topic": "session", "seq": 2, "id": "a"}),
        ));
        // Inserting after the front-most remaining item must land in the middle.
        mirror.apply(&op(json!({"op": "item.add", "topic": "session", "seq": 3,
            "item": {"id": "d", "kind": "user", "ts": 4}, "after": "b"})));
        let ids: Vec<&str> = mirror.items().iter().map(|i| item_id(i).unwrap()).collect();
        assert_eq!(ids, ["b", "d", "c"]);
        // Every id resolves to its own slot.
        for (pos, id) in ids.iter().enumerate() {
            assert_eq!(mirror.index[*id], pos);
        }
    }

    #[test]
    fn mirror_ignores_other_topics_resets_and_junk() {
        let mut mirror = mirror_with(json!([{"id": "e_1", "kind": "user", "ts": 1}]));
        assert_eq!(
            mirror.apply(&op(json!({"op": "item.add", "topic": "lane:2", "seq": 1,
                "item": {"id": "e_2", "kind": "user", "ts": 2}, "after": null}))),
            Applied::Ignored
        );
        assert_eq!(mirror.len(), 1);
        let hello = op(json!({"op": "hello", "epoch": "7f3a", "seq": 9}));
        assert_eq!(hello.seq, 9);
        assert_eq!(
            hello.kind,
            OpKind::Hello {
                epoch: "7f3a".into()
            }
        );
        assert_eq!(mirror.apply(&hello), Applied::Ignored);
        assert_eq!(
            mirror.apply(&op(
                json!({"op": "topic.reset", "topic": "session", "reason": "session_switched"})
            )),
            Applied::TopicReset(TopicResetReason::SessionSwitched)
        );
        assert_eq!(
            mirror.apply(&op(json!({"op": "stream.reset", "reason": "restarted"}))),
            Applied::StreamReset(StreamResetReason::Restarted)
        );
        assert_eq!(
            mirror.apply_value(&json!({"op": "item.add", "topic": "session"})),
            Applied::Ignored
        );
        assert_eq!(mirror.len(), 1);
    }

    #[test]
    fn mirror_reseeds_after_a_topic_reset() {
        let mut mirror = mirror_with(json!([{"id": "e_1", "kind": "user", "ts": 1}]));
        mirror.reseed(&TopicSnapshot {
            state: json!({"status": "idle"}),
            items: vec![json!({"id": "e_9", "kind": "user", "ts": 9})],
            has_more: false,
        });
        assert_eq!(mirror.len(), 1);
        assert!(mirror.item("e_1").is_none());
        assert!(mirror.item("e_9").is_some());
    }

    #[test]
    fn a_full_mirror_drops_the_oldest_and_caps_at_capacity() {
        let mut mirror = TopicMirror::new(TOPIC_SESSION);
        mirror.set_capacity(3);
        for n in 1..=5 {
            mirror.apply(&op(json!({"op": "item.add", "topic": "session", "seq": n,
                "item": {"id": format!("e_{n}"), "kind": "user", "ts": n}, "after": Value::Null})));
        }
        let ids: Vec<&str> = mirror.items().iter().map(|i| item_id(i).unwrap()).collect();
        assert_eq!(ids, ["e_3", "e_4", "e_5"]);
        assert!(mirror.has_more());
        assert!(mirror.item("e_1").is_none());
        assert_eq!(mirror.typed_item("e_5").unwrap().kind.name(), "user");
    }

    fn request_value() -> Value {
        json!({"rid": "rid-1", "op": "input.send", "args": {"text": "hi", "queue": "now"}})
    }

    #[test]
    fn op_request_and_reply_round_trip() {
        let request = OpRequest::new("rid-1", "input.send", json!({"text": "hi", "queue": "now"}));
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.starts_with(r#"{"rid":"rid-1","op":"input.send","args":{"#));
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            request_value()
        );
        // An op with no args still sends `{}`.
        assert_eq!(
            serde_json::to_string(&OpRequest::new("r", "goal.pause", Value::Null)).unwrap(),
            r#"{"rid":"r","op":"goal.pause","args":{}}"#
        );

        let ok: OpReply = serde_json::from_value(json!({
            "rid": "rid-1", "ok": true, "seq": 12,
            "result": {"item_id": "e_1", "queued": true, "blocked": null}
        }))
        .unwrap();
        assert!(ok.ok);
        assert_eq!(ok.seq, 12);
        let result: InputSendResult = serde_json::from_value(ok.result.clone()).unwrap();
        assert_eq!(result.item_id, "e_1");
        assert!(result.queued && result.blocked.is_none());

        let refused: OpReply = serde_json::from_value(json!({
            "rid": "rid-2", "ok": false,
            "error": {"code": "busy", "message": "a run is in flight", "detail": {}}
        }))
        .unwrap();
        assert_eq!(refused.code(), Some(ErrorCode::Busy));
        assert_eq!(refused.error().unwrap().message, "a run is in flight");

        // A code from a newer server is a refusal, not a parse error.
        let future: OpReply = serde_json::from_value(json!({
            "rid": "rid-3", "ok": false, "error": {"code": "too_slow", "message": "no"}
        }))
        .unwrap();
        assert_eq!(future.code(), Some(ErrorCode::Unknown));
        assert_eq!(ErrorCode::Unknown.as_str(), "unknown");
        assert_eq!(
            serde_json::to_value(ErrorCode::NotQuiescent).unwrap(),
            json!("not_quiescent")
        );
    }

    #[test]
    fn catalog_decodes_with_and_without_lanes() {
        let catalog: Catalog = serde_json::from_value(json!({
            "models": [{"id": "m", "provider": "p", "name": "M", "api": "anthropic",
                        "context_window": 1000000, "reasoning": true, "images": true,
                        "ready": true, "reason": null}],
            "providers": [{"name": "p", "api": "anthropic", "has_key": true, "key_env": "ANTHROPIC_API_KEY"}],
            "default_model": {"id": "m", "provider": "p"},
            "thinking_levels": ["off", "low", "medium", "high", "xhigh"],
            "languages": [{"code": "en", "name": "English"}],
            "ops": [{"name": "input.send", "args": {"type": "object"}, "precondition": "none"}],
            "commands": [{"name": "lore", "description": "d", "args_hint": "<text>"}],
            "skills": [{"name": "s", "description": "d"}],
            "tools": [{"name": "bash", "description": "d"}],
            "lanes": {"models": [{"id": "m", "provider": "p", "ok": false, "reason": "no key"}]},
            "warnings": ["dropped one model"]
        }))
        .unwrap();
        assert_eq!(catalog.models[0].context_window, Some(1000000));
        assert!(catalog.providers[0].has_key);
        assert_eq!(catalog.ops[0].precondition.as_deref(), Some("none"));
        assert!(!catalog.lanes.as_ref().unwrap().models[0].ok);
        assert_eq!(catalog.warnings.len(), 1);

        let bare: Catalog = serde_json::from_value(json!({})).unwrap();
        assert!(bare.models.is_empty() && bare.lanes.is_none());
    }

    #[test]
    fn sessions_and_check_bodies_decode() {
        let sessions: SessionsBody = serde_json::from_value(json!({
            "sessions": [{"id": "s1", "path": "/p/1.sexp", "cwd": "/w", "program": "evo-swarm",
                          "swarm_id": "20260929T-a", "title": "the first thing I said",
                          "created_at": 1, "updated_at": 2, "entries": 12}]
        }))
        .unwrap();
        assert_eq!(
            sessions.sessions[0].title.as_deref(),
            Some("the first thing I said")
        );
        assert_eq!(sessions.sessions[0].entries, Some(12));

        let check: CheckBody = serde_json::from_value(json!({
            "ok": false,
            "model": {"id": "m", "provider": "p", "name": "M", "ok": true, "reason": null},
            "lane_model": {"id": "l", "provider": "p", "ok": false, "reason": "no key"},
            "problems": [{"code": "no_key", "message": "the lane model has no key"}]
        }))
        .unwrap();
        assert!(!check.ok);
        assert!(check.model.ok);
        assert_eq!(check.model.rest["name"], "M");
        assert_eq!(check.lane_model.unwrap().reason.as_deref(), Some("no key"));
        assert_eq!(check.problems[0].code, "no_key");
    }

    #[test]
    fn lane_topics_round_trip() {
        assert_eq!(lane_topic(3), "lane:3");
        assert_eq!(lane_number("lane:3"), Some(3));
        assert_eq!(lane_number("lane:x"), None);
        assert_eq!(lane_number("session"), None);
    }
}
