//! proofs — milestone proof tests against a real evo-swarm, with no UI.
//!
//! Each milestone is one test binary under `tests/`, driving the same three crates
//! the tab page drives — `tab_engine` for the I/O, `session` for the view models,
//! `store` for what the app keeps on disk — against a real `evo-swarm serve` in a
//! hermetic temp `HOME` whose model is scripted (`swarm_client::harness`, feature
//! `test-harness`). Nothing is faked but the model.
//!
//! Run them with `CARGO_TARGET_DIR=target/proofs cargo test -p proofs` (the
//! isolation the proofs want: a swarm is a supervisor plus a process per lane, and
//! these binaries start real ones). What each milestone proves, and the result of
//! the last run, is in `docs/proofs.md`.
//!
//! What the UI adds on top of this is drawing: the folding of a `tab_engine`
//! [`Update`] into a `session::TabModel` is the same turn by turn (it is the
//! `absorb` of `crates/workspace/src/tab.rs`'s `Live`), so a proof that asserts on
//! the model asserts on what the tab page renders.
//!
//! [`Update`]: tab_engine::Update

pub use session::TabModel;
