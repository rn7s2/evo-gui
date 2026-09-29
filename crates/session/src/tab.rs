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
//! on_event(agent, id, kind, &data)       → rows / todos / lanes / needs_resync
//! on_lanes(&lanes)            → the lane list
//! on_cache_seed(Option<&seed>)→ readout
//! on_stream(agent, status)    → the stream badge
//! select(agent)               → the selection and the center column
//! ```
//!
//! # Which agent is which
//!
//! The coordinator's model holds the transcript, the checklist, the activity and the §7.3
//! readout the composer shows; a lane's model holds what its own stream carries (rows, its
//! checklist from `todo-changed`, `report` rows). The activity and the readout are the
//! **coordinator's** only — lanes are watched, never typed to (spec §14.4) — so a lane's
//! events never raise [`Changes::activity`] or [`Changes::readout`].

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::Value;

use crate::cache::cache_totals_from_seed;
use crate::{
    Activity, AgentModel, DimStyle, LaneList, LaneRow, Readout, Row, RowChanges, RowKind, Todo,
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
    /// The §7.3 readout text changed.
    pub readout: bool,
    /// The coordinator's activity changed (the Send/Stop button's face).
    pub activity: bool,
    /// These agents' stream badges changed.
    pub stream: BTreeSet<AgentKey>,
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
    selected: AgentKey,
    streams: BTreeMap<AgentKey, StreamStatus>,
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
            selected: AgentKey::Coordinator,
            streams: BTreeMap::new(),
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
        self.agent_model(self.selected).map(|model| model.rows()).unwrap_or(&[])
    }

    pub fn selected_todos(&self) -> &[Todo] {
        self.agent_model(self.selected).map(|model| model.todos()).unwrap_or(&[])
    }

    /// The coordinator's readout — the §7.3 status line the composer renders.
    pub fn readout(&self) -> &Readout {
        self.coordinator.readout()
    }

    pub fn readout_text(&self) -> String {
        self.coordinator.readout().text()
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

    /// Why lane N is down, for the `✗` row's tooltip — the §9.7 "reason from the
    /// transcript's `output` lines".
    ///
    /// The lane's own transcript first: that is where the lane says what went wrong (a model
    /// it cannot register, an internal error). When it has no error line — or was never
    /// loaded — the coordinator's own stream is asked instead, for an error line about that
    /// lane (`[lane N] is down: its process exited…`, `swarm/lanes.lisp`). Either way the
    /// **most recent** line wins; `None` when neither source has one.
    pub fn lane_down_reason(&self, n: u32) -> Option<String> {
        if let Some(reason) = self.lane_models.get(&n).and_then(last_error_line) {
            return Some(reason);
        }
        let about = format!("[lane {}]", n);
        last_error_line_where(&self.coordinator, |text| text.contains(&about))
    }

    /// The lane rows as the left column draws them.
    pub fn lane_rows(&self) -> &[LaneRow] {
        &self.lanes.lanes
    }

    // --- selecting -------------------------------------------------------

    /// Show AGENT's transcript in the center column. A lane's model is created here (the
    /// first thing a selection does is give the lane somewhere to land its transcript).
    pub fn select(&mut self, agent: AgentKey) -> Changes {
        let mut changes = Changes::default();
        if let AgentKey::Lane(n) = agent {
            self.lane_models.entry(n).or_default();
        }
        if self.selected != agent {
            self.selected = agent;
            changes.selection = true;
            // The center column is a different agent now: whatever row views it held
            // belong to the other one, so the row set is new from the UI's point of view.
            changes.rows.insert(agent, RowChanges::Rebuilt);
        }
        changes
    }

    // --- updates from the I/O layer --------------------------------------

    /// `GET /transcript` for AGENT, stamped with the revision of the fetch it answered. Only
    /// a strictly newer revision is applied: an answer older than the last one is a refetch
    /// that overtook it, and an equal one is the same answer twice, so neither changes
    /// anything (the tab's I/O layer numbers these monotonically per agent).
    pub fn on_transcript(&mut self, agent: AgentKey, revision: u64, transcript: &Value) -> Changes {
        if self.transcript_revisions.get(&agent).is_some_and(|last| revision <= *last) {
            return Changes::default();
        }
        self.transcript_revisions.insert(agent, revision);
        let effect = self.model_mut(agent).rebuild_from_transcript(transcript);
        self.absorb(agent, effect)
    }

    /// `GET /state` — the coordinator's (`/state` is only ever the coordinator's: the app
    /// talks to the coordinator, and lane status comes from `/lanes`).
    pub fn on_state(&mut self, state: &Value) -> Changes {
        let effect = self.coordinator.apply_state(state);
        self.absorb(AgentKey::Coordinator, effect)
    }

    /// `GET /registry` — the model catalog, which the readout needs to tell an unambiguous
    /// model id from one registered under several providers.
    pub fn on_registry(&mut self, registry: &Value) -> Changes {
        let before = self.coordinator.readout().text();
        self.coordinator.readout_mut().apply_registry(registry);
        Changes { readout: self.coordinator.readout().text() != before, ..Changes::default() }
    }

    /// `GET /lanes` — the whole lane list.
    pub fn on_lanes(&mut self, lanes: &Value) -> Changes {
        Changes { lanes: self.lanes.apply_lanes(lanes), ..Changes::default() }
    }

    /// One SSE event of AGENT's stream.
    pub fn on_event(&mut self, agent: AgentKey, id: u64, kind: &str, data: &Value) -> Changes {
        let effect = self.model_mut(agent).apply_event(id, kind, data);
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
            changes.lanes = self.lanes.apply_lane_state(data);
        }
        changes
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
        Changes { readout: self.coordinator.readout().text() != before, ..Changes::default() }
    }

    /// The stream badge for AGENT (§9.7).
    pub fn on_stream(&mut self, agent: AgentKey, status: StreamStatus) -> Changes {
        if self.stream_status(agent) == status {
            return Changes::default();
        }
        self.streams.insert(agent, status);
        Changes { stream: BTreeSet::from([agent]), ..Changes::default() }
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
        changes.needs_resync.extend(self.lane_models.keys().map(|n| AgentKey::Lane(*n)));
        changes
    }

    // --- internals -------------------------------------------------------

    /// The model of AGENT, creating a lane's on first use.
    fn model_mut(&mut self, agent: AgentKey) -> &mut AgentModel {
        match agent {
            AgentKey::Coordinator => &mut self.coordinator,
            AgentKey::Lane(n) => self.lane_models.entry(n).or_default(),
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
        if effect.contains(crate::Effect::RESYNC) {
            changes.needs_resync.insert(agent);
        }
        // The readout and the activity are the coordinator's: lanes are watched, never
        // typed to, so nothing about a lane moves the composer (§14.4).
        if agent == AgentKey::Coordinator {
            changes.readout = effect.contains(crate::Effect::READOUT);
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

/// The newest error line of one agent's transcript whose text passes `want`.
fn last_error_line_where(model: &AgentModel, want: impl Fn(&str) -> bool) -> Option<String> {
    model.rows().iter().rev().find_map(|row| match &row.kind {
        RowKind::Dim { style: DimStyle::Error, text } if !text.is_empty() && want(text) => {
            Some(text.clone())
        }
        _ => None,
    })
}
