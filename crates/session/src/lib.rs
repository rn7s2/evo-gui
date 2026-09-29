//! session — pure per-agent view model (no gpui, no I/O).
//!
//! CONTRACT (shared with the `transcript`, `composer`, `workspace` crates): the public
//! types below are what the UI crates render. Additive changes are fine; renames or
//! removals must be agreed with the coordinator first.
//!
//! Inputs are raw JSON (`serde_json::Value`) exactly as the server sends them, so this
//! crate does not depend on `swarm_client`.
//!
//! # What the UI does with it
//!
//! * one [`AgentModel`] per agent (the coordinator, and one per watched lane);
//! * [`AgentModel::apply_event`] returns an [`Effect`] saying what has to happen —
//!   the transcript re-renders, the todo panel changed, the readout changed, the
//!   activity changed, or the whole view must resync from `/state` + `/transcript`;
//! * the coordinator's tab also keeps a [`LaneList`] (fed by `GET /lanes` and by
//!   `lane-state` events) and a [`Readout`] (the §7.3 status line, seeded by
//!   `/state` + `/registry` + the journal's `cache-stats` entry).

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
    /// A user turn (`user-input`, `steering`, or a user message from /transcript
    /// that no extension injected — see [`RowKind::Context`]).
    User { text: String },
    /// Content an extension injected with `evo:inject-context`, which journals a
    /// `:custom-message` entry carrying a `:key`; the fold tags the message
    /// `:meta (:key ...)` and /transcript sends `"meta": {"key": "<key>"}`
    /// (`evo-agent/src/journal/journal.lisp`, captured in
    /// `tests/fixtures/context-transcript.json`). The memory extension injects
    /// `global-memory` and `project-memory` at the start of a fresh session, and
    /// `recovery` carries a supervisor's account of a replaced run.
    ///
    /// It arrives as a user-role message, but the reader did not write it: it is
    /// context around the conversation, so it is its own row kind rather than a
    /// turn of theirs — and it never opens a turn.
    Context { key: String, text: String },
    /// Assistant message. `markdown` is the whole source so far; UI calls
    /// `TextViewState::set_text(markdown)` whenever `version` changes.
    Assistant {
        markdown: String,
        thinking: String,
        streaming: bool,
        error: Option<String>,
    },
    /// One tool call, completed by its result.
    Tool {
        call_id: String,
        name: String,
        arguments: String,
        result: Option<ToolResult>,
    },
    /// A lane's report: the lane's own `report` event, or the same report as the swarm
    /// passed it to the coordinator in its own words — `[lane 2 report] done: …\nevidence:
    /// …\nnext: …\nblocked: …\nrequests: …\ngoal: …` (`swarm/lanes.lisp:220`). Both are
    /// one row, because they are one thing: what a lane said it did.
    Report {
        done: String,
        evidence: String,
        next: String,
        blocked: String,
        requests: String,
        /// The lane's goal status, when the report carried one (`active`, `complete`,
        /// …). evo appends it as a last `goal: …` line and only when the lane has a
        /// goal, so an absent one is `None` rather than an empty field.
        goal: Option<String>,
        /// Which lane reported, for a row built from the coordinator's own input
        /// (`[lane N report] …`); `None` for a row from the lane's own stream, whose
        /// lane the tab showing it already knows.
        lane: Option<u32>,
    },
    /// A line the swarm wrote to the coordinator about one of its lanes: `[lane 1] run
    /// ended (stop) — task: …`, `[lane 1] failed to start — see …/lane.log`,
    /// `[lane 1] initialization failed: …`, `[lane 1] error: …`, `[lane 1] is down: …`,
    /// `[lane 1] crashed and was restarted …` (`swarm/lanes.lisp`).
    ///
    /// These reach the coordinator as input through `tell-coordinator` →
    /// `evo:steer` (`lanes.lisp:19`), with no `meta` key and no event of their own, so
    /// the `[lane N]` prefix — evo's own fixed format — is what marks them: a reader
    /// does not type it, and the line is the swarm talking, not a turn of theirs.
    LaneNotice {
        lane: u32,
        /// The message without its `[lane N] ` prefix, as evo wrote it.
        text: String,
        /// How loudly to say it: [`DimStyle::Error`] for the lines evo sends
        /// `:style :error` (a lane that failed, errored or is down), [`DimStyle::Notice`]
        /// for a run that merely ended.
        tone: DimStyle,
    },
    /// `output` lines and status events (compaction, provider-retry, a reconnect...).
    Dim { style: DimStyle, text: String },
    /// A run that ended badly: `run-end` with an outcome that is not a clean stop
    /// (`error`, `aborted`, `length`). `text` is the line to show. A run that stopped as
    /// asked emits no row — the turn boundary already says it finished — and `run-start`
    /// never emits one at all.
    RunOutcome { outcome: String, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub is_error: bool,
    pub content: String,
    pub content_chars: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimStyle {
    Dim,
    Notice,
    Error,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub text: String,
    pub status: TodoStatus,
}

/// Coordinator activity as the composer button sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Running,
    Compacting,
}

/// Lane list glyphs (§7.3): ● working, ◐ compacting, ○ idle, ◌ starting, ✗ down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneStatus {
    Working,
    Compacting,
    Idle,
    Starting,
    Down,
}

mod cache;
mod effect;
mod lanes;
mod launcher;
mod model;
mod readout;
mod tab;
mod todos;

pub use cache::{
    cache_seed_limits, cache_seed_next_limit, cache_stats_from_journal, cache_totals_from_seed,
};
pub use effect::{Effect, RowChanges};
pub use lanes::{lane_task_label, short_duration, LaneList, LaneRow, SwarmInfo};
pub use launcher::{
    coordinator_chooser, history_rows, home_short, lanes_chooser, lanes_model_note, relative_time,
    swarm_lisp_path, swarm_workers_setting, workers_chooser, Choice, Chooser, ChooserOption,
    HistoryEntry, HistoryRow, HistorySource, LaunchPlan, Launcher, When, DEFAULT_KEY,
    NEEDS_EXTENSION_API, WORKERS_MAX,
};
pub use model::{AgentModel, StepClock};
pub use readout::{k_tokens, round_div_half_even, CacheTotals, GoalState, Readout};
pub use tab::{AgentKey, Changes, StreamStatus, TabModel};
pub use todos::{todo_status_from_str, todos_from_json};

impl LaneStatus {
    /// The state vocabulary of a lane (`swarm/state.lisp`: `:starting :idle :working
    /// :compacting :down :stopped`), as `GET /lanes` and `lane-state` report it.
    /// Anything unknown reads as down — the swarm's own glyph function does the same.
    pub fn from_state(state: &str) -> LaneStatus {
        match state {
            "working" => LaneStatus::Working,
            "compacting" => LaneStatus::Compacting,
            "idle" => LaneStatus::Idle,
            "starting" => LaneStatus::Starting,
            _ => LaneStatus::Down,
        }
    }

    /// The left-column icon (§7.3), the swarm TUI's own glyphs.
    pub fn glyph(self) -> char {
        match self {
            LaneStatus::Working => '●',
            LaneStatus::Compacting => '◐',
            LaneStatus::Idle => '○',
            LaneStatus::Starting => '◌',
            LaneStatus::Down => '✗',
        }
    }
}

impl TodoStatus {
    /// The todo panel's glyph: ☑ done, ◐ in progress, ☐ pending (`core-ext/todo.lisp`).
    pub fn glyph(self) -> char {
        match self {
            TodoStatus::Done => '☑',
            TodoStatus::InProgress => '◐',
            TodoStatus::Pending => '☐',
        }
    }
}
