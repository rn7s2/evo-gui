//! Topic state — the readouts a topic carries beside its items (CONTRACT §4.2/§4.3).
//!
//! The state is the *only* source of the status line now: the server publishes
//! `segments` already formatted (a core registry builds them for the TUI and the GUI
//! alike), so nothing here composes `ctx 48k/936k` out of parts. Clocks are absolute
//! (`*_started_at` in epoch milliseconds), so a lane's step clock is arithmetic, never a
//! guessed start.
//!
//! State arrives as a whole object (`GET /snapshot`) or as merge patches
//! (`state.patch`), and every patch may clear a field with `null`. To keep the merge
//! honest it is applied to the raw object and the typed view rebuilt from it — the state
//! is small, and one JSON merge is cheaper than modelling every field twice.

use serde_json::Value;

use crate::format::merge_patch;

/// The state of one topic: how its agent is doing, and everything the chrome reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopicState {
    pub status: Status,
    pub task: Option<Task>,
    pub model: Option<ModelInfo>,
    pub thinking: Option<String>,
    pub language: Option<String>,
    pub context: Option<ContextInfo>,
    pub goal: Option<GoalInfo>,
    pub todos: Vec<Todo>,
    /// Ids of user items still queued, oldest first (CONTRACT §4.2).
    pub queue: Vec<String>,
    pub jobs: Vec<Job>,
    pub segments: Vec<Segment>,
    pub session: Option<SessionInfo>,
    /// The wire object, so a `state.patch` can be merged into it. Never read by the UI.
    raw: Value,
}

impl TopicState {
    /// Read a whole state object.
    pub fn from_json(value: &Value) -> TopicState {
        TopicState {
            status: Status::from_name(value.get("status").and_then(Value::as_str).unwrap_or("")),
            task: object(value, "task").map(|value| Task::from_json(&value)),
            model: object(value, "model").map(|value| ModelInfo::from_json(&value)),
            thinking: optional_string(value, "thinking"),
            language: optional_string(value, "language"),
            context: object(value, "context").map(|value| ContextInfo::from_json(&value)),
            goal: object(value, "goal").map(|value| GoalInfo::from_json(&value)),
            todos: todos_from_json(value.get("todos").unwrap_or(&Value::Null)),
            queue: string_array(value.get("queue")),
            jobs: value
                .get("jobs")
                .and_then(Value::as_array)
                .map(|jobs| jobs.iter().map(Job::from_json).collect())
                .unwrap_or_default(),
            segments: value
                .get("segments")
                .and_then(Value::as_array)
                .map(|segments| segments.iter().map(Segment::from_json).collect())
                .unwrap_or_default(),
            session: object(value, "session").map(|value| SessionInfo::from_json(&value)),
            raw: value.clone(),
        }
    }

    /// Merge one `state.patch` and re-read the state.
    pub fn apply_patch(&mut self, patch: &Value) {
        merge_patch(&mut self.raw, patch);
        *self = TopicState::from_json(&self.raw);
    }

    /// Whether this topic is doing something (the composer's face, a row's clock).
    pub fn is_busy(&self) -> bool {
        matches!(self.status, Status::Running | Status::Compacting)
    }

    /// The context figure, in the words the status line uses — `48k/936k`, or `48k`
    /// while the window is unknown.
    pub fn context_label(&self) -> Option<String> {
        let context = self.context.as_ref()?;
        Some(match context.window {
            Some(window) if window > 0 => format!(
                "{}/{}",
                crate::k_tokens(context.tokens),
                crate::k_tokens(window)
            ),
            _ => crate::k_tokens(context.tokens),
        })
    }

    /// The step clock as of `now_millis`: how long the current step has been running.
    /// `None` while nothing is in flight, or when the state carries no start.
    pub fn step_clock(&self, now_millis: u64) -> Option<String> {
        let task = self.task.as_ref()?;
        if !self.is_busy() {
            return None;
        }
        let started = if task.step_started_at > 0 {
            task.step_started_at
        } else {
            task.started_at
        };
        if started == 0 {
            return None;
        }
        Some(crate::short_duration(
            now_millis.saturating_sub(started) / 1000,
        ))
    }
}

/// How the topic is doing (`idle|running|compacting|waiting`). `waiting` is a coordinator
/// that settled but is held while its lanes work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Running,
    Compacting,
    Waiting,
}

impl Status {
    pub fn from_name(status: &str) -> Status {
        match status {
            "running" => Status::Running,
            "compacting" => Status::Compacting,
            "waiting" => Status::Waiting,
            _ => Status::Idle,
        }
    }

    /// The word a row shows for it.
    pub fn label(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Running => "running",
            Status::Compacting => "compacting",
            Status::Waiting => "waiting on lanes",
        }
    }
}

/// The task in flight (`kind` is `run` or `compact`).
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub turn: Option<u64>,
    pub started_at: u64,
    pub step_started_at: u64,
}

impl Task {
    fn from_json(value: &Value) -> Task {
        Task {
            id: string(value, "id"),
            kind: string(value, "kind"),
            turn: value.get("turn").and_then(Value::as_u64),
            started_at: u64_field(value, "started_at"),
            step_started_at: u64_field(value, "step_started_at"),
        }
    }

    pub fn is_compaction(&self) -> bool {
        self.kind == "compact"
    }
}

/// The model a topic runs on, and whether it is usable.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub ready: bool,
    /// Why it is not ready, when it is not.
    pub reason: Option<String>,
}

impl ModelInfo {
    fn from_json(value: &Value) -> ModelInfo {
        ModelInfo {
            id: string(value, "id"),
            provider: string(value, "provider"),
            ready: value.get("ready").and_then(Value::as_bool).unwrap_or(true),
            reason: optional_string(value, "reason"),
        }
    }

    /// The model as a label: `id`, or `id (provider)` while the provider matters.
    pub fn label(&self) -> String {
        if self.provider.is_empty() {
            return self.id.clone();
        }
        format!("{} ({})", self.id, self.provider.to_lowercase())
    }
}

/// How full the context is.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextInfo {
    pub tokens: u64,
    pub window: Option<u64>,
    /// `usage` or `estimate`.
    pub source: Option<String>,
}

impl ContextInfo {
    fn from_json(value: &Value) -> ContextInfo {
        ContextInfo {
            tokens: u64_field(value, "tokens"),
            window: value.get("window").and_then(Value::as_u64),
            source: optional_string(value, "source"),
        }
    }
}

/// The session's goal.
#[derive(Clone, Debug, PartialEq)]
pub struct GoalInfo {
    pub goal_id: String,
    pub objective: String,
    pub status: String,
    pub budget: Option<u64>,
    pub tokens: u64,
}

impl GoalInfo {
    fn from_json(value: &Value) -> GoalInfo {
        GoalInfo {
            goal_id: string(value, "goal_id"),
            objective: string(value, "objective"),
            status: string(value, "status"),
            budget: value.get("budget").and_then(Value::as_u64),
            tokens: u64_field(value, "tokens"),
        }
    }

    /// `12k/50k`, or `12k` while the goal has no budget.
    pub fn tokens_label(&self) -> String {
        match self.budget {
            Some(budget) => format!(
                "{}/{}",
                crate::k_tokens(self.tokens),
                crate::k_tokens(budget)
            ),
            None => crate::k_tokens(self.tokens),
        }
    }
}

/// One job the session is running in the background.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub status: String,
    pub started_at: u64,
}

impl Job {
    fn from_json(value: &Value) -> Job {
        Job {
            id: string(value, "id"),
            name: string(value, "name"),
            status: string(value, "status"),
            started_at: u64_field(value, "started_at"),
        }
    }
}

/// One status-line segment, as the server's registry built it (CONTRACT §4.2). The UI
/// renders `text` as it is: it does not know what a segment means.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub name: String,
    pub order: i64,
    pub side: Side,
    pub text: String,
    pub data: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Segment {
    fn from_json(value: &Value) -> Segment {
        Segment {
            name: string(value, "name"),
            order: value.get("order").and_then(Value::as_i64).unwrap_or(0),
            side: match value.get("side").and_then(Value::as_str) {
                Some("right") => Side::Right,
                _ => Side::Left,
            },
            text: string(value, "text"),
            data: value.get("data").cloned().unwrap_or(Value::Null),
        }
    }
}

/// The session a topic is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub path: String,
    pub leaf: Option<String>,
    /// `evo-agent`, `evo-swarm` or `lane`.
    pub program: String,
    pub started_at: u64,
}

impl SessionInfo {
    fn from_json(value: &Value) -> SessionInfo {
        SessionInfo {
            id: string(value, "id"),
            path: string(value, "path"),
            leaf: optional_string(value, "leaf"),
            program: string(value, "program"),
            started_at: u64_field(value, "started_at"),
        }
    }
}

/// The status line's segments, in the order the server gave them: left side first, then
/// right, each by `order`.
pub fn ordered_segments(segments: &[Segment]) -> (Vec<&Segment>, Vec<&Segment>) {
    let mut left: Vec<&Segment> = segments.iter().filter(|s| s.side == Side::Left).collect();
    let mut right: Vec<&Segment> = segments.iter().filter(|s| s.side == Side::Right).collect();
    left.sort_by_key(|segment| segment.order);
    right.sort_by_key(|segment| segment.order);
    (left, right)
}

// --- the swarm topic --------------------------------------------------------------------

/// Topic `swarm` (CONTRACT §4.3): the swarm as one observable thing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwarmState {
    pub id: String,
    pub workers: u64,
    pub busy: u64,
    /// The coordinator is settled but held while its lanes work.
    pub waiting_on_lanes: bool,
    pub lane_model: Option<ModelInfo>,
    pub lane_thinking: Option<String>,
    pub lanes: Vec<SwarmLane>,
    raw: Value,
}

/// One lane, as the swarm topic reports it. Every field is the swarm's own; a lane's step
/// clock is `now - step_started_at`, an absolute time.
#[derive(Clone, Debug, PartialEq)]
pub struct SwarmLane {
    pub n: u32,
    pub state: String,
    pub task: Option<String>,
    pub task_started_at: Option<u64>,
    pub step_started_at: Option<u64>,
    pub restarts: u64,
    pub pid: Option<u64>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub model: Option<ModelInfo>,
    pub context: Option<ContextInfo>,
    pub goal: Option<GoalInfo>,
    pub todos: Vec<Todo>,
    pub reports: u64,
    pub last_item: Option<LastItem>,
}

/// The newest thing a lane did, as the swarm summarizes it for a row that has no mirror
/// of that lane yet.
#[derive(Clone, Debug, PartialEq)]
pub struct LastItem {
    pub kind: String,
    pub summary: String,
}

impl SwarmState {
    pub fn from_json(value: &Value) -> SwarmState {
        let status = value.get("status").cloned().unwrap_or(Value::Null);
        let config = value.get("config").cloned().unwrap_or(Value::Null);
        SwarmState {
            id: string(value, "id"),
            workers: u64_field(value, "workers"),
            busy: u64_field(&status, "busy"),
            waiting_on_lanes: status
                .get("waiting_on_lanes")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            lane_model: object(&config, "lane_model").map(|value| ModelInfo::from_json(&value)),
            lane_thinking: optional_string(&config, "lane_thinking"),
            lanes: value
                .get("lanes")
                .and_then(Value::as_array)
                .map(|lanes| lanes.iter().map(swarm_lane).collect())
                .unwrap_or_default(),
            raw: value.clone(),
        }
    }

    pub fn apply_patch(&mut self, patch: &Value) {
        merge_patch(&mut self.raw, patch);
        *self = SwarmState::from_json(&self.raw);
    }

    /// Whether the swarm is doing anything at all: the face of the action button.
    pub fn is_busy(&self) -> bool {
        self.busy > 0 || self.waiting_on_lanes
    }

    pub fn lane(&self, n: u32) -> Option<&SwarmLane> {
        self.lanes.iter().find(|lane| lane.n == n)
    }
}

impl SwarmLane {
    /// Whether the lane is working or compacting — a live step.
    pub fn is_busy(&self) -> bool {
        matches!(self.state.as_str(), "working" | "compacting")
    }

    /// The lane's step clock at `now_millis`, from its absolute start.
    pub fn step_clock(&self, now_millis: u64) -> Option<String> {
        if !self.is_busy() {
            return None;
        }
        let started = self.step_started_at.or(self.task_started_at)?;
        Some(crate::short_duration(
            now_millis.saturating_sub(started) / 1000,
        ))
    }
}

fn swarm_lane(value: &Value) -> SwarmLane {
    SwarmLane {
        n: u64_field(value, "n") as u32,
        state: string(value, "state"),
        task: optional_string(value, "task"),
        task_started_at: value.get("task_started_at").and_then(Value::as_u64),
        step_started_at: value.get("step_started_at").and_then(Value::as_u64),
        restarts: u64_field(value, "restarts"),
        pid: value.get("pid").and_then(Value::as_u64),
        worktree: optional_string(value, "worktree"),
        branch: optional_string(value, "branch"),
        model: object(value, "model").map(|value| ModelInfo::from_json(&value)),
        context: object(value, "context").map(|value| ContextInfo::from_json(&value)),
        goal: object(value, "goal").map(|value| GoalInfo::from_json(&value)),
        todos: todos_from_json(value.get("todos").unwrap_or(&Value::Null)),
        reports: u64_field(value, "reports"),
        last_item: object(value, "last_item").map(|item| LastItem {
            kind: string(&item, "kind"),
            summary: string(&item, "summary"),
        }),
    }
}

// --- todos ------------------------------------------------------------------------------

/// One checklist item.
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    /// The panel's glyph: ☑ done, ◐ in progress, ☐ pending.
    pub fn glyph(self) -> char {
        match self {
            TodoStatus::Done => '☑',
            TodoStatus::InProgress => '◐',
            TodoStatus::Pending => '☐',
        }
    }
}

/// One status string as the panel's status. An unknown status reads as pending, so an
/// item is never dropped for its wording.
pub fn todo_status_from_str(status: &str) -> TodoStatus {
    match status {
        "done" => TodoStatus::Done,
        "in-progress" | "in_progress" => TodoStatus::InProgress,
        _ => TodoStatus::Pending,
    }
}

/// The checklist from a `todos` array; items without text are dropped.
pub fn todos_from_json(value: &Value) -> Vec<Todo> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item.get("text").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(todo_status_from_str)
                .unwrap_or(TodoStatus::Pending);
            Some(Todo {
                text: text.to_string(),
                status,
            })
        })
        .collect()
}

// --- small readers -----------------------------------------------------------------------

fn object(value: &Value, key: &str) -> Option<Value> {
    if key.is_empty() {
        return value.is_object().then(|| value.clone());
    }
    match value.get(key) {
        Some(Value::Object(_)) => value.get(key).cloned(),
        _ => None,
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

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
