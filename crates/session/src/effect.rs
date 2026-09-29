//! What the UI must do after the model changed.

use std::ops::{BitOr, BitOrAssign};

/// A set of "the UI has work to do" flags, returned by [`AgentModel::apply_event`].
///
/// The UI applies the flags it cares about: a `ROWS` flag re-renders the transcript, a
/// `RESYNC` flag refetches `/state` + `/transcript` and rebuilds the model.
///
/// [`AgentModel::apply_event`]: crate::AgentModel::apply_event
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effect(u32);

impl Effect {
    /// Nothing the UI renders changed.
    pub const NONE: Effect = Effect(0);
    /// Transcript rows were added or changed (content and/or version).
    pub const ROWS: Effect = Effect(1);
    /// The rows were rebuilt from scratch: the row set is new and every row view
    /// the UI holds for the previous revision is stale and must be dropped
    /// (row ids are only unique within a revision — see [`AgentModel::revision`]).
    ///
    /// [`AgentModel::revision`]: crate::AgentModel::revision
    pub const REBUILT: Effect = Effect(2);
    /// The todo list changed.
    pub const TODOS: Effect = Effect(4);
    /// The §7.3 status readout text changed.
    pub const READOUT: Effect = Effect(8);
    /// Coordinator activity (idle / running / compacting) changed.
    pub const ACTIVITY: Effect = Effect(16);
    /// A `lane-state` event went by: feed its payload to the tab's [`LaneList`] as
    /// well — the agent model does not own the lane list.
    ///
    /// [`LaneList`]: crate::LaneList
    pub const LANES: Effect = Effect(32);
    /// The event log or the session changed underneath us (`hello`, `gap`,
    /// `session-switched`, `settled`): refetch `/state` + `/transcript` and rebuild.
    pub const RESYNC: Effect = Effect(64);

    /// Whether every flag of `other` is set in `self`.
    pub const fn contains(self, other: Effect) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Effect {
    type Output = Effect;
    fn bitor(self, other: Effect) -> Effect {
        Effect(self.0 | other.0)
    }
}

impl BitOrAssign for Effect {
    fn bitor_assign(&mut self, other: Effect) {
        self.0 |= other.0;
    }
}

/// Which rows of one agent changed since the last look: what a UI holding per-row views
/// has to re-set, and nothing else.
///
/// A [`Changed`] list is the cheap case — only those [`RowId`]s were touched, so only
/// their views need rebuilding. A [`RowId`] in the list that [`AgentModel::row`] no longer
/// knows is a row that went away and whose view must be dropped.
///
/// [`Changed`]: RowChanges::Changed
/// [`RowId`]: crate::RowId
/// [`AgentModel::row`]: crate::AgentModel::row
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RowChanges {
    /// Nothing changed.
    #[default]
    Unchanged,
    /// The row set was rebuilt from scratch: every row view for this agent is stale
    /// (the ids are a fresh space), so drop them all and rebuild from
    /// [`AgentModel::rows`].
    ///
    /// [`AgentModel::rows`]: crate::AgentModel::rows
    Rebuilt,
    /// Exactly these rows changed, in the order they were touched.
    Changed(Vec<crate::RowId>),
}

impl RowChanges {
    pub fn is_empty(&self) -> bool {
        matches!(self, RowChanges::Unchanged)
    }

    /// Whether this is a full rebuild (every row view stale).
    pub fn is_rebuilt(&self) -> bool {
        matches!(self, RowChanges::Rebuilt)
    }
}
