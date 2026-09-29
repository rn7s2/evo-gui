//! tab_engine — one tab's I/O, off the UI thread (docs/architecture.md).
//!
//! One tab is one `evo-swarm serve` in a chosen folder (§3). This crate owns
//! that server's whole life and turns its HTTP/SSE into [`Update`]s for the UI,
//! which are applied to the `session` crate's view models. Nothing here blocks:
//!
//! ```text
//!   UI thread ──Command──▶ [ tab-engine thread ] ──HTTP/SSE──▶ evo-swarm serve
//!   UI thread ◀──Update─── [ tab-engine thread ] ◀──events─── (coordinator, lanes)
//! ```
//!
//! [`TabEngine::start`] returns an [`EngineHandle`] (commands in) and a
//! `Receiver<Update>` (updates out). The handle's `Drop` runs §3's shutdown
//! ladder through the engine thread and joins it, so no thread is leaked and no
//! server is left behind.
//!
//! What the engine does, per docs/PROMPT.md:
//!
//! * **boot** (§3) — spawn, readiness, and a [`Update::BootFailed`] carrying the
//!   log tail when the server never comes up;
//! * **assembly** (§9.1) — `/health` cursor → `/transcript` → `/state`, plus
//!   `/registry` and `/lanes` once, then the live `/events?since=<cursor>`;
//! * **live** (§5) — every SSE event as [`Update::Event`], resyncing on
//!   `settled`, `gap`, `hello`, `session-switched`, a stream reset or a
//!   reconnect, each with a fresh revision;
//! * **lanes** (§D21) — at most one lane stream, opened on
//!   [`Command::WatchLane`], reading only (never a lane's token or URL);
//! * **cache seed** (§7.3) — the journal's newest `cache-stats` entry;
//! * **commands** — `/prompt`, `/interrupt`, refetch, watch, shutdown.

pub mod catalog;
mod engine;
mod types;

pub use engine::{EngineHandle, ShutdownReport, TabEngine, shutdown_all};
pub use types::{
    Agent, Command, PostError, ReqId, StreamStatus, TabSpec, Update,
};

// Re-exported so the UI can read the payloads an [`Update`] carries without
// depending on `swarm_client` directly.
pub use swarm_client::{Envelope, Health, ShutdownOutcome};
