//! store — everything the app keeps on disk, and nothing the UI should know
//! about (§6 of `docs/PROMPT.md`).
//!
//! Pure Rust, no gpui: the UI crates depend on this one for state and never
//! touch a path themselves.
//!
//! ```text
//! ~/.evo/desktop/
//!   app.json           window bounds, tab set, binary paths, schema version
//!   lock               single-instance lock (flock; pid inside)
//!   activate.sock      a second launch knocks here so the first one can raise
//!   model-cache.json   last `catalog --json` body, for the empty tab's choosers
//!   tabs/<id>/         ready.json (written by the server, 0600), swarm.log, tab.json
//! ```
//!
//! Everything else the app needs from evo it asks evo for, through the offline
//! CLIs (§9) — one process, one JSON document, no listener:
//!
//! * [`history`] runs `evo-agent sessions --json` for the resumable swarms;
//! * [`catalog`]/[`model_cache`] keep what `evo-swarm catalog --json` printed;
//! * [`launch`] builds the argv of a `serve`, and the `check --json` that
//!   validates it first.
//!
//! Nothing here reads a journal, writes a project file, or starts a server. Every
//! write is atomic (temp file + rename) and owner-only, every load is tolerant (a
//! corrupt file becomes a `.bak` and the defaults apply), and nothing here holds
//! a secret: the bearer token stays in the server's own 0600 ready file
//! (§2 rule 4).

pub mod app_state;
pub mod catalog;
pub mod cli;
pub mod history;
pub mod launch;
pub mod model_cache;
pub mod paths;
pub mod single;
pub mod tab;
pub mod time;

pub use app_state::{AppState, Binaries, Recent, Theme, WindowBounds, SCHEMA_VERSION};
pub use catalog::{
    Catalog, CheckReport, LaneModel, Model, ModelCheck, ModelRef, Problem, ProblemTarget,
};
pub use history::{HistoryEntry, HistorySource, Session, SessionsQuery};
pub use launch::{LaunchSpec, Program};
pub use model_cache::ModelCache;
pub use paths::{Root, TabId};
pub use single::{Activation, Primary, Secondary, SingleInstance};
pub use tab::{TabModels, TabState};
