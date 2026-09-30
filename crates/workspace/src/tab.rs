//! One tab: what a single swarm is doing, and the content the window shows for
//! it (§7.1, §7.2, §7.3).
//!
//! The tab strip belongs to [`crate::WorkspaceView`], which owns the tab set and
//! the selection. Everything below the strip belongs here: `TabContent` is the
//! retained view behind one tab, and it reports what it needs from the window by
//! emitting [`TabContentEvent`] rather than changing the tab set itself.
//!
//! A tab that has started a swarm is also the UI end of that swarm's engine:
//! [`TabContent::launch`] hands it a [`tab_engine`] engine and the channel its
//! [`Update`]s arrive on, and one task — the *pump* — carries them onto the UI
//! thread, folds each one into the tab's [`TabModel`], and pushes exactly the
//! [`Changes`] the model reports into the views that show them (§9.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::list::{ListEvent, ListState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Subscription, Task,
    Window,
};

use async_channel::Receiver;
use composer::{Composer, ComposerEvent};
use session::{
    AgentKey, Changes, ItemChange, LaunchPlan, Op, Queue, Status, StreamStatus, TabModel,
};
use swarm_client::{ErrorCode, OpError, OpReply};
use tab_engine::{EngineHandle, Update};
use transcript::TranscriptView;

use agent_list::{AgentList, AgentListEvent};

use crate::chrome::LauncherData;
use crate::empty_tab::{Choosers, HistoryList};
use crate::history::{folder_name, HistoryRow};
use crate::launch::{Launch, LaunchEnv};

/// How often the step clock re-renders while a turn runs. It is a UI ticker and
/// nothing else: no request, no state (§7.3).
const STEP_TICK: Duration = Duration::from_secs(1);

/// How long a notice above the composer stays: long enough to read a refusal,
/// short enough that it is gone before it becomes furniture (§4).
const NOTICE_LIFETIME: Duration = Duration::from_secs(4);

/// How many items one page of scrollback asks for (`GET /items?before=&limit=`, §5.4).
const PAGE_ITEMS: u32 = 100;

/// A transient line above the composer (§4, §9.2).
///
/// The server answered a POST with something other than 200: `409 not now` is
/// the server saying it cannot take this right now — dim, not a mistake — and
/// anything else is a failure, shown in the danger colour with the reply's own
/// words. Neither is ever a modal, and neither is re-validated here (§8): the
/// text is the server's.
///
/// A POST the server never answered has no words to show, so it gets [`notice_words`]'
/// plain sentence instead — and `detail` keeps the raw error text for the hover.
pub(crate) struct Notice {
    pub(crate) text: SharedString,
    /// The error behind a paraphrased line, shown on hover (§4): the socket's own
    /// words, kept rather than thrown away.
    pub(crate) detail: Option<SharedString>,
    pub(crate) tone: NoticeTone,
}

/// Which of the two a notice is (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    /// `409 not now`: busy, not wrong.
    Dim,
    /// `422`, `503` or a transport failure: something went wrong.
    Error,
}

impl NoticeTone {
    /// The place a test can look for: the two tones are different elements, so a
    /// test can tell a refusal from a failure without reading colours.
    pub(crate) fn element_id(self) -> &'static str {
        match self {
            NoticeTone::Dim => "composer-notice-dim",
            NoticeTone::Error => "composer-notice-error",
        }
    }
}

/// What a refused op reads as (§4).
///
/// The decision is the server's, not ours: `busy` and `not_quiescent` are the
/// server saying it cannot take this now — dim, not a mistake — and every other
/// code is a failure carrying the reply's own words.
pub(crate) fn notice_tone(error: &OpError) -> NoticeTone {
    match error.code {
        ErrorCode::Busy | ErrorCode::NotQuiescent => NoticeTone::Dim,
        _ => NoticeTone::Error,
    }
}

/// What a refused op says above the composer, and the raw text to keep for the
/// hover (§4).
///
/// A refusal is the server's own words, always: the reply's `message` is the
/// swarm talking about the request, and re-stating it is the one thing §8 forbids.
/// The server's messages never quote a user or config value (CONTRACT §5.5), so
/// there is nothing to scrub here either.
fn notice_words(error: &OpError) -> (String, Option<String>) {
    let text = if error.message.trim().is_empty() {
        format!("The swarm refused that ({:?}).", error.code)
    } else {
        error.message.clone()
    };
    let detail = error
        .detail
        .as_object()
        .filter(|detail| !detail.is_empty())
        .map(|detail| serde_json::Value::Object(detail.clone()).to_string());
    (text, detail)
}

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
    /// The swarm never came up, or one that had been up went away: why, and the
    /// tail of its log (§9.7).
    ///
    /// `message` is the engine's own one-line reason — a binary that is not
    /// there, a folder that cannot be written — which is what a person needs
    /// first; `log_tail` is the evidence behind it, and may be empty.
    Failed {
        folder: PathBuf,
        message: Option<String>,
        log_tail: String,
        /// Whether the swarm had answered `/health` before this. A boot that
        /// never came up and a swarm that died under a session someone was
        /// working in are both failures, and they are not the same thing to
        /// read (§9.7) — nor the same thing to Retry (see
        /// [`TabContent::retry`]).
        was_up: bool,
        /// The `tabs/<id>/` directory that log and the swarm's token live in,
        /// while the tab still knows it. The tail is shortened against it before
        /// it is shown — the raw log keeps its paths (§9.7).
        tab_dir: Option<PathBuf>,
    },
    /// The tab is being taken down — its swarm is running §3's ladder somewhere
    /// that is not the UI thread (§9.8).
    Stopping { folder: PathBuf },
}

/// What a tab's content asks the window for.
///
/// The workspace owns the tab set, so the content reports intent instead of
/// acting on it: a tab never adds, closes or reorders tabs itself.
#[derive(Clone, Debug, PartialEq)]
pub enum TabContentEvent {
    /// The user chose a folder, and the models and worker count to start with:
    /// this tab should boot a swarm there (§7.2).
    Launch { folder: PathBuf, plan: LaunchPlan },
    /// The user picked a past swarm: this tab resumes that session in its own
    /// folder (§7.2, §9.5).
    Resume {
        session_path: PathBuf,
        folder: PathBuf,
    },
    /// The user chose a folder and left every chooser at Default (§7.2).
    ///
    /// Kept while the empty tab moves to [`TabContentEvent::Launch`], which
    /// carries the plan.
    FolderPicked(PathBuf),
    /// The user asked to try a failed boot again (§9.7).
    RetryRequested(PathBuf),
    /// The user asked this tab to go, from a screen that owns no close button —
    /// a boot that failed (§9.7).
    CloseRequested,
    /// The run this tab's coordinator had in flight is over. The strip is what
    /// decides whether anyone was watching: a tab in the background keeps a dot
    /// until it is looked at (§7.1).
    RunFinished,
    /// This tab is drawing something else now — the choosers, a boot, the page, a
    /// failure. The screen the keyboard was on is gone with the old one, so the
    /// window moves it to whatever the new screen starts with (§7.1).
    ScreenChanged,
    /// Someone double-clicked the split between the middle column and one of the
    /// sides (§7.3): that side goes back to the width it starts at. The window
    /// owns the widths, so the gesture is reported rather than acted on.
    ResetPane(crate::panes::PaneSide),
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
    /// One transcript view **per agent**, created when that agent is first
    /// shown, so its rows, its markdown documents, its scroll position and its
    /// todo panel all belong to one agent and to one revision counter: the
    /// transcript's own state is per agent, so switching columns cannot leak one
    /// agent's scroll position, documents or todos into another's.
    pub(crate) transcripts: BTreeMap<AgentKey, Entity<TranscriptView>>,
    pub(crate) composer: Entity<Composer>,
    /// What every tab of this window starts its swarms with: the two binaries, the
    /// app's data root and the environment a hermetic run needs (§1).
    config: Arc<LaunchEnv>,
    /// The side columns' widths (§7.3), as the window has them: the same in
    /// every tab, so the page of a tab opened now looks like the one next to it.
    pub(crate) panes: store::app_state::Panes,
    /// The drag machinery behind those widths, shared by every page in the
    /// window (§7.3). A tab the window built gets one; a bare tab — the ones the
    /// unit tests open — has none, and its page keeps the widths it was given.
    pub(crate) pane_state: Option<Entity<gpui_kit::component::ResizableState>>,
    /// The swarm this tab is driving, once one has started.
    live: Option<Live>,
    /// The last launch, so a failed boot's Retry can ask for the same thing again
    /// (§9.7).
    last_launch: Option<Launch>,
    /// Why the tab's swarm is gone, when it went away on its own.
    gone: Option<SharedString>,
    /// What the server last refused, until it ages out (§4, §9.2).
    notice: Option<Notice>,
    /// The task that takes the notice away again.
    notice_task: Option<Task<()>>,
    /// The session the tab's swarm is writing to, once `/state` has said which it
    /// is: the `--resume` argument, and what the app persists (§9.5).
    session: Option<PathBuf>,
    /// The left column: the agents, drawn by `agent_list`, fed from the model
    /// (§7.3).
    pub(crate) agents: Entity<AgentList>,
    _subscriptions: Vec<Subscription>,
}

/// What one agent's transcript view has to be told, gathered from one batch of
/// changes before any view is touched.
#[derive(Default)]
struct ViewPlan {
    /// The whole item list.
    reset: Option<Vec<session::Item>>,
    /// Older items, paged in at the front.
    prepend: Option<Vec<session::Item>>,
    upsert: Vec<session::Item>,
    remove: Vec<session::ItemId>,
    /// The topic's state moved (the checklist, the segments).
    state: bool,
}

impl ViewPlan {
    fn is_empty(&self) -> bool {
        self.reset.is_none()
            && self.prepend.is_none()
            && self.upsert.is_empty()
            && self.remove.is_empty()
            && !self.state
    }
}

/// Everything the agent list draws, read off the model in one go (§7.3).
struct AgentsSnapshot {
    lanes: session::LaneList,
    activity: Status,
    reconnecting: bool,
    selected: AgentKey,
    /// The coordinator's own step clock, already formatted.
    clock: Option<String>,
    /// The moment both clocks are counted to (§7.3): a lane's step clock is an
    /// age as of the last read, and this is what it is counted on towards.
    now_millis: u64,
    /// Why a lane is down, by lane number (§9.7).
    down_reasons: Vec<(u32, Option<String>)>,
}

/// What one outstanding op was, so its reply is read for what it is: only a *send*'s
/// `ok` clears the draft — a stop is answered `ok` too, and the text the person was
/// typing is none of its business (§7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// `input.send` — the reply is the one that clears the draft.
    Send,
    /// A `run.interrupt`, from the button, from a lane's Stop, or from `Esc`.
    Interrupt,
}

/// One tab's live swarm: the engine, the model its updates land in, and the pump
/// that carries them across.
struct Live {
    /// Shared, because a transcript row's own ask (page older items, fetch one item
    /// whole, take a queued input back) is a `'static` handler that has to hold on to
    /// it. The handle is single-threaded, like the UI that calls it.
    engine: Rc<EngineHandle>,
    model: TabModel,
    /// The task that applies the engine's updates on the UI thread. Held only so
    /// it lives exactly as long as the tab does: dropping the tab — or handing
    /// the engine over to be stopped — cancels the pump.
    _pump: Task<()>,
    /// `tabs/<id>/` — where the swarm keeps its log and its ready file (§6).
    tab_dir: PathBuf,
    /// The ops a reply is still expected for, by the rid the engine minted, and what
    /// each was (§5.5). An op's refusal is shown whether or not it was the composer's.
    pending: BTreeMap<String, Pending>,
    /// The one stream's state, for the `reconnecting` badge (§5.3, §9.7).
    stream: StreamStatus,
    /// True once this tab's session has been recorded as a recent (§9.5).
    recorded: bool,
    /// True while that recording is out on its thread, so the `/state` resyncs
    /// that arrive meanwhile do not each start another one.
    recording: bool,
    /// The swarm process's pid, from `/health` (§3).
    pid: Option<u32>,
    /// The `tabs/<id>/` directory this tab's swarm keeps its files in (§6).
    store_id: store::paths::TabId,
    /// The one-second re-render that keeps the coordinator's step clock moving
    /// while a turn runs; `None` when nothing is running.
    ticker: Option<Task<()>>,
}

impl TabContent {
    /// A fresh tab: nothing chosen yet (§7.2). The choosers need the window they
    /// will be rendered in.
    pub fn new(
        id: TabId,
        config: Arc<LaunchEnv>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The rows come from the launcher's background scan (§9.5); the empty
        // tab fills this list as they arrive.
        let history = cx.new(|cx| ListState::new(HistoryList::new(Vec::new()), window, cx));
        let history_subscription = cx.subscribe_in(
            &history,
            window,
            |_this, history, event: &ListEvent, _window, cx| {
                let ListEvent::Confirm(ix) = event else {
                    return;
                };
                // A history row is a session plus the folder it ran in; opening
                // it resumes that swarm in a tab of its own (§9.5).
                let row = history.read(cx).delegate().row(ix.row).cloned();
                if let Some(row) = row {
                    cx.emit(TabContentEvent::Resume {
                        session_path: row.session_path,
                        folder: row.folder,
                    });
                }
            },
        );

        let composer = cx.new(|cx| Composer::new(window, cx));
        // The status line is the tab page's, under the transcript, where it can
        // name the agent being shown rather than the coordinator alone (§7.3):
        // the composer keeps the input and the button.
        composer.update(cx, |composer, cx| composer.set_show_readout(false, cx));
        let composer_subscription = cx.subscribe_in(
            &composer,
            window,
            |this, _composer, event: &ComposerEvent, window, cx| {
                this.on_composer_event(event.clone(), window, cx);
            },
        );

        // The agent list selects nothing on its own: it asks, and the tab decides
        // — the same path a click on the old placeholder took (§7.3).
        let agents = cx.new(AgentList::new);
        let agents_subscription = cx.subscribe(
            &agents,
            |this, _list, event: &AgentListEvent, cx| match event {
                AgentListEvent::Select(agent) => this.select_agent(*agent, cx),
                // The one human action on a lane: stop it (§7.4).
                AgentListEvent::StopLane(lane) => this.on_stop_lane(*lane),
            },
        );

        let mut tab = TabContent {
            id,
            state: TabState::Empty,
            choosers: Choosers::new(window, cx),
            history,
            transcripts: BTreeMap::new(),
            composer,
            config,
            panes: store::app_state::Panes::default(),
            pane_state: None,
            live: None,
            last_launch: None,
            gone: None,
            notice: None,
            notice_task: None,
            session: None,
            agents,
            _subscriptions: vec![
                history_subscription,
                composer_subscription,
                agents_subscription,
            ],
        };
        // The empty tab's check runs the binary this app would spawn, and says so
        // when it cannot: the app's own path, not the installed default (§9, §13).
        let bin = tab.config.swarm_bin.clone();
        tab.set_swarm_bin(bin, cx);
        tab
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
            | TabState::Failed { folder, .. }
            | TabState::Stopping { folder } => Some(folder),
        }
    }

    /// The session the tab's swarm is writing to, while `/state` has named it.
    pub fn session_path(&self) -> Option<&Path> {
        self.session.as_deref()
    }

    /// Put the keyboard where this tab's work starts (§7.1), and say whether
    /// there was anywhere to put it.
    ///
    /// A tab that drives a swarm is a tab someone means to type into, so the caret
    /// goes to its composer — the same thing opening a tab does. An empty tab has
    /// nothing to type into: its first thing to choose belongs to the empty tab
    /// (§7.2), and its focus handles are its own.
    pub fn focus_primary(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match &self.state {
            // The empty tab knows where its own keyboard goes (§7.2): the
            // coordinator chooser, or the folder card beside it.
            TabState::Empty => self.choosers.focus_primary(window, cx),
            // The composer is the only thing on the tab page to type into, and the
            // page is what a running tab shows.
            TabState::Running { .. } => {
                self.composer
                    .update(cx, |composer, cx| composer.focus_input(window, cx));
                true
            }
            // Starting, stopping, failed: nothing on the screen takes a keystroke,
            // and the window is better with the keyboard than a caret that is not
            // drawn.
            _ => false,
        }
    }

    /// The tab's own state, told to the window: a screen change is a keyboard
    /// change (§7.1).
    fn set_state(&mut self, state: TabState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.emit(TabContentEvent::ScreenChanged);
        }
    }

    /// The process behind the tab: the swarm's own pid, once `/health` has
    /// answered (§3). Diagnostics, and how §9.7's failure modes are exercised.
    pub fn swarm_pid(&self) -> Option<u32> {
        self.live.as_ref().and_then(|live| live.pid)
    }

    /// Give this tab's page the window's drag machinery for its side columns
    /// (§7.3): every tab's page is the same three columns, so they share one
    /// state and a split dragged in one tab moves in all of them.
    pub(crate) fn set_pane_state(
        &mut self,
        state: Entity<gpui_kit::component::ResizableState>,
        cx: &mut Context<Self>,
    ) {
        self.pane_state = Some(state);
        cx.notify();
    }

    /// The widths the window is showing its side columns at (§7.3).
    pub fn panes(&self) -> store::app_state::Panes {
        self.panes
    }

    /// The same widths, as the window changed them. The panels carry the resize
    /// themselves; this is what the page opens a tab with.
    pub(crate) fn set_panes(&mut self, panes: store::app_state::Panes, cx: &mut Context<Self>) {
        if self.panes != panes {
            self.panes = panes;
            cx.notify();
        }
    }

    /// The `tabs/<id>/` directory this tab's swarm writes to, while it has one.
    /// It is the id a stored tab set has to name, or the prune would not find the
    /// directory (§6, §9.8).
    pub fn store_id(&self) -> Option<&store::paths::TabId> {
        self.live.as_ref().map(|live| &live.store_id)
    }

    /// The tab's model, while it has a swarm to have one for.
    pub fn model(&self) -> Option<&TabModel> {
        self.live.as_ref().map(|live| &live.model)
    }

    /// Which agent the center column shows; the coordinator while there is no
    /// swarm to select in.
    pub fn selected_agent(&self) -> AgentKey {
        self.model()
            .map(|model| model.selected())
            .unwrap_or(AgentKey::Coordinator)
    }

    /// Whether the selected agent's stream is retrying, which the tab page shows
    /// as a badge and the tab's tooltip as "reconnecting" (§9.7).
    pub fn is_reconnecting(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.stream.is_reconnecting())
    }

    /// Whether the coordinator has a run in flight. A compaction counts: the swarm
    /// is busy either way, and the readout says which.
    ///
    /// This is the coordinator alone; the tab strip asks
    /// [`is_working`](Self::is_working), which is this plus the lanes.
    pub fn is_running(&self) -> bool {
        self.model().is_some_and(|model| model.activity().is_busy())
    }

    /// Whether this tab's swarm is working at all — a lane at work, or the
    /// coordinator's own run — which is what the strip's dot breathes to (§7.1).
    ///
    /// [`is_running`](Self::is_running) is the coordinator alone: a coordinator
    /// between turns with a lane still at work is a working swarm, and the strip
    /// is where a person looks to see whether anything is happening in a tab.
    /// ([`swarm_is_busy`] is the same rule the composer's Stop answers.)
    pub fn is_working(&self) -> bool {
        self.model().is_some_and(swarm_is_busy)
    }

    /// The label on the tab: the folder's name, or "New tab" while empty.
    pub fn title(&self) -> SharedString {
        match self.folder() {
            Some(folder) => folder_name(folder),
            None => SharedString::from("New tab"),
        }
    }

    /// The tab's tooltip: the whole path plus what the tab is doing (§7.1, §9.7).
    pub fn tooltip(&self) -> SharedString {
        let what = match &self.state {
            TabState::Empty => "no folder chosen",
            TabState::Booting { .. } => "starting the swarm…",
            TabState::Stopping { .. } => "stopping the swarm…",
            TabState::Failed { was_up: true, .. } => "swarm gone: the server exited",
            TabState::Failed { .. } => "failed to start",
            TabState::Running { .. } => match &self.gone {
                Some(reason) => reason.as_ref(),
                None if self.is_reconnecting() => "swarm: reconnecting…",
                None => "swarm: running",
            },
        };
        match self.folder() {
            Some(folder) => format!("{}\n{}", folder.display(), what).into(),
            None => SharedString::from(what),
        }
    }

    /// The selected agent's transcript view, once the tab has a swarm (§7.3).
    pub fn transcript(&self) -> Option<&Entity<TranscriptView>> {
        self.transcripts.get(&self.selected_agent())
    }

    /// The coordinator's composer: the input, the readout and the one action
    /// button (§7.3).
    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// The app's catalog and session list, forwarded to the empty tab that shows
    /// them (§5.6, §2).
    ///
    /// Everything here is data the app read once, for every tab: one catalog body
    /// (`evo-swarm catalog --json`), one session index, one clock. Nothing is fetched
    /// per tab.
    pub fn set_launcher_data(
        &mut self,
        data: &LauncherData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(catalog) = &data.catalog {
            self.set_catalog(catalog, window, cx);
        }
        self.set_catalog_error(data.catalog_error.clone(), cx);
        self.set_history_loading(data.history_loading, cx);
        if !data.history.is_empty() {
            self.set_history_entries(
                &data.history,
                data.now,
                data.offset_seconds,
                data.home.as_deref(),
                cx,
            );
        }
        self.set_history_error(data.history_error.clone(), cx);
        self.set_home(data.home.clone(), cx);
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

    // --- starting and stopping the swarm ---------------------------------

    /// Start this tab's swarm (§3).
    ///
    /// The tab directory and the folder's `swarm.lisp` block are written first, so
    /// a launch that cannot write them fails the tab instead of starting a swarm
    /// that would run the wrong lanes model (§9.6).
    pub fn launch(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        let folder = launch.folder().clone();
        match crate::launch::start(&self.config, &launch) {
            Ok(started) => {
                self.last_launch = Some(launch);
                self.gone = None;
                // A new swarm writes a new session; `/state` names it when it does.
                self.session = None;
                self.set_state(TabState::Booting { folder }, cx);
                self.attach(started, window, cx);
            }
            Err(error) => {
                // Nothing is running: show why, with what little there is to show.
                self.last_launch = Some(launch);
                self.set_state(
                    TabState::Failed {
                        folder,
                        message: Some(format!("could not prepare this tab: {error}")),
                        log_tail: String::new(),
                        was_up: false,
                        // The prep failed before a directory was made, and there
                        // is no log to shorten.
                        tab_dir: None,
                    },
                    cx,
                );
                cx.notify();
            }
        }
    }

    /// Try again after a failure (§9.7).
    ///
    /// A boot that never came up has nothing to go back to, so Retry is the same
    /// launch again. A swarm that *was* up left a session behind, and what a
    /// retry of that is for is the conversation rather than a new one: it
    /// resumes the session by path (§3's `--resume`). The file is the gate — evo
    /// writes the journal at the first assistant message, so a session that was
    /// never journalled cannot be opened, and that tab falls back to a fresh
    /// launch.
    pub fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resume = self
            .session
            .clone()
            .filter(|session| session.is_file())
            .zip(self.folder().map(Path::to_path_buf))
            .map(|(session, folder)| Launch::Resume { folder, session });
        if let Some(launch) = resume.or_else(|| self.last_launch.clone()) {
            self.launch(launch, window, cx);
        }
    }

    /// Take the engine out of the tab, so its swarm can be stopped somewhere that
    /// is not the UI thread (§9.8). The tab keeps what it shows; it stops
    /// watching and stops typing to the server.
    pub fn take_engine(&mut self, cx: &mut Context<Self>) -> Option<EngineHandle> {
        let live = self.live.take()?;
        if let Some(folder) = self.folder().map(Path::to_path_buf) {
            self.set_state(TabState::Stopping { folder }, cx);
            cx.notify();
        }
        // The handle is shared with whatever row asked for a read; when a view still
        // holds one, dropping the last reference is what closes the child's stdin —
        // the server's own signal to stop — so the tab has nothing more to hand over.
        Rc::try_unwrap(live.engine).ok()
    }

    /// Take a started swarm: keep its engine, start the pump, and show the boot
    /// screen until `/health` answers (§3).
    fn attach(
        &mut self,
        started: crate::launch::Started,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pump = self.spawn_pump(started.updates, window, cx);
        self.transcripts.clear();
        // The coordinator is what a fresh tab page shows (§7.3).
        let coordinator = cx.new(TranscriptView::new);
        coordinator.update(cx, |view, cx| view.set_agent(AgentKey::Coordinator, cx));
        self.transcripts
            .insert(AgentKey::Coordinator, coordinator.clone());
        self.live = Some(Live {
            engine: Rc::new(started.engine),
            model: TabModel::new(),
            _pump: pump,
            tab_dir: started.tab_dir,
            pending: BTreeMap::new(),
            stream: StreamStatus::Connected,
            recorded: false,
            recording: false,
            pid: None,
            store_id: started.store_id,
            ticker: None,
        });
        // The coordinator's rows can page back, fetch one item whole and take a queued
        // input back from the moment the tab has a server to ask (§5.4, §5.5).
        self.wire_view(&AgentKey::Coordinator, cx);
        let _ = window;
        cx.notify();
    }

    /// Give one agent's transcript the three asks a row can make: older items
    /// (`GET /items?before=`), one item whole (`GET /items/<id>`), and taking a queued
    /// input back (`input.cancel`).
    ///
    /// Each is a read or an op on that agent's own topic, sent by the engine. A view
    /// whose callbacks are never installed simply offers nothing to click: the history
    /// header is drawn only when the owner said there is more, and a tool row's "load
    /// the whole output" only when there is a way to fetch it.
    fn wire_view(&self, agent: &AgentKey, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let Some(view) = self.transcripts.get(agent).cloned() else {
            return;
        };
        let topic = agent.topic();
        let engine = live.engine.clone();
        let page_topic = topic.clone();
        let item_topic = topic.clone();
        let cancel_topic = topic;
        view.update(cx, |view, cx| {
            view.on_load_older(
                move |oldest, _window, _cx| {
                    engine.page(&page_topic, Some(oldest), PAGE_ITEMS);
                },
                cx,
            );
            let engine = live.engine.clone();
            view.on_fetch_item(
                move |id, _window, _cx| {
                    engine.item(&item_topic, id);
                },
                cx,
            );
            let engine = live.engine.clone();
            view.on_cancel_input(
                move |id, _window, _cx| {
                    engine.request(session::OpRequest::input_cancel(id));
                },
                cx,
            );
            let engine = live.engine.clone();
            view.on_fetch_image(
                move |id, n, _window, _cx| {
                    engine.media(&cancel_topic, id, n);
                },
                cx,
            );
        });
    }

    /// The task that carries the engine's updates onto the UI thread (§9.1).
    ///
    /// Everything already queued is applied in one turn of the loop, so a burst
    /// of `text-delta`s costs one task poll and one render instead of one each.
    fn spawn_pump(
        &self,
        updates: Receiver<Update>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                let mut batch = vec![update];
                while let Ok(queued) = updates.try_recv() {
                    batch.push(queued);
                }
                let applied = this.update_in(cx, |tab, window, cx| {
                    for update in batch {
                        tab.apply(update, window, cx);
                    }
                });
                if applied.is_err() {
                    // The tab is gone; there is nothing left to apply to.
                    break;
                }
            }
        })
    }

    /// One update from the engine, applied to the model and to the views (§9.1).
    fn apply(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        // What the strip's dot is about: a run that was in flight and is over (§7.1).
        let was_running = self.is_running();
        match update {
            Update::Booting => {}
            Update::Ready { pid, session, .. } => {
                if let Some(live) = self.live.as_mut() {
                    live.pid = Some(pid);
                }
                // The session the swarm is writing to is what history resumes (§2).
                self.session = Some(PathBuf::from(&session.path));
                self.record_recent(cx);
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    self.set_state(TabState::Running { folder }, cx);
                    cx.notify();
                }
            }
            Update::BootFailed { reason, log_tail } => {
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    let tab_dir = self.live.as_ref().map(|live| live.tab_dir.clone());
                    self.set_state(
                        TabState::Failed {
                            folder,
                            message: Some(reason),
                            log_tail,
                            was_up: false,
                            tab_dir,
                        },
                        cx,
                    );
                    cx.notify();
                }
            }
            // One topic of a snapshot, and one stream frame: both go straight into
            // the model, which is the only thing that reads them (§5.2, §5.3).
            Update::Snapshot { topic, body } => {
                let changes = self
                    .live
                    .as_mut()
                    .map(|live| live.model.on_snapshot(&topic, &body));
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
                self.session_from_state(cx);
            }
            Update::Op { topic, op } => {
                let changes = self.live.as_mut().map(|live| live.model.on_op(&topic, &op));
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
                if let Op::TopicReset { reason } = &op {
                    if reason == "lane_restarted" {
                        if let Some(lane) = topic.strip_prefix("lane:").and_then(|n| n.parse().ok())
                        {
                            let changes = self
                                .live
                                .as_mut()
                                .map(|live| live.model.on_lane_restarted(lane));
                            if let Some(changes) = changes {
                                self.push(changes, cx);
                            }
                        }
                    }
                }
            }
            Update::Stream { status } => {
                if let Some(live) = self.live.as_mut() {
                    live.stream = status;
                }
                self.sync_agents(cx);
            }
            // Older items, paged in at the front: the scrollback walking back
            // through compactions (§5.4).
            Update::ItemsBefore { topic, body } => {
                let changes = self
                    .live
                    .as_mut()
                    .map(|live| live.model.on_items_before(&topic, &body));
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
            }
            // One item, whole: a tool row's untruncated output (§5.4). The view
            // holding that row takes it.
            Update::Item { topic, body } => self.on_full_item(&topic, &body, cx),
            // Image bytes, decoded once on a thread of its own and handed to the row
            // that asked for them (§5.4).
            Update::Media {
                topic,
                id,
                n,
                bytes,
                ..
            } => self.on_media(&topic, &id, n, bytes, cx),
            // A read that failed. The view stays as it was, and one quiet line says
            // what could not be fetched: nothing is retried (§5.4).
            Update::FetchFailed { what, reason } => {
                self.show_notice(
                    format!("Could not read {what}."),
                    Some(reason),
                    NoticeTone::Dim,
                    cx,
                );
            }
            Update::OpReply { rid, op, reply } => self.on_op_reply(&rid, &op, *reply, window, cx),
            Update::ServerGone => self.server_gone(window, cx),
            Update::Exited { outcome } => {
                // The engine stopped. A tab being closed never sees this; one that
                // is still on screen says so and keeps what it has.
                self.gone = Some(format!("swarm stopped ({outcome:?})").into());
                cx.notify();
            }
        }
        // The run ended with this update. The tab does not know whether anyone was
        // watching it go — that is the window's business — so it says so and the
        // strip decides (§7.1).
        if was_running && !self.is_running() {
            cx.emit(TabContentEvent::RunFinished);
        }
    }

    /// The session the swarm is writing to, once a snapshot has named it (§2).
    fn session_from_state(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let path = live
            .model
            .state(session::AgentKey::Coordinator)
            .and_then(|state| state.session.as_ref())
            .map(|session| PathBuf::from(&session.path));
        if let Some(path) = path.filter(|path| Some(path) != self.session.as_ref()) {
            self.session = Some(path);
            self.record_recent(cx);
        }
    }

    /// Hand the model's [`Changes`] to the views that show them (§9.1).
    ///
    /// The changes are keyed by topic, which is what the views are keyed by: one
    /// transcript per agent, one status line for the selected one, and one lane list
    /// for the whole swarm.
    fn push(&mut self, changes: Changes, cx: &mut Context<Self>) {
        if self.live.is_none() {
            return;
        }
        // Read everything the views need first, and let the model's borrow end: the
        // views are updated through `cx`, which needs `&mut self`.
        let mut plans: Vec<(AgentKey, ViewPlan)> = Vec::new();
        {
            let live = self.live.as_ref().expect("checked above");
            for (topic, topic_changes) in &changes.topics {
                let Some(agent) = AgentKey::from_topic(topic) else {
                    continue;
                };
                let mut plan = ViewPlan::default();
                if topic_changes.reset {
                    plan.reset = Some(live.model.items(agent).to_vec());
                }
                if topic_changes.prepended > 0 {
                    plan.prepend =
                        Some(live.model.items(agent)[..topic_changes.prepended].to_vec());
                }
                for change in topic_changes.items() {
                    match change {
                        ItemChange::Upsert { id, .. } => {
                            if let Some(item) =
                                live.model.items(agent).iter().find(|item| &item.id == id)
                            {
                                plan.upsert.push(item.clone());
                            }
                        }
                        ItemChange::Remove { id } => plan.remove.push(id.clone()),
                    }
                }
                if topic_changes.state {
                    plan.state = true;
                }
                if !plan.is_empty() {
                    plans.push((agent, plan));
                }
            }
            // A topic the model holds but the tab has never shown still has a view
            // once anything happens to it, so nothing is dropped on the floor.
            for (agent, _) in &plans {
                self.transcripts
                    .entry(*agent)
                    .or_insert_with(|| cx.new(TranscriptView::new))
                    .clone()
                    .update(cx, |view, cx| view.set_agent(*agent, cx));
            }
        }
        // A view that has just been made knows whose transcript it is, and what its
        // rows may ask for.
        let fresh: Vec<AgentKey> = plans.iter().map(|(agent, _)| *agent).collect();
        {
            for agent in fresh {
                self.wire_view(&agent, cx);
            }
        }

        for (agent, plan) in plans {
            let Some(view) = self.transcripts.get(&agent).cloned() else {
                continue;
            };
            // What the topic says about the history behind it, read before the view is
            // touched.
            let has_older = self
                .live
                .as_ref()
                .and_then(|live| live.model.agent_topic(agent))
                .is_some_and(|topic| topic.has_older());
            view.update(cx, |view, cx| {
                if let Some(items) = plan.reset {
                    view.replace(items, cx);
                    view.set_history(has_older, view.is_loading_older(), cx);
                }
                if let Some(older) = plan.prepend {
                    view.prepend(older, cx);
                    // The page the reader asked for has landed: the header stops saying
                    // it is in flight, and knows whether there is more behind it.
                    view.set_history(has_older, false, cx);
                }
                for id in plan.remove {
                    view.remove(&id, cx);
                }
                for item in plan.upsert {
                    view.upsert(item, cx);
                }
            });
        }

        let selected = self.live.as_ref().expect("checked above").model.selected();
        let selected_changed = changes
            .for_topic(&selected.topic())
            .is_some_and(|topic| topic.state)
            || changes.selection;
        if selected_changed {
            let todos = self
                .live
                .as_ref()
                .expect("checked above")
                .model
                .selected_todos()
                .to_vec();
            if let Some(view) = self.transcripts.get(&selected).cloned() {
                view.update(cx, |view, cx| view.set_todos(todos, cx));
            }
        }
        let live = self.live.as_ref().expect("checked above");
        let (left, right) = session::ordered_segments(live.model.selected_segments());
        let left: Vec<session::Segment> = left.into_iter().cloned().collect();
        let right: Vec<session::Segment> = right.into_iter().cloned().collect();
        let swarm_busy = swarm_is_busy(&live.model);
        self.composer.update(cx, |composer, cx| {
            composer.set_segments(&left, &right, cx);
            composer.set_swarm_busy(swarm_busy, cx);
        });
        // The left column takes the same facts, on the same batch (§7.3).
        self.sync_agents(cx);
        cx.notify();
    }

    /// Feed the agent list from the model: the rows, the coordinator's status and step
    /// clock, the selection, and why each down lane is down (§7.3, §9.7).
    fn sync_agents(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.agents_snapshot() else {
            return;
        };
        self.agents.update(cx, |list, cx| {
            list.set_now(snapshot.now_millis, cx);
            list.set_lanes(&snapshot.lanes, cx);
            list.set_coordinator(snapshot.activity, snapshot.reconnecting, cx);
            list.set_coordinator_clock(snapshot.clock, cx);
            list.set_selected(snapshot.selected, cx);
            for (lane, reason) in snapshot.down_reasons {
                list.set_down_reason(lane, reason, cx);
            }
        });
    }

    /// The agent list's inputs, read off the model in one go: the list and the
    /// model are different entities, so the model's borrow ends here.
    fn agents_snapshot(&self) -> Option<AgentsSnapshot> {
        let live = self.live.as_ref()?;
        let model = &live.model;
        let selected = model.selected();
        // One reading of the clock for both of them: the coordinator's and the
        // lanes' are the same moment (§7.3).
        let now = now_millis();
        Some(AgentsSnapshot {
            lanes: model.lane_list().clone(),
            activity: model.activity(),
            reconnecting: live.stream.is_reconnecting(),
            selected,
            clock: model
                .selected_state()
                .and_then(|state| state.step_clock(now)),
            now_millis: now,
            down_reasons: model
                .lane_rows()
                .iter()
                .map(|lane| (lane.n, model.lane_down_reason(lane.n)))
                .collect(),
        })
    }

    /// Whether anything on the page is counting seconds (§7.3): the coordinator's own
    /// step, or a lane that is working or compacting.
    fn stepping(&self) -> bool {
        self.model().is_some_and(clocks_running)
    }

    /// Start the once-a-second re-render while anything has a clock to move, and stop it
    /// when nothing has (§7.3).
    fn ensure_step_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A ticker that has ended is not a ticker: the next step starts a new one.
        let ticker_live = self
            .live
            .as_ref()
            .and_then(|live| live.ticker.as_ref())
            .is_some_and(|ticker| !ticker.is_ready());
        if !self.stepping() || ticker_live {
            return;
        }

        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(STEP_TICK).await;
                let stepped = this.update_in(cx, |tab, _window, cx| {
                    let stepping = tab.stepping();
                    if stepping {
                        // The clocks are the only thing that changed, and they count
                        // from the absolute starts the server published (§7.3).
                        tab.sync_agents(cx);
                        cx.notify();
                    }
                    stepping
                });
                match stepped {
                    Ok(true) => {}
                    // The tab is gone, or the last clock stopped: stop ticking.
                    _ => break,
                }
            }
        });
        if let Some(live) = self.live.as_mut() {
            live.ticker = Some(ticker);
        }
    }

    /// How long the selected agent's current step has been running, in seconds (§7.3).
    pub fn step_seconds(&self) -> Option<u64> {
        let started = self
            .model()?
            .selected_state()?
            .task
            .as_ref()
            .map(|task| task.step_started_at.max(task.started_at))
            .filter(|started| *started > 0)?;
        Some(now_millis().saturating_sub(started) / 1000)
    }

    /// Show AGENT's transcript in the center column — and watch only the lane
    /// being shown (§9.3, §14.4).
    pub fn select_agent(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.selected_agent() == agent {
            // Clicking the row already shown changes nothing — and reopening the
            // same lane's stream would only re-seed it (§9.3).
            return;
        }
        let changes = {
            let Some(live) = self.live.as_mut() else {
                return;
            };
            // One stream carries every topic (§5.3), so selecting an agent is the
            // view's business alone: nothing new is subscribed to.
            live.model.select(agent)
        };
        // An agent's view exists from the moment it is first shown, and lives as
        // long as the tab: switching back keeps its scroll and its documents.
        let created = !self.transcripts.contains_key(&agent);
        let view = self
            .transcripts
            .entry(agent)
            .or_insert_with(|| cx.new(TranscriptView::new))
            .clone();
        // The view says whose transcript it is, which is what an empty one shows.
        view.update(cx, |view, cx| view.set_agent(agent, cx));
        if created {
            self.wire_view(&agent, cx);
        }
        self.push(changes, cx);
        self.sync_agents(cx);
    }

    /// What the composer asked for: a turn, a stop of the whole swarm, or a stop of the
    /// coordinator's own run (§7.3, §5.5).
    ///
    /// Each is one [`OpRequest`] handed to the engine, which POSTs it; the reply comes
    /// back as [`Update::OpReply`]. The reader's words appear when the server adds the
    /// item — an `item.add` on topic `session` — so nothing is drawn here that the
    /// server has not taken.
    fn on_composer_event(
        &mut self,
        event: ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sent = match self.live.as_mut() {
            Some(live) => {
                let (request, pending) = match event {
                    ComposerEvent::Send(text) => {
                        // A run in flight takes the words at its next boundary; an idle
                        // coordinator runs them now (§5.5).
                        let queue = if live.model.activity() == Status::Idle {
                            Queue::Now
                        } else {
                            Queue::AfterRun
                        };
                        (live.model.send_input(&text, queue), Pending::Send)
                    }
                    ComposerEvent::StopSwarm => (live.model.interrupt_swarm(), Pending::Interrupt),
                    ComposerEvent::Interrupt => {
                        (live.model.interrupt_session(), Pending::Interrupt)
                    }
                };
                // The engine mints the request's id, and the reply comes back tagged
                // with it: that is what says which op was answered (§5.5).
                match live.engine.request(request) {
                    Some(rid) => {
                        live.pending.insert(rid, pending);
                        true
                    }
                    None => false,
                }
            }
            // No swarm: there is nothing to send to, and the button must not stay
            // disabled waiting for a reply that cannot come.
            None => false,
        };
        if !sent {
            self.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            });
        }
    }

    /// A lane's Stop, from the agent list: `run.interrupt` with scope `lane`, which is
    /// the one human action a lane answers to (CONTRACT §7.4).
    fn on_stop_lane(&mut self, lane: u32) {
        if let Some(live) = self.live.as_mut() {
            let request = live.model.interrupt_lane(lane);
            live.engine.request(request);
        }
    }

    /// The bytes of one image, from `GET /media/<id>/<n>`: decoded once, on a thread of
    /// its own (a megabyte of PNG is not a frame's work), and handed to the row that asked
    /// for it.
    fn on_media(&mut self, topic: &str, id: &str, n: u32, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Some(agent) = AgentKey::from_topic(topic) else {
            return;
        };
        let (bridge, _worker) = crate::bridge::Bridge::spawn(
            crate::bridge::Revision::new(0),
            move |updates: crate::bridge::BridgeSender<Option<Arc<gpui_kit::RenderImage>>>| {
                let image = transcript::decode_image(&bytes);
                let _ = updates.send(image);
            },
        );
        let id = id.to_string();
        bridge
            .drive_into(
                cx,
                |_tab: &TabContent| crate::bridge::Revision::new(0),
                move |tab, image: Option<Arc<gpui_kit::RenderImage>>, cx| {
                    let Some(view) = tab.transcripts.get(&agent).cloned() else {
                        return;
                    };
                    let id = id.clone();
                    view.update(cx, |view, cx| match image {
                        Some(image) => view.set_image(&id, n, image, cx),
                        None => view.set_image_failed(&id, n, cx),
                    });
                },
            )
            .detach();
    }

    /// The whole of one item, as `GET /items/<id>` answered: the row that asked for it
    /// takes the untruncated text (§5.4).
    fn on_full_item(&mut self, topic: &str, body: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(agent) = AgentKey::from_topic(topic) else {
            return;
        };
        let Some(item) = body.get("item").and_then(session::Item::from_json) else {
            return;
        };
        let session::ItemKind::Tool(tool) = &item.kind else {
            return;
        };
        let Some(result) = tool.result.as_ref() else {
            return;
        };
        if let Some(view) = self.transcripts.get(&agent).cloned() {
            let (id, text) = (item.id.clone(), result.text.clone());
            view.update(cx, |view, cx| view.set_full_result(&id, text, cx));
        }
    }

    /// Put a transient line above the composer, and start the clock that takes it
    /// away again (§4, §9.2).
    ///
    /// A newer notice replaces an older one; the task that would have cleared the
    /// older one finds different words and leaves the new one alone.
    fn show_notice(
        &mut self,
        text: String,
        detail: Option<String>,
        tone: NoticeTone,
        cx: &mut Context<Self>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let text = SharedString::from(text);
        self.notice = Some(Notice {
            text: text.clone(),
            detail: detail.map(SharedString::from),
            tone,
        });
        cx.notify();
        // The notice needs no window of its own: it clears itself wherever the tab
        // is showing, or is dropped with the tab.
        self.notice_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_LIFETIME).await;
            let _ = this.update(cx, |tab, cx| {
                if tab.notice.as_ref().map(|notice| notice.text.clone()) == Some(text) {
                    tab.notice = None;
                    cx.notify();
                }
            });
        }));
    }

    /// What the composer shows above itself, while there is something to say.
    pub(crate) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The line above the composer, while there is something to say (§4).
    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|notice| notice.text.as_ref())
    }

    /// The raw error behind that line, when it had to be paraphrased: the text
    /// the notice's tooltip carries (§4).
    pub fn notice_detail(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .and_then(|notice| notice.detail.as_deref())
    }

    /// An op's reply arrived: the composer takes its outcome, and a refusal is shown
    /// from the reply's own words — never re-validated here (§5.5, §8).
    ///
    /// Only a *send* may clear the draft: a stop is answered `ok` too, and it leaves the
    /// half-written text where it was (§7.3).
    fn on_op_reply(
        &mut self,
        rid: &str,
        op: &str,
        reply: OpReply,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked = self.live.as_mut().and_then(|live| live.pending.remove(rid));
        if let Some(asked) = asked {
            if asked == Pending::Send {
                self.composer.update(cx, |composer, cx| {
                    composer.request_finished(reply.ok, window, cx)
                });
            } else {
                // A stop's reply only re-arms the button; the draft is untouched.
                self.composer.update(cx, |composer, cx| {
                    composer.request_finished(false, window, cx)
                });
            }
        }
        if let Some(error) = reply.error() {
            let tone = notice_tone(error);
            let (text, detail) = notice_words(error);
            // Which op was refused is the reader's to know: `input.cancel` and
            // `run.interrupt` look nothing alike.
            self.show_notice(format!("{op}: {text}"), detail, tone, cx);
        }
    }

    /// The swarm's server exited on its own (§3, §9.7): the tab shows why, from
    /// the log the server was writing to.
    fn server_gone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(log) = self
            .live
            .as_ref()
            .map(|live| live.tab_dir.join("swarm.log"))
        else {
            return;
        };
        self.gone = Some("swarm: the server exited".into());
        // Reading the log is I/O, so it happens on a thread of its own and comes
        // back through the bridge like every other result.
        let (bridge, _worker) =
            crate::bridge::Bridge::spawn(crate::bridge::Revision::new(0), move |updates| {
                let _ = updates.send(swarm_client::log_tail(&log, 40));
            });
        bridge
            .drive_into(
                cx,
                |_tab: &TabContent| crate::bridge::Revision::new(0),
                |tab, log_tail: String, cx| tab.fail_with_log(log_tail, cx),
            )
            .detach();
        let _ = window;
    }

    /// Record the session this tab started, once it is really on disk, so the
    /// app's own swarms appear in history (§9.5).
    ///
    /// `/state` names the session as soon as the swarm is up, but evo writes the
    /// journal at the *first assistant message*: a recent recorded from the name
    /// alone is a row `--resume` cannot open. The file is therefore the gate, and
    /// it is re-checked on every `/state` until it is there — a `stat` in front of
    /// a write that both happen on a thread of their own, like every other piece
    /// of file I/O the UI asks for.
    fn record_recent(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if session.as_os_str().is_empty() {
            return;
        }
        let folder = match self.folder() {
            Some(folder) => folder.to_path_buf(),
            None => return,
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if live.recorded || live.recording {
            return;
        }
        live.recording = true;
        let models = self
            .last_launch
            .as_ref()
            .and_then(|launch| launch.plan())
            .map(|plan| store::tab::TabModels {
                coordinator: plan.model.as_ref().map(|(id, _)| id.clone()),
                lanes: plan.lanes_model.as_ref().map(|(id, _)| id.clone()),
            })
            .unwrap_or_default();
        let lanes = self
            .last_launch
            .as_ref()
            .and_then(|launch| launch.plan())
            .and_then(|plan| plan.workers)
            .map(u32::from)
            .or_else(|| live.model.swarm().map(|swarm| swarm.workers as u32))
            .unwrap_or_default();

        let mut recent = store::app_state::Recent::new(session.clone(), folder, lanes);
        recent.models = models;
        let root = self.config.root.clone();
        let (bridge, _worker) = crate::bridge::Bridge::<bool>::spawn(
            crate::bridge::Revision::new(0),
            move |updates: crate::bridge::BridgeSender<bool>| {
                if !session.is_file() {
                    // Nothing has been journalled yet, so there is nothing to
                    // resume: the `/state` that names the session is not enough.
                    let _ = updates.send(false);
                    return;
                }
                let mut state = store::app_state::AppState::load(&root);
                state.touch_recent(recent);
                let _ = state.save(&root);
                let _ = updates.send(true);
            },
        );
        bridge
            .drive_into(
                cx,
                |_tab: &TabContent| crate::bridge::Revision::new(0),
                |tab, recorded: bool, _cx| {
                    if let Some(live) = tab.live.as_mut() {
                        live.recording = false;
                        // Only a real record stops the next `/state` from asking
                        // again.
                        live.recorded |= recorded;
                    }
                },
            )
            .detach();
    }

    /// Show a boot-style failure for a swarm that went away after it was up.
    fn fail_with_log(&mut self, log_tail: String, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder().map(Path::to_path_buf) {
            let tab_dir = self.live.as_ref().map(|live| live.tab_dir.clone());
            // A swarm that died after it was up has no one-line reason: the log
            // is the whole story.
            self.set_state(
                TabState::Failed {
                    folder,
                    message: None,
                    log_tail,
                    // It answered /health: this is a swarm that went away, not
                    // one that never came up (§9.7).
                    was_up: true,
                    tab_dir,
                },
                cx,
            );
            cx.notify();
        }
    }
}

/// Whether the composer's button should be offering to stop the swarm (§7.5).
///
/// The swarm topic's own flags count *lanes*: its `busy` is how many lanes are
/// working, and its `waiting_on_lanes` is false while the coordinator itself is
/// working. A coordinator-only turn is busy by this model's own account, and
/// nothing else on the page says so — which is the run a person most often wants
/// to stop.
fn swarm_is_busy(model: &TabModel) -> bool {
    model.lanes_busy() || model.activity() != Status::Idle
}

/// Whether this tab has a clock to move (§7.3).
///
/// Both clocks on the page count from an absolute start the server published — the
/// coordinator's `task.step_started_at` and a lane's — so between updates they are
/// counted on by the frame the tab draws. This is the one question that decides whether
/// a frame a second is worth paying for: a busy lane's clock is as much a reason as the
/// coordinator's own step, and a page where neither is running has nothing to redraw.
fn clocks_running(model: &TabModel) -> bool {
    model.activity().is_busy() || model.lane_rows().iter().any(|lane| lane.is_busy())
}

impl EventEmitter<TabContentEvent> for TabContent {}

impl Render for TabContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A step that is running needs a frame a second to show it moving; the
        // ticker ends itself the moment it is not, and asks nothing of the
        // server (§7.3).
        self.ensure_step_ticker(window, cx);
        self.render_for_state(cx)
    }
}

/// Epoch milliseconds, for the step clock.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, px, size, AnyWindowHandle, Bounds, Entity, TestAppContext, WindowBounds,
        WindowOptions,
    };
    use swarm_client::ErrorCode;

    /// A tab showing a page of a swarm: what a refusal's line needs, and nothing more.
    fn running_tab(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TabContent>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1000.), px(700.)),
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let tab = cx.new(|cx| {
                    TabContent::new(TabId::new(1), Arc::new(LaunchEnv::default()), window, cx)
                });
                tab.update(cx, |tab, _cx| {
                    tab.state = TabState::Running {
                        folder: PathBuf::from("/tmp/proj"),
                    };
                });
                tab
            })
            .expect("tab window")
        })
    }

    fn error(code: ErrorCode, message: &str) -> OpError {
        OpError {
            code,
            message: message.to_string(),
            detail: serde_json::Value::Null,
        }
    }

    /// What a refusal reads as, and in whose words (§4, §5.5).
    #[test]
    fn only_a_refusal_the_swarm_calls_busy_is_dim() {
        assert_eq!(
            notice_tone(&error(ErrorCode::Busy, "a run is in flight")),
            NoticeTone::Dim
        );
        assert_eq!(
            notice_tone(&error(ErrorCode::NotQuiescent, "a run is in flight")),
            NoticeTone::Dim
        );
        assert_eq!(
            notice_tone(&error(
                ErrorCode::AlreadySent,
                "that input was already taken"
            )),
            NoticeTone::Error
        );
        assert_eq!(
            notice_tone(&error(ErrorCode::Unknown, "who knows")),
            NoticeTone::Error
        );
    }

    /// A refusal is the server's own words, always (§8): nothing is re-stated, and
    /// the structured detail is kept for the hover rather than dropped.
    #[test]
    fn a_refusal_is_shown_as_the_server_wrote_it() {
        let (text, detail) = notice_words(&error(ErrorCode::Busy, "a run is in flight"));
        assert_eq!(text, "a run is in flight");
        assert_eq!(detail, None);

        let mut with_detail = error(ErrorCode::InvalidArgs, "the objective is empty");
        with_detail.detail = serde_json::json!({ "field": "objective" });
        let (text, detail) = notice_words(&with_detail);
        assert_eq!(text, "the objective is empty");
        assert_eq!(detail.as_deref(), Some(r#"{"field":"objective"}"#));

        // A server that says nothing still gets a line rather than an empty row.
        let (text, _) = notice_words(&error(ErrorCode::OpFailed, ""));
        assert!(text.contains("OpFailed"), "{text}");
    }

    /// §4: a refused op is a line above the composer, in the reply's own words, and
    /// never a modal — the page keeps its place and the line leaves on its own.
    #[gpui_kit::test]
    fn a_refused_op_is_a_line_above_the_composer(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.on_op_reply(
                    "r1",
                    "input.send",
                    OpReply {
                        rid: "r1".to_string(),
                        ok: false,
                        seq: 4,
                        result: serde_json::Value::Null,
                        error: Some(error(ErrorCode::Busy, "a run is in flight")),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the refused op");

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let line = window.find("composer-notice-dim");
            assert_eq!(line.label(), Some("input.send: a run is in flight"));
            assert!(
                window.try_find("composer-notice-error").is_none(),
                "a busy refusal is not an error"
            );
        })
        .expect("the dim line");

        // A failure is the error tone, and the page is still there.
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.on_op_reply(
                    "r2",
                    "goal.set",
                    OpReply {
                        rid: "r2".to_string(),
                        ok: false,
                        seq: 5,
                        result: serde_json::Value::Null,
                        error: Some(error(ErrorCode::InvalidArgs, "the objective is empty")),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the refused op");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("composer-notice-error").label(),
                Some("goal.set: the objective is empty")
            );
        })
        .expect("the error line");
    }

    /// §5.4: a read that failed is one quiet line, saying what could not be fetched
    /// and in the socket's or the server's own words. Nothing is retried.
    #[gpui_kit::test]
    fn a_read_that_failed_is_said_quietly(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.apply(
                    Update::FetchFailed {
                        what: "item e_3 in session".to_string(),
                        reason: "404 not_found".to_string(),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the failed read");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("composer-notice-dim").label(),
                Some("Could not read item e_3 in session.")
            );
        })
        .expect("the quiet line");
    }

    /// A clock is what makes a frame a second worth paying for, and it is the
    /// *absolute* starts the server publishes that decide it (§7.3).
    #[test]
    fn a_tab_with_nothing_running_has_no_clock_to_move() {
        let mut model = TabModel::new();
        assert!(!clocks_running(&model));

        model.on_snapshot(
            "session",
            &serde_json::json!({ "state": { "status": "running" }, "items": [] }),
        );
        assert!(clocks_running(&model), "the coordinator's own step counts");

        model.on_snapshot(
            "session",
            &serde_json::json!({ "state": { "status": "waiting" }, "items": [] }),
        );
        assert!(!clocks_running(&model));

        model.on_snapshot(
            "swarm",
            &serde_json::json!({ "state": { "id": "sw", "workers": 1, "status": { "busy": 1 }, "lanes": [
                { "n": 1, "state": "working", "reports": 0 }
            ] } }),
        );
        assert!(clocks_running(&model), "a busy lane's clock counts too");
    }

    /// §7.5: the button says `■ Stop swarm` while anything is going on — the
    /// coordinator's own run, its wait for the lanes, or a lane still working —
    /// and `Send` when nothing is.
    #[test]
    fn the_button_offers_to_stop_while_anything_is_going_on() {
        let mut model = TabModel::new();
        assert!(!swarm_is_busy(&model), "nothing is going on");

        // The coordinator's own run. The swarm topic says nothing about it: its
        // `busy` counts lanes, and `waiting_on_lanes` is false while the
        // coordinator works — so this one has to come from the session's status.
        model.on_snapshot(
            "session",
            &serde_json::json!({ "state": { "status": "running" }, "items": [] }),
        );
        assert!(swarm_is_busy(&model), "the coordinator's own run");

        model.on_snapshot(
            "session",
            &serde_json::json!({ "state": { "status": "waiting" }, "items": [] }),
        );
        assert!(swarm_is_busy(&model), "held for its lanes");

        model.on_snapshot(
            "session",
            &serde_json::json!({ "state": { "status": "idle" }, "items": [] }),
        );
        assert!(!swarm_is_busy(&model), "and back to Send");

        model.on_snapshot(
            "swarm",
            &serde_json::json!({ "state": { "id": "sw", "workers": 1, "status": { "busy": 1 }, "lanes": [
                { "n": 1, "state": "working", "reports": 0 }
            ] } }),
        );
        assert!(swarm_is_busy(&model), "a lane still working");
    }
}
