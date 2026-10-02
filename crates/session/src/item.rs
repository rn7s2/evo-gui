//! Items — one thing a transcript draws (CONTRACT §4.1).
//!
//! An item is the wire's own object: `{id, kind, ts, …}`, its `id` the stable string the
//! server minted (a journal entry id, or `"t_"+call_id` for a tool call). Ids survive a
//! refetch, a reconnect and a process restart, which is what lets the UI keep a row view
//! keyed by id: an item that changes is *patched*, never replaced by a new row.
//!
//! Every variant keeps the wire object it was parsed from ([`Item::raw`]), because
//! `item.append` and `item.patch` are JSON operations on that object (concatenate a field,
//! merge-patch the rest) and re-parsing one item is far cheaper than modelling every field
//! twice. `Item::from_json` is the only parser; nothing here guesses at prose.

use serde_json::Value;

use crate::format::merge_patch;

/// A stable item id, as the server minted it (`"e_9c1f"`, `"t_call_3"`).
pub type ItemId = String;

/// One item: the wire object, and the typed view of it the UI renders.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: ItemId,
    /// Epoch milliseconds the item was created.
    pub ts: u64,
    pub kind: ItemKind,
    /// The wire object, kept so a patch can be applied to it as JSON.
    raw: Value,
}

/// What an item is, and everything a row needs to draw it.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    User(UserItem),
    Assistant(AssistantItem),
    Tool(ToolItem),
    LaneReport(LaneReport),
    LaneEvent(LaneEvent),
    Goal(GoalItem),
    Context(ContextItem),
    CommandNote(CommandNote),
    HumanAction(HumanAction),
    Notice(Notice),
    RunOutcome(RunOutcome),
    Compaction(Compaction),
    Recovery(Recovery),
    ProviderRetry(ProviderRetry),
    /// A kind a later evo version added, or one whose payload did not parse: kept and
    /// drawn as a quiet line, never dropped. An unknown item is data from outside this
    /// build, so it is shown, not acted on.
    Unknown {
        kind: String,
        text: String,
    },
}

/// The field an `item.append` concatenates onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendField {
    Text,
    Thinking,
}

impl AppendField {
    /// The JSON field name the append targets.
    pub fn field(self) -> &'static str {
        match self {
            AppendField::Text => "text",
            AppendField::Thinking => "thinking",
        }
    }
}

// --- user -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct UserItem {
    pub text: String,
    pub images: Vec<Image>,
    pub status: UserStatus,
    /// `now` or `after_run` while the input is still queued, absent once it is sent.
    pub queue: Option<QueuePosition>,
}

impl UserItem {
    /// Whether evo has not taken these words yet — what makes the row cancellable
    /// (`input.cancel`) and what the transcript draws as held back.
    pub fn is_queued(&self) -> bool {
        self.status == UserStatus::Queued
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub name: String,
    pub media_type: String,
    pub bytes: u64,
    /// `/media/<id>/<n>` — the bytes are fetched with `GET`, never inlined here.
    pub href: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserStatus {
    Queued,
    Sent,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePosition {
    Now,
    AfterRun,
}

// --- assistant --------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantItem {
    pub text: String,
    pub thinking: String,
    pub status: AssistantStatus,
    pub error: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub usage: Option<Usage>,
}

impl AssistantItem {
    pub fn is_streaming(&self) -> bool {
        self.status == AssistantStatus::Streaming
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantStatus {
    Streaming,
    Final,
    Error,
    Aborted,
    Length,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// The count the next request would send: the context figure a status line shows.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

// --- tool -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct ToolItem {
    pub call_id: String,
    pub name: String,
    /// The call's arguments as an object, exactly as the model wrote them.
    pub args: Value,
    pub status: ToolStatus,
    /// Truncated to 4 KiB on the wire; the whole of it is `GET /items/<id>`.
    pub result: Option<ToolResult>,
    /// The assistant item the call belongs to.
    pub parent: Option<ItemId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Error,
    Blocked,
}

impl ToolStatus {
    /// Whether the call is still in flight (the row's pips) or finished.
    pub fn is_running(self) -> bool {
        self == ToolStatus::Running
    }

    pub fn is_error(self) -> bool {
        matches!(self, ToolStatus::Error | ToolStatus::Blocked)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub text: String,
    /// The result's length before truncation, so a row can say what it is not showing.
    pub chars: u64,
    pub truncated: bool,
}

// --- a lane's report --------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LaneReport {
    pub lane: u32,
    pub done: String,
    pub evidence: String,
    pub next: String,
    pub blocked: String,
    pub requests: String,
    pub goal: Option<String>,
}

// --- a lane's transition ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct LaneEvent {
    pub lane: u32,
    pub event: LaneEventKind,
    pub outcome: Option<String>,
    pub goal_status: Option<String>,
    pub detail: Option<String>,
    pub severity: NoticeSeverity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneEventKind {
    RunEnded,
    Error,
    FailedToStart,
    InitFailed,
    Crashed,
    Down,
    Restarted,
    Other(String),
}

impl LaneEventKind {
    pub fn from_name(kind: &str) -> LaneEventKind {
        match kind {
            "run_ended" => LaneEventKind::RunEnded,
            "error" => LaneEventKind::Error,
            "failed_to_start" => LaneEventKind::FailedToStart,
            "init_failed" => LaneEventKind::InitFailed,
            "crashed" => LaneEventKind::Crashed,
            "down" => LaneEventKind::Down,
            "restarted" => LaneEventKind::Restarted,
            other => LaneEventKind::Other(other.to_string()),
        }
    }

    /// The words the row opens with.
    pub fn label(&self) -> &str {
        match self {
            LaneEventKind::RunEnded => "run ended",
            LaneEventKind::Error => "error",
            LaneEventKind::FailedToStart => "failed to start",
            LaneEventKind::InitFailed => "initialization failed",
            LaneEventKind::Crashed => "crashed",
            LaneEventKind::Down => "is down",
            LaneEventKind::Restarted => "restarted",
            LaneEventKind::Other(other) => other,
        }
    }
}

// --- a goal's transition ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct GoalItem {
    pub event: GoalEventKind,
    pub goal_id: Option<String>,
    pub objective: Option<String>,
    pub budget: Option<u64>,
    pub tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalEventKind {
    Created,
    Continue,
    Wrapup,
    ObjectiveUpdated,
    Paused,
    Resumed,
    Complete,
    BudgetLimited,
    Other(String),
}

impl GoalEventKind {
    pub fn from_name(kind: &str) -> GoalEventKind {
        match kind {
            "created" => GoalEventKind::Created,
            "continue" => GoalEventKind::Continue,
            "wrapup" => GoalEventKind::Wrapup,
            "objective_updated" => GoalEventKind::ObjectiveUpdated,
            "paused" => GoalEventKind::Paused,
            "resumed" => GoalEventKind::Resumed,
            "complete" => GoalEventKind::Complete,
            "budget_limited" => GoalEventKind::BudgetLimited,
            other => GoalEventKind::Other(other.to_string()),
        }
    }

    /// The words the row opens with.
    pub fn label(&self) -> &str {
        match self {
            GoalEventKind::Created => "created",
            GoalEventKind::Continue => "continue",
            GoalEventKind::Wrapup => "budget exhausted — wrap up",
            GoalEventKind::ObjectiveUpdated => "objective updated",
            GoalEventKind::Paused => "paused",
            GoalEventKind::Resumed => "resumed",
            GoalEventKind::Complete => "complete",
            GoalEventKind::BudgetLimited => "budget limited",
            GoalEventKind::Other(other) => other,
        }
    }
}

// --- the rest of the kinds --------------------------------------------------------------

/// Content an extension injected (`evo:inject-context`'s `:key`): context around the
/// conversation, not a turn of the reader's.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextItem {
    pub key: String,
    pub text: String,
}

/// A command the reader ran, answered with instructions for the agent.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandNote {
    pub command: String,
    pub text: String,
}

/// A human's own action on the swarm — the one thing a person may do to lanes besides
/// typing to the coordinator (CONTRACT §7.4).
#[derive(Clone, Debug, PartialEq)]
pub struct HumanAction {
    pub action: String,
    pub lanes: Vec<u32>,
}

impl HumanAction {
    /// The lanes the action named, as `Lane 2`, `Lanes 2, 3`, or nothing for the whole
    /// swarm.
    ///
    /// Capitalised because it is a name — the one the column's rows, the band and a
    /// report card all use (`Lane 2`) — and both of its readers put it in a sentence the
    /// reader already knows: the transcript's `Stopped Lane 2`, and the one-line summary
    /// `interrupt Lane 2`.
    pub fn lanes_label(&self) -> Option<String> {
        match self.lanes.as_slice() {
            [] => None,
            [one] => Some(format!("Lane {one}")),
            many => Some(format!(
                "Lanes {}",
                many.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// A line the server says in place: an error, a warning, a durable note.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub severity: NoticeSeverity,
    pub text: String,
    pub source: NoticeSource,
    /// `true` when the notice is also in the journal, so it survives a rebuild.
    pub durable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeSeverity {
    Info,
    Warn,
    Error,
}

impl NoticeSeverity {
    pub fn from_name(severity: &str) -> NoticeSeverity {
        match severity {
            "warn" | "warning" => NoticeSeverity::Warn,
            "error" => NoticeSeverity::Error,
            _ => NoticeSeverity::Info,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeSource {
    Command,
    Extension,
    Swarm,
    Serve,
    Goal,
    Other(String),
}

impl NoticeSource {
    pub fn from_name(source: &str) -> NoticeSource {
        match source {
            "command" => NoticeSource::Command,
            "extension" => NoticeSource::Extension,
            "swarm" => NoticeSource::Swarm,
            "serve" => NoticeSource::Serve,
            "goal" => NoticeSource::Goal,
            other => NoticeSource::Other(other.to_string()),
        }
    }

    /// The word a row puts in front of a notice, when there is one.
    pub fn label(&self) -> Option<&str> {
        match self {
            NoticeSource::Command => Some("command"),
            NoticeSource::Extension => Some("extension"),
            NoticeSource::Swarm => Some("swarm"),
            NoticeSource::Serve => Some("serve"),
            NoticeSource::Goal => Some("goal"),
            NoticeSource::Other(other) => Some(other),
        }
    }
}

/// A run that ended as something other than `stop`.
#[derive(Clone, Debug, PartialEq)]
pub struct RunOutcome {
    pub outcome: String,
    pub error: Option<String>,
}

impl RunOutcome {
    /// The line the row shows: the outcome in the run's own vocabulary, with the error
    /// when there is one.
    pub fn text(&self) -> String {
        let head = match self.outcome.as_str() {
            "" | "ok" | "stop" => "Run stopped".to_string(),
            "aborted" => "Run aborted".to_string(),
            "error" => "Run failed".to_string(),
            "length" => "Run stopped at the length limit".to_string(),
            other => format!("Run ended: {other}"),
        };
        match self.error.as_deref().filter(|error| !error.is_empty()) {
            Some(error) => format!("{head}: {error}"),
            None => head,
        }
    }
}

/// A compaction: a marker in the transcript, and the point the scrollback pages across.
#[derive(Clone, Debug, PartialEq)]
pub struct Compaction {
    pub summary: String,
    pub tokens_before: u64,
    pub tokens_after: u64,
    /// `true` when a person asked for it (`/compact`), `false` when it was automatic.
    pub manual: bool,
}

/// A supervisor's account of a replaced run.
#[derive(Clone, Debug, PartialEq)]
pub struct Recovery {
    pub status: String,
    pub code: Option<String>,
    pub attempt: Option<u64>,
    pub reason: Option<String>,
}

/// A provider retry: ephemeral, never journaled (CONTRACT §4.1).
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderRetry {
    pub attempt: u64,
    pub max: u64,
    pub delay_ms: u64,
    pub reason: Option<String>,
}

// --- parsing ----------------------------------------------------------------------------

impl Item {
    /// Parse one wire item. `None` only when there is no usable id: an item without an
    /// id cannot be keyed, patched or removed, so it is dropped rather than guessed at.
    pub fn from_json(value: &Value) -> Option<Item> {
        let id = value.get("id").and_then(Value::as_str)?.to_string();
        let ts = value.get("ts").and_then(Value::as_u64).unwrap_or(0);
        let kind = ItemKind::from_json(value);
        Some(Item {
            id,
            ts,
            kind,
            raw: value.clone(),
        })
    }

    /// The wire object this item was parsed from.
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// Concatenate streamed text onto the item's field (`item.append`).
    pub fn apply_append(&mut self, field: AppendField, text: &str) {
        let key = field.field();
        let current = self.raw.get(key).and_then(Value::as_str).unwrap_or("");
        self.raw[key] = Value::String(format!("{current}{text}"));
        self.kind = ItemKind::from_json(&self.raw);
    }

    /// Merge-patch the item (`item.patch`) and re-read it.
    pub fn apply_patch(&mut self, patch: &Value) {
        merge_patch(&mut self.raw, patch);
        self.kind = ItemKind::from_json(&self.raw);
    }

    /// Replace the item with a full wire object (`GET /items/<id>`): the untruncated
    /// result of a tool call, or a message's whole thinking.
    pub fn replace_with(&mut self, value: &Value) {
        self.raw = value.clone();
        self.kind = ItemKind::from_json(&self.raw);
    }

    /// Whether this item is a compaction marker: what the scrollback pages across.
    pub fn is_compaction(&self) -> bool {
        matches!(self.kind, ItemKind::Compaction(_))
    }

    /// The assistant text still streaming in this item, if it is one.
    pub fn streaming_text(&self) -> Option<&str> {
        match &self.kind {
            ItemKind::Assistant(assistant) if assistant.is_streaming() => Some(&assistant.text),
            _ => None,
        }
    }

    /// What this item is, in one line: the live activity a lane's row shows, and the
    /// summary a `last_item` falls back to.
    pub fn summary(&self) -> String {
        match &self.kind {
            ItemKind::User(user) => user.text.clone(),
            ItemKind::Assistant(assistant) => {
                if assistant.text.trim().is_empty() {
                    if assistant.is_streaming() {
                        "writing…".to_string()
                    } else if let Some(error) = assistant.error.as_deref() {
                        error.to_string()
                    } else {
                        "thinking…".to_string()
                    }
                } else {
                    assistant.text.clone()
                }
            }
            ItemKind::Tool(tool) => format!("{} · {}", tool.name, tool_status_word(tool.status)),
            ItemKind::LaneReport(report) => {
                if report.done.trim().is_empty() {
                    "report".to_string()
                } else {
                    format!("report: {}", report.done)
                }
            }
            ItemKind::LaneEvent(event) => match event.detail.as_deref() {
                Some(detail) => format!("{} — {detail}", event.event.label()),
                None => event.event.label().to_string(),
            },
            ItemKind::Goal(goal) => format!("goal {}", goal.event.label()),
            ItemKind::Context(context) => format!("context · {}", context.key),
            ItemKind::CommandNote(note) => format!("command {}", note.command),
            ItemKind::HumanAction(action) => match action.lanes_label() {
                Some(lanes) => format!("{} {lanes}", action.action),
                None => action.action.clone(),
            },
            ItemKind::Notice(notice) => notice.text.clone(),
            ItemKind::RunOutcome(outcome) => outcome.text(),
            ItemKind::Compaction(compaction) => {
                if compaction.summary.trim().is_empty() {
                    "context compacted".to_string()
                } else {
                    compaction.summary.clone()
                }
            }
            ItemKind::Recovery(recovery) => match recovery.reason.as_deref() {
                Some(reason) => format!("recovered · {reason}"),
                None => format!("recovered · {}", recovery.status),
            },
            ItemKind::ProviderRetry(retry) => {
                format!("retrying provider ({}/{})", retry.attempt, retry.max)
            }
            ItemKind::Unknown { kind, text } => {
                if text.trim().is_empty() {
                    kind.clone()
                } else {
                    text.clone()
                }
            }
        }
    }
}

/// The word a tool row's status shows.
pub fn tool_status_word(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "running",
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
        ToolStatus::Blocked => "blocked",
    }
}

impl ItemKind {
    /// The wire's name for this kind: `user`, `assistant`, `tool`, `lane_report`, …
    pub fn label(&self) -> &str {
        match self {
            ItemKind::User(_) => "user",
            ItemKind::Assistant(_) => "assistant",
            ItemKind::Tool(_) => "tool",
            ItemKind::LaneReport(_) => "lane_report",
            ItemKind::LaneEvent(_) => "lane_event",
            ItemKind::Goal(_) => "goal",
            ItemKind::Context(_) => "context",
            ItemKind::CommandNote(_) => "command_note",
            ItemKind::HumanAction(_) => "human_action",
            ItemKind::Notice(_) => "notice",
            ItemKind::RunOutcome(_) => "run_outcome",
            ItemKind::Compaction(_) => "compaction",
            ItemKind::Recovery(_) => "recovery",
            ItemKind::ProviderRetry(_) => "provider_retry",
            ItemKind::Unknown { kind, .. } => kind,
        }
    }

    fn from_json(value: &Value) -> ItemKind {
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "user" => ItemKind::User(user_item(value)),
            "assistant" => ItemKind::Assistant(assistant_item(value)),
            "tool" => ItemKind::Tool(tool_item(value)),
            "lane_report" => ItemKind::LaneReport(lane_report(value)),
            "lane_event" => ItemKind::LaneEvent(lane_event(value)),
            "goal" => ItemKind::Goal(goal_item(value)),
            "context" => ItemKind::Context(ContextItem {
                key: string(value, "key"),
                text: string(value, "text"),
            }),
            "command_note" => ItemKind::CommandNote(CommandNote {
                command: string(value, "command"),
                text: string(value, "text"),
            }),
            "human_action" => ItemKind::HumanAction(HumanAction {
                action: string(value, "action"),
                lanes: value
                    .get("lanes")
                    .and_then(Value::as_array)
                    .map(|lanes| {
                        lanes
                            .iter()
                            .filter_map(Value::as_u64)
                            .map(|n| n as u32)
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "notice" => ItemKind::Notice(Notice {
                severity: NoticeSeverity::from_name(
                    value.get("severity").and_then(Value::as_str).unwrap_or(""),
                ),
                text: string(value, "text"),
                source: NoticeSource::from_name(
                    value.get("source").and_then(Value::as_str).unwrap_or(""),
                ),
                durable: value
                    .get("durable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            "run_outcome" => ItemKind::RunOutcome(RunOutcome {
                outcome: string(value, "outcome"),
                error: optional_string(value, "error"),
            }),
            "compaction" => ItemKind::Compaction(Compaction {
                summary: string(value, "summary"),
                tokens_before: u64_field(value, "tokens_before"),
                tokens_after: u64_field(value, "tokens_after"),
                manual: value
                    .get("manual")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            "recovery" => ItemKind::Recovery(Recovery {
                status: string(value, "status"),
                code: optional_string(value, "code"),
                attempt: value.get("attempt").and_then(Value::as_u64),
                reason: optional_string(value, "reason"),
            }),
            "provider_retry" => ItemKind::ProviderRetry(ProviderRetry {
                attempt: u64_field(value, "attempt"),
                max: u64_field(value, "max"),
                delay_ms: u64_field(value, "delay_ms"),
                reason: optional_string(value, "reason"),
            }),
            other => ItemKind::Unknown {
                kind: other.to_string(),
                text: string(value, "text"),
            },
        }
    }
}

fn user_item(value: &Value) -> UserItem {
    UserItem {
        text: string(value, "text"),
        images: value
            .get("images")
            .and_then(Value::as_array)
            .map(|images| {
                images
                    .iter()
                    .map(|image| Image {
                        name: string(image, "name"),
                        media_type: string(image, "media_type"),
                        bytes: u64_field(image, "bytes"),
                        href: string(image, "href"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        status: match value.get("status").and_then(Value::as_str) {
            Some("queued") => UserStatus::Queued,
            Some("cancelled") => UserStatus::Cancelled,
            _ => UserStatus::Sent,
        },
        queue: match value.get("queue").and_then(Value::as_str) {
            Some("now") => Some(QueuePosition::Now),
            Some("after_run") => Some(QueuePosition::AfterRun),
            _ => None,
        },
    }
}

fn assistant_item(value: &Value) -> AssistantItem {
    AssistantItem {
        text: string(value, "text"),
        thinking: string(value, "thinking"),
        status: match value.get("status").and_then(Value::as_str) {
            Some("streaming") => AssistantStatus::Streaming,
            Some("error") => AssistantStatus::Error,
            Some("aborted") => AssistantStatus::Aborted,
            Some("length") => AssistantStatus::Length,
            _ => AssistantStatus::Final,
        },
        error: optional_string(value, "error"),
        model: optional_string(value, "model"),
        provider: optional_string(value, "provider"),
        usage: value
            .get("usage")
            .and_then(Value::as_object)
            .map(|_| Usage {
                input: u64_field(value.get("usage").unwrap_or(&Value::Null), "input"),
                output: u64_field(value.get("usage").unwrap_or(&Value::Null), "output"),
                cache_read: u64_field(value.get("usage").unwrap_or(&Value::Null), "cache_read"),
                cache_write: u64_field(value.get("usage").unwrap_or(&Value::Null), "cache_write"),
            }),
    }
}

fn tool_item(value: &Value) -> ToolItem {
    let call_id = string(value, "call_id");
    ToolItem {
        call_id,
        name: string(value, "name"),
        args: value.get("args").cloned().unwrap_or(Value::Null),
        status: match value.get("status").and_then(Value::as_str) {
            Some("ok") => ToolStatus::Ok,
            Some("error") => ToolStatus::Error,
            Some("blocked") => ToolStatus::Blocked,
            _ => ToolStatus::Running,
        },
        result: value
            .get("result")
            .and_then(Value::as_object)
            .map(|result| {
                let result = Value::Object(result.clone());
                ToolResult {
                    text: string(&result, "text"),
                    chars: u64_field(&result, "chars"),
                    truncated: result
                        .get("truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            }),
        parent: optional_string(value, "parent"),
    }
}

fn lane_report(value: &Value) -> LaneReport {
    LaneReport {
        lane: u64_field(value, "lane") as u32,
        done: string(value, "done"),
        evidence: string(value, "evidence"),
        next: string(value, "next"),
        blocked: string(value, "blocked"),
        requests: string(value, "requests"),
        goal: optional_string(value, "goal"),
    }
}

fn lane_event(value: &Value) -> LaneEvent {
    LaneEvent {
        lane: u64_field(value, "lane") as u32,
        event: LaneEventKind::from_name(value.get("event").and_then(Value::as_str).unwrap_or("")),
        outcome: optional_string(value, "outcome"),
        goal_status: optional_string(value, "goal_status"),
        detail: optional_string(value, "detail"),
        severity: NoticeSeverity::from_name(
            value.get("severity").and_then(Value::as_str).unwrap_or(""),
        ),
    }
}

fn goal_item(value: &Value) -> GoalItem {
    GoalItem {
        event: GoalEventKind::from_name(value.get("event").and_then(Value::as_str).unwrap_or("")),
        goal_id: optional_string(value, "goal_id"),
        objective: optional_string(value, "objective"),
        budget: value.get("budget").and_then(Value::as_u64),
        tokens: value.get("tokens").and_then(Value::as_u64),
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}
