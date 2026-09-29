//! The whole tab as one value: the coordinator's view model, the lane list, one view model
//! per watched lane, the selection and the stream badges — so the gpui side stays a thin
//! shell that reads what it renders and does what [`Changes`] says.
//!
//! # Where things come from
//!
//! Nothing here does I/O. The tab's I/O layer (another crate) hands over raw JSON pieces —
//! `GET /transcript`, `GET /state`, `GET /registry`, `GET /lanes`, each SSE event, the
//! journal seed, and stream status — and every entry point returns the [`Changes`] the UI
//! must apply. That keeps "which rows changed" a question this crate answers once instead
//! of each frontend guessing.
//!
//! ```text
//! on_state(&state)            → readout / activity / todos of the coordinator
//! on_registry(&registry)      → readout (the model label)
//! on_transcript(agent, rev, &transcript) → rebuilt rows of that agent
//! on_event(agent, id, kind, &data)       → rows / todos / lanes / step / needs_resync
//! on_event_at(agent, id, kind, &data, now_millis)  → the same, with the arrival time the
//!                                                    step clock counts from
//! on_lanes(&lanes)            → the lane list
//! on_cache_seed(Option<&seed>)→ readout
//! on_stream(agent, status)    → the stream badge
//! select(agent)               → the selection and the center column
//! ```
//!
//! # Which agent is which
//!
//! The coordinator's model holds the transcript, the checklist, the activity and the §7.3
//! readout the center pane's status row shows while `main` is selected; a lane's model
//! holds what its own stream carries (rows, its checklist from `todo-changed`, `report`
//! rows) and a readout of its own, built from what the API carries for a lane — its
//! transcript's model, provider and usage, and the window its model has in `/registry`
//! ([`TabModel::selected_readout_text`]). The activity is the **coordinator's** only —
//! lanes are watched, never typed to (spec §14.4) — so a lane's events never raise
//! [`Changes::activity`]; they raise [`Changes::readout`] only for the lane the center
//! pane is showing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::Value;

use crate::cache::cache_totals_from_seed;
use crate::{
    Activity, AgentModel, DimStyle, LaneList, LaneRow, LaneStatus, Readout, Row, RowChanges,
    RowKind, StepClock, Todo,
};

/// One agent of a tab: the coordinator, or lane N.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentKey {
    Coordinator,
    Lane(u32),
}

impl AgentKey {
    /// The lane number, when this is a lane.
    pub fn lane(self) -> Option<u32> {
        match self {
            AgentKey::Lane(n) => Some(n),
            AgentKey::Coordinator => None,
        }
    }
}

/// The state of one agent's event stream, for the "reconnecting" badge (§9.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamStatus {
    /// The stream is up (the default: no badge).
    #[default]
    Connected,
    /// The stream dropped and is retrying with `Last-Event-ID` after a backoff.
    Reconnecting { retry_in: Duration },
}

impl StreamStatus {
    pub fn is_reconnecting(self) -> bool {
        matches!(self, StreamStatus::Reconnecting { .. })
    }
}

/// What the UI has to re-set after one update. Everything the tab did not touch is left
/// alone, so a text delta re-renders one row instead of the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Rows that changed, per agent: only those views are stale. [`RowChanges::Rebuilt`]
    /// means the whole row set is new (the ids are a fresh space) — the case a transcript
    /// resync, a restarted coordinator and a selection switch all produce.
    ///
    /// [`RowChanges::Rebuilt`]: crate::RowChanges::Rebuilt
    pub rows: BTreeMap<AgentKey, RowChanges>,
    /// These agents' todo panels changed.
    pub todos: BTreeSet<AgentKey>,
    /// The lane list changed.
    pub lanes: bool,
    /// The §7.3 readout text the UI shows changed — the selected agent's
    /// ([`TabModel::selected_readout_text`]), which is the coordinator's while `main` is
    /// selected.
    pub readout: bool,
    /// The coordinator's activity changed (the Send/Stop button's face).
    pub activity: bool,
    /// These agents' stream badges changed.
    pub stream: BTreeSet<AgentKey>,
    /// These agents' step clock changed: a step began, or the run ended (read
    /// [`TabModel::coordinator_step_started`] for the coordinator's).
    pub step: BTreeSet<AgentKey>,
    /// The selection changed: the center column shows a different agent.
    pub selection: bool,
    /// These agents must be refetched (`/state` + `/transcript`, or the lane's transcript):
    /// `settled`, `gap`, `hello`, `session-switched`, or a reset.
    pub needs_resync: BTreeSet<AgentKey>,
}

impl Changes {
    /// Nothing to do — the update carried no news for the UI.
    pub fn is_empty(&self) -> bool {
        self == &Changes::default()
    }

    /// The row changes for one agent, if any.
    pub fn rows_for(&self, agent: AgentKey) -> Option<&RowChanges> {
        self.rows.get(&agent)
    }
}

/// One tab: the coordinator, the swarm's lanes, one model per watched lane, the selection
/// and the stream badges.
#[derive(Clone, Debug)]
pub struct TabModel {
    coordinator: AgentModel,
    lanes: LaneList,
    /// Lane number → its model. Created the first time a lane is selected or an update
    /// arrives for it (a lane's stream is only open while it is watched, so an update *is*
    /// the watch).
    lane_models: BTreeMap<u32, AgentModel>,
    /// Lane number → the swarm's last account of it going down (`[lane N] crashed and
    /// was restarted by its supervisor…`, `[lane N] is down: its process exited…`,
    /// `[lane N] failed to start…`), kept here rather than read back out of the rows: a
    /// `settled` resync rebuilds the coordinator's rows from `/transcript`, which carries
    /// no `output` lines, and the red row's reason would be lost with them.
    lane_announcements: BTreeMap<u32, String>,
    selected: AgentKey,
    streams: BTreeMap<AgentKey, StreamStatus>,
    /// The newest `GET /registry` body. A lane's model is created by a selection or by the
    /// lane's first update — after this was read — and its readout needs the catalog to
    /// name the window its model runs with, so it is kept and applied to each lane model as
    /// it is born ([`TabModel::model_mut`]).
    registry: Option<Value>,
    /// The last transcript revision applied per agent — a late answer from an older fetch
    /// is dropped (§9.1). Cleared for an agent when `hello` says the server restarted:
    /// past that point the two numbering schemes are not comparable.
    transcript_revisions: BTreeMap<AgentKey, u64>,
}

impl Default for TabModel {
    fn default() -> Self {
        TabModel::new()
    }
}

impl TabModel {
    pub fn new() -> TabModel {
        TabModel {
            coordinator: AgentModel::new(),
            lanes: LaneList::new(),
            lane_models: BTreeMap::new(),
            lane_announcements: BTreeMap::new(),
            selected: AgentKey::Coordinator,
            streams: BTreeMap::new(),
            registry: None,
            transcript_revisions: BTreeMap::new(),
        }
    }

    // --- reading ---------------------------------------------------------

    pub fn coordinator(&self) -> &AgentModel {
        &self.coordinator
    }

    pub fn lanes(&self) -> &LaneList {
        &self.lanes
    }

    pub fn selected(&self) -> AgentKey {
        self.selected
    }

    /// The model of one agent, or `None` for a lane that has never been selected or
    /// watched (its transcript has not been fetched, so there is nothing to show yet).
    pub fn agent_model(&self, agent: AgentKey) -> Option<&AgentModel> {
        match agent {
            AgentKey::Coordinator => Some(&self.coordinator),
            AgentKey::Lane(n) => self.lane_models.get(&n),
        }
    }

    pub fn lane_model(&self, n: u32) -> Option<&AgentModel> {
        self.lane_models.get(&n)
    }

    pub fn selected_rows(&self) -> &[Row] {
        self.agent_model(self.selected)
            .map(|model| model.rows())
            .unwrap_or(&[])
    }

    pub fn selected_todos(&self) -> &[Todo] {
        self.agent_model(self.selected)
            .map(|model| model.todos())
            .unwrap_or(&[])
    }

    /// The coordinator's readout — the §7.3 status line.
    pub fn readout(&self) -> &Readout {
        self.coordinator.readout()
    }

    pub fn readout_text(&self) -> String {
        self.coordinator.readout().text()
    }

    /// The line the center pane's status row shows: the **selected** agent's §7.3
    /// readout — the coordinator's on `main`, the lane's own when a lane is selected —
    /// or `None` while nothing about that agent is known yet (a lane selected before
    /// anything about it was read), which the UI writes as its own muted "no metrics
    /// yet".
    ///
    /// A lane's line is made of what the swarm's API carries for it: the model and
    /// provider its transcript's assistant messages name, the context figure and the
    /// cache share of the usage they report, and the window its model runs with in
    /// `/registry` ([`Readout::seed_from_transcript`]). A lane has no `/state` of its own
    /// (the tab talks to the coordinator, §14.4), so the two segments that live only there
    /// — the thinking level and the goal — have nothing to read and are left out
    /// (`docs/api-gaps.md`, "a lane has no state endpoint").
    pub fn selected_readout_text(&self) -> Option<String> {
        let readout = self.agent_model(self.selected)?.readout();
        readout.known().then(|| readout.text())
    }

    /// The coordinator's activity, for the Send/Stop button's face.
    pub fn activity(&self) -> Activity {
        self.coordinator.activity()
    }

    /// The coordinator's model id and provider, for the tab's tooltip.
    pub fn coordinator_model(&self) -> Option<&str> {
        self.coordinator.readout().model_id()
    }

    pub fn coordinator_provider(&self) -> Option<&str> {
        self.coordinator.readout().provider()
    }

    pub fn stream_status(&self, agent: AgentKey) -> StreamStatus {
        self.streams.get(&agent).copied().unwrap_or_default()
    }

    pub fn is_reconnecting(&self, agent: AgentKey) -> bool {
        self.stream_status(agent).is_reconnecting()
    }

    /// The coordinator's step in flight, for a live step clock: the turn, the event that
    /// began it, and the arrival time when the caller stamped one
    /// ([`TabModel::on_event_at`]). `None` while nothing runs.
    pub fn coordinator_step_started(&self) -> Option<StepClock> {
        self.coordinator.step_started()
    }

    /// Why lane N is down, for the `✗` row's tooltip — the §9.7 "reason from the
    /// transcript's `output` lines".
    ///
    /// **Only a lane the list shows as down has a reason.** A lane that is back up
    /// (idle, working, starting) has none: whatever it said while it was being
    /// brought back is not current, and a tooltip over a working lane that claims it
    /// is down would be a lie.
    ///
    /// The reason itself is, in order:
    ///
    /// 1. the swarm's own account of the lane going down — `[lane N] crashed and was
    ///    restarted by its supervisor…`, `[lane N] is down: its process exited…`,
    ///    `[lane N] failed to start…` (`swarm/lanes.lisp`). It is the *most recent
    ///    such* line, because a lane that has just been re-initialized complains
    ///    about its own fresh state (no model registered yet, an extension it has
    ///    not loaded) and those lines are symptoms of the restart, not the reason
    ///    for the red row;
    /// 2. failing that, the lane's own transcript: what it said went wrong (a model
    ///    it cannot register, an internal error);
    /// 3. failing that, the newest error line about that lane on the coordinator's
    ///    stream.
    pub fn lane_down_reason(&self, n: u32) -> Option<String> {
        if self.lanes.lane(u64::from(n)).map(|row| row.status) != Some(LaneStatus::Down) {
            return None;
        }
        if let Some(announcement) = self.lane_announcements.get(&n) {
            return Some(announcement.clone());
        }
        let about = format!("[lane {}]", n);
        if let Some(reason) = self.lane_models.get(&n).and_then(last_error_line) {
            return Some(reason);
        }
        last_error_line_where(&self.coordinator, |text| text.contains(&about))
    }

    /// The lane rows as the left column draws them.
    pub fn lane_rows(&self) -> &[LaneRow] {
        &self.lanes.lanes
    }

    // --- selecting -------------------------------------------------------

    /// Show AGENT's transcript in the center column. A lane's model is created here (the
    /// first thing a selection does is give the lane somewhere to land its transcript, and
    /// with it the catalog its readout needs).
    pub fn select(&mut self, agent: AgentKey) -> Changes {
        let mut changes = Changes::default();
        self.model_mut(agent);
        if self.selected != agent {
            self.selected = agent;
            changes.selection = true;
            // The center column is a different agent now: whatever row views it held
            // belong to the other one, so the row set is new from the UI's point of view.
            changes.rows.insert(agent, RowChanges::Rebuilt);
            // ...and so is the status line under it, which shows the selected agent's
            // readout ([`TabModel::selected_readout_text`]).
            changes.readout = true;
        }
        changes
    }

    // --- updates from the I/O layer --------------------------------------

    /// `GET /transcript` for AGENT, stamped with the revision of the fetch it answered. Only
    /// a strictly newer revision is applied: an answer older than the last one is a refetch
    /// that overtook it, and an equal one is the same answer twice, so neither changes
    /// anything (the tab's I/O layer numbers these monotonically per agent).
    pub fn on_transcript(&mut self, agent: AgentKey, revision: u64, transcript: &Value) -> Changes {
        if self
            .transcript_revisions
            .get(&agent)
            .is_some_and(|last| revision <= *last)
        {
            return Changes::default();
        }
        self.transcript_revisions.insert(agent, revision);
        let mut effect = self.model_mut(agent).rebuild_from_transcript(transcript);
        // A lane's readout has no `/state` behind it (the tab talks to the coordinator,
        // §14.4), so its transcript is the seed: the model its assistant messages name and
        // the usage they report. The coordinator's readout is `/state`'s and the journal's
        // — rebuilding it from a transcript must not move it, and does not (the seed fills
        // only what is still unknown).
        if agent != AgentKey::Coordinator {
            let model = self.model_mut(agent);
            let before = model.readout().text();
            model.readout_mut().seed_from_transcript(transcript);
            if model.readout().text() != before {
                effect |= crate::Effect::READOUT;
            }
        }
        self.absorb(agent, effect)
    }

    /// `GET /state` — the coordinator's (`/state` is only ever the coordinator's: the app
    /// talks to the coordinator, and lane status comes from `/lanes`).
    pub fn on_state(&mut self, state: &Value) -> Changes {
        let effect = self.coordinator.apply_state(state);
        self.absorb(AgentKey::Coordinator, effect)
    }

    /// `GET /registry` — the model catalog, which the readout needs to tell an unambiguous
    /// model id from one registered under several providers, and to name the window of a
    /// model whose `/state` it never saw. Every agent's readout takes it: the catalog is
    /// the same for the whole swarm.
    pub fn on_registry(&mut self, registry: &Value) -> Changes {
        let before = self.selected_readout_text();
        self.registry = Some(registry.clone());
        self.coordinator.readout_mut().apply_registry(registry);
        for model in self.lane_models.values_mut() {
            model.readout_mut().apply_registry(registry);
        }
        Changes {
            readout: self.selected_readout_text() != before,
            ..Changes::default()
        }
    }

    /// `GET /lanes` — the whole lane list, each row's step age stamped as observed at
    /// `now_millis` so the clock counts on between reads ([`LaneRow::step_clock_at`]).
    /// `None` leaves the rows unstamped: their clocks stand where the swarm left them.
    pub fn on_lanes_at(&mut self, lanes: &Value, now_millis: Option<u64>) -> Changes {
        Changes {
            lanes: self.lanes.apply_lanes_at(lanes, now_millis),
            ..Changes::default()
        }
    }

    /// `GET /lanes` — the whole lane list, with no moment to stamp its clocks with.
    pub fn on_lanes(&mut self, lanes: &Value) -> Changes {
        self.on_lanes_at(lanes, None)
    }

    /// One SSE event of AGENT's stream.
    ///
    /// [`TabModel::on_event_at`] is the same with the time the I/O layer saw the event,
    /// which is what the step clock counts from; without it the step carries no arrival time
    /// and the frontend can start its own clock when [`Changes::step`] says one began.
    pub fn on_event(&mut self, agent: AgentKey, id: u64, kind: &str, data: &Value) -> Changes {
        self.on_event_inner(agent, id, kind, data, None)
    }

    /// [`TabModel::on_event`] with the arrival time of the event, in epoch milliseconds:
    /// the step a `run-start`/`turn-start`/`compaction-*` begins then knows when it began.
    pub fn on_event_at(
        &mut self,
        agent: AgentKey,
        id: u64,
        kind: &str,
        data: &Value,
        now_millis: u64,
    ) -> Changes {
        self.on_event_inner(agent, id, kind, data, Some(now_millis))
    }

    fn on_event_inner(
        &mut self,
        agent: AgentKey,
        id: u64,
        kind: &str,
        data: &Value,
        arrival: Option<u64>,
    ) -> Changes {
        let effect = self
            .model_mut(agent)
            .apply_event_with(id, kind, data, arrival);
        // The restarted server is a new event log whose ids start again at 1 (§3): the
        // transcript revision this agent last accepted belongs to the old numbering, so
        // drop it rather than refuse the fresh transcript as stale.
        if kind == "hello" {
            self.transcript_revisions.remove(&agent);
        }
        let mut changes = self.absorb(agent, effect);
        // `lane-state` rides the coordinator's stream and is the lane list's, not the
        // coordinator's: the tab's left column follows every lane from the one stream it
        // already holds (§9.3).
        if agent == AgentKey::Coordinator && kind == "lane-state" {
            // Stamped with the moment the tab saw the event: a lane that enters a step
            // here starts its clock at zero from that moment (§7.3).
            changes.lanes = self.lanes.apply_lane_state_at(data, arrival);
            // A lane that is no longer down has nothing said about it any more.
            if data.get("state").and_then(Value::as_str) != Some("down") {
                if let Some(n) = data.get("lane").and_then(Value::as_u64) {
                    self.lane_announcements.remove(&(n as u32));
                }
            }
        }
        // The swarm's own account of a lane going down is remembered when it goes by.
        if agent == AgentKey::Coordinator && kind == "output" {
            self.note_lane_announcement(data);
        }
        changes
    }

    /// Remember the swarm's account of a lane going down: a coordinator `output` line,
    /// error-styled, that names a lane and says what became of it
    /// ([`is_down_announcement`]).
    fn note_lane_announcement(&mut self, data: &Value) {
        if data.get("style").and_then(Value::as_str) != Some("error") {
            return;
        }
        let Some(text) = data.get("text").and_then(Value::as_str) else {
            return;
        };
        if !is_down_announcement(text) {
            return;
        }
        if let Some((n, _, _)) = crate::model::lane_prefix(text) {
            self.lane_announcements.insert(n, text.to_owned());
        }
    }

    /// The journal walk's answer for the cache figure: the reply it carried, or `None` when
    /// that limit held no `cache-stats` entry (grow the limit) — a `None` never clears
    /// totals that are already live.
    pub fn on_cache_seed(&mut self, seed: Option<&Value>) -> Changes {
        let Some(totals) = seed.and_then(cache_totals_from_seed) else {
            return Changes::default();
        };
        let before = self.coordinator.readout().text();
        self.coordinator.readout_mut().set_cache_totals(totals);
        Changes {
            readout: self.coordinator.readout().text() != before,
            ..Changes::default()
        }
    }

    /// The stream badge for AGENT (§9.7).
    pub fn on_stream(&mut self, agent: AgentKey, status: StreamStatus) -> Changes {
        if self.stream_status(agent) == status {
            return Changes::default();
        }
        self.streams.insert(agent, status);
        Changes {
            stream: BTreeSet::from([agent]),
            ..Changes::default()
        }
    }

    /// The I/O layer's `Reset`: the view is not trustworthy any more, so every agent is
    /// refetched. The rows are kept — a refetch that finds the same session rebuilds them
    /// identically, and one that finds a new session replaces them — and the transcript
    /// revisions are forgotten, because whatever restart caused this may have restarted
    /// the numbering too.
    pub fn on_reset(&mut self) -> Changes {
        self.transcript_revisions.clear();
        let mut changes = Changes::default();
        changes.needs_resync.insert(AgentKey::Coordinator);
        changes
            .needs_resync
            .extend(self.lane_models.keys().map(|n| AgentKey::Lane(*n)));
        changes
    }

    // --- internals -------------------------------------------------------

    /// The model of AGENT, creating a lane's on first use.
    fn model_mut(&mut self, agent: AgentKey) -> &mut AgentModel {
        match agent {
            AgentKey::Coordinator => &mut self.coordinator,
            AgentKey::Lane(n) => {
                if !self.lane_models.contains_key(&n) {
                    // A lane's model is born when the lane is first selected or an update
                    // arrives for it — long after the catalog was read once, and it is not
                    // re-read for a lane. The newest catalog is applied here so the lane's
                    // readout can name the window its model runs with.
                    let registry = self.registry.clone();
                    let model = self.lane_models.entry(n).or_default();
                    if let Some(registry) = registry.as_ref() {
                        model.readout_mut().apply_registry(registry);
                    }
                }
                self.lane_models.get_mut(&n).expect("just created")
            }
        }
    }

    /// Turn one model's [`Effect`] into the UI's [`Changes`], draining the row change list.
    ///
    /// [`Effect`]: crate::Effect
    fn absorb(&mut self, agent: AgentKey, effect: crate::Effect) -> Changes {
        let mut changes = Changes::default();
        let rows = self.model_mut(agent).take_row_changes();
        if !rows.is_empty() {
            changes.rows.insert(agent, rows);
        }
        if effect.contains(crate::Effect::TODOS) {
            changes.todos.insert(agent);
        }
        if effect.contains(crate::Effect::STEP) {
            changes.step.insert(agent);
        }
        if effect.contains(crate::Effect::RESYNC) {
            changes.needs_resync.insert(agent);
        }
        // The status line the UI shows is the *selected* agent's readout, so a lane's own
        // usage moves it — while that lane is the one on screen. The activity stays the
        // coordinator's: lanes are watched, never typed to (§14.4).
        changes.readout = effect.contains(crate::Effect::READOUT)
            && (agent == AgentKey::Coordinator || agent == self.selected);
        if agent == AgentKey::Coordinator {
            changes.activity = effect.contains(crate::Effect::ACTIVITY);
        }
        changes
    }
}

/// The newest error line of one agent's transcript: what `output` said, in the order serve
/// sent it — the agent's own account of what went wrong.
fn last_error_line(model: &AgentModel) -> Option<String> {
    last_error_line_where(model, |_| true)
}

/// Whether a lane's line is the swarm's own account of it going down — the two
/// messages `swarm/lanes.lisp` sends (`crashed and was restarted by its supervisor`,
/// `is down: its process exited`, `failed to start`) — rather than something the lane
/// said about itself while it was being brought back.
fn is_down_announcement(text: &str) -> bool {
    text.contains("crashed and was restarted")
        || text.contains("is down")
        || text.contains("failed to start")
}

/// The newest error line of one agent's transcript whose text passes `want`.
fn last_error_line_where(model: &AgentModel, want: impl Fn(&str) -> bool) -> Option<String> {
    model.rows().iter().rev().find_map(|row| match &row.kind {
        RowKind::Dim {
            style: DimStyle::Error,
            text,
        } if !text.is_empty() && want(text) => Some(text.clone()),
        _ => None,
    })
}
