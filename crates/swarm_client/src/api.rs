//! The typed side of serve's protocol, with the raw JSON kept beside it.
//!
//! Every read returns a [`Payload<T>`]: the fields this crate names, and the
//! `serde_json::Value` exactly as it arrived — the view-model crates fold the
//! raw form (docs/PROMPT.md §9.1), so nothing here has to be complete.
//!
//! Field names and shapes come from docs/serve.md, docs/swarm.md, the routes in
//! `../evo-agent/src/serve/routes.lisp` and `../evo-agent/swarm/api.lisp`, and
//! were checked against a live `evo-swarm serve` (see tests/swarm_e2e.rs).

use std::ops::Deref;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result, StatusError};
use crate::http::{HttpClient, HttpResponse, Token};

/// A typed read, with the JSON it came from.
#[derive(Clone, Debug)]
pub struct Payload<T> {
    pub typed: T,
    /// The reply as it arrived: the source of truth for anything not typed here.
    pub raw: Value,
}

impl<T> Deref for Payload<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.typed
    }
}

impl<T> Payload<T> {
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

/// A parsed reply: the fields, plus the whole value.
fn typed<T: for<'de> Deserialize<'de>>(raw: Value) -> Result<Payload<T>> {
    let value = serde_json::from_value(raw.clone())?;
    Ok(Payload { typed: value, raw })
}

// ---------------------------------------------------------------- health (§4)

/// `GET /health` — what this server is, before anything is asked of it.
#[derive(Clone, Debug, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub pid: u32,
    /// The last event id published: the SSE bootstrap cursor (§9.1).
    #[serde(default)]
    pub cursor: Option<i64>,
    /// `evo-swarm` or `evo-agent` (docs/serve.md §"The two seams").
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// What the program serves beyond the protocol: `["swarm"]` for the swarm.
    #[serde(default)]
    pub features: Vec<String>,
}

impl Health {
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

// ----------------------------------------------------------------- state (§4)

/// The coordinator's status (`/state.status`, `task-start`/`settled`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Running,
    Compacting,
    Unknown,
}

impl Activity {
    pub fn parse(text: &str) -> Activity {
        match text {
            "idle" => Activity::Idle,
            "running" => Activity::Running,
            "compacting" => Activity::Compacting,
            _ => Activity::Unknown,
        }
    }

    pub fn is_busy(self) -> bool {
        matches!(self, Activity::Running | Activity::Compacting)
    }
}

/// A task: a run or a compaction (`/state.task`, `/lanes`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TaskInfo {
    #[serde(default)]
    pub id: String,
    /// `run` or `compact`.
    #[serde(default)]
    pub kind: String,
    /// A universal time.
    #[serde(default)]
    pub started: Option<i64>,
    /// Seconds in the whole task.
    #[serde(default)]
    pub age: Option<f64>,
    /// Seconds in the current step (one turn, or one compaction) — the TUI's
    /// activity clock.
    #[serde(default)]
    pub step_age: Option<f64>,
}

/// A session goal (`/state.goal`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Goal {
    #[serde(default, alias = "id")]
    pub goal_id: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    /// `active`, `complete`, `paused` or `budget-limited`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: Option<u64>,
    #[serde(default)]
    pub tokens_used_live: Option<u64>,
}

impl Goal {
    /// `tokens_used + tokens_used_live`, the figure the status readout shows.
    pub fn tokens(&self) -> u64 {
        self.tokens_used.unwrap_or(0) + self.tokens_used_live.unwrap_or(0)
    }
}

/// One todo (`/state.todos`, `todo-changed`).
#[derive(Clone, Debug, Deserialize)]
pub struct Todo {
    #[serde(default)]
    pub text: String,
    /// As evo spells it (`pending`, `in-progress`, `done`, …).
    #[serde(default)]
    pub status: String,
}

impl Todo {
    pub fn kind(&self) -> TodoKind {
        TodoKind::parse(&self.status)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoKind {
    Pending,
    InProgress,
    Done,
    Unknown,
}

impl TodoKind {
    pub fn parse(text: &str) -> TodoKind {
        let normal = text.replace('_', "-");
        match normal.as_str() {
            "pending" => TodoKind::Pending,
            "done" | "completed" => TodoKind::Done,
            "in-progress" | "active" => TodoKind::InProgress,
            _ => TodoKind::Unknown,
        }
    }
}

/// `GET /state` — everything a status line shows, and then some.
#[derive(Clone, Debug, Deserialize)]
pub struct State {
    /// `idle`, `running` or `compacting`.
    pub status: String,
    #[serde(default)]
    pub task: Option<TaskInfo>,
    #[serde(default)]
    pub turn: u64,
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model_ready: Option<bool>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub context_tokens: u64,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub goal: Option<Goal>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// Background jobs, when evo has any (kept raw: nothing here draws them).
    #[serde(default)]
    pub jobs: Option<Value>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_started: Option<bool>,
    #[serde(default)]
    pub leaf: Option<String>,
    #[serde(default)]
    pub pending_input: Option<Value>,
}

impl State {
    pub fn activity(&self) -> Activity {
        Activity::parse(&self.status)
    }

    pub fn is_idle(&self) -> bool {
        self.activity() == Activity::Idle
    }
}

// ------------------------------------------------------------ transcript (§4)

/// `GET /transcript` and `GET /lanes/N/transcript` — the folded context the next
/// turn sends. The messages stay raw: the session crate folds them itself (§9.1),
/// and each is `{role, content:[…]}` with an assistant's `api`, `model`,
/// `stop_reason` and `usage` alongside.
#[derive(Clone, Debug, Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub messages: Vec<Value>,
}

// -------------------------------------------------------------- registry (§4)

/// One registered model.
#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
    /// The wire API it speaks (`anthropic-messages`, …) — what a
    /// `--no-userspace` lane can register (§9.4).
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub thinking_mode: Option<String>,
    /// The levels it accepts — a list, or `true`/`null`.
    #[serde(default)]
    pub effort: Option<Value>,
}

/// A provider, never its key: `has_api_key` and the name of the variable it
/// reads one from.
#[derive(Clone, Debug, Deserialize)]
pub struct Provider {
    pub key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub has_api_key: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Command {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Language {
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub native: Option<String>,
}

/// `GET /registry` — what the session can use, with no secrets.
#[derive(Clone, Debug, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub providers: Vec<Provider>,
    /// The kernel's own wire APIs: how a lane's `--no-userspace` probe tells
    /// which models a lane can register (§9.4).
    #[serde(default)]
    pub apis: Vec<String>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub active_tools: Vec<String>,
    #[serde(default)]
    pub commands: Vec<Command>,
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub languages: Vec<Language>,
    /// The settings evo is running with (kept raw: it is an open set).
    #[serde(default)]
    pub settings: Value,
}

impl Registry {
    /// Whether a `--no-userspace` lane can register this model: its `api` is one
    /// the kernel itself defines, which the probe's `/registry.apis` lists (§9.4).
    pub fn lane_ready(&self, model: &Model) -> bool {
        match model.api.as_deref() {
            Some(api) => self.apis.iter().any(|known| known == api),
            None => true,
        }
    }
}

// --------------------------------------------------------------- journal (§4)

/// `GET /journal` — the entries on the root→leaf path, as journaled.
#[derive(Clone, Debug, Deserialize)]
pub struct Journal {
    pub path: String,
    #[serde(default)]
    pub header: Value,
    #[serde(default)]
    pub leaf: Option<String>,
    /// Journal entries, kept raw (§9.3 walks them for the cache-stats entry).
    #[serde(default)]
    pub entries: Vec<Value>,
}

impl Journal {
    /// The `data` of the newest `:custom` entry whose `key` is KEY — how the
    /// cache figure is seeded from `/journal?limit=N` (§7.3).
    pub fn newest_custom(&self, key: &str) -> Option<&Value> {
        self.entries.iter().rev().find(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("custom")
                && entry.get("key").and_then(Value::as_str) == Some(key)
        })
    }
}

// ----------------------------------------------------------------- lanes (§4)

/// What a lane is doing, as `GET /lanes` reports it: `● ◐ ○ ◌ ✗`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// `starting` — the process is up, its baseline is not evaluated yet.
    Starting,
    Idle,
    Working,
    Compacting,
    /// `down` — it crashed and has not come back.
    Down,
    /// `stopped` — shutting down with the swarm.
    Stopped,
    Unknown,
}

impl LaneState {
    pub fn parse(text: &str) -> LaneState {
        match text {
            "starting" => LaneState::Starting,
            "idle" => LaneState::Idle,
            "working" => LaneState::Working,
            "compacting" => LaneState::Compacting,
            "down" => LaneState::Down,
            "stopped" => LaneState::Stopped,
            _ => LaneState::Unknown,
        }
    }
}

/// One lane, as `GET /lanes` and `lane-state` report it. No token, no URL: a
/// client reads lanes, it never talks to them (§D21).
#[derive(Clone, Debug, Deserialize)]
pub struct Lane {
    pub n: u32,
    pub state: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub task_age: Option<f64>,
    #[serde(default)]
    pub step_age: Option<f64>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub restarts: u64,
    #[serde(default)]
    pub reports: u64,
    /// The lane's goal status, when it has a goal.
    #[serde(default)]
    pub goal: Option<String>,
}

impl Lane {
    pub fn state(&self) -> LaneState {
        LaneState::parse(&self.state)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct SwarmSummary {
    pub id: String,
    pub dir: String,
    pub cwd: String,
    #[serde(default)]
    pub workers: u32,
    #[serde(default)]
    pub busy: u32,
    #[serde(default)]
    pub stopping: Option<bool>,
}

impl SwarmSummary {
    pub fn is_stopping(&self) -> bool {
        self.stopping.unwrap_or(false)
    }
}

/// `GET /lanes` — the swarm and one row per lane.
#[derive(Clone, Debug, Deserialize)]
pub struct Lanes {
    pub swarm: SwarmSummary,
    #[serde(default)]
    pub lanes: Vec<Lane>,
}

impl Lanes {
    pub fn lane(&self, n: u32) -> Option<&Lane> {
        self.lanes.iter().find(|lane| lane.n == n)
    }

    pub fn all_idle(&self) -> bool {
        self.lanes.iter().all(|lane| lane.state() == LaneState::Idle)
    }
}

// ------------------------------------------------------- POST reply envelope

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStyle {
    Plain,
    Dim,
    Notice,
    Success,
    Error,
    Unknown,
}

impl OutputStyle {
    pub fn parse(text: &str) -> OutputStyle {
        match text {
            "plain" => OutputStyle::Plain,
            "dim" => OutputStyle::Dim,
            "notice" => OutputStyle::Notice,
            "success" => OutputStyle::Success,
            "error" => OutputStyle::Error,
            _ => OutputStyle::Unknown,
        }
    }
}

/// One line the TUI would have scrolled.
#[derive(Clone, Debug, Deserialize)]
pub struct OutputLine {
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub text: String,
}

impl OutputLine {
    pub fn kind(&self) -> OutputStyle {
        OutputStyle::parse(&self.style)
    }
}

/// One entry of a picker a command would have opened in the TUI.
#[derive(Clone, Debug, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Choices {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub items: Vec<Choice>,
}

/// Every `POST` answers with this (docs/serve.md §Command replies). The HTTP
/// status is the reply's status, so a refusal arrives as an [`Error`] instead;
/// this is the shape of a reply that succeeded.
#[derive(Clone, Debug, Deserialize)]
pub struct Envelope {
    pub ok: bool,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub output: Vec<OutputLine>,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub choices: Option<Choices>,
    /// The id of the last event published before the command ran: everything it
    /// caused is after this.
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default)]
    pub task: Option<TaskInfo>,
}

impl Envelope {
    pub fn is_ok(&self) -> bool {
        self.ok
    }

    /// The output lines joined, for a notice shown in one piece.
    pub fn output_text(&self) -> String {
        self.output.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

// -------------------------------------------------------------------- client

/// A client for one server: a port, a token, and the endpoints §4 uses.
#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
}

impl Client {
    /// The coordinator of a tab: loopback, that tab's token.
    pub fn loopback(port: u16, token: Token) -> Client {
        Client { http: HttpClient::loopback(port, token) }
    }

    pub fn new(host: impl Into<String>, port: u16, token: Token) -> Result<Client> {
        Ok(Client { http: HttpClient::new(host, port, token)? })
    }

    /// The same server, asked with a shorter patience.
    pub fn with_timeout(&self, timeout: std::time::Duration) -> Client {
        Client { http: self.http.with_timeout(timeout) }
    }

    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    pub fn port(&self) -> u16 {
        self.http.port()
    }

    pub fn token(&self) -> &Token {
        self.http.token()
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<Payload<T>> {
        let reply = self.http.get(path)?;
        if !reply.is_success() {
            return Err(Error::Status(reply.error()));
        }
        typed(reply.json()?)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Envelope> {
        let reply = self.http.post(path, body)?;
        self.envelope(reply)
    }

    fn envelope(&self, reply: HttpResponse) -> Result<Envelope> {
        if !reply.is_success() {
            return Err(Error::Status(reply.error()));
        }
        let raw = reply.json()?;
        let envelope: Envelope = serde_json::from_value(raw.clone())?;
        if !envelope.ok {
            // A 2xx reply that says it failed: type it by its own status.
            let status = envelope.status.unwrap_or(200);
            let text = envelope.output_text();
            return Err(Error::Status(StatusError::from_reply(status, raw, &text)));
        }
        Ok(envelope)
    }

    /// `GET /health` — readiness and the SSE bootstrap cursor.
    pub fn health(&self) -> Result<Payload<Health>> {
        self.read("/health")
    }

    /// `GET /state`.
    pub fn state(&self) -> Result<Payload<State>> {
        self.read("/state")
    }

    /// `GET /transcript?limit=N` — the coordinator's folded context.
    pub fn transcript(&self, limit: Option<u32>) -> Result<Payload<Transcript>> {
        self.read(&with_limit("/transcript", limit))
    }

    /// `GET /registry` — models, providers, tools, commands, settings.
    pub fn registry(&self) -> Result<Payload<Registry>> {
        self.read("/registry")
    }

    /// `GET /journal?limit=N` — the entries on the root→leaf path.
    pub fn journal(&self, limit: Option<u32>) -> Result<Payload<Journal>> {
        self.read(&with_limit("/journal", limit))
    }

    /// `GET /lanes` — the swarm and its lanes (feature `swarm`).
    pub fn lanes(&self) -> Result<Payload<Lanes>> {
        self.read("/lanes")
    }

    /// `GET /lanes/N/transcript?limit=N`.
    pub fn lane_transcript(&self, n: u32, limit: Option<u32>) -> Result<Payload<Transcript>> {
        self.read(&with_limit(&format!("/lanes/{n}/transcript"), limit))
    }

    /// Any read this crate has not typed.
    pub fn get_raw(&self, path: &str) -> Result<Value> {
        let reply = self.http.get(path)?;
        if !reply.is_success() {
            return Err(Error::Status(reply.error()));
        }
        reply.json()
    }

    /// `POST /prompt {"text": …}` — the user's turn. It starts a run, or lands
    /// at the running one's next turn boundary.
    pub fn prompt(&self, text: &str) -> Result<Envelope> {
        self.post("/prompt", &serde_json::json!({ "text": text }))
    }

    /// `POST /prompt` with a body of your own (`images`, `stream`).
    pub fn prompt_with(&self, body: &Value) -> Result<Envelope> {
        self.post("/prompt", body)
    }

    /// `POST /steer {"text": …}` — mid-run input; `409 Not now` when nothing runs.
    pub fn steer(&self, text: &str) -> Result<Envelope> {
        self.post("/steer", &serde_json::json!({ "text": text }))
    }

    /// `POST /follow-up {"text": …}` — input for after the run settles.
    pub fn follow_up(&self, text: &str) -> Result<Envelope> {
        self.post("/follow-up", &serde_json::json!({ "text": text }))
    }

    /// `POST /interrupt` — the TUI's esc.
    pub fn interrupt(&self) -> Result<Envelope> {
        self.post("/interrupt", &serde_json::json!({}))
    }

    /// `POST /command {"text": "/goal …"}` — any slash command, resolved as the
    /// TUI resolves it.
    pub fn command(&self, text: &str) -> Result<Envelope> {
        self.post("/command", &serde_json::json!({ "text": text }))
    }

    /// `POST /shutdown` — end the session, lanes included.
    pub fn shutdown(&self) -> Result<Envelope> {
        self.post("/shutdown", &serde_json::json!({}))
    }

    /// Any command this crate has not typed.
    pub fn post_raw(&self, path: &str, body: &Value) -> Result<Envelope> {
        self.post(path, body)
    }
}

fn with_limit(path: &str, limit: Option<u32>) -> String {
    match limit {
        Some(n) => format!("{path}?limit={n}"),
        None => path.to_owned(),
    }
}
