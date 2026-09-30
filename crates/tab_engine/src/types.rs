//! What the UI [asks a tab](EngineHandle) and what a [tab tells the UI](Update).
//!
//! The UI never runs an op itself: it builds a [`session::OpRequest`] from the
//! tab's own model (`TabModel::send_input`, `interrupt_lane`, …) and hands it to
//! an [`session::OpSink`], which the [`EngineHandle`](crate::EngineHandle) is.
//!
//! How a server is launched is not decided here either: the tab is started from a
//! [`ServerConfig`](swarm_client::ServerConfig) whose argv `store::launch` built.
//! In the other direction a tab hands over the protocol's own JSON — one topic's
//! snapshot body, one parsed [`session::Op`] — and the `session` crate is the
//! only thing that knows what any of it means.

use swarm_client::{OpReply, SessionRef};

/// The stream's state, for the "reconnecting" badge: session's own vocabulary,
/// so the UI has one enum and not two.
pub use session::StreamStatus;

/// What a tab tells the UI. Everything here goes straight into the `session` crate.
// The `Op` variant carries a parsed item, so it is the widest by far; updates are
// moved, never collected in bulk, so sizing the enum for it costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The server is being started.
    Booting,
    /// The server is ready: its epoch, its process id, its port, its session.
    Ready {
        epoch: String,
        pid: u32,
        port: u16,
        session: SessionRef,
    },
    /// The server never came up; `log_tail` is what the tab shows.
    BootFailed { message: String, log_tail: String },
    /// One topic of a snapshot (§5.2), exactly as the server sent it: the body
    /// of `topics[name]` — `{state, items, has_more}`. `TabModel::on_snapshot`.
    Snapshot {
        topic: String,
        body: serde_json::Value,
    },
    /// One stream frame (§5.3). `TabModel::on_op`.
    Op { topic: String, op: session::Op },
    /// The stream's state, for the "reconnecting" line.
    Stream { status: StreamStatus },
    /// The reply to an op the UI sent (§5.5).
    OpReply(Box<OpReply>),
    /// The server process died on its own; the engine has stopped.
    ServerGone,
    /// The engine has stopped, and how the server went.
    Exited {
        outcome: swarm_client::ShutdownOutcome,
    },
}

/// The topics a tab always reads: the coordinator's own, the swarm's, and every
/// lane's. `lane:*` is expanded by the server, so a lane that starts or restarts
/// needs no new subscription.
pub fn tab_topics() -> Vec<String> {
    vec![
        swarm_client::TOPIC_SESSION.to_owned(),
        swarm_client::TOPIC_SWARM.to_owned(),
        swarm_client::TOPIC_LANE_WILDCARD.to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_topics_are_the_coordinators_the_swarms_and_every_lane() {
        assert_eq!(tab_topics(), vec!["session", "swarm", "lane:*"]);
    }
}
