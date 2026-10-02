//! The lane list: topic `swarm`, one row per lane (CONTRACT §4.3).
//!
//! This is the read-only window the left column draws. The swarm topic carries every lane
//! transition — `starting → idle` included — and an absolute step start, so a row's clock
//! is arithmetic rather than a stamp ([`LaneRow::step_clock`]). A row is its glyph, its
//! number, the task it was given and its state: what a lane is *doing* right now is its
//! transcript's business, and a lane's own topic adds nothing to the row but the newest
//! item it landed ([`LaneRow::last_item`]), which is a tooltip's.

use crate::{k_tokens, short_duration, SwarmLane, SwarmState};

/// Lane list glyphs (§7.3): ● working, ◐ compacting, ○ idle, ◌ starting, ✗ down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneStatus {
    Working,
    Compacting,
    Idle,
    Starting,
    Down,
    Stopped,
}

impl LaneStatus {
    /// The swarm's own vocabulary (`starting :idle :working :compacting :down :stopped`).
    /// Anything unknown reads as down — the swarm's own glyph function does the same.
    pub fn from_state(state: &str) -> LaneStatus {
        match state {
            "working" => LaneStatus::Working,
            "compacting" => LaneStatus::Compacting,
            "idle" => LaneStatus::Idle,
            "starting" => LaneStatus::Starting,
            "stopped" => LaneStatus::Stopped,
            _ => LaneStatus::Down,
        }
    }

    /// The left-column icon.
    pub fn glyph(self) -> char {
        match self {
            LaneStatus::Working => '●',
            LaneStatus::Compacting => '◐',
            LaneStatus::Idle => '○',
            LaneStatus::Starting => '◌',
            LaneStatus::Down => '✗',
            LaneStatus::Stopped => '◇',
        }
    }

    /// Whether the lane is doing something (● or ◐).
    pub fn is_busy(self) -> bool {
        matches!(self, LaneStatus::Working | LaneStatus::Compacting)
    }

    /// The word a row's trailing cell shows: what the lane is, in the swarm's own
    /// vocabulary — the same words the state arrived in.
    pub fn word(self) -> &'static str {
        match self {
            LaneStatus::Working => "working",
            LaneStatus::Compacting => "compacting",
            LaneStatus::Idle => "idle",
            LaneStatus::Starting => "starting",
            LaneStatus::Stopped => "stopped",
            LaneStatus::Down => "down",
        }
    }
}

/// The swarm itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwarmInfo {
    pub id: String,
    pub workers: u64,
    /// How many lanes are working or compacting, as the swarm counted them.
    pub busy: u64,
    /// The coordinator is settled but held while its lanes work.
    pub waiting_on_lanes: bool,
    pub lane_model: Option<String>,
    pub lane_thinking: Option<String>,
}

/// One lane's row.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneRow {
    pub n: u32,
    pub status: LaneStatus,
    /// The raw state string, for a tooltip: the vocabulary stays the swarm's.
    pub state: String,
    /// The task the lane was last given, which the swarm still reports while it is idle.
    pub task: Option<String>,
    pub task_started_at: Option<u64>,
    /// When the lane's current step (one turn, or one compaction) began, epoch ms.
    pub step_started_at: Option<u64>,
    pub restarts: u64,
    pub pid: Option<u64>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub goal_status: Option<String>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub reports: u64,
    /// The newest thing the lane did, as the swarm summarizes it — or, when the tab holds
    /// that lane's mirror, as the lane's own newest item does. A tooltip carries it.
    pub last_item: Option<(String, String)>,
}

impl LaneRow {
    /// The left-column icon.
    pub fn glyph(&self) -> char {
        self.status.glyph()
    }

    pub fn is_busy(&self) -> bool {
        self.status.is_busy()
    }

    /// The step clock the left column shows while the lane works, from the absolute start
    /// the swarm published. `None` unless the lane is working or compacting: an idle lane
    /// has no step to count.
    pub fn step_clock(&self, now_millis: u64) -> Option<String> {
        if !self.is_busy() {
            return None;
        }
        let started = self.step_started_at.or(self.task_started_at)?;
        Some(short_duration(now_millis.saturating_sub(started) / 1000))
    }

    /// The task on one line, for the row's middle cell: what this lane was told to do.
    ///
    /// `None` when the swarm has not been told one — a lane that has only just started —
    /// which is an empty cell rather than the lane's state said twice.
    pub fn task_label(&self) -> Option<String> {
        self.task
            .as_deref()
            .map(crate::lane_task_label)
            .filter(|task| !task.is_empty())
    }

    /// The context figure, when the swarm reports one.
    pub fn context_label(&self) -> Option<String> {
        let tokens = self.context_tokens?;
        Some(match self.context_window {
            Some(window) if window > 0 => {
                format!("{}/{}", k_tokens(tokens), k_tokens(window))
            }
            _ => k_tokens(tokens),
        })
    }

    /// The lane as the swarm's own list writes it (`swarm/tools.lisp`'s
    /// `lane-status-line`), for a tooltip, plus what only the new topic carries.
    ///
    /// The name is the one the column's rows and the band call the lane — `Lane 2` — not
    /// the swarm's own lower-case spelling of it.
    pub fn tooltip(&self, now_millis: u64) -> String {
        let mut line = format!("Lane {}  {}", self.n, self.state.to_lowercase());
        if let Some(clock) = self.step_clock(now_millis) {
            line.push_str(&format!(" · step {clock}"));
        }
        if let Some(task) = self.task_label() {
            line.push_str(&format!(" · task: {task}"));
        }
        if let Some(worktree) = self.worktree.as_deref() {
            line.push_str(&format!(" · worktree {worktree}"));
        }
        if let Some(branch) = self.branch.as_deref() {
            line.push_str(&format!(" ({branch})"));
        }
        line.push_str(&format!(
            " · {} {}",
            self.reports,
            crate::plural(self.reports, "report")
        ));
        if self.restarts > 0 {
            line.push_str(&format!(
                " · {} {}",
                self.restarts,
                crate::plural(self.restarts, "restart")
            ));
        }
        if let Some(pid) = self.pid {
            line.push_str(&format!(" · pid {pid}"));
        }
        if let Some(goal) = self.goal_status.as_deref() {
            line.push_str(&format!(" · goal {goal}"));
        }
        if let Some(context) = self.context_label() {
            line.push_str(&format!(" · ctx {context}"));
        }
        line
    }
}

/// The tab's lane list: `main` (the coordinator) is not in it — the tab draws that row
/// itself — so this is exactly the lanes the swarm reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneList {
    pub swarm: Option<SwarmInfo>,
    pub lanes: Vec<LaneRow>,
}

impl LaneList {
    pub fn new() -> LaneList {
        LaneList::default()
    }

    pub fn lane(&self, n: u32) -> Option<&LaneRow> {
        self.lanes.iter().find(|lane| lane.n == n)
    }

    /// How many lanes are working or compacting — counted from the rows, so a header
    /// cannot outlive the state it summarizes.
    pub fn busy(&self) -> u64 {
        self.lanes.iter().filter(|lane| lane.is_busy()).count() as u64
    }

    /// Whether the swarm is doing anything at all: the action button's face.
    pub fn is_busy(&self) -> bool {
        self.swarm
            .as_ref()
            .is_some_and(|swarm| swarm.busy > 0 || swarm.waiting_on_lanes)
            || self.busy() > 0
    }
}

/// Build the list from the swarm topic, with each lane's own newest item where the tab
/// holds that lane's mirror — the lane's own topic knows the item before the swarm's
/// summary of it does.
pub fn lane_list(
    state: &SwarmState,
    last_item: impl Fn(u32) -> Option<(String, String)>,
) -> LaneList {
    let lanes = state
        .lanes
        .iter()
        .map(|lane| lane_row(lane, last_item(lane.n)))
        .collect();
    LaneList {
        swarm: Some(SwarmInfo {
            id: state.id.clone(),
            workers: state.workers,
            busy: state.busy,
            waiting_on_lanes: state.waiting_on_lanes,
            lane_model: state.lane_model.as_ref().map(|model| model.label()),
            lane_thinking: state.lane_thinking.clone(),
        }),
        lanes,
    }
}

fn lane_row(lane: &SwarmLane, last: Option<(String, String)>) -> LaneRow {
    LaneRow {
        n: lane.n,
        status: LaneStatus::from_state(&lane.state),
        state: lane.state.clone(),
        task: lane.task.clone(),
        task_started_at: lane.task_started_at,
        step_started_at: lane.step_started_at,
        restarts: lane.restarts,
        pid: lane.pid,
        worktree: lane.worktree.clone(),
        branch: lane.branch.clone(),
        model: lane.model.as_ref().map(|model| model.label()),
        goal_status: lane.goal.as_ref().map(|goal| goal.status.clone()),
        context_tokens: lane.context.as_ref().map(|context| context.tokens),
        context_window: lane.context.as_ref().and_then(|context| context.window),
        reports: lane.reports,
        // The lane's own mirror is the fresher source: the item it just saw land, and
        // only then the swarm's own summary of the last one.
        last_item: last.or_else(|| {
            lane.last_item
                .as_ref()
                .map(|item| (item.kind.clone(), item.summary.clone()))
        }),
    }
}
