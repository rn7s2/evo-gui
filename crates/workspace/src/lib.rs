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
mod launch;
mod panes;
pub mod prompt_note;
mod tab;
mod tab_page;
mod tab_strip;

pub use bridge::{Bridge, BridgeSender, Revision, Tagged, Worker};
pub use chrome::{
    bind_tab_keys, fit_to_work_area, initial_window_bounds, window_options, HistoryStale,
    HoldToQuit, LauncherData, QuitHeld, SelectLastTab, SelectNextTab, SelectPreviousTab, SelectTab,
    WorkspaceView, QUIT_HOLD,
};
pub use empty_tab::OpenSettings;
pub use history::{placeholder_history, HistoryRow};
pub use launch::{check, check_spec, launch_spec, Launch, LaunchEnv};
pub use prompt_note::{PromptNote, NOTE};
pub use tab::{TabContent, TabContentEvent, TabId, TabState};

/// A test's own evo home, for the tests that open the app's Settings pages.
///
/// The four files those pages read are then a directory that does not exist — which
/// is what a machine that has never been configured looks like, and which reading
/// creates nothing of — rather than whatever the machine running the tests has in
/// `~/.evo`. Set once for the whole test binary, before any page is made.
#[cfg(test)]
pub(crate) fn test_evo_home() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join(format!("workspace-evo-home-{}", std::process::id()));
        std::env::set_var("EVO_HOME", home);
    });
}
