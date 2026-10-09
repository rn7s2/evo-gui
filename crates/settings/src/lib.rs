//! settings — the app's Settings surfaces (§13).
//!
//! Two of them, and nothing else is v1's to configure:
//!
//! * [`SettingsPanel`] — the dialog over the two binaries this app spawns, the
//!   theme, and the terminal pane's font — its family and the size it draws at.
//!   The app owns it: it opens the panel from the Settings… menu item, persists
//!   what `Saved` carries through [`store::AppState`], and spawns new tabs with it.
//!   A running tab keeps the binaries and theme it started with, which the panel
//!   says in its own note.
//! * [`ConfigEditor`] — the page over evo's own raw config files (`init.lisp`,
//!   `swarm.lisp`, `memory.sexp`, `lore.sexp`), for the global home or one project.
//!   It reads, checks and writes them; it never evaluates them.
//!
//! ```text
//! SettingsPanel::new(SettingsValues::from_state(&binaries, theme, font), window, cx)
//!     ↓  a path is checked by running `<path> --version` on the app's own executor
//! SettingsEvent::Saved(SettingsValues)   → store::app_state, and the next tab
//! DismissEvent                           → the dialog closes (Save and Cancel both)
//!
//! ConfigEditor::new(ConfigScope::Global | Project(folder), window, cx)
//!     ↓  each document read on the background executor; it emits nothing —
//!     ↓  a host reads is_dirty / is_saving / saving_file when it needs them
//! ```
//!
//! ```text
//! crates/settings/
//!   panel.rs         the dialog: the two path rows, the theme row, the font
//!                    row, Reset/Save/Cancel
//!   config_editor.rs the raw-file page: four tabs, SBCL check, conflict-aware save
//!   probe.rs         `<path> --version` and the four ways of not answering
//!   appearance.rs    what System means on this window (§7.1)
//! ```

mod appearance;
mod config_editor;
mod panel;
mod probe;

pub use appearance::mode_for;
pub use config_editor::{
    ConfigEditor, CONFIG_DISCARD_ID, CONFIG_EDITOR_ID, CONFIG_PATH_ID, CONFIG_RELOAD_ID,
    CONFIG_REMINDER_ID, CONFIG_SAVE_ID, CONFIG_STATUS_ID, CONFIG_SURFACE_ID, CONFIG_TABS_ID,
    REMINDER_TEXT,
};
pub use panel::{
    SettingsEvent, SettingsPanel, SettingsValues, AGENT_CHOOSE_ID, AGENT_PATH_ID, AGENT_STATUS_ID,
    CANCEL_ID, PANEL_ID, PANEL_SIZE, RESET_ID, SAVE_ID, SWARM_CHOOSE_ID, SWARM_PATH_ID,
    SWARM_STATUS_ID, TERMINAL_FONT_ID, TERMINAL_FONT_SIZE_ID, THEME_CHOICES_ID, THEME_ID,
};
pub use probe::{probe, Check, CHECKING};
/// The scope and document types are `store`'s, re-exported so a host has one
/// import for the page and for what it edits.
pub use store::{ConfigFile, ConfigScope};
