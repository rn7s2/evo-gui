//! workspace — the window, its chrome and its tab pages (docs/architecture.md).
//!
//! The crate owns everything that is not a transcript or a composer: the window
//! options (§7.1), the title bar and its tab strip, the empty tab of a swarm that
//! does not exist yet (§7.2), the tab page layout (§7.3), and the bridge every
//! other piece of the app uses to get worker results onto the UI thread.

mod bridge;
mod chrome;
mod empty_tab;
mod history;
mod tab;
mod tab_page;

pub use bridge::{Bridge, BridgeSender, Revision, Tagged, Worker};
pub use chrome::{fit_to_work_area, initial_window_bounds, window_options, WorkspaceView};
pub use history::{placeholder_history, HistoryRow};
pub use tab::{TabContent, TabContentEvent, TabId, TabState};
