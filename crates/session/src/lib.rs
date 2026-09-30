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
    /// The reader's own words, shown from the moment they send them — before evo
    /// has taken them.
    ///
    /// A `POST /prompt` issued while a run is in flight is only *queued* by the
    /// server (its reply says `"queued": true`) and evo says nothing until it
    /// drains the queue at the running turn's next boundary
    /// (`src/kernel/loop.lisp`'s `drain-steering`), which is after the current
    /// model response and all of its tool calls. Without this row the reader's
    /// message is simply absent from the transcript until then — the tab looks as
    /// if it had dropped it.
    ///
    /// The row is the reader's echo and nothing else: it is not a turn yet, so it
    /// opens no turn boundary and no `user-input`/`steering` event carries it. The
    /// event that does carry the text promotes it into a [`RowKind::User`] row,
    /// which may sit later in the transcript than the pending row did — evo
    /// inserts the turn after the assistant message and tool calls of the turn
    /// that was running. See [`AgentModel::push_pending_user`].
    ///
    /// [`AgentModel::push_pending_user`]: crate::AgentModel::push_pending_user
    PendingUser { text: String },
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
    /// A nudge evo steered into its own agent because a goal outlived the run:
    /// `goal-continuation-message` — "You are idle but your goal is still active.
    /// Continue working toward it now." — when a run settles with the goal unfinished
    /// (`src/kernel/goal.lisp:69`), and `goal-wrapup-message` — "Your goal's token
    /// budget is exhausted (…). Do not start new work." — when the budget is spent
    /// (:106). `queue-steering` (:149, :153) queues them like any other input, with no
    /// `meta` key and no event of their own, so the fixed opening sentence is what
    /// marks them: a reader never types it, and it is evo talking, not a turn of
    /// theirs.
    GoalNudge {
        kind: GoalNudgeKind,
        /// The goal's objective: the body of the continuation's `<goal objective=…>`
        /// block (`goal.lisp:73`), or the tail of the wrap-up's last line.
        objective: String,
        /// The budget as the message states it — `12,345 tokens used of 50,000 (37,655
        /// remaining)`, `0 tokens used (no limit)`, `(45,001 used of 45,000)` — without
        /// the punctuation that puts it in the sentence.
        budget: String,
        /// The whole message, which is what an opened row shows.
        text: String,
    },
    /// A command the reader ran, answered with instructions for the agent: an
    /// extension steers in what the agent should do about it — `/memory <query>` and
    /// `/global-memory <query>` ("The user invoked `/global-memory` with an intention
    /// or query about global memory…", `src/core-ext/memory.lisp:242`), `/lore <text>`
    /// ("The user added global-lore (durable guidance, applies from now on): …",
    /// `src/command/command.lisp:397`), and any extension that opens with the same
    /// phrasing (`extensions/360-baby-evo.lisp:844`'s `/notify doctor`).
    ///
    /// The reader typed the command, not this: the message is what the extension wants
    /// the agent to do, so its row names the command that caused it and keeps the words
    /// for a reader who wants to see them. No `meta` key marks these either — the
    /// phrase each format opens with is all there is.
    CommandNote {
        /// The command the reader ran, as the message names it: `/global-memory`,
        /// `/lore`, `/notify doctor`.
        command: String,
        /// The whole message.
        text: String,
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
    /// asked emits no row — the turn boundary already says it finished — `run-start`
    /// never emits one at all, and neither does a failure the failing message already
    /// carries: `message-end`'s own `error` is that message's row, and the tab would
    /// otherwise say the same sentence twice.
    RunOutcome { outcome: String, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub is_error: bool,
    pub content: String,
    pub content_chars: Option<u64>,
}

/// Which of evo's goal nudges a row is (`src/kernel/goal.lisp`, and the `/goal` command
/// that can change an objective mid-run, `src/command/command.lisp:227`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalNudgeKind {
    /// The goal is still active and the run settled: keep going (:69).
    Continue,
    /// The goal's token budget is spent: wrap up and summarize (:106).
    Wrapup,
    /// The reader gave a running goal a new objective (`command.lisp:227`).
    Updated,
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
