//! The serve protocol's wire types, exactly as CONTRACT.md describes them.
//!
//! This module is deliberately **transport-level only**: it knows the shape of a
//! frame and the name of an op, and nothing about what an item or a topic state
//! *means*. The `session` crate owns all of that — items, states, mirrors, the
//! merge-patch rules, paging — and it does so on raw `serde_json::Value`, so a
//! server that grows a field or an item kind never needs a change here.
//!
//! What lives here:
//!
//! * §1 — the **ready file** a serving child writes once it is listening
//!   ([`ReadyFile`]). Its `epoch` identifies one process lifetime: a cursor from
//!   another epoch can never be resumed, which is how a restart is detected
//!   without anyone watching a pid.
//! * §5.3 — a **stream frame** ([`StreamFrame`]): the cursor `<epoch>.<seq>`, the
//!   op's name, and its `data` object, still untyped.
//! * §5.2 — the **snapshot envelope** ([`Snapshot`]); `topics` is handed on as
//!   the JSON it is.
//! * §5.5 — the `POST /ops` **request/reply envelope** and its error codes.
//! * §5.6, §2 — the **catalog** and the `sessions`/`check` CLI bodies, which the
//!   store reads as they arrive.

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

/// The `op` names of the stream (§5.3). The two resets are the only ones a
/// transport acts on; everything else is `session`'s to interpret.
pub mod op_name {
    /// Always the first frame: where this stream starts.
    pub const HELLO: &str = "hello";
    pub const ITEM_ADD: &str = "item.add";
    pub const ITEM_APPEND: &str = "item.append";
    pub const ITEM_PATCH: &str = "item.patch";
    pub const ITEM_REMOVE: &str = "item.remove";
    pub const STATE_PATCH: &str = "state.patch";
    /// Re-snapshot the one topic this op names.
    pub const TOPIC_RESET: &str = "topic.reset";
    /// Re-snapshot everything and reconnect with the new cursor.
    pub const STREAM_RESET: &str = "stream.reset";
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

/// The session a server is serving, as the ready file records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    pub path: String,
}

// ---------------------------------------------------------------------------
// §5.2 snapshot, §5.3 stream
// ---------------------------------------------------------------------------

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

    /// Does this cursor belong to another process lifetime?
    pub fn other_epoch(&self, other: &Cursor) -> bool {
        self.epoch != other.epoch
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.encode())
    }
}

/// One frame of the op stream: the cursor it carries, the op's name, and its
/// `data` object, undecoded.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamFrame {
    /// `<epoch>.<seq>` from the SSE `id:` line, when it is one.
    pub cursor: Option<Cursor>,
    /// `hello`, `item.add`, `stream.reset`, … — the `op` field of the frame's
    /// data (the SSE `event:` is always `op`).
    pub op: String,
    /// The frame's data object, undecoded; `null` when the body was not JSON.
    pub data: Value,
}

impl StreamFrame {
    /// Build a frame from an SSE event's parts: the `event:` name, the `id:`
    /// line and the `data:` body. A body that is not JSON still yields a frame —
    /// with `data: null` — because a frame this client cannot read is not a
    /// reason to drop the stream.
    pub fn parse(event: Option<&str>, id: Option<&str>, data: &str) -> StreamFrame {
        let cursor = id.and_then(Cursor::decode);
        let data = serde_json::from_str::<Value>(data).unwrap_or(Value::Null);
        let op = data
            .get("op")
            .and_then(Value::as_str)
            .or(event)
            .unwrap_or_default()
            .to_string();
        StreamFrame { cursor, op, data }
    }

    /// One field of the frame's data.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// The topic this op names, `None` for hello and a stream reset.
    pub fn topic(&self) -> Option<&str> {
        self.get("topic").and_then(Value::as_str)
    }

    pub fn is_hello(&self) -> bool {
        self.op == op_name::HELLO
    }

    /// This frame's reason, if it is a `stream.reset`.
    pub fn stream_reset(&self) -> Option<StreamResetReason> {
        (self.op == op_name::STREAM_RESET).then(|| StreamResetReason::parse(self.reason()))
    }

    /// This frame's reason, if it is a `topic.reset`.
    pub fn topic_reset(&self) -> Option<TopicResetReason> {
        (self.op == op_name::TOPIC_RESET).then(|| TopicResetReason::parse(self.reason()))
    }

    fn reason(&self) -> &str {
        self.get("reason").and_then(Value::as_str).unwrap_or("")
    }
}

/// Why the stream itself must be re-read from a fresh cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamResetReason {
    /// The server restarted; the epoch changed.
    Restarted,
    /// The cursor is older than the op log's retention.
    CursorTooOld,
    /// The cursor was never issued by this process.
    CursorUnknown,
    /// A reason from a newer server. The client re-snapshots all the same.
    Unknown,
}

impl StreamResetReason {
    pub fn parse(text: &str) -> StreamResetReason {
        match text {
            "restarted" => StreamResetReason::Restarted,
            "cursor_too_old" => StreamResetReason::CursorTooOld,
            "cursor_unknown" => StreamResetReason::CursorUnknown,
            _ => StreamResetReason::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StreamResetReason::Restarted => "restarted",
            StreamResetReason::CursorTooOld => "cursor_too_old",
            StreamResetReason::CursorUnknown => "cursor_unknown",
            StreamResetReason::Unknown => "unknown",
        }
    }
}

/// Why one topic must be re-snapshotted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicResetReason {
    SessionSwitched,
    LeafMoved,
    LaneRestarted,
    SwarmSwitched,
    /// A reason from a newer server. The client re-snapshots that topic anyway.
    Unknown,
}

impl TopicResetReason {
    pub fn parse(text: &str) -> TopicResetReason {
        match text {
            "session_switched" => TopicResetReason::SessionSwitched,
            "leaf_moved" => TopicResetReason::LeafMoved,
            "lane_restarted" => TopicResetReason::LaneRestarted,
            "swarm_switched" => TopicResetReason::SwarmSwitched,
            _ => TopicResetReason::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TopicResetReason::SessionSwitched => "session_switched",
            TopicResetReason::LeafMoved => "leaf_moved",
            TopicResetReason::LaneRestarted => "lane_restarted",
            TopicResetReason::SwarmSwitched => "swarm_switched",
            TopicResetReason::Unknown => "unknown",
        }
    }
}

/// `GET /snapshot?topics=…&items=N` — atomic across the topics requested: every
/// op with `seq` ≤ this `seq` is reflected, none after.
///
/// `topics` stays JSON: an entry is `{"state":{…},"items":[…],"has_more":bool}`
/// and it is the `session` crate that reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub epoch: String,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub topics: Value,
}

impl Snapshot {
    /// The cursor this snapshot starts from.
    pub fn cursor(&self) -> Cursor {
        Cursor::new(self.epoch.clone(), self.seq)
    }

    /// One topic's entry, if the snapshot carried it.
    pub fn topic(&self, name: &str) -> Option<&Value> {
        self.topics.get(name)
    }

    /// The topic names the snapshot carried.
    pub fn topic_names(&self) -> Vec<&str> {
        self.topics
            .as_object()
            .map(|topics| topics.keys().map(String::as_str).collect())
            .unwrap_or_default()
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
    /// A request whose `args` are always an object, `{}` when there are none.
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
    /// The refusal, whether the server set `ok:false` or sent only an `error`.
    pub fn error(&self) -> Option<&OpError> {
        self.error.as_ref()
    }

    /// The refusal's code, if it refused.
    pub fn code(&self) -> Option<ErrorCode> {
        self.error.as_ref().map(|e| e.code)
    }
}

/// Why an op was refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub detail: Value,
}

/// The refusal codes of §5.5. A code from a newer server decodes as
/// [`ErrorCode::Unknown`], so an unexpected refusal still reads as a refusal.
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
    // 401 is the transport's, but a client that meets one should say so.
    Unauthorized,
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
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Unknown => "unknown",
        }
    }

    /// A refusal the user can act on by waiting and trying again.
    pub fn transient(self) -> bool {
        matches!(
            self,
            ErrorCode::Busy | ErrorCode::NotQuiescent | ErrorCode::ShuttingDown
        )
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
            "unauthorized" => ErrorCode::Unauthorized,
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

/// One model of the catalog.
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
    /// The levels this model takes, in ladder order — `["low","high","max"]` for a
    /// provider whose ladder is not a run of levels, `[]` for a model with no effort
    /// parameter at all (§5.6, evo-agent 3ad8d0a). Empty also reads a *server* that
    /// predates the field: a client shows no effort for such a model rather than
    /// inventing a rung, which is what an empty list means anyway.
    #[serde(default)]
    pub effort_levels: Vec<String>,
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `{"id","provider"}` — how a model is named outside the catalog's own entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
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

/// One op of the catalog, with the argument schema a form could be built from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub precondition: Option<String>,
}

/// One slash command, as `/catalog` lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub args_hint: Option<String>,
}

/// A skill or a tool: a name and a description.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NamedSpec {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// The lanes' half of the catalog: evo-swarm only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneCatalog {
    #[serde(default)]
    pub models: Vec<LaneModel>,
}

/// A model a lane can run — or cannot, with the reason.
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
    /// The levels this model takes, as its `models[]` entry carries them: a lane's
    /// model is the same registration, so its ladder is the same ladder.
    #[serde(default)]
    pub effort_levels: Vec<String>,
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

/// `evo-swarm check --json` (§2) — the answer a launcher needs before it spawns
/// anything.
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

/// One model as `check` reports it: the model's own entry plus its verdict.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(ready.epoch, "7f3a91c2");
        assert_eq!(ready.session.path, "/abs/journal.sexp");
        assert_eq!(ready.program, "evo-swarm");
    }

    #[test]
    fn cursor_round_trips_and_rejects_junk() {
        let cursor = Cursor::new("7f3a91c2", 1042);
        assert_eq!(cursor.encode(), "7f3a91c2.1042");
        assert_eq!(cursor.to_string(), "7f3a91c2.1042");
        assert_eq!(Cursor::decode("7f3a91c2.1042"), Some(cursor));
        assert_eq!(Cursor::decode("7f3a91c2"), None);
        assert_eq!(Cursor::decode(".7"), None);
        assert_eq!(Cursor::decode("7f3a.x"), None);
        // An epoch with dots in it still splits at the last dot.
        assert_eq!(Cursor::decode("a.b.3"), Some(Cursor::new("a.b", 3)),);
    }

    #[test]
    fn a_frame_carries_its_cursor_op_and_data() {
        let frame = StreamFrame::parse(
            Some("op"),
            Some("7f3a.1042"),
            r#"{"seq":1042,"topic":"session","op":"item.append","id":"e_1","field":"text","text":"He"}"#,
        );
        assert_eq!(frame.cursor, Some(Cursor::new("7f3a", 1042)));
        assert_eq!(frame.op, "item.append");
        assert_eq!(frame.get("id"), Some(&json!("e_1")));
        assert_eq!(frame.topic(), Some("session"));
        assert_eq!(frame.stream_reset(), None);
        assert_eq!(frame.topic_reset(), None);
    }

    #[test]
    fn reset_frames_and_a_hello_are_recognised_by_name() {
        let hello =
            StreamFrame::parse(Some("op"), None, r#"{"op":"hello","epoch":"7f3a","seq":9}"#);
        assert!(hello.is_hello());
        assert_eq!(hello.cursor, None);
        assert_eq!(hello.get("epoch"), Some(&json!("7f3a")));

        // The id line is where the cursor comes from, and a reset names its reason.
        let reset = StreamFrame::parse(
            Some("op"),
            Some("7f3a.1050"),
            r#"{"op":"stream.reset","reason":"cursor_too_old"}"#,
        );
        assert_eq!(reset.stream_reset(), Some(StreamResetReason::CursorTooOld));
        assert_eq!(reset.topic_reset(), None);

        let topic = StreamFrame::parse(
            Some("op"),
            Some("7f3a.1051"),
            r#"{"op":"topic.reset","topic":"lane:2","reason":"lane_restarted"}"#,
        );
        assert_eq!(topic.topic_reset(), Some(TopicResetReason::LaneRestarted));
        assert_eq!(topic.topic(), Some("lane:2"));

        // A reason from a newer server is still a reset.
        let future = StreamFrame::parse(
            Some("op"),
            None,
            r#"{"op":"stream.reset","reason":"time_travel"}"#,
        );
        assert_eq!(future.stream_reset(), Some(StreamResetReason::Unknown));
        assert_eq!(StreamResetReason::Unknown.as_str(), "unknown");
    }

    #[test]
    fn a_frame_the_transport_cannot_read_still_arrives() {
        let frame = StreamFrame::parse(Some("op"), Some("not a cursor"), "not json");
        assert_eq!(frame.cursor, None);
        assert_eq!(frame.data, Value::Null);
        assert_eq!(frame.op, "op");
        assert_eq!(frame.topic(), None);
        // Neither reset is claimed by a frame with no reason.
        let frame = StreamFrame::parse(Some("op"), None, r#"{"op":"stream.reset"}"#);
        assert_eq!(frame.stream_reset(), Some(StreamResetReason::Unknown));
    }

    #[test]
    fn snapshot_keeps_topics_as_the_json_they_are() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "epoch": "7f3a", "seq": 42,
            "topics": {
                "session": {"state": {"status": "idle"}, "items": [{"id": "e_1", "kind": "user", "ts": 1}], "has_more": false},
                "swarm": {"state": {"id": "s1", "lanes": []}}
            }
        }))
        .unwrap();
        assert_eq!(snapshot.cursor(), Cursor::new("7f3a", 42));
        assert_eq!(
            snapshot.topic("session").unwrap()["items"][0]["kind"],
            "user"
        );
        assert_eq!(snapshot.topic("lane:3"), None);
        let mut names = snapshot.topic_names();
        names.sort();
        assert_eq!(names, ["session", "swarm"]);

        // A snapshot with no topics at all is still a snapshot.
        let bare: Snapshot = serde_json::from_value(json!({"epoch": "e", "seq": 0})).unwrap();
        assert!(bare.topic_names().is_empty());
    }

    #[test]
    fn op_request_and_reply_round_trip() {
        let request = OpRequest::new("rid-1", "input.send", json!({"text": "hi", "queue": "now"}));
        assert_eq!(
            serde_json::from_str::<Value>(&serde_json::to_string(&request).unwrap()).unwrap(),
            json!({"rid": "rid-1", "op": "input.send", "args": {"text": "hi", "queue": "now"}})
        );
        // An op with no args still sends `{}`, never `null`.
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
        assert_eq!(ok.result["item_id"], "e_1");
        assert!(ok.error().is_none());

        let refused: OpReply = serde_json::from_value(json!({
            "rid": "rid-2", "ok": false,
            "error": {"code": "busy", "message": "a run is in flight", "detail": {}}
        }))
        .unwrap();
        assert_eq!(refused.code(), Some(ErrorCode::Busy));
        assert!(refused.code().unwrap().transient());
        assert_eq!(refused.error().unwrap().message, "a run is in flight");
        assert_eq!(refused.result, Value::Null);

        // A code from a newer server is a refusal, not a parse error.
        let future: OpReply = serde_json::from_value(json!({
            "rid": "rid-3", "ok": false, "error": {"code": "too_slow", "message": "no"}
        }))
        .unwrap();
        assert_eq!(future.code(), Some(ErrorCode::Unknown));
        assert!(!future.code().unwrap().transient());
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
        // A server that predates `effort_levels` names none, and one that carries it
        // is read in its own order.
        assert!(catalog.models[0].effort_levels.is_empty());
        assert!(catalog.lanes.as_ref().unwrap().models[0]
            .effort_levels
            .is_empty());
        assert!(catalog.providers[0].has_key);
        assert_eq!(catalog.default_model.as_ref().unwrap().id, "m");
        assert_eq!(catalog.thinking_levels.len(), 5);
        assert_eq!(catalog.ops[0].precondition.as_deref(), Some("none"));
        assert_eq!(catalog.commands[0].args_hint.as_deref(), Some("<text>"));
        assert!(!catalog.lanes.as_ref().unwrap().models[0].ok);
        assert_eq!(catalog.warnings.len(), 1);

        let levelled: Catalog = serde_json::from_value(json!({
            "models": [{"id": "m", "provider": "p", "effort_levels": ["low", "high", "max"]},
                       {"id": "n", "provider": "p", "effort_levels": []}],
            "lanes": {"models": [{"id": "m", "provider": "p", "ok": true,
                                  "effort_levels": ["low", "high", "max"]}]}
        }))
        .unwrap();
        assert_eq!(
            levelled.models[0].effort_levels,
            vec!["low".to_string(), "high".to_string(), "max".to_string()]
        );
        assert!(levelled.models[1].effort_levels.is_empty());
        assert_eq!(levelled.lanes.unwrap().models[0].effort_levels.len(), 3);

        // An agent's catalog has no lane half; an empty catalog is still a catalog.
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
        assert_eq!(sessions.sessions[0].program.as_deref(), Some("evo-swarm"));

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
