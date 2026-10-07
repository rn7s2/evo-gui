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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::list::{ListEvent, ListState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, IntoElement, Render, SharedString,
    Subscription, Task, Window,
};

use async_channel::Receiver;
use composer::{Candidate, Composer, ComposerEvent, ModelRow};
use session::{
    AgentKey, Changes, ItemChange, LaunchPlan, Op, Queue, Status, StreamStatus, TabModel,
};
use settings::ConfigEditor;
use store::ConfigScope;
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

/// How often a pump whose tab has no window asks for one again.
///
/// Every update it is holding is waiting on a frame that draws the tab — a page
/// rebuilt in another window, a window that has not been opened yet — and the wait
/// only ends when one arrives (§9.1). Short, because a tab that comes back is live
/// again by the next frame; it costs nothing while a tab has a window, which is
/// every tab a reader is looking at.
const PUMP_RETRY: Duration = Duration::from_millis(100);

/// Say, once, that a batch of updates never reached a window.
///
/// The one way this tab can lose an update: its entity was released while a batch
/// was in hand (a tab closed, its engine still writing). There is nothing else to
/// see from the outside — the tab is gone — so one line, and no more.
fn say_dropped(said: &AtomicBool, dropped: usize) {
    if said.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!("evo-desktop: a closed tab dropped {dropped} update(s) it had no window for");
}

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
///
/// One failure is not the server's at all, and not a refusal: a request the server
/// never answered. `tab_engine` marks that one in the reply's own `detail` (it is
/// the only thing that knows the answer was lost rather than refused), and it reads
/// dim for the same reason `busy` does — nothing was refused, so nothing is shown
/// as having failed to be taken. See `tab_engine::engine::lost_reply`.
pub(crate) fn notice_tone(error: &OpError) -> NoticeTone {
    if error
        .detail
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        == Some("unknown")
    {
        return NoticeTone::Dim;
    }
    match error.code {
        ErrorCode::Busy | ErrorCode::NotQuiescent => NoticeTone::Dim,
        _ => NoticeTone::Error,
    }
}

/// What a refused op says above the composer, and the raw text to keep for the
/// hover (§4).
///
/// A refusal is the server's own words, always: the reply's `message` is the
/// server talking about the request, and re-stating it is the one thing §8 forbids.
/// The server's messages never quote a user or config value (CONTRACT §5.5), so
/// there is nothing to scrub here either.
fn notice_words(error: &OpError) -> (String, Option<String>) {
    let text = if error.message.trim().is_empty() {
        format!("The session refused that ({:?}).", error.code)
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

/// What the composer attached, as the wire takes it ([`session::Attached`]) — the one
/// place the two crates' types meet, because a tile's own kind is the composer's
/// business and the op's payload is the session's.
///
/// Nothing is decided here: which of the three a tile is was decided when the reader
/// added it (a dropped file, a paste), and what each becomes on the wire is
/// [`session::attachment_turn`]'s.
fn attached(attachments: &[composer::Attachment]) -> Vec<session::Attached> {
    attachments
        .iter()
        .map(|attachment| match &attachment.kind {
            composer::AttachmentKind::ImageFile(path) => session::Attached::ImageFile(path.clone()),
            composer::AttachmentKind::ImageBytes { media_type, bytes } => {
                session::Attached::ImageBytes {
                    name: attachment.name.clone(),
                    media_type: media_type.clone(),
                    bytes: bytes.as_ref().clone(),
                }
            }
            composer::AttachmentKind::File(path) => session::Attached::File(path.clone()),
        })
        .collect()
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
    /// The app's own settings: the raw editors for the four files evo reads, in the
    /// evo home every session shares.
    ///
    /// This tab drives no swarm and owns no folder, so it is never started, never
    /// persisted and never waited on at quit — it is a screen in the window, and
    /// the window keeps at most one of it (§7.1).
    Settings,
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
    /// The user picked a past session: this tab resumes that journal in its own
    /// folder (§7.2, §9.5), with the program that wrote it — an `evo-agent` journal
    /// is a single agent's, and a swarm cannot run it.
    Resume {
        session_path: PathBuf,
        folder: PathBuf,
        swarm: bool,
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
    /// Someone double-clicked the split between the agent column and the
    /// conversation (§7.3): the column goes back to the width it starts at. The
    /// window owns the width, so the gesture is reported rather than acted on.
    ResetPane,
}

/// Which of the two screens a [`TabContent`] opens on (§7.1, §7.2): a New Swarm
/// page with a swarm behind it, or the app's own Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Swarm,
    Settings,
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
    /// The failure screen's log box scroll position: the kit's thumb is driven by
    /// a handle, and one made fresh each frame would put the box back at the top.
    pub(crate) log_scroll: gpui_kit::ScrollHandle,
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
    /// (§9.7) — and so a tab knows which program it started with (§7.2).
    pub(crate) last_launch: Option<Launch>,
    /// Why the tab's swarm is gone, when it went away on its own.
    gone: Option<SharedString>,
    /// What the server last refused, until it ages out (§4, §9.2).
    notice: Option<Notice>,
    /// The task that takes the notice away again.
    notice_task: Option<Task<()>>,
    /// The session the tab's swarm is writing to, once `/state` has said which it
    /// is: the `--resume` argument, and what the app persists (§9.5).
    session: Option<PathBuf>,
    /// Closed, and waiting for its swarm to exit: the page stays on screen under
    /// a layer that takes every click and keystroke, until the window removes
    /// the tab (§7.1).
    pub(crate) terminating: bool,
    /// Where the keyboard goes while the tab is terminating, so nothing on the
    /// frozen page — the composer above all — takes a keystroke.
    pub(crate) terminating_focus: FocusHandle,
    /// The left column: the agents, drawn by `agent_list`, fed from the model
    /// (§7.3).
    pub(crate) agents: Entity<AgentList>,
    /// The global Settings editor — the four files in the evo home every session
    /// shares. `Some` exactly on a [`TabState::Settings`] tab.
    pub(crate) settings: Option<Entity<ConfigEditor>>,
    /// The project's own editors, made the first time the folder row at the foot of
    /// the lane column is pressed and kept from then on, so a draft survives going
    /// back to a lane and returning (§7.3).
    pub(crate) project_editor: Option<Entity<ConfigEditor>>,
    /// The conversation column is showing the project's settings rather than the
    /// selected agent's transcript — which is what a press on the folder row does,
    /// and what selecting a lane or the coordinator undoes (§7.3).
    pub(crate) project_settings: bool,
    /// The thinking level the shown agent was last set to, kept beside the composer
    /// it was fed: the header's reveal reads it before any thinking text has arrived
    /// (§4.2, §7.3).
    ///
    /// It is the same reading the drawer gets — one topic state, taken in
    /// [`TabContent::sync_composer`] — because the drawer's own copy is the
    /// composer's private business.
    pub(crate) thinking_level: Option<String>,
    /// A close was asked for while an editor here holds unsaved changes: the page
    /// draws the Discard / Keep Editing question instead of the tab going.
    pub(crate) close_prompt: bool,
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
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    /// `input.send` — the reply is the one that clears the draft.
    Send,
    /// `run.interrupt`, from the button, from a lane's Stop, or from `Esc`.
    Interrupt,
    /// `model.set` or `thinking.set`, from a drawer. The composer's button has nothing
    /// to do with these: the reply only releases the composer's own in-flight flag.
    Settings,
    /// `command.run` — the slash command a whole message was. Its own output reaches
    /// this client twice: as `notices` on this reply, and as the session's `notice`
    /// items, the same lines word for word (the server publishes what it also
    /// answers). The transcript draws the items, so the reply's copy is not drawn —
    /// one line, one place — and what is read off the reply is `data.draft`, the text
    /// `/rewind` and `/tree` hand back for editing.
    Command,
    /// The `complete` op behind one half-typed word, carried with the question it
    /// asked about: what the server says is only ever an answer about *that* text, at
    /// *that* caret.
    Complete(Completing),
}

/// One question about a half-typed word: the text the caret was in, and where it was
/// in it (a byte offset), as the composer asked it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Completing {
    text: String,
    cursor: usize,
}

/// The questions an op map is still waiting on, taken out of it: the answers are not
/// coming any more, and the question a box asked is what its next one waits for
/// ([`composer::Composer::set_completion`]).
///
/// The rids go with them — nothing will be replied to under them, and a late reply
/// would be about a session that is over — and every other op is left where it is: a
/// stop or a send is released by its own reply, or by the tab going away with it.
fn waiting_completions(pending: &mut BTreeMap<String, Pending>) -> Vec<Completing> {
    let waiting: Vec<Completing> = pending
        .values()
        .filter_map(|pending| match pending {
            Pending::Complete(question) => Some(question.clone()),
            _ => None,
        })
        .collect();
    pending.retain(|_, pending| !matches!(pending, Pending::Complete(_)));
    waiting
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
    /// Whether this tab runs a swarm or one `evo-agent` (§7.2), taken from the launch
    /// it started with: a resumed `evo-agent` journal is that agent's, whatever the
    /// workers card's switch says now.
    swarm: bool,
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
        TabContent::build(id, config, Kind::Swarm, window, cx)
    }

    /// The window's Settings tab (§7.1): raw editors for the four files evo reads,
    /// in the home every session on this machine shares.
    ///
    /// It drives no swarm and owns no folder. The window keeps at most one of these
    /// ([`crate::WorkspaceView::open_settings_tab`]), it is never written into the
    /// stored tab set, and nothing about it is waited on at quit.
    pub fn new_settings(
        id: TabId,
        config: Arc<LaunchEnv>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        TabContent::build(id, config, Kind::Settings, window, cx)
    }

    /// Both kinds are one retained view with one screen swapped out; this is the
    /// shared half, so a Settings tab is a tab like any other on the strip.
    fn build(
        id: TabId,
        config: Arc<LaunchEnv>,
        kind: Kind,
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
                        swarm: row.swarm,
                    });
                }
            },
        );

        let composer = cx.new(|cx| Composer::new(window, cx));
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
                AgentListEvent::Select(agent) => {
                    // Choosing an agent is choosing a transcript: what it puts back
                    // after the project's settings were showing (§7.3).
                    this.hide_project_settings(cx);
                    this.select_agent(*agent, cx);
                    // A drawer is about the agent that was selected when it was
                    // opened: showing another agent's transcript folds it back, which
                    // is what a press on the row does by landing outside the box —
                    // and what the keyboard's own selection does here.
                    this.composer
                        .update(cx, |composer, cx| composer.close_drawer(cx));
                }
                // The one human action on a lane: stop it (§7.4).
                AgentListEvent::StopLane(lane) => this.on_stop_lane(*lane),
            },
        );

        let mut tab = TabContent {
            id,
            state: match kind {
                Kind::Swarm => TabState::Empty,
                Kind::Settings => TabState::Settings,
            },
            choosers: Choosers::new(window, cx),
            history,
            transcripts: BTreeMap::new(),
            composer,
            log_scroll: gpui_kit::ScrollHandle::default(),
            config,
            panes: store::app_state::Panes::default(),
            pane_state: None,
            live: None,
            last_launch: None,
            gone: None,
            notice: None,
            notice_task: None,
            session: None,
            terminating: false,
            terminating_focus: cx.focus_handle(),
            agents,
            // The global editor is the Settings tab's whole reason to exist; a swarm
            // tab has none until its folder row is pressed.
            settings: match kind {
                Kind::Swarm => None,
                Kind::Settings => {
                    Some(cx.new(|cx| ConfigEditor::new(ConfigScope::Global, window, cx)))
                }
            },
            project_editor: None,
            project_settings: false,
            thinking_level: None,
            close_prompt: false,
            _subscriptions: vec![
                history_subscription,
                composer_subscription,
                agents_subscription,
            ],
        };
        // The empty tab's check runs the binary this app would spawn, and says so
        // when it cannot: the app's own path, not the installed default (§9, §13).
        // A Settings tab starts no swarm and checks nothing.
        if kind == Kind::Swarm {
            let swarm = tab.config.swarm_bin.clone();
            let agent = tab.config.agent_bin.clone();
            tab.set_swarm_bin(swarm, cx);
            tab.set_agent_bin(agent, cx);
        }
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
            // Settings belongs to no folder: it edits the home every session shares.
            TabState::Empty | TabState::Settings => None,
            TabState::Booting { folder }
            | TabState::Running { folder }
            | TabState::Failed { folder, .. } => Some(folder),
        }
    }

    /// The session the tab's swarm is writing to, while `/state` has named it.
    pub fn session_path(&self) -> Option<&Path> {
        self.session.as_deref()
    }

    /// Whether this tab has `session` open: the one its swarm is writing to, or
    /// the one it was launched to resume while `/state` has not named it yet. A
    /// New Swarm page holds none.
    pub fn holds_session(&self, session: &Path) -> bool {
        if self.state == TabState::Empty {
            return false;
        }
        let wanted = same_file_key(session);
        let launched = self.last_launch.as_ref().and_then(Launch::session);
        [self.session.as_deref(), launched.map(PathBuf::as_path)]
            .into_iter()
            .flatten()
            .any(|held| same_file_key(held) == wanted)
    }

    /// Whether the tab was closed and its swarm is still on its way out.
    pub fn is_terminating(&self) -> bool {
        self.terminating
    }

    /// Close the tab's swarm and freeze the page until it has exited: the server is
    /// told to stop now (stdin EOF), and the layer goes over the page and takes the
    /// keyboard (§7.1). Returns the engine to watch — shared, because the
    /// transcript's rows keep their own handle to it — or `None` when the tab has
    /// no swarm and can simply go.
    pub fn terminate(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Rc<EngineHandle>> {
        let live = self.live.take()?;
        live.engine.shutdown();
        self.terminating = true;
        self.terminating_focus.focus(window, cx);
        cx.notify();
        Some(live.engine)
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
                // The project's settings, while they are what the page is showing,
                // are what the keyboard is for: coming back to a tab whose folder
                // row was pressed is coming back to that editor.
                if self.project_settings {
                    if let Some(editor) = &self.project_editor {
                        editor.update(cx, |editor, cx| editor.focus_into(window, cx));
                        return true;
                    }
                }
                self.composer
                    .update(cx, |composer, cx| composer.focus_input(window, cx));
                true
            }
            // A Settings tab is its editors: the first of them takes the caret, so
            // an opened Settings tab is ready to be typed into.
            TabState::Settings => {
                let Some(editor) = &self.settings else {
                    return false;
                };
                editor.update(cx, |editor, cx| editor.focus_into(window, cx));
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
    ///
    /// A tab back on the New Swarm page has started nothing, and the workers card's
    /// switch is that page's for one launch (§7.2): it opens on a swarm again, whatever
    /// it was flipped to before the launch that led here.
    fn set_state(&mut self, state: TabState, cx: &mut Context<Self>) {
        if self.state != state {
            let back_to_the_page = state == TabState::Empty;
            self.state = state;
            if back_to_the_page {
                self.set_use_swarm(true, cx);
            }
            cx.emit(TabContentEvent::ScreenChanged);
            self.sync_folder(cx);
        }
    }

    /// Tell every transcript the folder its swarm runs in, so a relative path in a
    /// row's text is measured from the project (`crates/transcript/src/lib.rs`) and
    /// not from wherever the app was started (§7.3).
    fn sync_folder(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder().map(Path::to_path_buf);
        for view in self.transcripts.values() {
            view.update(cx, |view, cx| view.set_folder(folder.clone(), cx));
        }
    }

    /// Tell every transcript which program this tab is (§7.2).
    ///
    /// An empty transcript's invitation is the coordinator's — `Ask the coordinator to
    /// get started` — or the one agent's, so the program reaches the view when it is
    /// made and again on every batch while the tab runs. A cached view keeps the copy it
    /// was last told, which is why this goes through `set_swarm`, which notifies.
    fn sync_swarm(&mut self, cx: &mut Context<Self>) {
        let swarm = self.swarm(cx);
        for view in self.transcripts.values() {
            view.update(cx, |view, cx| view.set_swarm(swarm, cx));
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

    /// The label on the tab: the folder's name, `New Swarm` — or `New Session`, one
    /// agent's page (§7.2) — while empty, or `Settings`.
    pub fn title(&self, cx: &App) -> SharedString {
        match &self.state {
            TabState::Settings => SharedString::from("Settings"),
            _ => match self.folder() {
                Some(folder) => folder_name(folder),
                None if self.swarm(cx) => SharedString::from("New Swarm"),
                None => SharedString::from("New Session"),
            },
        }
    }

    /// The tab's tooltip: the whole path plus what the tab is doing (§7.1, §9.7).
    ///
    /// The words are the session's, not a swarm's: the tab may hold either program, and
    /// `session` is what both of them are.
    pub fn tooltip(&self) -> SharedString {
        let what = match &self.state {
            _ if self.terminating => "terminating the session…",
            TabState::Settings => "the settings files evo reads",
            TabState::Empty => "no folder chosen",
            TabState::Booting { .. } => "starting the session…",
            TabState::Failed { was_up: true, .. } => "session gone: the server exited",
            TabState::Failed { .. } => "failed to start",
            TabState::Running { .. } => match &self.gone {
                Some(reason) => reason.as_ref(),
                None if self.is_reconnecting() => "session: reconnecting…",
                None => "session: running",
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

    // --- the settings editors (§7.1, §7.3) ---------------------------------

    /// The global Settings editor, on a Settings tab and nowhere else.
    pub fn settings_editor(&self) -> Option<&Entity<ConfigEditor>> {
        self.settings.as_ref()
    }

    /// The project's own editor, once the folder row at the foot of the lane column
    /// has been pressed.
    pub fn project_editor(&self) -> Option<&Entity<ConfigEditor>> {
        self.project_editor.as_ref()
    }

    /// Whether the conversation area is showing the project's settings instead of
    /// the selected agent's transcript.
    pub fn project_settings_shown(&self) -> bool {
        self.project_settings
    }

    /// Whether an editor on this tab holds changes that are not on disk yet — the
    /// question a close has to ask first, wherever the editor is (a Settings tab, or
    /// a running tab's project settings).
    pub fn is_settings_dirty(&self, cx: &App) -> bool {
        let dirty = |editor: &Entity<ConfigEditor>| editor.read(cx).is_dirty(cx);
        self.settings.as_ref().is_some_and(dirty) || self.project_editor.as_ref().is_some_and(dirty)
    }

    /// Whether an editor on this tab is writing a document right now.
    ///
    /// A save runs on the background executor with a copy of the text, so an editor
    /// dropped under it would leave a write landing on a file nobody is looking at —
    /// and a discard answered "yes" while it is in flight would not undo it. Nothing
    /// may close this tab, or throw its drafts away, until the write has landed.
    pub fn is_settings_saving(&self, cx: &App) -> bool {
        let saving = |editor: &Entity<ConfigEditor>| editor.read(cx).is_saving();
        self.settings.as_ref().is_some_and(saving)
            || self.project_editor.as_ref().is_some_and(saving)
    }

    /// Throw away every unsaved change this tab's editors hold — the "Discard" of a
    /// close question, and what the app's own quit uses.
    ///
    /// Each page puts its drafts back to the text it last read, which is an input's
    /// own work and needs the window the input lives in.
    pub fn discard_settings_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for editor in [self.settings.clone(), self.project_editor.clone()]
            .into_iter()
            .flatten()
        {
            editor.update(cx, |editor, cx| editor.discard(window, cx));
        }
        self.close_prompt = false;
        cx.notify();
    }

    /// Show the project's settings in the conversation area — the press on the folder
    /// row at the foot of the lane column (§7.3).
    ///
    /// The editors are made the first time and kept from then on, so leaving them for
    /// a lane's transcript and coming back finds the draft where it was left.
    pub fn show_project_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.folder().map(Path::to_path_buf) else {
            return;
        };
        let editor = match self.project_editor.clone() {
            Some(editor) => editor,
            None => {
                let editor =
                    cx.new(|cx| ConfigEditor::new(ConfigScope::Project(folder), window, cx));
                self.project_editor = Some(editor.clone());
                editor
            }
        };
        if !self.project_settings {
            self.project_settings = true;
            cx.notify();
        }
        editor.update(cx, |editor, cx| editor.focus_into(window, cx));
    }

    /// Put the transcript back: what a press on a lane or the coordinator row does
    /// (§7.3). The editor keeps its draft.
    pub fn hide_project_settings(&mut self, cx: &mut Context<Self>) {
        if self.project_settings {
            self.project_settings = false;
            cx.notify();
        }
    }

    /// A close was asked for while an editor here is dirty: put the Discard / Keep
    /// Editing question up instead of letting the tab go. Answers whether the
    /// question is the thing holding the close back.
    pub fn ask_close(&mut self, cx: &mut Context<Self>) -> bool {
        self.close_prompt = true;
        cx.notify();
        true
    }

    /// Whether that question is up.
    pub fn is_closing_prompted(&self) -> bool {
        self.close_prompt
    }

    /// The question's "Discard": drop every unsaved change and ask the window to
    /// close this tab — which it does at once, since a Settings tab drives no swarm.
    pub fn discard_and_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A document on its way to disk cannot be un-drafted: the question stays up
        // until the write has landed (see [`Self::is_settings_saving`]).
        if self.is_settings_saving(cx) {
            return;
        }
        self.discard_settings_changes(window, cx);
        cx.emit(TabContentEvent::CloseRequested);
    }

    /// The question's "Keep Editing": the question goes and the tab stays, with the
    /// draft where it was.
    pub fn keep_editing(&mut self, cx: &mut Context<Self>) {
        if self.close_prompt {
            self.close_prompt = false;
            cx.notify();
        }
    }

    /// Bring whatever this tab cannot walk away from to the front (§7.1, §9.8): the
    /// project's settings rather than the transcript, when it is the project's editor
    /// that holds the draft or the write — and, inside either page, the document that
    /// holds it, rather than whichever document happened to be in front.
    ///
    /// A page that is writing is taken to the document it is writing, whichever one
    /// the reader has since moved to; one that is not is taken to its first draft.
    pub fn reveal_dirty_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.project_editor.clone() {
            let (dirty, saving) = {
                let editor = editor.read(cx);
                (editor.is_dirty(cx), editor.is_saving())
            };
            if (dirty || saving) && !self.project_settings {
                self.project_settings = true;
                cx.notify();
            }
        }
        for editor in [self.settings.clone(), self.project_editor.clone()]
            .into_iter()
            .flatten()
        {
            let file = {
                let editor = editor.read(cx);
                editor
                    .saving_file()
                    .or_else(|| editor.dirty_files(cx).into_iter().next())
            };
            let Some(file) = file else {
                continue;
            };
            editor.update(cx, |editor, cx| editor.select(file, window, cx));
        }
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
            self.set_composer_catalog(catalog, cx);
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
        // The retry resumes in the program this tab has been running all along: a
        // `--resume` is a journal only the program that wrote it can open (§7.2).
        let swarm = self
            .last_launch
            .as_ref()
            .map(Launch::swarm)
            .unwrap_or_else(|| self.swarm(cx));
        let resume = self
            .session
            .clone()
            .filter(|session| session.is_file())
            .zip(self.folder().map(Path::to_path_buf))
            .map(|(session, folder)| Launch::Resume {
                folder,
                session,
                swarm,
            });
        if let Some(launch) = resume.or_else(|| self.last_launch.clone()) {
            self.launch(launch, window, cx);
        }
    }

    /// The engine behind this tab's swarm, while it has one — shared, because the
    /// page's own rows hold a handle to it too (§9.8).
    ///
    /// This is what a quit stops and waits for: [`EngineHandle::shutdown`] takes
    /// `&self`, so the window can watch a swarm out without taking the tab's own
    /// view of it away.
    pub fn engine(&self) -> Option<Rc<EngineHandle>> {
        self.live.as_ref().map(|live| live.engine.clone())
    }

    /// Take the engine out of the tab, so its swarm can be stopped somewhere that
    /// is not the UI thread (§9.8). The tab keeps what it shows; it stops
    /// watching and stops typing to the server.
    ///
    /// The tab hands back the *shared* handle: a row that asked for a read holds
    /// one of its own, and the last of them is what closes the child's stdin (the
    /// server's own signal to stop) when the tab goes.
    pub fn take_engine(&mut self, cx: &mut Context<Self>) -> Option<Rc<EngineHandle>> {
        let live = self.live.take()?;
        cx.notify();
        Some(live.engine)
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
        // A transcript born here is the program this launch started (§7.2): a resume, a
        // retry and a fresh tab all come through this door, and an empty view already
        // knows whether it is a swarm's or one agent's.
        self.sync_swarm(cx);
        // Which program this tab just started: the launch said so, and the page, the
        // column and the box all read it from here (§7.2).
        let swarm = self.swarm(cx);
        self.live = Some(Live {
            engine: Rc::new(started.engine),
            model: TabModel::new(),
            _pump: pump,
            tab_dir: started.tab_dir,
            pending: BTreeMap::new(),
            stream: StreamStatus::Connected,
            swarm,
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
        let folder = self.folder().map(Path::to_path_buf);
        let page_topic = topic.clone();
        let item_topic = topic.clone();
        let cancel_topic = topic;
        view.update(cx, |view, cx| {
            // A relative path in a row's text is measured from the project the tab
            // works in (§7.3).
            view.set_folder(folder, cx);
            view.on_load_older(
                move |oldest, _window, _cx| {
                    engine.page(&page_topic, Some(oldest), session::PAGE_ITEMS);
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
    ///
    /// Only the engine ends this. An update is taken out of the channel and held
    /// until a window applies it, so a tab whose window went away — a page rebuilt
    /// in another window, the window closed — is driven again the moment a frame
    /// draws the tab. The engine cannot tell a tab that fell behind from a tab that
    /// is dead, and a pump that ends on the first failure leaves a running swarm
    /// writing into a channel nobody reads: the tab draws its last frame for the
    /// rest of its life.
    fn spawn_pump(
        &self,
        updates: Receiver<Update>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        // Said once per tab, not once per update: what is dropped here is dropped
        // because there is no window left to draw it in, and a reader who is still
        // looking deserves one line rather than a stream of them.
        let said = Arc::new(AtomicBool::new(false));
        cx.spawn_in(window, async move |this, cx| loop {
            let Ok(update) = updates.recv().await else {
                // The engine is gone, or its handle was dropped: the tab has
                // nothing left to be told. This is the only end there is.
                return;
            };
            let mut batch = vec![update];
            while let Ok(queued) = updates.try_recv() {
                batch.push(queued);
            }
            // The batch is drained by the closure that applies it, so a tab with no
            // window right now — `update_in` fails before that closure is reached —
            // still holds every update in it, and is asked again until a frame
            // draws the tab somewhere.
            loop {
                let applied = this.update_in(cx, |tab, window, cx| {
                    for update in batch.drain(..) {
                        tab.apply(update, window, cx);
                    }
                });
                if applied.is_ok() {
                    break;
                }
                if this.upgrade().is_none() {
                    // The tab itself is gone: this batch has no reader, now or
                    // later. Nothing else in this tab can lose an update.
                    say_dropped(&said, batch.len());
                    return;
                }
                cx.background_executor().timer(PUMP_RETRY).await;
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
                // An answer that added no row — a page overlapping what is held, or the
                // last page of all — is still an answer: the header must stop saying it
                // is loading even when nothing changed.
                self.settle_history(&topic, cx);
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
            Update::FetchFailed { what, reason, page } => {
                // A page that failed still ends the wait: the header stops saying it is
                // loading, and the reader can ask for that page again.
                if let Some(topic) = page {
                    self.settle_history(&topic, cx);
                }
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
                self.answer_pending_completions(cx);
                self.gone = Some(format!("session stopped ({outcome:?})").into());
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

    /// Every question still waiting for an answer, answered with nothing: the engine or
    /// the server behind it is gone, so the answer is not coming, and a box left waiting
    /// on one would never ask about another word (§7.3).
    ///
    /// The entries go with them — the ids they were filed under will never be answered,
    /// and a reply that arrived for one anyway would be about a tab that is over.
    fn answer_pending_completions(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        for question in waiting_completions(&mut live.pending) {
            self.composer.update(cx, |composer, cx| {
                composer.set_completion(
                    &question.text,
                    question.cursor,
                    composer::Answer::none(),
                    cx,
                )
            });
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
                            // The topic's own index, not a scan of its items: a streamed
                            // delta arrives per token, and a live mirror is long.
                            if let Some(item) = live
                                .model
                                .agent_topic(agent)
                                .and_then(|topic| topic.item(id))
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
            // A view made here is the tab's program too, however late it is first shown
            // (§7.2).
            self.sync_swarm(cx);
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
            // A running agent shows its working pips from the request's start.
            let running = self
                .live
                .as_ref()
                .and_then(|live| live.model.state(agent))
                .is_some_and(|state| state.status == session::Status::Running);
            view.update(cx, |view, cx| {
                view.set_running(running, cx);
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
            self.hold_the_record_to_the_topic(agent, &view, cx);
        }

        // The todos of the selected agent are the composer's: the design puts the
        // strip inside the composer box and reads them off the topic state, so the
        // transcript is fed items only (`sync_composer`).
        self.sync_composer(cx);
        // The lane column takes the same facts, on the same batch (§7.3).
        self.sync_agents(cx);
        cx.notify();
    }

    /// Hold one agent's transcript to the topic it mirrors.
    ///
    /// The list is exactly as tall as the view *was told* it is, so a view that is
    /// behind its topic — an op the model took and the view never saw, a splice the
    /// list mislaid, a record built from a stale read — is invisible from the
    /// outside: the transcript draws what it holds, and it holds what it was told.
    /// That is a reader looking at a conversation that has ended while the session
    /// goes on, and the newest rows are the ones missing.
    ///
    /// So the two are held to each other on every batch, which costs two numbers:
    /// the record plus the items the view declined (a notice the server itself does
    /// not keep) is the topic's own list. A disagreement is said once on stderr —
    /// it is a bug in this client, and nobody watching the window can do anything
    /// about it — and settled by re-reading the topic, which is what "the view is
    /// behind" means. The check cannot loop: a `replace` from the topic is the
    /// topic's own list, declined and all.
    fn hold_the_record_to_the_topic(
        &mut self,
        agent: AgentKey,
        view: &Entity<TranscriptView>,
        cx: &mut Context<Self>,
    ) {
        let Some(topic_len) = self.live.as_ref().map(|live| live.model.items(agent).len()) else {
            return;
        };
        let (held, declined) = {
            let view = view.read(cx);
            (view.items(cx).len(), view.declined(cx))
        };
        if held + declined == topic_len {
            return;
        }
        eprintln!(
            "evo-desktop: {agent:?}'s transcript holds {held} item(s) and declined \
             {declined}, while its topic has {topic_len}: re-reading the topic"
        );
        let items = self
            .live
            .as_ref()
            .map(|live| live.model.items(agent).to_vec())
            .unwrap_or_default();
        view.update(cx, |view, cx| view.replace(items, cx));
    }

    /// Release one topic's transcript from "Loading earlier items…" (§5.4).
    ///
    /// The header that asks for a page stays in flight until an answer lands, and an
    /// answer is not always a row: a page that overlaps what the topic holds, the last
    /// page of all, and a page that failed all change nothing, yet each must end the
    /// wait. Only the topic the read was about is settled; the model still says whether
    /// there is more behind what is held.
    fn settle_history(&mut self, topic: &str, cx: &mut Context<Self>) {
        let Some(agent) = AgentKey::from_topic(topic) else {
            return;
        };
        let Some(view) = self.transcripts.get(&agent).cloned() else {
            return;
        };
        let has_older = self
            .live
            .as_ref()
            .and_then(|live| live.model.agent_topic(agent))
            .is_some_and(|topic| topic.has_older());
        view.update(cx, |view, cx| view.set_history(has_older, false, cx));
    }

    /// Feed the composer from the model: the selected agent's own topic state — the
    /// chips, the todos, the model and the goal it draws (CONTRACT §4.2) — and whether
    /// this box may change the model and the effort.
    ///
    /// `model.set` and `thinking.set` act on the session (CONTRACT §5), and a lane's
    /// model is the swarm's, fixed when it starts. So the coordinator's drawer is live
    /// and a lane's states what it runs (§7.3).
    fn sync_composer(&mut self, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let selected = live.model.selected();
        let empty = session::TopicState::default();
        let state = live.model.state(selected).unwrap_or(&empty).clone();
        let busy = swarm_is_busy(&live.model);
        // What the drawer calls the agent it is showing: a swarm is coordinated, one
        // agent is not (§7.2). `AgentKey::name` writes that name once, for the drawer,
        // the band above the transcript and the lane column's own rows.
        let name = selected.name(live.swarm);
        let settable = selected == AgentKey::Coordinator;
        // The same reading the header takes for its reveal: the level this agent is
        // set to, kept where the page can reach it (the drawer's own copy is the
        // composer's).
        self.thinking_level = state.thinking.clone();
        let swarm = live.swarm;
        self.composer.update(cx, |composer, cx| {
            composer.set_agent(&state, &name, settable, cx);
            composer.set_swarm_busy(busy, cx);
            composer.set_swarm(swarm, cx);
        });
        // The same program, told to the transcripts: only an empty one draws it, and it
        // draws the coordinator's invitation or the one agent's (§7.2).
        self.sync_swarm(cx);
    }

    /// Feed the composer's drawer from the catalog (§5.6): the same `/catalog` body the
    /// empty tab's choosers read, in the two shapes the drawer draws.
    ///
    /// The ladder is the server's own (`thinking_levels`) — a client never writes the
    /// rungs down — and the models are the registrations it lists, each with the
    /// provider lower-cased and, where evo cannot reach one, evo's own reason for it.
    /// Lives here rather than in the composer because it is the workspace that holds
    /// both: the catalog body arrives at the tab, and a running tab keeps its composer.
    fn set_composer_catalog(&mut self, catalog: &serde_json::Value, cx: &mut Context<Self>) {
        let levels = session::thinking_levels(catalog);
        let models = session::model_options(catalog)
            .into_iter()
            .map(|model| ModelRow {
                reason: if model.ready {
                    None
                } else {
                    model.ready_reason
                },
                effort_levels: model.effort_levels,
                id: model.id,
                provider: model.provider,
                detail: model.detail,
            })
            .collect();
        // The commands the registry lists are the popup's candidates: evo's own table,
        // extension commands and skills included, never a list this client keeps.
        let commands = session::command_options(catalog)
            .into_iter()
            .map(|command| Candidate {
                name: command.name,
                description: command.description,
            })
            .collect();
        self.composer.update(cx, |composer, cx| {
            composer.set_catalog(levels, models, commands, cx)
        });
    }

    /// Feed the agent list from the model: the rows, the coordinator's status and step
    /// clock, the selection, and why each down lane is down (§7.3, §9.7).
    fn sync_agents(&mut self, cx: &mut Context<Self>) {
        // Which program the column is showing is the tab's own knowledge, not the
        // model's: a swarm's first row is its coordinator and one agent's is `Main`,
        // and only a swarm has lanes to count (§7.2).
        let swarm = self.swarm(cx);
        self.agents.update(cx, |list, cx| list.set_swarm(swarm, cx));
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
            // And which program the tab is: a lane first shown mid-run draws no
            // invitation, but the coordinator's own view may be empty and is the one
            // program's or the other's (§7.2).
            self.sync_swarm(cx);
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
        // The question this event asks, kept for the half below where nobody can take
        // it: the box holds its next question until the one it asked is released
        // ([`composer::Composer::set_completion`]), so a question nobody answers is the
        // popup never coming back — the first one a tab asks (about its empty box, the
        // moment the catalog reaches it) included.
        let asked_about = match &event {
            ComposerEvent::Complete { text, cursor } => Some(Completing {
                text: text.clone(),
                cursor: *cursor,
            }),
            _ => None,
        };
        // A send is ended by `send_finished` and by nothing else: when nobody can take
        // one, that is what releases the box, or it says `Sending…` for good.
        let is_send = matches!(event, ComposerEvent::Send(_));
        let mut unanswered: Option<Completing> = None;
        let sent = match self.live.as_mut() {
            Some(live) => {
                let (request, pending) = match event {
                    ComposerEvent::Send(outgoing) => {
                        // What the reader attached, as the wire takes it (§5.5): an
                        // image rides *with* the turn, in the op's own `images` — evo
                        // journals it, which is what puts it in the transcript and
                        // brings it back on resume — and a file is named by its own
                        // absolute path, in the message's text. Nothing embeds a file:
                        // the agent reads one with its tools if it wants to.
                        let (text, images) = session::attachment_turn(
                            &outgoing.text,
                            &attached(&outgoing.attachments),
                        );
                        // An idle coordinator runs the words now; a run in flight takes
                        // them at its next step boundary (§5.5) — `queue: now` either
                        // way. `after_run` would hold them until the whole run ends,
                        // which for a working coordinator can be a very long time.
                        (
                            live.model.send_input_with(&text, images, Queue::Now),
                            Pending::Send,
                        )
                    }
                    ComposerEvent::StopSwarm => (live.model.interrupt_swarm(), Pending::Interrupt),
                    ComposerEvent::Interrupt => {
                        (live.model.interrupt_session(), Pending::Interrupt)
                    }
                    // The drawer's two settings are the session's (CONTRACT §5): the
                    // composer only offers them for the coordinator.
                    ComposerEvent::ModelSet { id, provider } => (
                        session::OpRequest::model_set(&id, Some(&provider)),
                        Pending::Settings,
                    ),
                    ComposerEvent::ThinkingSet(level) => {
                        (session::OpRequest::thinking_set(&level), Pending::Settings)
                    }
                    // A message that is one slash command is the command layer's: the
                    // server dispatches it exactly as the TUI does, extension commands
                    // and all, and says `not_found` for a word it does not know.
                    ComposerEvent::Command { name, args } => (
                        session::OpRequest::command_run(&name, &args),
                        Pending::Command,
                    ),
                    // The popup's question about the word being typed: what the caret
                    // is on and what could fill it, which is evo's own answer — the
                    // client does not decide what a word is, and does not evaluate
                    // anything to read a name list.
                    ComposerEvent::Complete { text, cursor } => {
                        let question = Completing { text, cursor };
                        let request = session::complete_request(&question.text, question.cursor);
                        // The op counts characters; the box works in bytes.
                        (request, Pending::Complete(question))
                    }
                };
                // The engine mints the request's id, and the reply comes back tagged
                // with it: that is what says which op was answered (§5.5).
                match live.engine.request(request) {
                    Some(rid) => {
                        live.pending.insert(rid, pending);
                        true
                    }
                    // The engine is gone — the tab is stopping. A question about a
                    // word would leave the popup waiting for an answer that cannot
                    // come, so it is answered with nothing.
                    None => {
                        if let Pending::Complete(question) = pending {
                            unanswered = Some(question);
                        }
                        false
                    }
                }
            }
            // No swarm: there is nothing to send to, and the button must not stay
            // disabled waiting for a reply that cannot come.
            None => false,
        };
        if !sent {
            // Nobody could take the question: there is no server under this tab at all
            // — it has not launched yet, or its session is gone — or the engine that
            // would carry it is. A question is answered with nothing either way:
            // "nothing" is a true answer about a caret no server is reading, and it is
            // what releases the box to ask about the word typed once there is one.
            match unanswered.or(asked_about) {
                Some(question) => {
                    self.composer.update(cx, |composer, cx| {
                        composer.set_completion(
                            &question.text,
                            question.cursor,
                            composer::Answer::none(),
                            cx,
                        )
                    });
                }
                None => {
                    self.composer.update(cx, |composer, cx| {
                        if is_send {
                            composer.send_finished(false, window, cx)
                        } else {
                            composer.request_finished(false, window, cx)
                        }
                    });
                }
            }
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
            match asked {
                Pending::Send => {
                    // A sent message clears the draft when the server took it — and
                    // only then: a refusal (an image evo cannot read is `invalid_args`)
                    // leaves the words where they were, and the strip with them, so the
                    // reader can take the picture out or name another file and send the
                    // same message again. The line above the composer is the server's
                    // own reason for it ([`notice_words`]).
                    self.composer.update(cx, |composer, cx| {
                        composer.send_finished(reply.ok, window, cx)
                    });
                    // A message the server took answers the refusal above it: the
                    // reader fixed what was wrong and sent again, and a red line that
                    // stays for the rest of its four seconds would say the fix failed.
                    if reply.ok
                        && self
                            .notice
                            .as_ref()
                            .is_some_and(|notice| notice.text.starts_with("input.send"))
                    {
                        self.notice = None;
                        cx.notify();
                    }
                }
                Pending::Command => {
                    // A command that hands text back — `/rewind`, `/tree <id>` on a
                    // message — puts it in the input for editing, which is the whole of
                    // what those commands are for; every other command was taken, and
                    // clears the draft the way a sent message does. A command's output
                    // is the session's own `notice` items, which the transcript has
                    // already drawn (§4.1) — the reply is not a second place to say it.
                    let handed_back = reply
                        .result
                        .get("data")
                        .and_then(|data| data.get("draft"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string);
                    self.composer.update(cx, |composer, cx| match &handed_back {
                        Some(text) => composer.set_draft(text, window, cx),
                        None => composer.request_finished(reply.ok, window, cx),
                    });
                }
                Pending::Complete(question) => {
                    // What the caret is on, and what could fill it, read off the op's
                    // own result: the kind, the range a candidate's name replaces, and
                    // the candidates. A refusal — a server that does not offer the op, a
                    // reply that never came — reads as nothing to complete, which is an
                    // answer like any other: the popup has none to draw, and the
                    // question is released so the next word can be asked about.
                    let completion = session::completion(&question.text, &reply.result);
                    let answer = composer::Answer {
                        kind: completion.kind.map(|kind| match kind {
                            session::CompletionKind::Command => composer::CompletionKind::Command,
                            session::CompletionKind::Symbol => composer::CompletionKind::Symbol,
                        }),
                        name: completion.start..completion.end,
                        items: completion
                            .items
                            .into_iter()
                            .map(|item| Candidate {
                                name: item.name,
                                description: item.description,
                            })
                            .collect(),
                    };
                    self.composer.update(cx, |composer, cx| {
                        composer.set_completion(&question.text, question.cursor, answer, cx)
                    });
                }
                // A stop's reply only re-arms the button; the draft is untouched.
                _ => {
                    self.composer.update(cx, |composer, cx| {
                        composer.request_finished(false, window, cx)
                    });
                }
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
        self.answer_pending_completions(cx);
        let Some(log) = self
            .live
            .as_ref()
            .map(|live| live.tab_dir.join("swarm.log"))
        else {
            return;
        };
        self.gone = Some("session: the server exited".into());
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
        let (models, lanes) = recorded_facts(
            &live.model,
            self.last_launch.as_ref().and_then(|launch| launch.plan()),
        );

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

/// What the app files about a session it started (§9.5): the models the swarm really
/// runs with, and how many lanes it runs.
///
/// The **server's own topics** are the truth — the session's `model` is the coordinator
/// evo resolved, the swarm's `config.lane_model` and `workers` are what the lanes got —
/// because a launch passes only the controls a person set by hand: ask the plan alone
/// and every swarm evo resolved for itself records "no model". The plan is the fallback
/// for the moment before `/state` has spoken, when a person's own choice is all there is
/// to say.
fn recorded_facts(model: &TabModel, plan: Option<&LaunchPlan>) -> (store::tab::TabModels, u32) {
    let asked_model = |pick: Option<&(String, String)>| pick.map(|(id, _)| id.clone());
    let models = store::tab::TabModels {
        coordinator: model
            .state(AgentKey::Coordinator)
            .and_then(|state| state.model.as_ref())
            .map(|info| info.id.clone())
            .or_else(|| asked_model(plan.and_then(|plan| plan.model.as_ref()))),
        lanes: model
            .swarm()
            .and_then(|swarm| swarm.lane_model.as_ref())
            .map(|info| info.id.clone())
            .or_else(|| asked_model(plan.and_then(|plan| plan.lanes_model.as_ref()))),
    };
    let lanes = model
        .swarm()
        .map(|swarm| swarm.workers as u32)
        .or_else(|| plan.and_then(|plan| plan.workers).map(u32::from))
        .unwrap_or_default();
    (models, lanes)
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
        self.render_for_state(window, cx)
    }
}

/// One spelling per file: a session path as the index writes it and as `/state`
/// names it can differ by a trailing slash or a symlinked prefix
/// (`/var` → `/private/var`).
fn same_file_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let text = path.to_string_lossy();
        PathBuf::from(text.trim_end_matches('/'))
    })
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
        div, point, px, size, AnyWindowHandle, Bounds, Entity, TestAppContext, WindowBounds,
        WindowOptions,
    };
    use session::Item;
    use std::cell::RefCell;
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

    /// §7.2: which program a tab is. Before it launches, the workers card's switch is
    /// the answer — that is what the page would start — and once it has launched, what
    /// it launched with is: a swarm keeps running as a swarm even if the switch moves
    /// under it, because its pages, its column and its box are all the swarm's.
    #[gpui_kit::test]
    fn a_tabs_program_is_the_one_it_launched_with(cx: &mut TestAppContext) {
        let (_window, tab) = running_tab(cx);
        cx.update(|cx| assert!(tab.read(cx).swarm(cx), "a swarm until told otherwise"));

        tab.update(cx, |tab, cx| tab.set_use_swarm(false, cx));
        cx.update(|cx| {
            assert!(
                !tab.read(cx).swarm(cx),
                "an unlaunched tab is whatever the switch would start"
            )
        });

        tab.update(cx, |tab, _| {
            tab.last_launch = Some(Launch::New {
                folder: PathBuf::from("/tmp/proj"),
                plan: LaunchPlan {
                    swarm: true,
                    ..LaunchPlan::default()
                },
            });
        });
        cx.update(|cx| {
            assert!(
                tab.read(cx).swarm(cx),
                "a launch fixes it: this tab is the swarm it started"
            )
        });

        // And a resumed single agent's tab is that agent's, whatever the switch says.
        tab.update(cx, |tab, cx| {
            tab.last_launch = Some(Launch::Resume {
                folder: PathBuf::from("/tmp/proj"),
                session: PathBuf::from("/tmp/proj/session.sexp"),
                swarm: false,
            });
            tab.set_use_swarm(true, cx);
        });
        cx.update(|cx| assert!(!tab.read(cx).swarm(cx)));
    }

    /// §7.2: the workers card's switch is one page's, for one launch, and a tab back on
    /// the empty page has started nothing — so it opens on a swarm again, whatever the
    /// launch that led here was asked for.
    #[gpui_kit::test]
    fn a_tab_back_on_the_page_opens_on_a_swarm_again(cx: &mut TestAppContext) {
        let (_window, tab) = running_tab(cx);
        tab.update(cx, |tab, _| {
            tab.state = TabState::Running {
                folder: PathBuf::from("/tmp/proj"),
            };
        });
        tab.update(cx, |tab, cx| tab.set_use_swarm(false, cx));
        cx.update(|cx| {
            assert!(
                !tab.read(cx).swarm(cx),
                "a launch asked for one agent: the switch says so"
            )
        });

        // Back to the New Swarm page: this tab has started nothing, and the page it is
        // on is the one every tab opens on.
        tab.update(cx, |tab, cx| tab.set_state(TabState::Empty, cx));
        cx.update(|cx| {
            assert!(
                tab.read(cx).swarm(cx),
                "the page it came back to opens on a swarm"
            )
        });
    }

    /// §7.3: the reveal is offered before any thinking has arrived, to an agent whose
    /// effort is a rung that thinks. The quietest rung and an effort nobody has read
    /// wait for text.
    /// §7.2: the strip's label for an empty tab is the page's own name — a swarm, or one
    /// agent's session, which is what the workers switch says — and a tab that has
    /// started something is its folder, whatever the switch is set to afterwards.
    #[gpui_kit::test]
    fn the_empty_tabs_label_follows_the_workers_switch(cx: &mut TestAppContext) {
        let (_window, tab) = running_tab(cx);
        tab.update(cx, |tab, _| tab.state = TabState::Empty);
        let label = |cx: &App| tab.read(cx).title(cx);

        cx.update(|cx| {
            assert_eq!(label(cx), "New Swarm", "a swarm until told otherwise");
        });
        tab.update(cx, |tab, cx| tab.set_use_swarm(false, cx));
        cx.update(|cx| {
            assert_eq!(label(cx), "New Session", "one agent's page is a session");
        });

        // A tab with a folder is that folder: the switch has nothing to say about it.
        tab.update(cx, |tab, cx| {
            tab.state = TabState::Running {
                folder: PathBuf::from("/tmp/proj"),
            };
            tab.set_use_swarm(true, cx);
        });
        cx.update(|cx| {
            assert_eq!(label(cx), "proj", "the tab is where it is running");
        });
    }

    #[gpui_kit::test]
    fn an_effort_above_low_offers_the_reveal_before_any_text(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
            // An answer that carried no thinking at all: the only thing that could
            // offer the reveal is the effort.
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_1", "the answer", "")], cx);
            });
        });

        let offered = |cx: &mut TestAppContext, level: Option<&str>| {
            let level = level.map(str::to_string);
            cx.update(|cx| tab.update(cx, |tab, _| tab.thinking_level = level));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("transcript-thinking").is_some()
            })
            .expect("the band")
        };

        assert!(
            offered(cx, Some("medium")),
            "medium thinks: the reveal is offered while nothing has arrived yet"
        );
        assert!(offered(cx, Some("high")), "and so does every rung above it");
        assert!(offered(cx, Some("xhigh")));
        assert!(offered(cx, Some("max")));
        assert!(
            offered(cx, Some("ultra")),
            "a rung this app has not been handed still thinks: the level, not a \
             ladder's order, is what decides"
        );
        assert!(
            !offered(cx, Some("low")),
            "the quietest rung has nothing to reveal until it has revealed it"
        );
        assert!(!offered(cx, Some("off")), "and neither has the retired one");
        assert!(
            !offered(cx, None),
            "an agent whose effort was never read is not guessed at"
        );

        // Thinking that arrived is the other way in, whatever the effort was set to
        // — and the reveal left on is still the view's own state.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_2", "the answer", "a thought")], cx);
            });
        });
        assert!(
            offered(cx, Some("low")),
            "text in the transcript offers the reveal at any effort"
        );
        assert!(offered(cx, None));
        cx.update_window(window, |_, window, cx| {
            window.click("transcript-thinking", cx)
        })
        .expect("the press");
        assert!(
            cx.read(|cx| view.read(cx).is_showing_thinking(cx)),
            "the press is the view's own state"
        );
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_3", "the answer", "another thought")], cx);
            });
        });
        assert!(
            offered(cx, Some("low")),
            "and the choice survives the transcript moving on"
        );
    }

    /// §7.3: the reveal belongs to the agent being shown — its own transcript's
    /// thinking, its own effort — so an agent with neither has none, even while
    /// another agent's transcript does.
    #[gpui_kit::test]
    fn the_reveal_is_the_shown_agents_own(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let lane = AgentKey::Lane(1);
        let lane_view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(lane, lane_view.clone());
                tab.thinking_level = Some("medium".to_string());
            });
            lane_view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_1", "the answer", "")], cx);
            });
        });
        // The coordinator is what a tab with no model shows, and it has no transcript
        // here: nothing to reveal, whoever else has.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("transcript-thinking").is_none(),
                "the shown agent's own transcript is what the band reads"
            );
        })
        .expect("the band");
    }

    /// A window holding one Settings tab, and nothing else: what the reveal below
    /// needs, and what a page that is writing looks like without a swarm around it.
    fn settings_tab(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TabContent>) {
        crate::test_evo_home();
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
                cx.new(|cx| {
                    TabContent::new_settings(
                        TabId::new(7),
                        Arc::new(LaunchEnv::default()),
                        window,
                        cx,
                    )
                })
            })
            .expect("settings window")
        })
    }

    /// §9.8: a page that is writing is taken to the document the *write* is about,
    /// whichever one the reader has since moved to — which is what "show me what the
    /// quit would be waiting for" has to land on.
    #[gpui_kit::test]
    fn revealing_a_writing_page_lands_on_the_document_it_is_writing(cx: &mut TestAppContext) {
        let (window, tab) = settings_tab(cx);
        let editor = cx.update(|cx| {
            tab.read(cx)
                .settings_editor()
                .cloned()
                .expect("a settings tab has its editors")
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            // Showing the tab is what puts the caret in the page, as a window's own
            // selection does (nobody else is focusing anything here).
            tab.update(cx, |tab, cx| {
                tab.focus_primary(window, cx);
            });
            window.input("(a)", cx);
        })
        .expect("the settings page");
        // The page reads its documents once, on the background executor: the draft is
        // measured against what was read, so let the read land before saving it.
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| editor.read(cx).selected()),
            store::ConfigFile::Init,
            "the page opens on `init.lisp`"
        );

        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |editor, cx| editor.save_selected(window, cx));
            assert!(editor.read(cx).is_saving(), "the write is in flight");
            // The reader moves on while it is: another document is in front now.
            editor.update(cx, |editor, cx| {
                editor.select(store::ConfigFile::Memory, window, cx)
            });
            assert_eq!(editor.read(cx).selected(), store::ConfigFile::Memory);

            tab.update(cx, |tab, cx| tab.reveal_dirty_settings(window, cx));
            assert_eq!(
                editor.read(cx).selected(),
                store::ConfigFile::Init,
                "the reveal goes to the document being written, not the one being read"
            );
        })
        .expect("the settings page again");
    }

    /// §7.3: the folder row at the foot of the lane column is one button — the
    /// project's own way in. A press puts that project's settings where the
    /// conversation was, composer and all; picking an agent puts the conversation
    /// back, and either way the draft stays where it was left.
    #[gpui_kit::test]
    fn the_project_row_puts_the_project_settings_in_the_conversations_place(
        cx: &mut TestAppContext,
    ) {
        let (window, tab) = running_tab(cx);
        // A folder long enough that the row has to elide it: the row is one line, and
        // the path is what gives way, never the gear beside it.
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.state = TabState::Running {
                    folder: PathBuf::from(
                        "/Users/somebody/coding/a-rather-long-workspace-name/deeper/still/project",
                    ),
                };
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("composer").is_some(),
                "the page opens on the conversation"
            );

            // The footer is one row, drawn as the list's own rows are: a lane row's
            // height, with everything it holds inside it.
            let row = window.find("project-row").bounds();
            let path = window.find("project-row-path").bounds();
            let gear = window.find("project-row-gear").bounds();
            assert_eq!(
                row.size.height,
                px(agent_list::ROW_HEIGHT),
                "the project's row is a lane row's height"
            );
            assert!(
                path.right() <= gear.left(),
                "a long path is elided, never run under the gear: {path:?} against {gear:?}"
            );
            assert!(
                path.left() >= row.left() - px(1.) && gear.right() <= row.right() + px(1.),
                "and both stay inside the row: {path:?} and {gear:?} against {row:?}"
            );
            assert_eq!(
                window.find("project-row").selected(),
                Some(false),
                "and the row says it is not the thing being shown"
            );

            window.click("project-row", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("project-settings").is_some(),
                "the project's settings are where the conversation was"
            );
            assert!(
                window.try_find("composer").is_none(),
                "the composer went with the conversation"
            );
            assert!(
                window.try_find("agent-column").is_some(),
                "the lane column stays"
            );
            assert_eq!(
                window.find("project-row").selected(),
                Some(true),
                "the row says it is the one being shown"
            );
            // The press is what put the keyboard in the editors, so this lands in
            // the file the page is showing.
            window.input("(a)", cx);
        })
        .expect("the page");

        let editor = cx.update(|cx| {
            tab.read(cx)
                .project_editor()
                .cloned()
                .expect("the row made the project's editors")
        });
        assert!(
            cx.read(|cx| editor.read(cx).is_dirty(cx)),
            "what was typed is a draft"
        );
        // Both of the tab's questions are about either editor: this is the draft a
        // close has to ask about, and it is not a write in flight.
        assert!(
            cx.read(|cx| tab.read(cx).is_settings_dirty(cx)),
            "the project's draft is the tab's to answer for"
        );
        assert!(
            !cx.read(|cx| tab.read(cx).is_settings_saving(cx)),
            "and nothing here is being written"
        );

        // Picking an agent is what puts the transcript back — the row's own event,
        // on its own update.
        cx.update(|cx| {
            tab.update(cx, |tab, cx| {
                tab.agents.update(cx, |_list, cx| {
                    cx.emit(agent_list::AgentListEvent::Select(AgentKey::Coordinator))
                });
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("composer").is_some(),
                "picking an agent puts the conversation back"
            );
            assert!(window.try_find("project-settings").is_none());
            assert_eq!(window.find("project-row").selected(), Some(false));
        })
        .expect("the page again");
        assert!(
            cx.read(|cx| editor.read(cx).is_dirty(cx)),
            "and the draft was not thrown away on the way out"
        );

        cx.update_window(window, |_, window, cx| {
            window.click("project-row", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("project-settings").is_some(),
                "the way back is the same press"
            );
        })
        .expect("the page once more");
        assert!(
            cx.read(|cx| editor.read(cx).is_dirty(cx)),
            "with the draft where it was left"
        );

        // Discarding is the tab's own answer, and it is the whole of what is thrown
        // away: the project's page goes back to what is on disk. (The page reads its
        // files once, on the background executor; a draft cannot be thrown away
        // against a text that has not arrived yet.)
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| tab.discard_settings_changes(window, cx));
            window.render_frame(cx);
        })
        .expect("the page one last time");
        assert!(
            !cx.read(|cx| editor.read(cx).is_dirty(cx)),
            "the drafts are gone"
        );
        assert!(!cx.read(|cx| tab.read(cx).is_settings_dirty(cx)));
    }

    /// §9.5: a session is filed under what the swarm really ran — the server's own
    /// topics — and not under the empty tab's controls, whose plan carries nothing for
    /// anything evo resolved for itself. A server that has not spoken yet leaves the
    /// person's own choices, and only those.
    #[test]
    fn a_recent_is_filed_with_what_the_swarm_really_runs() {
        let mut model = TabModel::new();
        model.on_snapshot(
            "session",
            &serde_json::json!({"state": {
                "status": "idle",
                "model": {"id": "claude-opus-5-5", "provider": "super_relay"}
            }}),
        );
        model.on_snapshot(
            "swarm",
            &serde_json::json!({"state": {
                "id": "sw", "workers": 9,
                "status": {"busy": 0, "waiting_on_lanes": false},
                "config": {"lane_model": {"id": "deepseek-v4.1-flash",
                                          "provider": "acme"}}
            }}),
        );
        let (models, lanes) = recorded_facts(&model, None);
        assert_eq!(models.coordinator.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(models.lanes.as_deref(), Some("deepseek-v4.1-flash"));
        assert_eq!(lanes, 9);

        // Before the server has said anything, a person's own picks are all there is:
        // what the swarm resolves — and what the plan therefore leaves out — is not a
        // model this app can name.
        let asked = LaunchPlan {
            model: Some(("seed-evolving".to_string(), "super_relay".to_string())),
            workers: Some(4),
            ..LaunchPlan::default()
        };
        let (models, lanes) = recorded_facts(&TabModel::new(), Some(&asked));
        assert_eq!(models.coordinator.as_deref(), Some("seed-evolving"));
        assert_eq!(models.lanes, None);
        assert_eq!(lanes, 4);
    }

    /// One assistant answer, with or without the thinking behind it, as a fixture
    /// item (`GET /items`, §5.4).
    fn assistant(id: &str, text: &str, thinking: &str) -> Item {
        Item::from_json(&serde_json::json!({
            "id": id, "ts": 1, "kind": "assistant", "text": text,
            "thinking": thinking, "status": "final",
        }))
        .expect("a fixture item has an id")
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

        // A lost reply is not a refusal: the tab's engine marks it in the reply's
        // own detail, and it reads as a quiet line rather than as a failure.
        let mut lost = error(
            ErrorCode::Unknown,
            "the server did not answer this send (io: Broken pipe)",
        );
        lost.detail = serde_json::json!({ "outcome": "unknown" });
        assert_eq!(notice_tone(&lost), NoticeTone::Dim);
        // While the same code from the server itself, with its own detail, is still
        // a failure: only the marker reads quiet.
        let mut server_said = error(ErrorCode::Unknown, "who knows");
        server_said.detail = serde_json::json!({ "field": "objective" });
        assert_eq!(notice_tone(&server_said), NoticeTone::Error);
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

    /// A tab with a live engine behind it: the fake server's ready file, its snapshot
    /// and its stream, fed into the page's own model. `push` is what this drives, and
    /// `push` is a tab's — a page with no server has no model to be fed.
    fn live_tab(
        cx: &mut TestAppContext,
    ) -> (
        AnyWindowHandle,
        Entity<TabContent>,
        swarm_client::harness::TempDir,
    ) {
        let dir = swarm_client::harness::TempDir::new("live-tab").expect("a temp dir");
        let config = swarm_client::harness::fake_config(dir.path(), &[]).expect("a config");
        let (engine, updates) = tab_engine::TabEngine::start(config);
        let (window, tab) = running_tab(cx);
        let tab_dir = dir.path().to_path_buf();
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.attach(
                    crate::launch::Started {
                        engine,
                        updates,
                        tab_dir,
                        store_id: store::paths::TabId::new(),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the tab's window");
        (window, tab, dir)
    }

    /// Pump the UI thread until `done` holds, which is what a test waits with: the
    /// engine is a real thread behind a real socket.
    fn pump_until(
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The ids one agent's transcript holds, in order.
    fn held_ids(tab: &Entity<TabContent>, cx: &TestAppContext) -> Vec<String> {
        cx.read(|cx| {
            tab.read(cx)
                .transcripts
                .get(&AgentKey::Coordinator)
                .map(|view| {
                    view.read(cx)
                        .items(cx)
                        .iter()
                        .map(|item| item.id.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// A user turn, as the stream publishes one.
    fn turn(id: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "op": "item.add", "topic": "session", "after": null,
            "item": {"id": id, "kind": "user", "ts": 1, "text": text, "status": "sent"}
        })
    }

    /// §9.1: the transcript holds the topic, and a view that has fallen behind it is
    /// re-read rather than left drawing a conversation that ended.
    ///
    /// A view that is *behind* its topic is invisible from the outside: the virtual
    /// list is exactly as tall as the view was told it is, so the tab holds the two
    /// to each other on every batch and re-reads the topic when they disagree. The
    /// divergence here is staged by taking a row off the view behind the model's
    /// back — the shape of an op the model took and the view never saw.
    #[gpui_kit::test]
    fn a_transcript_that_fell_behind_its_topic_is_re_read(cx: &mut TestAppContext) {
        // The engine wakes the app's tasks from its own thread, which the
        // deterministic scheduler has to allow.
        cx.executor().allow_parking();
        let (window, tab, dir) = live_tab(cx);
        // The tab's own server is a process of its own: its ready file is where the
        // test's control endpoints become reachable.
        pump_until(cx, "the tab's server", |_| {
            dir.path().join("ready.json").exists()
        });
        let control = swarm_client::harness::Control::attach(dir.path()).expect("the fake server");
        // A tab's boot is one snapshot and then one stream from the cursor that
        // snapshot was atomic at: an op published before that cursor is not this
        // tab's to see. The fake records the stream's own request, which is the
        // moment the cursor is fixed.
        pump_until(cx, "the tab's own stream", |_| {
            !control.requests_on("/stream").is_empty()
        });

        // Two turns on the stream: the page's model holds them and the transcript
        // draws them.
        control.emit(turn("e_1", "the first thing")).expect("emit");
        control.emit(turn("e_2", "and the second")).expect("emit");
        pump_until(cx, "the two turns in the transcript", |cx| {
            held_ids(&tab, cx) == vec!["e_1".to_string(), "e_2".to_string()]
        });

        // The view loses a row the topic still holds: what a missed op leaves behind.
        cx.update(|cx| {
            tab.update(cx, |tab, cx| {
                let view = tab
                    .transcripts
                    .get(&AgentKey::Coordinator)
                    .cloned()
                    .unwrap();
                view.update(cx, |view, cx| {
                    view.remove("e_1", cx);
                });
            });
        });
        assert_eq!(
            held_ids(&tab, cx),
            vec!["e_2".to_string()],
            "the view is behind its topic, and the list is as tall as the view says"
        );

        // The next op from the session is one more push: the two are compared, the
        // disagreement is said once, and the topic is re-read.
        control.emit(turn("e_3", "and a third")).expect("emit");
        pump_until(cx, "the topic re-read into the transcript", |cx| {
            held_ids(&tab, cx) == vec!["e_1".to_string(), "e_2".to_string(), "e_3".to_string()]
        });

        // And the invariant holds where it did not: the topic's own list, item for
        // item, with nothing left over.
        let topic: Vec<String> = cx.read(|cx| {
            tab.read(cx)
                .live
                .as_ref()
                .map(|live| {
                    live.model
                        .items(AgentKey::Coordinator)
                        .iter()
                        .map(|item| item.id.clone())
                        .collect()
                })
                .unwrap_or_default()
        });
        assert_eq!(held_ids(&tab, cx), topic);
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("the tab window");
    }

    /// The sequence the real journal shows at the moment a reader reported their
    /// transcript going quiet, item for item: a long `bash` call in flight, the
    /// reader's words sent into it (a queued row), the call aborted, the goal notice
    /// the aborted run minted, the queued row taken and answered, and the turns after
    /// it — every one of which the reader did not see
    /// (`~/.evo/sessions/-Users-bytedance-coding-evo-gui/20261001T112141Z_…sexp`,
    /// entries 1932-1943: the tool result with `:is-error t`, a `:source :goal`
    /// notice, the user's own entry, then four more assistant and tool entries).
    ///
    /// Nothing in the region is unusual — no removal, no reset, no id that moves —
    /// and this is the claim tested: the view takes every item of it, in the order
    /// the session published them, with the reader's own turn among the newest rows.
    #[gpui_kit::test]
    fn the_journals_own_sequence_at_the_reported_freeze_is_ordinary(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, tab, dir) = live_tab(cx);
        pump_until(cx, "the tab's server", |_| {
            dir.path().join("ready.json").exists()
        });
        let control = swarm_client::harness::Control::attach(dir.path()).expect("the fake server");
        pump_until(cx, "the tab's own stream", |_| {
            !control.requests_on("/stream").is_empty()
        });

        // The call in flight when the reader typed — a bash row, running.
        control
            .emit(serde_json::json!({
                "op": "item.add", "topic": "session", "after": null,
                "item": {"id": "t_call", "kind": "tool", "ts": 1, "call_id": "toolu_01Ny",
                         "name": "bash", "args": {"command": "cargo test"},
                         "status": "running", "result": null, "parent": "e_before"}
            }))
            .expect("emit");
        // The reader's words, queued into it: what the view publishes the moment they
        // are typed, with the id the entry will keep.
        control
            .emit(serde_json::json!({
                "op": "item.add", "topic": "session", "after": "t_call",
                "item": {"id": "c292ff93", "kind": "user", "ts": 2,
                         "text": "wait. evo-agent CI failing 7 checks. what are them? why?",
                         "images": [], "status": "queued", "queue": "now"}
            }))
            .expect("emit");
        // The abort: the same row, now an error, and the goal notice the run left.
        control
            .emit(serde_json::json!({
                "op": "item.patch", "topic": "session", "id": "t_call",
                "patch": {"status": "error",
                          "result": {"text": "Tool error: Command aborted by user.",
                                     "chars": 41, "truncated": false}}
            }))
            .expect("emit");
        control
            .emit(serde_json::json!({
                "op": "item.add", "topic": "session", "after": "c292ff93",
                "item": {"id": "06e743d2", "kind": "notice", "ts": 3, "severity": "info",
                         "text": "◆ goal g-fb41: complete", "source": "goal", "durable": true}
            }))
            .expect("emit");
        control
            .emit(serde_json::json!({
                "op": "item.add", "topic": "session", "after": "06e743d2",
                "item": {"id": "ro_1", "kind": "run_outcome", "ts": 4, "outcome": "aborted",
                         "error": null}
            }))
            .expect("emit");
        // Taken, and answered: the row is the same one, sent.
        control
            .emit(serde_json::json!({
                "op": "item.patch", "topic": "session", "id": "c292ff93",
                "patch": {"status": "sent"}
            }))
            .expect("emit");
        for (id, step) in [("e_answer", 5u64), ("t_after", 6), ("e_answer2", 7)] {
            control
                .emit(serde_json::json!({
                    "op": "item.add", "topic": "session", "after": null,
                    "item": {"id": id, "kind": "assistant", "ts": step,
                             "text": "and the answer", "status": "final"}
                }))
                .expect("emit");
        }

        let wanted = vec![
            "t_call".to_string(),
            "c292ff93".to_string(),
            "06e743d2".to_string(),
            "ro_1".to_string(),
            "e_answer".to_string(),
            "t_after".to_string(),
            "e_answer2".to_string(),
        ];
        pump_until(cx, "every item of the sequence in the transcript", |cx| {
            held_ids(&tab, cx) == wanted
        });

        // And the reader's own turn is taken, not still queued: the row the sequence
        // ends on is what the session says it is.
        let sent = cx.read(|cx| {
            tab.read(cx)
                .transcripts
                .get(&AgentKey::Coordinator)
                .map(|view| {
                    view.read(cx)
                        .items(cx)
                        .iter()
                        .find(|item| item.id == "c292ff93")
                        .map(|item| session::ItemKind::label(&item.kind).to_string())
                })
                .unwrap_or_default()
        });
        assert_eq!(sent.as_deref(), Some("user"));

        // The reader is at the foot of the list, following it — which is what the
        // reader who reported this saw (no "Jump to latest"), and what makes a row
        // that never arrives a row they cannot see. That the newest row is *built* and
        // above the fold when the view follows its tail is the transcript's own storm
        // test's; this one is about the sequence that got here.
        let (following, away) = cx.read(|cx| {
            let view = tab
                .read(cx)
                .transcripts
                .get(&AgentKey::Coordinator)
                .cloned()
                .expect("the coordinator's transcript");
            (
                view.read(cx).is_following_tail(cx),
                view.read(cx).is_away_from_latest(cx),
            )
        });
        assert!(following && !away, "the transcript is at its latest");
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("the tab window");
    }

    /// A window whose whole view is one tab: what drawing a tab again in another
    /// window is, with no chrome around it.
    struct Drawn(Entity<TabContent>);

    impl Render for Drawn {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    /// §9.1: the pump is the tab's only reader, and only the engine ends it.
    ///
    /// A tab's window can go away while the tab and its swarm live on — a page
    /// rebuilt in another window, a window closed — and every update after that has
    /// no window to be applied in. The pump must hold it, not end: ending it leaves a
    /// running swarm writing into a channel nobody reads, and the tab drawing the
    /// last frame it had for the rest of its life.
    #[gpui_kit::test]
    fn a_tab_whose_window_went_away_is_driven_again_in_the_next_one(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (window, tab, dir) = live_tab(cx);
        pump_until(cx, "the tab's server", |_| {
            dir.path().join("ready.json").exists()
        });
        let control = swarm_client::harness::Control::attach(dir.path()).expect("the fake server");
        pump_until(cx, "the tab's own stream", |_| {
            !control.requests_on("/stream").is_empty()
        });

        control
            .emit(turn("e_1", "before the window went"))
            .expect("emit");
        pump_until(cx, "the first turn", |cx| {
            held_ids(&tab, cx) == vec!["e_1".to_string()]
        });

        // The window the tab is drawn in goes away under it: this call marks it
        // removed, and the app drops it — and the entities it drew — on the way out.
        let _ = cx.update_window(window, |_, window, _| window.remove_window());
        cx.run_until_parked();

        // An op published now has no window to be applied in, and the pump has had
        // every chance to try: this is where a pump that ends on the first failure
        // gives up, taking the update with it.
        control
            .emit(turn("e_2", "with no window to be drawn in"))
            .expect("emit");
        // The pump asks again on a timer, and a test's clock is the test's to move:
        // three waits, which is three attempts.
        for _ in 0..3 {
            cx.executor().advance_clock(PUMP_RETRY);
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            held_ids(&tab, cx),
            vec!["e_1".to_string()],
            "the turn has nowhere to be drawn yet"
        );

        // The tab is drawn again, in another window: the update that had no window
        // is applied to the one that has it, and the view catches up.
        let other = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(1000.), px(700.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_window, cx| cx.new(|_cx| Drawn(tab.clone())),
            )
            .expect("a second window")
        });
        pump_until(cx, "the turn that had no window", |cx| {
            // The new window's own frame is what gives the tab a window again; the
            // pump's next wait is what applies the update it has been holding.
            cx.executor().advance_clock(PUMP_RETRY);
            held_ids(&tab, cx) == vec!["e_1".to_string(), "e_2".to_string()]
        });

        // And the tab is live in it, not merely caught up: an op published now
        // arrives because it was published, not because a window was drawn again.
        control
            .emit(turn("e_3", "after the second window"))
            .expect("emit");
        pump_until(cx, "the turn after it", |cx| {
            held_ids(&tab, cx) == vec!["e_1".to_string(), "e_2".to_string(), "e_3".to_string()]
        });

        // The first window really is gone: otherwise none of this was about a tab
        // whose window went away.
        assert!(
            cx.update_window(window, |_, _window, _| ()).is_err(),
            "the first window is gone"
        );
        drop(other);
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
                        page: None,
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

    /// §2.8: the conversation column embeds the transcript as a *cached* view. A
    /// notification anywhere else in the window — a lane's breathing dot asks for a frame
    /// every frame, the tab seals a refusal — must not render and lay out a journal of
    /// hundreds of rows again; the transcript's own news still does.
    #[gpui_kit::test]
    fn a_notify_elsewhere_does_not_re_render_the_transcript(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
            let items: Vec<Item> = (0..400)
                .map(|i| assistant(&format!("e_{i:04}"), "an answer", ""))
                .collect();
            view.update(cx, |view, cx| view.replace(items, cx));
        });
        // `Window::refresh` turns caching off for a frame — it is for a window that has
        // to be laid out from scratch — so these frames are drawn rather than refreshed.
        let frames = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                for _ in 0..3 {
                    window.draw(cx).clear(cx);
                }
            })
            .expect("the tab window");
        };
        frames(cx);
        let rendered = cx.read(|cx| view.read(cx).renders());
        assert!(rendered > 0, "the transcript rendered when it was shown");
        // The cached embedding still gives the transcript the whole box under the
        // header: a cached view is not measured from its rows, so a style that does not
        // size it outright leaves it 0px tall.
        cx.update_window(window, |_, window, _cx| {
            use gpui_kit::test::TestWindowExt as _;
            let transcript = window.find("transcript").bounds();
            let column = window.find("conversation-column").bounds();
            assert!(
                transcript.size.height > column.size.height / 2.,
                "the transcript fills the conversation column: {transcript:?} in {column:?}"
            );
        })
        .expect("the tab window");

        cx.update(|cx| tab.update(cx, |_, cx| cx.notify()));
        frames(cx);
        assert_eq!(
            cx.read(|cx| view.read(cx).renders()),
            rendered,
            "a notify elsewhere in the window does not render the transcript again"
        );

        // The transcript's own news is its own: it is rendered again.
        cx.update(|cx| view.update(cx, |view, cx| view.set_running(true, cx)));
        frames(cx);
        assert!(
            cx.read(|cx| view.read(cx).renders()) > rendered,
            "a notify on the transcript itself renders it"
        );
    }

    /// §5.4: a page that answered with nothing new — an overlap with what the topic
    /// holds, or the last page of all — still ends the wait. "Loading earlier items…" is
    /// the scrollback's only in-flight state, and an answer that changed no row used to
    /// leave its header stuck.
    #[gpui_kit::test]
    fn an_empty_page_still_ends_the_wait(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
        });
        // The reader asked for a page: the header is in flight.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_2", "newest", "")], cx);
                view.set_history(true, true, cx);
            });
        });
        assert!(cx.read(|cx| view.read(cx).is_loading_older()));

        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.apply(
                    Update::ItemsBefore {
                        topic: "session".to_string(),
                        body: serde_json::json!({"items": [], "has_more": true}),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the empty page");
        assert!(
            !cx.read(|cx| view.read(cx).is_loading_older()),
            "the header stops waiting though no row changed"
        );
    }

    /// §5.4: the same for a page that failed. Nothing is retried, but the wait ends and
    /// the reader can ask again — a failure must not leave the header loading forever.
    #[gpui_kit::test]
    fn a_failed_page_still_ends_the_wait(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
        });
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_2", "newest", "")], cx);
                view.set_history(true, true, cx);
            });
        });
        assert!(cx.read(|cx| view.read(cx).is_loading_older()));

        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| {
                tab.apply(
                    Update::FetchFailed {
                        what: "items before e_2 in session".to_string(),
                        reason: "503 unavailable".to_string(),
                        page: Some("session".to_string()),
                    },
                    window,
                    cx,
                );
            });
        })
        .expect("the failed page");
        assert!(
            !cx.read(|cx| view.read(cx).is_loading_older()),
            "a failed page ends the wait so the reader can ask again"
        );
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

    /// §7.3: the band over the transcript carries one quiet control — the reveal for
    /// thinking — and only while the transcript it heads has thinking to reveal. The
    /// state is the *view's* own, so the header asks the view rather than deciding.
    #[gpui_kit::test]
    fn the_thinking_reveal_is_offered_only_when_there_is_thinking(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let view = cx.update(|cx| cx.new(TranscriptView::new));
        cx.update(|cx| {
            tab.update(cx, |tab, _| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
        });

        // An answer with thinking: the band offers to show it, and pressing the
        // control is what flips it.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_1", "the answer", "a thought")], cx);
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("transcript-thinking").is_some(),
                "a transcript with thinking carries the reveal"
            );
        })
        .expect("the header's control");
        cx.update_window(window, |_, window, cx| {
            window.click("transcript-thinking", cx);
        })
        .expect("the press");
        assert!(
            cx.read(|cx| view.read(cx).is_showing_thinking(cx)),
            "the press is the view's own state"
        );

        // An answer that carried none: nothing to reveal, and so no control.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(vec![assistant("e_2", "the answer", "")], cx);
            });
        });
        assert!(!cx.read(|cx| view.read(cx).has_thinking(cx)));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("transcript-thinking").is_none(),
                "no thinking, no control"
            );
        })
        .expect("the header without it");
    }

    /// §7.3: a drawer folds out inside the box and is transient — a press that lands
    /// outside the box folds it, wherever on the page it lands, and a press inside the
    /// box leaves it alone (the design's `pointerdown` on the document, with the box
    /// answering for itself).
    #[gpui_kit::test]
    fn a_press_outside_the_box_folds_an_open_drawer(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let composer = cx.update(|cx| tab.read(cx).composer.clone());
        // The chips are the topic's own segments, so the drawer has a chip to open
        // from: an agent with a model, put in by hand the way the server would.
        cx.update(|cx| {
            composer.update(cx, |composer, cx| {
                composer.set_agent(&model_state(), "Coordinator", true, cx)
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("composer-chip-model", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_some(),
                "the drawer is out"
            );

            // A press on the transcript, which is the page's own surface and not the
            // box: the drawer folds.
            window.click("conversation-column", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_none(),
                "a press on the transcript folds it"
            );

            // And a press in the box — on the input, which is inside it — leaves it.
            window.click("composer-chip-model", cx);
            window.render_frame(cx);
            window.click("composer-box", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_some(),
                "a press on the box itself is not a press outside it"
            );
        })
        .expect("the page");
    }

    /// §7.3: the keyboard's own selection is a selection too — a drawer opened on one
    /// agent folds back when another is shown, without a press to carry it.
    #[gpui_kit::test]
    fn selecting_another_agent_folds_an_open_drawer(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let composer = cx.update(|cx| tab.read(cx).composer.clone());
        cx.update(|cx| {
            composer.update(cx, |composer, cx| {
                composer.set_agent(&model_state(), "Coordinator", true, cx)
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("composer-chip-model", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_some(),
                "the drawer is out"
            );
        })
        .expect("the page");
        // The selection, on its own update: an entity's event reaches its subscribers
        // when the update that emitted it returns, not inside it.
        cx.update(|cx| {
            tab.update(cx, |tab, cx| {
                tab.agents.update(cx, |_list, cx| {
                    cx.emit(agent_list::AgentListEvent::Select(AgentKey::Lane(1)))
                });
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_none(),
                "showing another agent's transcript folds the drawer"
            );
        })
        .expect("the page");
    }

    /// A `/catalog` body (§5.6), with evo's own casing on the providers and one
    /// registration it cannot reach.
    fn catalog_body() -> serde_json::Value {
        serde_json::json!({
            "models": [
                {"id": "stub-a", "provider": "OPENAI", "name": "Stub A", "api": "openai-chat",
                 "context_window": 200000, "reasoning": true, "images": true,
                 "effort_levels": ["low", "medium", "high", "xhigh", "max"],
                 "ready": true, "reason": null},
                {"id": "stub-b", "provider": "OPENAI", "name": "Stub B", "api": "openai-chat",
                 "context_window": 936000, "reasoning": false, "images": false,
                 "effort_levels": ["low", "high", "max"],
                 "ready": false, "reason": "no API key"},
            ],
            "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
            "commands": [
                {"name": "lore", "description": "durable guidance", "args_hint": "<text>"},
                {"name": "loop", "description": "run a prompt again and again",
                 "args_hint": null},
            ],
        })
    }

    /// §7.3, §5.6: the catalog's own `commands` are what a `/word` completes against, in a
    /// tab that is already running — evo's registry, never a list this client keeps.
    #[gpui_kit::test]
    /// The word a reader types is asked about (`complete`) and the answer comes back to
    /// the popup; the candidates a `/word` is *ranked* against are the catalog's own
    /// commands, which is what this test is about: the registry evo published reaches
    /// the box, in its own words.
    ///
    /// A running tab with no server under it has nothing to ask — its question is
    /// answered with nothing, and drawn as nothing. So the tab's own half of the round
    /// trip is what this test plays, with the question the box asked.
    #[gpui_kit::test]
    fn the_catalogs_commands_reach_a_running_tabs_popup(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let composer = cx.update(|cx| tab.read(cx).composer.clone());
        let asked: Rc<RefCell<Vec<ComposerEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = asked.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });

        cx.update_window(window, |_, window, cx| {
            let data = LauncherData {
                catalog: Some(catalog_body()),
                ..LauncherData::default()
            };
            tab.update(cx, |tab, cx| tab.set_launcher_data(&data, window, cx));
            // The caret in the input, then the word a reader would type. Rendering
            // first is what puts the input in the window's own tree, and
            // `focus_primary` is what opening this tab does to put the keyboard there.
            window.render_frame(cx);
            tab.update(cx, |tab, cx| {
                tab.focus_primary(window, cx);
            });
            window.input("/lo", cx);
        })
        .expect("the page");

        // The debounce, and then the answer to the question the box asked: the caret is
        // on the command word `/lo`, whose name replaces the two characters after the
        // slash — what `complete` answers, and what the popup is drawn from.
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        let Some(ComposerEvent::Complete { text, cursor }) = asked.borrow().last().cloned() else {
            panic!("the box asked nothing about the word it is typing");
        };
        cx.update(|cx| {
            composer.update(cx, |composer, cx| {
                composer.set_completion(
                    &text,
                    cursor,
                    composer::Answer {
                        kind: Some(composer::CompletionKind::Command),
                        name: 1..cursor,
                        items: Vec::new(),
                    },
                    cx,
                )
            })
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("completion-row-0").label(),
                Some("/lore · durable guidance"),
                "the registry's first command, in the registry's own words"
            );
            assert_eq!(
                window.find("completion-row-1").label(),
                Some("/loop · run a prompt again and again"),
                "and the second: the word is a prefix of one and a subsequence of the other"
            );
        })
        .expect("the page");

        // Enter takes the highlighted row: the word the registry's own list offered is
        // written out, and the message is not sent.
        cx.update_window(window, |_, window, cx| window.press("enter", cx))
            .expect("the page");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(("input", 4294967302u64)).value(),
                Some("/lore "),
                "the row was taken"
            );
        })
        .expect("the page");
    }

    /// §7.3: a question left waiting when the engine or the server goes is released, and
    /// with it the rid it was filed under — no reply is coming for either, and a box
    /// waiting on one would never ask about another word. Every other op stays pending,
    /// because its own reply (or the tab's own end) is what releases it.
    #[test]
    fn a_lost_session_releases_the_questions_it_was_holding() {
        let asked = |text: &str| Completing {
            text: text.to_string(),
            cursor: 3,
        };
        let mut pending: BTreeMap<String, Pending> = BTreeMap::new();
        pending.insert("1".into(), Pending::Send);
        pending.insert("2".into(), Pending::Complete(asked("/lo")));
        pending.insert("3".into(), Pending::Interrupt);
        pending.insert("4".into(), Pending::Complete(asked("/ev")));

        assert_eq!(
            waiting_completions(&mut pending),
            vec![asked("/lo"), asked("/ev")],
            "every question comes out, in the order they were filed; the other ops do not"
        );
        assert_eq!(
            pending.keys().collect::<Vec<_>>(),
            vec!["1", "3"],
            "and the rids nothing will answer go with them"
        );
        assert!(waiting_completions(&mut pending).is_empty());
    }

    /// §7.3: the box asks about the word under the caret, and a question **nothing could
    /// take** — a tab with no server under it yet, which is every tab between its first
    /// catalog and its launch — is still a question to answer. It is answered with
    /// nothing, and the word typed once there *is* a server is asked about like any
    /// other; a question left hanging is the box asked once and never again.
    #[gpui_kit::test]
    fn a_question_no_server_took_is_answered_and_the_next_one_is_asked(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let composer = cx.update(|cx| tab.read(cx).composer.clone());
        let asked: Rc<RefCell<Vec<ComposerEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = asked.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });

        // The catalog the app read before this tab had a server: the box takes it, and
        // asks about the caret in its own box.
        cx.update_window(window, |_, window, cx| {
            let data = LauncherData {
                catalog: Some(catalog_body()),
                ..LauncherData::default()
            };
            tab.update(cx, |tab, cx| tab.set_launcher_data(&data, window, cx));
        })
        .expect("the page");
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        assert!(
            !asked.borrow().is_empty(),
            "the box asks about the caret its catalog arrived on"
        );

        // Nobody took that question — this tab is running no server — and the word
        // typed now is still asked about, which is the popup's whole existence.
        let before = asked.borrow().len();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
            window.input("/lo", cx);
        })
        .expect("the page");
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        assert!(
            asked.borrow().len() > before,
            "a question nobody answered still lets the next one out: {:?}",
            asked.borrow()
        );
        assert_eq!(
            asked.borrow().last(),
            Some(&ComposerEvent::Complete {
                text: "/lo".to_string(),
                cursor: 3,
            }),
            "and it is about the word the caret is on"
        );
    }

    /// §5.6: the catalog the app learned reaches the composer of a tab that is already
    /// running — not only the empty tab's choosers read it — so the drawer lists evo's
    /// own registrations, and picking one is a `model.set` naming that registration.
    ///
    /// The id and the provider are the catalog's (a provider is matched lower-cased, as
    /// §5.6 compares it); a registration evo cannot reach is listed with evo's reason.
    #[gpui_kit::test]
    fn the_catalogs_models_reach_a_running_tabs_drawer(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);
        let composer = cx.update(|cx| tab.read(cx).composer.clone());
        let events: Rc<RefCell<Vec<ComposerEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });

        cx.update_window(window, |_, window, cx| {
            let data = LauncherData {
                catalog: Some(catalog_body()),
                ..LauncherData::default()
            };
            tab.update(cx, |tab, cx| tab.set_launcher_data(&data, window, cx));
            // The selected agent's own state, which is what gives the drawer its chip.
            composer.update(cx, |composer, cx| {
                composer.set_agent(&model_state(), "Coordinator", true, cx)
            });
            window.render_frame(cx);
            window.click("composer-chip-model", cx);
            window.render_frame(cx);
            assert!(
                window.find("drawer-model-stub-a-openai").visible()
                    && window.find("drawer-model-stub-b-openai").visible(),
                "the catalog's registrations are in the drawer"
            );
            assert!(
                window.find("composer-effort").visible(),
                "and the ladder the session accepts"
            );
            assert_eq!(
                window.find("drawer-model-stub-b-openai").label(),
                Some("openai · stub-b no API key"),
                "a registration evo cannot reach is listed with evo's own reason"
            );
            assert_eq!(
                window.find("drawer-model-stub-a-openai").label(),
                Some("openai · stub-a 200k ctx · vision · effort low, medium, high, xhigh, max"),
                "and a model it can run is listed with the catalog's own line: the ctx \
                 window, the modalities, and the levels that registration takes"
            );
        })
        .expect("the page");

        cx.update_window(window, |_, window, cx| {
            window.click("drawer-model-stub-b-openai", cx)
        })
        .expect("the page");
        assert_eq!(
            events.borrow().as_slice(),
            [ComposerEvent::ModelSet {
                id: "stub-b".to_string(),
                provider: "openai".to_string(),
            }],
            "picking a row is a model.set naming the catalog's registration"
        );
    }

    /// One topic's own state, as the server publishes it: a model, so the foot row has
    /// a chip that opens the model drawer (`GET /snapshot`).
    fn model_state() -> session::TopicState {
        session::TopicState::from_json(&serde_json::json!({
            "status": "idle",
            "model": {"id": "stub-a", "provider": "openai", "ready": true},
            "thinking": "medium",
            "segments": [
                {"name": "model", "order": 100, "side": "left", "text": "stub-a", "data": {}},
                {"name": "thinking", "order": 200, "side": "left", "text": "medium",
                 "data": {}},
            ],
        }))
    }
}
