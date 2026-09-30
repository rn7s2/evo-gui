//! tab_engine — one tab's I/O, off the UI thread.
//!
//! One tab is one `evo-swarm serve` in a chosen folder. This crate owns that
//! server's whole life and carries the protocol's own JSON to the UI as
//! [`Update`]s, which the `session` crate turns into view models. Nothing here
//! interprets a frame, and nothing here blocks the UI thread:
//!
//! ```text
//!   UI thread ──Command──▶ [ tab-engine thread ] ──snapshot, stream, ops──▶ evo-swarm serve
//!   UI thread ◀──Update─── [ tab-engine thread ] ◀──frames────────────────  (coordinator, lanes)
//! ```
//!
//! [`TabEngine::start`] returns an [`EngineHandle`] (commands in) and a
//! `Receiver<Update>` (updates out). The handle's `Drop` runs the shutdown
//! ladder through the engine thread and joins it, so no thread is leaked and no
//! server is left behind.

pub mod engine;
mod types;

pub use engine::{shutdown_all, EngineHandle, ShutdownReport, TabEngine};
pub use types::{tab_topics, Command, StreamStatus, TabSpec, Update};

// Re-exported so the UI can read what an [`Update`] carries without depending on
// `swarm_client` directly.
pub use swarm_client::{
    Cursor, ErrorCode, OpError, OpReply, SessionRef, ShutdownOutcome, Snapshot, StreamFrame,
    StreamResetReason, TopicResetReason,
};
