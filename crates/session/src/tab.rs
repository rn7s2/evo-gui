//! The whole tab as one value: one mirror per topic, the selection, the lane list and
//! what the UI has to re-set after each op.
//!
//! Nothing here does I/O. The tab's transport hands over `GET /snapshot` bodies and stream
//! frames; every entry point returns the [`Changes`] the UI must apply, so "which rows
//! changed" is answered once here instead of guessed per frontend.
//!
//! ```text
//! on_snapshot(topic, &body)     a topic's state and its newest items
//! on_op(topic, &op)             one stream frame
//! on_items_before(topic, &body) older items, paged in at the front
//! select(agent)                 the center column shows another agent
//! on_stream(topic, status)      the stream badge
//! ```
//!
//! The topics are `session` (the coordinator), `swarm` (the lane list) and `lane:<n>` (one
//! lane's own items and state). A topic is mirrored the first time anything arrives for
//! it — a lane's stream is opened once it is watched, so an update *is* the watch.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use crate::state::Status;
use crate::swarm::{lane_list, LaneList};
use crate::{Item, ItemId, LaneRow, Op, Segment, SwarmState, Topic, TopicState};

/// One agent of a tab: the coordinator, or lane N.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentKey {
    Coordinator,
    Lane(u32),
}

impl AgentKey {
    /// The topic this agent's items and state live under.
    pub fn topic(self) -> String {
        match self {
            AgentKey::Coordinator => "session".to_string(),
            AgentKey::Lane(n) => format!("lane:{n}"),
        }
    }

    /// The agent a topic names, or `None` for a topic that is not an agent's (`swarm`).
    pub fn from_topic(topic: &str) -> Option<AgentKey> {
        match topic {
            "session" => Some(AgentKey::Coordinator),
            other => Some(AgentKey::Lane(other.strip_prefix("lane:")?.parse().ok()?)),
        }
    }

    pub fn lane(self) -> Option<u32> {
        match self {
            AgentKey::Lane(n) => Some(n),
            AgentKey::Coordinator => None,
        }
    }

    /// What the UI calls this agent: a swarm's `Coordinator`, the one agent of a
    /// single-agent session `Main`, and a lane `Lane N`.
    ///
    /// An agent's name, so it is capitalised wherever it is drawn — the lanes column's
    /// rows, the band over the conversation, the composer's drawer — and the three say
    /// it from here rather than from three of their own literals.
    pub fn name(self, swarm: bool) -> String {
        match self {
            AgentKey::Coordinator if swarm => "Coordinator".to_string(),
            AgentKey::Coordinator => "Main".to_string(),
            AgentKey::Lane(n) => format!("Lane {n}"),
        }
    }
}

/// One item's change, as the UI's row views need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemChange {
    /// The item is new, or its content changed: `index` is where it sits.
    Upsert { id: ItemId, index: usize },
    /// The item is gone.
    Remove { id: ItemId },
}

/// What changed in one topic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicChanges {
    /// The topic's items are a fresh set: every row view for it is stale.
    pub reset: bool,
    /// The topic's state changed (the readouts, the segments, the checklist).
    pub state: bool,
    /// Items were inserted at the front: the topic holds them first.
    pub prepended: usize,
    pub(crate) items: Vec<ItemChange>,
}

impl TopicChanges {
    pub(crate) fn upsert(id: ItemId, index: usize) -> TopicChanges {
        TopicChanges {
            items: vec![ItemChange::Upsert { id, index }],
            ..TopicChanges::default()
        }
    }

    fn stale() -> TopicChanges {
        TopicChanges {
            reset: true,
            state: true,
            ..TopicChanges::default()
        }
    }

    /// The item changes, in the order they happened.
    pub fn items(&self) -> &[ItemChange] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        !self.reset && !self.state && self.prepended == 0 && self.items.is_empty()
    }
}

/// What the UI has to re-set after one update. Everything the tab did not touch is left
/// alone, so a text delta re-renders one row instead of the screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Per topic name: the item changes and whether the state moved.
    pub topics: BTreeMap<String, TopicChanges>,
    /// The lane list changed (the swarm's lanes, or a lane's own activity).
    pub lanes: bool,
    /// The selection changed: the center column shows a different agent.
    pub selection: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.topics.values().all(TopicChanges::is_empty) && !self.lanes && !self.selection
    }

    pub fn for_topic(&self, topic: &str) -> Option<&TopicChanges> {
        self.topics.get(topic)
    }

    fn absorb(&mut self, topic: &str, changes: TopicChanges) {
        if changes.is_empty() {
            return;
        }
        self.topics
            .entry(topic.to_string())
            .and_modify(|held| {
                held.reset |= changes.reset;
                held.state |= changes.state;
                held.prepended += changes.prepended;
                held.items.extend(changes.items.iter().cloned());
            })
            .or_insert(changes);
    }
}

/// The state of one topic's stream, for the "reconnecting" badge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamStatus {
    #[default]
    Connected,
    Reconnecting {
        retry_in: Duration,
    },
}

impl StreamStatus {
    pub fn is_reconnecting(self) -> bool {
        matches!(self, StreamStatus::Reconnecting { .. })
    }
}

/// One tab: a mirror of the session, the swarm and every lane it holds a mirror of.
#[derive(Clone, Debug)]
pub struct TabModel {
    session: Topic,
    /// Topic `swarm`: state only (CONTRACT §5.2 — it carries no items), `None` until
    /// anything is read.
    swarm: Option<SwarmState>,
    lanes: BTreeMap<u32, Topic>,
    selected: AgentKey,
    streams: BTreeMap<String, StreamStatus>,
    /// The lane rows, rebuilt whenever the swarm topic or a lane's own mirror moves.
    lane_list: LaneList,
}

impl Default for TabModel {
    fn default() -> Self {
        TabModel::new()
    }
}

impl TabModel {
    pub fn new() -> TabModel {
        TabModel {
            session: Topic::new("session"),
            swarm: None,
            lanes: BTreeMap::new(),
            selected: AgentKey::Coordinator,
            streams: BTreeMap::new(),
            lane_list: LaneList::new(),
        }
    }

    // --- reading ----------------------------------------------------------

    pub fn selected(&self) -> AgentKey {
        self.selected
    }

    /// One topic's mirror, by name. Topic `swarm` has no items, so it is `None` here.
    pub fn topic(&self, topic: &str) -> Option<&Topic> {
        match topic {
            "session" => Some(&self.session),
            "swarm" => None,
            other => self.lane_topic_of(other),
        }
    }

    /// One agent's topic, creating nothing: a lane the tab holds no mirror of has none.
    pub fn agent_topic(&self, agent: AgentKey) -> Option<&Topic> {
        match agent {
            AgentKey::Coordinator => Some(&self.session),
            AgentKey::Lane(n) => self.lanes.get(&n),
        }
    }

    /// One agent's items, oldest first — the transcript the center column draws.
    pub fn items(&self, agent: AgentKey) -> &[Item] {
        self.agent_topic(agent)
            .map(Topic::items)
            .unwrap_or_default()
    }

    /// The selected agent's items.
    pub fn selected_items(&self) -> &[Item] {
        self.items(self.selected)
    }

    /// One agent's state, or `None` while the tab holds no mirror of it yet.
    pub fn state(&self, agent: AgentKey) -> Option<&TopicState> {
        self.agent_topic(agent).map(Topic::state)
    }

    pub fn selected_state(&self) -> Option<&TopicState> {
        self.state(self.selected)
    }

    /// The selected agent's status-line segments, as the server built them.
    pub fn selected_segments(&self) -> &[Segment] {
        self.selected_state()
            .map(|state| state.segments.as_slice())
            .unwrap_or_default()
    }

    /// The selected agent's checklist.
    pub fn selected_todos(&self) -> &[crate::Todo] {
        self.selected_state()
            .map(|state| state.todos.as_slice())
            .unwrap_or_default()
    }

    /// The swarm topic's state.
    pub fn swarm(&self) -> Option<&SwarmState> {
        self.swarm.as_ref()
    }

    /// The lane rows as the left column draws them.
    pub fn lane_rows(&self) -> &[LaneRow] {
        &self.lane_list.lanes
    }

    pub fn lane_list(&self) -> &LaneList {
        &self.lane_list
    }

    /// Whether the swarm topic says the swarm is working: a lane busy, or the
    /// coordinator held waiting for one.
    ///
    /// The coordinator's *own* run is not in here. The swarms's `busy` counts
    /// lanes, and `waiting_on_lanes` is false while the coordinator itself works —
    /// so a caller that wants "is anything going on" (the action button's face)
    /// folds in [`TabModel::activity`] as well.
    pub fn lanes_busy(&self) -> bool {
        self.lane_list.is_busy()
    }

    /// The coordinator's status, for the coordinator's row and the button.
    ///
    /// A coordinator the swarm says is held for its lanes *is* waiting on them,
    /// even when its own topic has not caught up and still calls itself idle: the
    /// swarm is the one that knows it is held (§4.2). Nothing is invented that way
    /// — the flag is the server's own — and the two agree once the session's own
    /// status arrives, so the row never flickers through "idle".
    pub fn activity(&self) -> Status {
        let status = self.session.state().status;
        if status == Status::Idle && self.lanes_waiting() {
            Status::Waiting
        } else {
            status
        }
    }

    /// Whether the swarm topic says the coordinator is held while its lanes work.
    fn lanes_waiting(&self) -> bool {
        self.lane_list
            .swarm
            .as_ref()
            .is_some_and(|swarm| swarm.waiting_on_lanes)
    }

    /// The ids of the inputs still queued, oldest first: what a UI draws as held back and
    /// takes back with `input.cancel`.
    pub fn queue(&self) -> &[ItemId] {
        &self.session.state().queue
    }

    /// Why lane N is down, for the `✗` row's tooltip: the newest lane event about that
    /// lane that the swarm called an error — `crashed`, `down`, `failed_to_start`.
    ///
    /// **Only a lane the list shows as down has a reason.** A lane that is back up has
    /// none: whatever was said while it was being brought back is not current, and a
    /// tooltip over a working lane claiming it is down would be a lie.
    pub fn lane_down_reason(&self, n: u32) -> Option<String> {
        if self.lane_list.lane(n).map(|lane| lane.status) != Some(crate::LaneStatus::Down) {
            return None;
        }
        self.session
            .items()
            .iter()
            .rev()
            .find_map(|item| match &item.kind {
                crate::ItemKind::LaneEvent(event)
                    if event.lane == n && event.severity == crate::NoticeSeverity::Error =>
                {
                    Some(item.summary())
                }
                _ => None,
            })
    }

    pub fn stream_status(&self, topic: &str) -> StreamStatus {
        self.streams.get(topic).copied().unwrap_or_default()
    }

    // --- selecting --------------------------------------------------------

    /// Show AGENT's transcript in the center column. A lane's mirror is created here, so
    /// the column has somewhere to land its items.
    pub fn select(&mut self, agent: AgentKey) -> Changes {
        let mut changes = Changes::default();
        if let AgentKey::Lane(n) = agent {
            self.lanes
                .entry(n)
                .or_insert_with(|| Topic::new(format!("lane:{n}")));
        }
        if self.selected != agent {
            self.selected = agent;
            changes.selection = true;
            // The center column is another agent: whatever row views it held belong to
            // the other one, and so is the status line under it.
            changes.absorb(&agent.topic(), TopicChanges::stale());
        }
        changes
    }

    // --- what the transport delivers --------------------------------------

    /// One topic's `GET /snapshot` body: `{state, items, has_more}` (the `swarm` topic has
    /// state only).
    pub fn on_snapshot(&mut self, topic: &str, body: &Value) -> Changes {
        let mut changes = Changes::default();
        let state = body.get("state").unwrap_or(&Value::Null);
        if topic == "swarm" {
            self.swarm = Some(SwarmState::from_json(state));
            changes.absorb("swarm", TopicChanges::stale());
            self.refresh_lanes(&mut changes);
            return changes;
        }
        let Some(target) = self.topic_mut(topic) else {
            return changes;
        };
        changes.absorb(topic, target.apply_snapshot(body));
        self.refresh_lanes(&mut changes);
        changes
    }

    /// One stream frame.
    pub fn on_op(&mut self, topic: &str, op: &Op) -> Changes {
        let mut changes = Changes::default();
        // A stream reset is about every topic, not the one it arrived on: the transport
        // re-snapshots them all, and each re-read says what the UI must drop.
        if let Op::StreamReset { .. } = op {
            for name in self.topic_names() {
                changes.absorb(&name, TopicChanges::stale());
            }
            return changes;
        }
        if topic == "swarm" {
            match op {
                Op::StatePatch { patch } => {
                    let Some(swarm) = self.swarm.as_mut() else {
                        return changes;
                    };
                    swarm.apply_patch(patch);
                    changes.absorb(
                        "swarm",
                        TopicChanges {
                            state: true,
                            ..TopicChanges::default()
                        },
                    );
                    self.refresh_lanes(&mut changes);
                }
                // A lane was replaced by its supervisor: its own mirror is dropped, and
                // the swarm's lanes are re-read.
                Op::TopicReset { .. } => {
                    self.swarm = None;
                    changes.absorb("swarm", TopicChanges::stale());
                    self.refresh_lanes(&mut changes);
                }
                _ => {}
            }
            return changes;
        }
        let Some(target) = self.topic_mut(topic) else {
            return changes;
        };
        changes.absorb(topic, target.apply_op(op));
        self.refresh_lanes(&mut changes);
        changes
    }

    /// Older items (`GET /items?before=<id>`), paged in at the front of the topic.
    pub fn on_items_before(&mut self, topic: &str, body: &Value) -> Changes {
        let mut changes = Changes::default();
        let Some(target) = self.topic_mut(topic) else {
            return changes;
        };
        changes.absorb(topic, target.prepend_items(body));
        changes
    }

    /// The stream badge for one topic.
    pub fn on_stream(&mut self, topic: &str, status: StreamStatus) -> Changes {
        if self.stream_status(topic) == status {
            return Changes::default();
        }
        self.streams.insert(topic.to_string(), status);
        Changes::default()
    }

    /// A lane was replaced (`topic.reset lane:N` with `lane_restarted`): its own mirror is
    /// no longer an extension of what the tab holds, so it is dropped and re-snapshotted.
    pub fn on_lane_restarted(&mut self, n: u32) -> Changes {
        let mut changes = Changes::default();
        if let Some(topic) = self.lanes.get_mut(&n) {
            *topic = Topic::new(format!("lane:{n}"));
            changes.absorb(&format!("lane:{n}"), TopicChanges::stale());
            self.refresh_lanes(&mut changes);
        }
        changes
    }

    // --- commands ---------------------------------------------------------

    /// Send the reader's words to the coordinator. The server answers with the item id,
    /// which is what a queued input is cancelled by; the row itself arrives as an
    /// `item.add` on topic `session`.
    pub fn send_input(&self, text: &str, queue: crate::Queue) -> crate::OpRequest {
        self.send_input_with(text, Vec::new(), queue)
    }

    /// The same, carrying the turn's images ([`crate::attachments`], CONTRACT §5.5): one
    /// `{path}` or `{name, media_type, data}` entry each, as [`crate::attachment_turn`]
    /// builds them. What the *text* says about a file the reader attached is already in
    /// it — a message's files travel as their paths, in the message.
    pub fn send_input_with(
        &self,
        text: &str,
        images: Vec<serde_json::Value>,
        queue: crate::Queue,
    ) -> crate::OpRequest {
        crate::OpRequest::input_send(text, images, queue, Some("session"))
    }

    /// Take back an input still queued.
    pub fn cancel_input(&self, item_id: &str) -> crate::OpRequest {
        crate::OpRequest::input_cancel(item_id)
    }

    /// Stop the whole swarm: every lane, and the coordinator with it.
    pub fn interrupt_swarm(&self) -> crate::OpRequest {
        crate::OpRequest::run_interrupt(crate::Scope::Swarm, None)
    }

    /// Stop one lane.
    pub fn interrupt_lane(&self, n: u32) -> crate::OpRequest {
        crate::OpRequest::run_interrupt(crate::Scope::Lane, Some(n))
    }

    /// Stop the coordinator's own run.
    pub fn interrupt_session(&self) -> crate::OpRequest {
        crate::OpRequest::run_interrupt(crate::Scope::Session, None)
    }

    // --- internals --------------------------------------------------------

    fn lane_topic_of(&self, topic: &str) -> Option<&Topic> {
        let n: u32 = topic.strip_prefix("lane:")?.parse().ok()?;
        self.lanes.get(&n)
    }

    fn topic_names(&self) -> Vec<String> {
        let mut names = vec!["session".to_string(), "swarm".to_string()];
        names.extend(self.lanes.keys().map(|n| format!("lane:{n}")));
        names
    }

    /// The mirror of `topic`, created if it is a lane the tab has not seen.
    fn topic_mut(&mut self, topic: &str) -> Option<&mut Topic> {
        match topic {
            "session" => Some(&mut self.session),
            "swarm" => None,
            other => {
                let n: u32 = other.strip_prefix("lane:")?.parse().ok()?;
                Some(
                    self.lanes
                        .entry(n)
                        .or_insert_with(|| Topic::new(format!("lane:{n}"))),
                )
            }
        }
    }

    /// Rebuild the lane rows after anything that could move them, and say whether they
    /// changed.
    fn refresh_lanes(&mut self, changes: &mut Changes) {
        let empty = SwarmState::default();
        let swarm = self.swarm.as_ref().unwrap_or(&empty);
        let lanes = &self.lanes;
        let next = lane_list(swarm, |n| lanes.get(&n).and_then(lane_last_item));
        if next != self.lane_list {
            self.lane_list = next;
            changes.lanes = true;
        }
    }
}

/// The newest item one lane's own mirror knows about: its kind, and a one-line summary of
/// it. The lane's topic has it before the swarm's own summary of the lane catches up.
fn lane_last_item(topic: &Topic) -> Option<(String, String)> {
    topic
        .last_item()
        .map(|item| (item.kind.label().to_string(), item.summary()))
}
