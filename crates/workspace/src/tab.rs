//! One tab: what a single swarm is doing, and the content the window shows for
//! it (§7.1, §7.2, §7.3).
//!
//! The tab strip belongs to [`crate::WorkspaceView`], which owns the tab set and
//! the selection. Everything below the strip belongs here: `TabContent` is the
//! retained view behind one tab, and it reports what it needs from the window by
//! emitting [`TabContentEvent`] rather than changing the tab set itself.

use std::path::{Path, PathBuf};

use gpui_kit::component::list::{ListEvent, ListState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Subscription, Window,
};

use composer::Composer;
use transcript::TranscriptView;

use crate::empty_tab::{Choosers, HistoryList};
use crate::history::{folder_name, placeholder_history, HistoryRow};

/// Identity of a tab inside the window.
///
/// Monotonic, never reused, so it can key elements and outlive a tab being
/// closed and another opened in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

impl TabId {
    /// The window hands out ids in the order tabs are opened.
    pub(crate) fn new(value: u64) -> Self {
        TabId(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// What one tab is doing right now (§7.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabState {
    /// No folder chosen yet: the choosers and the history list (§7.2).
    Empty,
    /// A swarm is starting in this folder; its progress is what we show (§3).
    Booting { folder: PathBuf },
    /// The coordinator answered `/health`; the tab page is live (§7.3).
    Running { folder: PathBuf },
    /// The swarm never came up: show the tail of its log (§9.7).
    Failed { folder: PathBuf, log_tail: String },
}

/// What a tab's content asks the window for.
///
/// The workspace owns the tab set, so the content reports intent instead of
/// acting on it: a tab never adds, closes or reorders tabs itself.
#[derive(Clone, Debug, PartialEq)]
pub enum TabContentEvent {
    /// The user chose a folder: this tab should boot a swarm there (§7.2).
    FolderPicked(PathBuf),
    /// The user picked a past swarm: open it as a new tab (§7.2, §9.5).
    HistoryRowActivated(HistoryRow),
    /// The user asked to try a failed boot again (§9.7).
    RetryRequested(PathBuf),
}

/// The retained view behind one tab.
///
/// A tab holds both of its screens at once: the empty tab's choosers and history,
/// and the tab page's transcript and composer. Which one is drawn follows
/// [`TabState`], and because these are entities, moving between them keeps what
/// they hold.
pub struct TabContent {
    pub(crate) id: TabId,
    pub(crate) state: TabState,
    pub(crate) choosers: Choosers,
    pub(crate) history: Entity<ListState<HistoryList>>,
    pub(crate) transcript: Entity<TranscriptView>,
    pub(crate) composer: Entity<Composer>,
    _subscriptions: Vec<Subscription>,
}

impl TabContent {
    /// A fresh tab: nothing chosen yet (§7.2). The choosers need the window they
    /// will be rendered in.
    pub fn new(id: TabId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let history =
            cx.new(|cx| ListState::new(HistoryList::new(placeholder_history()), window, cx));
        let subscription = cx.subscribe_in(
            &history,
            window,
            |_this, history, event: &ListEvent, _window, cx| {
                let ListEvent::Confirm(ix) = event else {
                    return;
                };
                let row = history.read(cx).delegate().row(ix.row).cloned();
                if let Some(row) = row {
                    cx.emit(TabContentEvent::HistoryRowActivated(row));
                }
            },
        );

        TabContent {
            id,
            state: TabState::Empty,
            choosers: Choosers::new(window, cx),
            history,
            transcript: cx.new(TranscriptView::new),
            composer: cx.new(|cx| Composer::new(window, cx)),
            _subscriptions: vec![subscription],
        }
    }

    pub fn id(&self) -> TabId {
        self.id
    }

    pub fn state(&self) -> &TabState {
        &self.state
    }

    /// The folder this tab works in, once one is chosen.
    pub fn folder(&self) -> Option<&Path> {
        match &self.state {
            TabState::Empty => None,
            TabState::Booting { folder }
            | TabState::Running { folder }
            | TabState::Failed { folder, .. } => Some(folder),
        }
    }

    /// The label on the tab: the folder's name, or "New tab" while empty.
    pub fn title(&self) -> SharedString {
        match self.folder() {
            Some(folder) => folder_name(folder),
            None => SharedString::from("New tab"),
        }
    }

    /// The tab's tooltip: the whole path plus what the tab is doing (§7.1).
    pub fn tooltip(&self) -> SharedString {
        let state = match &self.state {
            TabState::Empty => SharedString::from("no folder chosen"),
            TabState::Booting { .. } => SharedString::from("starting the swarm…"),
            TabState::Running { .. } => SharedString::from("swarm: running"),
            TabState::Failed { .. } => SharedString::from("failed to start"),
        };
        match self.folder() {
            Some(folder) => format!("{}\n{}", folder.display(), state).into(),
            None => state,
        }
    }

    /// The selected agent's transcript: the coordinator's until lanes exist
    /// (§7.3).
    pub fn transcript(&self) -> &Entity<TranscriptView> {
        &self.transcript
    }

    /// The coordinator's composer: the input, the readout and the one action
    /// button (§7.3).
    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// The rows the history region currently lists.
    pub fn history_rows<'a>(&self, cx: &'a App) -> &'a [HistoryRow] {
        self.history.read(cx).delegate().rows()
    }

    /// The coordinator model (`--model`) this tab starts its swarm with (§3,
    /// §7.2).
    pub fn coordinator_model(&self, cx: &App) -> SharedString {
        self.choosers.coordinator_model(cx)
    }

    /// The lanes' default model, recorded in the folder's `swarm.lisp` (§9.6).
    pub fn lanes_model(&self, cx: &App) -> SharedString {
        self.choosers.lanes_model(cx)
    }

    /// The worker count (`--workers`), or `Default` for evo's own (§7.2).
    pub fn workers(&self, cx: &App) -> SharedString {
        self.choosers.workers(cx)
    }

    /// Move into [`TabState::Booting`] for `folder` (§7.2).
    ///
    /// Spawning the swarm comes with the client wiring; until then the tab
    /// shows the boot placeholder and stops there.
    pub fn begin_boot(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.state = TabState::Booting { folder };
        cx.notify();
    }

    /// Move into [`TabState::Running`]: the coordinator is up (§3, §7.3).
    pub fn mark_running(&mut self, cx: &mut Context<Self>) {
        if let TabState::Booting { folder } = &self.state {
            self.state = TabState::Running {
                folder: folder.clone(),
            };
            cx.notify();
        }
    }

    /// Move into [`TabState::Failed`], keeping the log tail a boot failure
    /// shows in the tab (§3, §9.7).
    pub fn fail(&mut self, log_tail: String, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder() {
            self.state = TabState::Failed {
                folder: folder.to_path_buf(),
                log_tail,
            };
            cx.notify();
        }
    }

    /// Stop showing the failure and try the same folder again (§9.7).
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder() {
            self.begin_boot(folder.to_path_buf(), cx);
        }
    }
}

impl EventEmitter<TabContentEvent> for TabContent {}

impl Render for TabContent {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_for_state(cx)
    }
}
