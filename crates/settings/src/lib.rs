//! settings — the Settings panel (§13): the two binaries this app spawns, and the theme.
//! Nothing else is v1's to configure, so nothing else is here.
//!
//! ```text
//! SettingsPanel::new(SettingsValues::from_state(&binaries, theme), window, cx)
//!     ↓  a path is checked by running `<path> --version` on the app's own executor
//! SettingsEvent::Saved(SettingsValues)   → store::app_state, and the next tab
//! DismissEvent                           → the dialog closes (Save and Cancel both)
//! ```
//!
//! The app owns the dialog: it opens the panel from the Settings… menu item, persists what
//! `Saved` carries through [`store::AppState`], and spawns new tabs with it. A running tab
//! keeps the binaries and theme it started with, which the panel says in its own note.
//!
//! ```text
//! crates/settings/
//!   panel.rs       the view: the two rows, the theme row, Reset/Save/Cancel
//!   probe.rs       `<path> --version` and the four ways of not answering
//!   appearance.rs  what System means on this window (§7.1)
//! ```

mod appearance;
mod panel;
mod probe;

pub use appearance::mode_for;
pub use panel::{
    SettingsEvent, SettingsPanel, SettingsValues, AGENT_CHOOSE_ID, AGENT_PATH_ID, AGENT_STATUS_ID,
    CANCEL_ID, PANEL_ID, PANEL_SIZE, RESET_ID, SAVE_ID, SWARM_CHOOSE_ID, SWARM_PATH_ID,
    SWARM_STATUS_ID, THEME_CHOICES_ID, THEME_ID,
};
pub use probe::{probe, Check, CHECKING};
