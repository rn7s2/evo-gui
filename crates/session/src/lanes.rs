//! The lane list: `GET /lanes` plus the coordinator stream's `lane-state` events.
//!
//! A lane's own serve stays authoritative for that lane (`swarm/api.lisp`); this is the
//! read-only window the tab renders in the left column — `main` first, then one row per
//! lane with its status icon, its current task and its step clock. `lane-state` is
//! published only when a lane's state, task or goal status changed
//! (`maybe-publish-lane-state`), so it is cheap to follow every lane on the one
//! coordinator stream.

use serde_json::Value;

use crate::readout::{optional_u64, string_field, u64_field};
use crate::LaneStatus;

/// The swarm itself (`GET /lanes`'s `swarm` object).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwarmInfo {
    pub id: String,
    pub dir: String,
    pub cwd: String,
    pub workers: u64,
    /// How many lanes are working or compacting, as `/lanes` counted them.
    ///
    /// Kept faithful to the payload, but not what the header shows: the count moves
    /// on with every lane-state event, and this one is a moment. [`LaneList::busy`]
    /// counts the rows instead.
    pub busy: u64,
    pub stopping: bool,
}

/// One lane's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneRow {
    pub n: u64,
    pub status: LaneStatus,
    /// The raw state string (`working`, `compacting`, `idle`, `starting`, `down`,
    /// `stopped`), for a tooltip — the vocabulary stays the swarm's.
    pub state: String,
    /// The task the lane was last given, which is also what `/lanes` still reports
    /// while the lane is idle.
    pub task: Option<String>,
    /// Seconds since the task started, or `None` when the lane has never had one.
    /// Only `GET /lanes` reports the clocks; a `lane-state` event carries neither.
    pub task_age: Option<u64>,
    /// Seconds in the lane's current *step* (one turn, or one compaction) — how a slow
    /// step is told from a wedged one.
    pub step_age: Option<u64>,
    pub pid: Option<u64>,
    /// Set while the lane is isolated in a git worktree.
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub restarts: u64,
    pub reports: u64,
    /// The lane's goal *status* (`active`, `complete`, …), which is all the lane list
    /// shows of it.
    pub goal_status: Option<String>,
}

impl LaneRow {
    /// The left-column icon.
    pub fn glyph(&self) -> char {
        self.status.glyph()
    }

    /// Whether the lane is doing something (● or ◐), the state the tab counts as busy.
    pub fn is_busy(&self) -> bool {
        matches!(self.status, LaneStatus::Working | LaneStatus::Compacting)
    }

    /// The step clock the left column shows while the lane works, as the swarm's own
    /// `lane-status-line` writes it (`swarm/tools.lisp`): `45s`, `3m`, `1h2m`.
    ///
    /// `None` unless the lane is working or compacting: the swarm reports a step age
    /// only for those states (`swarm/state.lisp`'s `lane-snapshot` puts
    /// `:step-age (and … (member (lane-state lane) '(:working :compacting)) …)`), so a
    /// row that kept one after an event said the lane went idle would be showing a
    /// clock the swarm itself has stopped.
    pub fn step_clock(&self) -> Option<String> {
        self.is_busy().then(|| self.step_age.map(short_duration)).flatten()
    }

    /// The task as the left column shows it: one line, truncated. The swarm truncates the
    /// same way in `lane-status-line` (`swarm/tools.lisp`).
    pub fn task_label(&self) -> Option<String> {
        self.task.as_deref().map(lane_task_label)
    }

    /// The lane as the swarm's own list writes it (`swarm/tools.lisp`'s
    /// `lane-status-line`), for the row's tooltip:
    ///
    /// ```text
    /// lane 1  working · step 0s · task: DELAY3 CALL todo … · 0 reports · pid 87897
    /// ```
    ///
    /// Field for field the swarm's string — the step clock only when the lane has one (so
    /// only while it works), the task truncated to 60 characters, worktree with the branch
    /// in parentheses, the report count always, restarts only when there were any, the pid
    /// when the lane has one — plus the goal status the lane list carries and
    /// `lane-status-line` does not.
    pub fn tooltip(&self) -> String {
        let mut line = format!("lane {}  {}", self.n, self.state.to_lowercase());
        if let Some(clock) = self.step_clock() {
            line.push_str(&format!(" · step {}", clock));
        }
        if let Some(task) = self.task_label() {
            line.push_str(&format!(" · task: {}", task));
        }
        if let Some(worktree) = self.worktree.as_deref() {
            line.push_str(&format!(" · worktree {}", worktree));
        }
        if let Some(branch) = self.branch.as_deref() {
            line.push_str(&format!(" ({})", branch));
        }
        line.push_str(&format!(" · {} {}", self.reports, plural(self.reports, "report")));
        if self.restarts > 0 {
            line.push_str(&format!(" · {} {}", self.restarts, plural(self.restarts, "restart")));
        }
        if let Some(pid) = self.pid {
            line.push_str(&format!(" · pid {}", pid));
        }
        if let Some(goal) = self.goal_status.as_deref() {
            line.push_str(&format!(" · goal {}", goal));
        }
        line
    }
}

/// `~:p`: the plural `s` for anything but one — `0 reports`, `1 report`.
fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{}s", word)
    }
}

/// A compact elapsed clock: `45s`, `3m`, `1h2m` — `evo.tui:short-duration`
/// (`src/tui/tui.lisp`), which the swarm reuses for a lane's step age
/// (`swarm/tools.lisp`'s `lane-status-line`).
pub fn short_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// A lane's task on one line, at most 60 characters: `(truncate-string (substitute #\Space
/// #\Newline task) 60 "…")`, exactly what `lane-status-line` puts in a lane row
/// (`swarm/tools.lisp`). A longer task keeps its first 60 characters and gains the
/// ellipsis. Only `\n` becomes a space — `substitute` replaces that one character and
/// leaves a `\r` alone, and this follows it rather than quietly differing.
pub fn lane_task_label(task: &str) -> String {
    const CHARS: usize = 60;
    let one_line: String = task.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() <= CHARS {
        return one_line;
    }
    let mut label: String = one_line.chars().take(CHARS).collect();
    label.push('…');
    label
}

/// The tab's lane list: `main` (the coordinator) is not in it — the tab draws that row
/// itself from `/state` — so this is exactly the lanes the swarm reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneList {
    pub swarm: Option<SwarmInfo>,
    pub lanes: Vec<LaneRow>,
}

impl LaneList {
    pub fn new() -> LaneList {
        LaneList::default()
    }

    /// The whole list from a `GET /lanes` body, replacing whatever was there.
    pub fn apply_lanes(&mut self, body: &Value) -> bool {
        let next = LaneList::from_lanes(body);
        if *self == next {
            false
        } else {
            *self = next;
            true
        }
    }

    /// A `GET /lanes` body as a list.
    pub fn from_lanes(body: &Value) -> LaneList {
        let swarm = body.get("swarm").and_then(swarm_info);
        let lanes = body
            .get("lanes")
            .and_then(Value::as_array)
            .map(|lanes| lanes.iter().map(lane_row).collect())
            .unwrap_or_default();
        LaneList { swarm, lanes }
    }

    /// One `lane-state` event (coordinator stream): the lane's state, task, goal status,
    /// restarts and pid — no clocks, which only `/lanes` reports. Returns whether the
    /// list changed; a lane the list has never seen is added (which is itself a change,
    /// even when the event's values are the ones a fresh row starts with).
    pub fn apply_lane_state(&mut self, data: &Value) -> bool {
        let Some(n) = optional_u64(data, "lane") else {
            return false;
        };
        let mut inserted = false;
        let row = match self.lanes.iter_mut().find(|row| row.n == n) {
            Some(row) => row,
            None => {
                // A lane we have not seen in a `GET /lanes` yet: it starts where the
                // swarm starts one, in `:starting`.
                self.lanes.push(LaneRow {
                    n,
                    status: LaneStatus::Starting,
                    state: "starting".to_string(),
                    task: None,
                    task_age: None,
                    step_age: None,
                    pid: None,
                    worktree: None,
                    branch: None,
                    restarts: 0,
                    reports: 0,
                    goal_status: None,
                });
                self.lanes.sort_by_key(|row| row.n);
                inserted = true;
                self.lanes.iter_mut().find(|row| row.n == n).expect("just inserted")
            }
        };
        let before = row.clone();
        if let Some(state) = string_field(data, "state") {
            row.status = LaneStatus::from_state(&state);
            row.state = state;
        }
        // Faithful to the event: the swarm reports the lane's stored task and pid, and
        // a null goal means the lane has none (or has not reported one yet).
        row.task = string_field(data, "task");
        row.restarts = u64_field(data, "restarts");
        row.pid = optional_u64(data, "pid");
        row.goal_status = goal_status(data.get("goal"));
        inserted || *row != before
    }

    pub fn lane(&self, n: u64) -> Option<&LaneRow> {
        self.lanes.iter().find(|row| row.n == n)
    }

    /// How many lanes are working or compacting — counted from the rows, which is
    /// what the header sits above and labels.
    ///
    /// Deliberately not `swarm.busy`: that is the same fact, but as of the last
    /// `GET /lanes`, and a lane's state moves on the stream — a `lane-state` event,
    /// or a lane's own `settled` — with no read behind it. Counting the snapshot's
    /// number there is what let the header outlive its lanes: the capture in
    /// `docs/screens/03-lane-todos-dark.png` says `2 lanes · 1 busy` over two idle
    /// rows, the count left over from a read taken while lane 1 was working. The
    /// rows are the fresher of the two (they take the events as well as the reads),
    /// so a header that disagrees with them can only be wrong.
    pub fn busy(&self) -> u64 {
        self.lanes.iter().filter(|row| row.is_busy()).count() as u64
    }
}

fn swarm_info(value: &Value) -> Option<SwarmInfo> {
    if !value.is_object() {
        return None;
    }
    Some(SwarmInfo {
        id: string_field(value, "id").unwrap_or_default(),
        dir: string_field(value, "dir").unwrap_or_default(),
        cwd: string_field(value, "cwd").unwrap_or_default(),
        workers: u64_field(value, "workers"),
        busy: u64_field(value, "busy"),
        stopping: value.get("stopping").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn lane_row(value: &Value) -> LaneRow {
    let state = string_field(value, "state").unwrap_or_default();
    LaneRow {
        n: u64_field(value, "n"),
        status: LaneStatus::from_state(&state),
        state,
        task: string_field(value, "task"),
        task_age: optional_u64(value, "task_age"),
        step_age: optional_u64(value, "step_age"),
        pid: optional_u64(value, "pid"),
        worktree: string_field(value, "worktree"),
        branch: string_field(value, "branch"),
        restarts: u64_field(value, "restarts"),
        reports: u64_field(value, "reports"),
        goal_status: goal_status(value.get("goal")),
    }
}

/// A lane's goal field: `/lanes` carries the lane's cached goal plist, a `lane-state`
/// event carries the goal's status string alone. Both reduce to the status.
fn goal_status(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(status)) => Some(status.clone()),
        Some(goal) => string_field(goal, "status"),
        None => None,
    }
}
