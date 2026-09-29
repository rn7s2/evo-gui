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
//!   model-cache.json   last /registry snapshot, for the empty tab's choosers
//!   probe/             scratch cwd used only to learn the model catalog
//!   tabs/<id>/         token (0600, written by the server), swarm.log, tab.json
//! ```
//!
//! The three things that read outside that directory are read-only, except for
//! the one write the spec asks for:
//!
//! * [`history`] walks `~/.evo/sessions/*/*.sexp` for resumable swarms (§9.5);
//! * [`model_cache`] carries the model catalog and the kernel api set (§9.4);
//! * [`swarm_config`] owns the managed block at the top of a project's
//!   `.evo/swarm.lisp` (§9.6) — the only file outside our root we ever write.
//!
//! Every write is atomic (temp file + rename) and owner-only, every load is
//! tolerant (a corrupt file becomes a `.bak` and the defaults apply), and
//! nothing here holds a secret: the tab token stays in the server's own 0600
//! file (§2 rule 4).

pub mod app_state;
pub mod history;
pub mod model_cache;
pub mod paths;
pub mod sexp;
pub mod single;
pub mod swarm_config;
pub mod tab;
pub mod time;

pub use app_state::{AppState, Binaries, Recent, Theme, WindowBounds, SCHEMA_VERSION};
pub use history::{HistoryEntry, HistorySource, ScanBudget, ScanOutcome};
pub use model_cache::{ModelCache, ModelInfo};
pub use paths::{Root, TabId};
pub use single::{Activation, Primary, Secondary, SingleInstance};
pub use swarm_config::{LanesModel, WriteOutcome};
pub use tab::{TabModels, TabState};
