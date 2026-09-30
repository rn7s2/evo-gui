//! proofs — the real-binary end-to-end proofs (CONTRACT.md).
//!
//! Each `tests/` file proves one thing about a **real** `evo-swarm serve` (or,
//! for the catalog, about the real offline CLIs), with no UI in the way: a temp
//! `HOME` whose evo home registers the scripted stub model, the binaries the
//! environment names, and this harness driving them over the real protocol.
//!
//! ```sh
//! EVO_SWARM_BIN=…/build/evo-swarm EVO_AGENT_BIN=…/build/evo-agent \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs -- --nocapture
//! ```
//!
//! What each proof asserts is the contract, not an implementation: the ready
//! file, the snapshot, the ops of §5.3, the items of §4.1. Nothing here reads a
//! journal, a port file or a pid list to find out what happened — and nothing
//! here polls a health endpoint to find out that a server is up, because the
//! ready file already said so.

pub mod fixture;
pub mod watch;

pub use fixture::{Bins, Fixture, NOTE, WAIT};
pub use watch::{deadline_after, wait_for, Mirror, Watcher};

/// One swarm at a time. A test binary runs its own tests in parallel threads and
/// these proofs start real processes with real lanes; the crate's tests are one
/// per binary, but a binary that grows a second proof must still take this.
pub fn one_swarm() -> std::sync::MutexGuard<'static, ()> {
    static SWARM: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SWARM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
