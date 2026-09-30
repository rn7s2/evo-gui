//! tab_engine — one tab's I/O, off the UI thread.
//!
//! One tab is one `evo-swarm serve` in a chosen folder. This crate owns that
//! server's whole life and carries the protocol's own JSON to the UI as
//! [`Update`]s, which the `session` crate turns into view models. Nothing here
//! interprets a frame, and nothing here blocks the UI thread:
//!
//! ```text
//!   UI thread ──OpRequest─▶ [ tab-engine thread ] ──snapshot, stream, ops──▶ evo-swarm serve
//!   UI thread ◀──Update─── [ tab-engine thread ] ◀──frames────────────────  (coordinator, lanes)
//! ```
//!
//! [`TabEngine::start`] returns an [`EngineHandle`] (ops in) and a
//! `Receiver<Update>` (updates out). The handle's `Drop` closes the child's
//! stdin — EOF, which is the server's own signal to stop — and returns, so a quit
//! never blocks the UI thread; the ladder itself runs on the engine thread.

pub mod engine;
mod types;

pub use engine::{EngineHandle, TabEngine};

// The UI builds an op from its own model and hands it to the handle; these are
// the pieces it needs to do that without depending on `session` directly.
pub use session::{AgentKey, Queue, Scope, TabModel};
pub use types::{tab_topics, StreamStatus, Update};

// Re-exported so the UI can read what an [`Update`] carries without depending on
// `swarm_client` directly.
pub use swarm_client::{
    Cursor, ErrorCode, OpError, OpReply, ServerConfig, SessionRef, ShutdownOutcome, Snapshot,
    StdinClose, StreamFrame, StreamResetReason, TopicResetReason,
};
