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
use session::{Activity, AgentKey, Changes, LaunchPlan, RowChanges, TabModel};
use tab_engine::{Agent, EngineHandle, ReqId, Update};
use transcript::TranscriptView;

use agent_list::{AgentList, AgentListEvent};

use crate::chrome::LauncherData;
use crate::empty_tab::{Choosers, HistoryList};
use crate::history::{folder_name, HistoryRow};
use crate::launch::{Launch, SwarmConfig};

/// Called with a live tab's `/registry`, so the app can refresh its model cache
/// from a real server (§9.4).
pub type RegistryHook = std::rc::Rc<dyn Fn(&serde_json::Value, &mut App)>;

/// How often the step clock re-renders while a turn runs. It is a UI ticker and
/// nothing else: no request, no state (§7.3).
const STEP_TICK: Duration = Duration::from_secs(1);

/// How long a notice above the composer stays: long enough to read a refusal,
/// short enough that it is gone before it becomes furniture (§4).
const NOTICE_LIFETIME: Duration = Duration::from_secs(4);

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

/// What a rejected POST reads as (§4).
///
/// The decision is the server's, not ours: `not_now` is tab_engine's reading of
/// `409`, and everything else — `422`, `503`, a broken connection — is a failure
/// carrying the reply's own `error`.
pub(crate) fn notice_tone(error: &tab_engine::PostError) -> NoticeTone {
    if error.not_now {
        NoticeTone::Dim
    } else {
        NoticeTone::Error
    }
}

/// What a failed POST says above the composer, and the raw error to keep for the
/// hover (§4).
///
/// A reply is the server's own words, always: `409`, `422`, `400` are the swarm
/// talking about the request, and re-stating them is the one thing §8 forbids.
/// The two cases with nothing to re-state are:
///
/// - **no reply at all** — the connection, the deadline, the half-read answer.
///   `io: Connection refused (os error 61)` is the socket talking, not the swarm,
///   and what happened is that the swarm is not there (yet).
/// - **`503`** — the server answered, and its answer is that it is going away.
///
/// Both get a sentence; the raw text is returned as the detail so it can ride in
/// the notice's tooltip instead of being dropped.
fn notice_words(error: &tab_engine::PostError) -> (String, Option<String>) {
    let plain = match error.status {
        None => "Can't reach the swarm — it may be restarting.",
        Some(503) => "The swarm is shutting down.",
        Some(_) => return (error.message.clone(), None),
    };
    let raw = error.message.trim();
    let detail = (!raw.is_empty() && raw != plain).then(|| raw.to_owned());
    (plain.to_owned(), detail)
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
    /// The swarm never came up: why, and the tail of its log (§9.7).
    ///
    /// `message` is the engine's own one-line reason — a binary that is not
    /// there, a folder that cannot be written — which is what a person needs
    /// first; `log_tail` is the evidence behind it, and may be empty.
    Failed {
        folder: PathBuf,
        message: Option<String>,
        log_tail: String,
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
    /// todo panel all belong to one agent and to one revision counter (see
    /// docs/review-1.md F5).
    pub(crate) transcripts: BTreeMap<AgentKey, Entity<TranscriptView>>,
    pub(crate) composer: Entity<Composer>,
    /// What every tab of this window starts its swarms with.
    config: Arc<SwarmConfig>,
    /// The swarm this tab is driving, once one has started.
    live: Option<Live>,
    /// The last launch, so a failed boot's Retry can ask for the same thing again
    /// (§9.7).
    last_launch: Option<Launch>,
    /// Why the tab's swarm is gone, when it went away on its own.
    gone: Option<SharedString>,
    /// Called with this tab's `/registry` (§9.4).
    registry_hook: Option<RegistryHook>,
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

/// Everything the agent list draws, read off the model in one go (§7.3).
struct AgentsSnapshot {
    lanes: session::LaneList,
    activity: Activity,
    reconnecting: bool,
    selected: AgentKey,
    clock: Option<String>,
    /// Why a lane is down, by lane number (§9.7).
    down_reasons: Vec<(u32, Option<String>)>,
}

/// One tab's live swarm: the engine, the model its updates land in, and the pump
/// that carries them across.
struct Live {
    engine: EngineHandle,
    model: TabModel,
    /// The task that applies the engine's updates on the UI thread. Held only so
    /// it lives exactly as long as the tab does: dropping the tab — or handing
    /// the engine over to be stopped — cancels the pump.
    _pump: Task<()>,
    /// `tabs/<id>/` — where the swarm keeps its `token` and its `swarm.log` (§6).
    tab_dir: PathBuf,
    /// The next `POST`'s id, and the one a reply is still expected for.
    next_req: ReqId,
    in_flight: Option<ReqId>,
    /// The last `/state` revision applied: an answer from an older fetch is
    /// dropped instead of overwriting newer state (§9.1).
    state_revision: u64,
    /// True once this tab's session has been recorded as a recent (§9.5).
    recorded: bool,
    /// The swarm process's pid, from `/health` (§3).
    pid: Option<u32>,
    /// The `tabs/<id>/` directory this tab's swarm keeps its files in (§6).
    store_id: store::paths::TabId,
    /// The one-second re-render that keeps the coordinator's step clock moving
    /// while a turn runs; `None` when nothing is running.
    ticker: Option<Task<()>>,
}

impl Live {
    fn next_req(&mut self) -> ReqId {
        self.next_req += 1;
        self.next_req
    }
}

impl TabContent {
    /// A fresh tab: nothing chosen yet (§7.2). The choosers need the window they
    /// will be rendered in.
    pub fn new(
        id: TabId,
        config: Arc<SwarmConfig>,
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
        let agents_subscription =
            cx.subscribe(&agents, |this, _list, event: &AgentListEvent, cx| {
                let AgentListEvent::Select(agent) = event;
                this.select_agent(*agent, cx);
            });

        TabContent {
            id,
            state: TabState::Empty,
            choosers: Choosers::new(window, cx),
            history,
            transcripts: BTreeMap::new(),
            composer,
            config,
            live: None,
            last_launch: None,
            gone: None,
            registry_hook: None,
            notice: None,
            notice_task: None,
            session: None,
            agents,
            _subscriptions: vec![
                history_subscription,
                composer_subscription,
                agents_subscription,
            ],
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
            | TabState::Failed { folder, .. }
            | TabState::Stopping { folder } => Some(folder),
        }
    }

    /// The session the tab's swarm is writing to, while `/state` has named it.
    pub fn session_path(&self) -> Option<&Path> {
        self.session.as_deref()
    }

    /// The process behind the tab: the swarm's own pid, once `/health` has
    /// answered (§3). Diagnostics, and how §9.7's failure modes are exercised.
    pub fn swarm_pid(&self) -> Option<u32> {
        self.live.as_ref().and_then(|live| live.pid)
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
        self.model()
            .is_some_and(|model| model.is_reconnecting(model.selected()))
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
    /// them (§9.4, §9.5).
    pub fn set_launcher_data(
        &mut self,
        data: &LauncherData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(raw) = &data.registry {
            // The engine hands `/registry` over as it arrived; the choosers want
            // it parsed, so a reply that no longer parses is simply skipped.
            if let Ok(typed) = serde_json::from_value::<swarm_client::Registry>(raw.clone()) {
                let registry = swarm_client::Payload {
                    typed,
                    raw: raw.clone(),
                };
                self.set_registry(&registry, window, cx);
            }
        }
        if let Some(cache) = &data.model_cache {
            self.set_model_cache(cache, window, cx);
        }
        self.set_catalog_error(data.catalog_error.clone(), cx);
        self.set_scanning(data.scanning, cx);
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

    /// Install the hook a live tab's `/registry` is reported to (§9.4).
    pub fn set_registry_hook(&mut self, hook: Option<RegistryHook>) {
        self.registry_hook = hook;
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
                self.state = TabState::Booting { folder };
                self.attach(started, window, cx);
            }
            Err(error) => {
                // Nothing is running: show why, with what little there is to show.
                self.last_launch = Some(launch);
                self.state = TabState::Failed {
                    folder,
                    message: Some(format!("could not prepare this tab: {error}")),
                    log_tail: String::new(),
                };
                cx.notify();
            }
        }
    }

    /// Try the same launch again, after a boot that failed (§9.7).
    pub fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(launch) = self.last_launch.clone() {
            self.launch(launch, window, cx);
        }
    }

    /// Take the engine out of the tab, so its swarm can be stopped somewhere that
    /// is not the UI thread (§9.8). The tab keeps what it shows; it stops
    /// watching and stops typing to the server.
    pub fn take_engine(&mut self, cx: &mut Context<Self>) -> Option<EngineHandle> {
        let live = self.live.take()?;
        if let Some(folder) = self.folder().map(Path::to_path_buf) {
            self.state = TabState::Stopping { folder };
            cx.notify();
        }
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
        self.transcripts.insert(AgentKey::Coordinator, coordinator);
        self.live = Some(Live {
            engine: started.engine,
            model: TabModel::new(),
            _pump: pump,
            tab_dir: started.tab_dir,
            next_req: 0,
            in_flight: None,
            state_revision: 0,
            recorded: false,
            pid: None,
            store_id: started.store_id,
            ticker: None,
        });
        cx.notify();
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
        match update {
            Update::Booting => {}
            Update::Ready { pid, .. } => {
                if let Some(live) = self.live.as_mut() {
                    live.pid = Some(pid);
                }
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    self.state = TabState::Running { folder };
                    cx.notify();
                }
            }
            Update::BootFailed { message, log_tail } => {
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    self.state = TabState::Failed {
                        folder,
                        message: Some(message),
                        log_tail,
                    };
                    cx.notify();
                }
            }
            Update::Registry { raw } => {
                if let Some(hook) = self.registry_hook.clone() {
                    hook(&raw, cx);
                }
                let changes = self.live.as_mut().map(|live| live.model.on_registry(&raw));
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
            }
            Update::State { revision, raw } => {
                let changes = match self.live.as_mut() {
                    Some(live) if revision > live.state_revision => {
                        live.state_revision = revision;
                        Some(live.model.on_state(&raw))
                    }
                    _ => None,
                };
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
                // The session the swarm is writing to is what history resumes
                // (§9.5); it is known once /state has answered.
                if let Some(session) = raw.get("session").and_then(serde_json::Value::as_str) {
                    self.session = Some(PathBuf::from(session));
                    self.record_recent(cx);
                }
            }
            Update::PostResult { req_id, result } => self.finish_post(req_id, result, window, cx),
            Update::ServerGone => self.server_gone(window, cx),
            Update::Exited { outcome } => {
                // The engine stopped. A tab being closed never sees this; one that
                // is still on screen says so and keeps what it has.
                self.gone = Some(format!("swarm stopped ({outcome:?})").into());
                cx.notify();
            }
            update => {
                let changes = match self.live.as_mut() {
                    Some(live) => live.absorb(update),
                    None => None,
                };
                if let Some(changes) = changes {
                    self.push(changes, cx);
                }
            }
        }
    }

    /// Hand the model's [`Changes`] to the views that show them (§9.1).
    fn push(&mut self, changes: Changes, cx: &mut Context<Self>) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let selected = live.model.selected();
        // The view's own counter is the model's per-agent revision: this view
        // shows one agent, so it never has to compare two agents' numbering.
        let revision = live
            .model
            .agent_model(selected)
            .map(|model| model.revision())
            .unwrap_or_default();

        let rebuilt = match changes.rows_for(selected) {
            Some(RowChanges::Rebuilt) => Some(live.model.selected_rows().to_vec()),
            _ => None,
        };
        let changed: Vec<session::Row> = match changes.rows_for(selected) {
            Some(RowChanges::Changed(ids)) => ids
                .iter()
                .filter_map(|id| live.model.agent_model(selected)?.row(*id).cloned())
                .collect(),
            _ => Vec::new(),
        };
        let todos = changes
            .todos
            .contains(&selected)
            .then(|| live.model.selected_todos().to_vec());
        let readout = changes.readout.then(|| live.model.readout_text());
        let activity = changes.activity.then(|| live.model.activity());

        let Some(view) = self.transcripts.get(&selected).cloned() else {
            return;
        };
        if let Some(rows) = rebuilt {
            view.update(cx, |view, cx| view.replace(revision, rows, cx));
        }
        if !changed.is_empty() {
            view.update(cx, |view, cx| {
                for row in changed {
                    view.upsert(revision, row, cx);
                }
            });
        }
        if let Some(todos) = todos {
            view.update(cx, |view, cx| view.set_todos(todos, cx));
        }
        if let Some(readout) = readout {
            self.composer
                .update(cx, |composer, cx| composer.set_readout(readout, cx));
        }
        if let Some(activity) = activity {
            self.composer
                .update(cx, |composer, cx| composer.set_activity(activity, cx));
        }
        // The left column takes the same facts, on the same batch (§7.3).
        self.sync_agents(cx);
        cx.notify();
    }

    /// Feed the agent list from the model: the rows, the coordinator's activity and
    /// step clock, the selection, and why each down lane is down (§7.3, §9.7).
    fn sync_agents(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.agents_snapshot() else {
            return;
        };
        self.agents.update(cx, |list, cx| {
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
        let model = self.model()?;
        let selected = model.selected();
        Some(AgentsSnapshot {
            lanes: model.lanes().clone(),
            activity: model.activity(),
            reconnecting: model.is_reconnecting(selected),
            selected,
            clock: model
                .coordinator_step_started()
                .and_then(|clock| clock.clock_label(now_millis())),
            down_reasons: model
                .lanes()
                .lanes
                .iter()
                .map(|lane| (lane.n as u32, model.lane_down_reason(lane.n as u32)))
                .collect(),
        })
    }

    /// Start the once-a-second re-render while the coordinator has a step in
    /// flight, and stop it when it does not (§7.3).
    fn ensure_step_ticker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let running = self
            .model()
            .is_some_and(|model| model.coordinator_step_started().is_some());
        // A ticker that has ended is not a ticker: the next step starts a new one.
        let ticker_live = self
            .live
            .as_ref()
            .and_then(|live| live.ticker.as_ref())
            .is_some_and(|ticker| !ticker.is_ready());
        if !running || ticker_live {
            return;
        }

        let ticker = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(STEP_TICK).await;
            let stepped = this.update_in(cx, |tab, _window, cx| {
                let running = tab
                    .model()
                    .is_some_and(|model| model.coordinator_step_started().is_some());
                if running {
                    // The step clock is the only thing that changed (§7.3).
                    tab.sync_agents(cx);
                    cx.notify();
                }
                running
            });
            match stepped {
                Ok(true) => {}
                // The tab is gone, or the step ended: stop ticking.
                _ => break,
            }
        });
        if let Some(live) = self.live.as_mut() {
            live.ticker = Some(ticker);
        }
    }

    /// How long the coordinator's current step has been running, in seconds
    /// (§7.3, §5's `turn-start` clock).
    pub fn step_seconds(&self) -> Option<u64> {
        let started = self
            .model()?
            .coordinator_step_started()?
            .started_at_millis?;
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
            let changes = live.model.select(agent);
            // Only the lane being shown is watched (§9.3).
            live.engine.watch_lane(agent.lane());
            changes
        };
        // An agent's view exists from the moment it is first shown, and lives as
        // long as the tab: switching back keeps its scroll and its documents.
        let view = self
            .transcripts
            .entry(agent)
            .or_insert_with(|| cx.new(TranscriptView::new))
            .clone();
        // The view says whose transcript it is, which is what an empty one shows.
        view.update(cx, |view, cx| view.set_agent(agent, cx));
        self.push(changes, cx);
        self.sync_agents(cx);
    }

    /// What the composer asked for: a turn, or an interrupt (§7.3, §9.2).
    fn on_composer_event(
        &mut self,
        event: ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sent = match self.live.as_mut() {
            Some(live) => {
                let req = live.next_req();
                let sent = match event {
                    ComposerEvent::Send(text) => live.engine.prompt(req, text),
                    ComposerEvent::Interrupt => live.engine.interrupt(req),
                };
                if sent {
                    live.in_flight = Some(req);
                }
                sent
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

    /// A `POST` came back: the button takes its outcome, and a refusal is shown
    /// from the reply's own words — never re-validated here (§8).
    fn finish_post(
        &mut self,
        req_id: ReqId,
        result: Result<swarm_client::Envelope, tab_engine::PostError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mine = self
            .live
            .as_mut()
            .is_some_and(|live| live.in_flight == Some(req_id));
        if mine {
            if let Some(live) = self.live.as_mut() {
                live.in_flight = None;
            }
            self.composer.update(cx, |composer, cx| {
                composer.request_finished(result.is_ok(), window, cx)
            });
        }
        if let Err(error) = result {
            let tone = notice_tone(&error);
            let (text, detail) = notice_words(&error);
            self.show_notice(text, detail, tone, cx);
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

    /// Record the session this tab started, once, so the app's own swarms appear
    /// in history (§9.5).
    ///
    /// The write is `~/.evo/desktop/app.json`, which is file I/O: it happens on a
    /// thread of its own, like every other write the UI asks for.
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
        if live.recorded {
            return;
        }
        live.recorded = true;
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
            .or_else(|| {
                live.model
                    .lanes()
                    .swarm
                    .as_ref()
                    .map(|swarm| swarm.workers as u32)
            })
            .unwrap_or_default();

        let mut recent = store::app_state::Recent::new(session, folder, lanes);
        recent.models = models;
        let root = self.config.root.clone();
        crate::bridge::Bridge::<()>::spawn(
            crate::bridge::Revision::new(0),
            move |_updates: crate::bridge::BridgeSender<()>| {
                let mut state = store::app_state::AppState::load(&root);
                state.touch_recent(recent);
                let _ = state.save(&root);
            },
        );
        let _ = cx;
    }

    /// Show a boot-style failure for a swarm that went away after it was up.
    fn fail_with_log(&mut self, log_tail: String, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder().map(Path::to_path_buf) {
            // A swarm that died after it was up has no one-line reason: the log
            // is the whole story.
            self.state = TabState::Failed {
                folder,
                message: None,
                log_tail,
            };
            cx.notify();
        }
    }
}

impl Live {
    /// Fold one engine update into the model, answering the [`Changes`] the UI
    /// has to apply — or `None` when the update was not the model's.
    fn absorb(&mut self, update: Update) -> Option<Changes> {
        let changes = match update {
            Update::Transcript {
                agent,
                revision,
                raw,
            } => self.model.on_transcript(agent_key(agent), revision, &raw),
            Update::Lanes { raw } => self.model.on_lanes(&raw),
            Update::Event {
                agent,
                id,
                kind,
                data,
            } => {
                let id = id.unwrap_or_default().max(0) as u64;
                // The event is stamped with when the UI saw it, which is what the
                // step clock counts from (§7.3).
                self.model
                    .on_event_at(agent_key(agent), id, &kind, &data, now_millis())
            }
            Update::Stream { agent, status } => self
                .model
                .on_stream(agent_key(agent), stream_status(status)),
            Update::CacheSeed { entry } => self.model.on_cache_seed(entry.as_ref()),
            _ => return None,
        };
        Some(changes)
    }
}

/// The engine names an agent; the model names the same one its own way.
fn agent_key(agent: Agent) -> AgentKey {
    match agent {
        Agent::Coordinator => AgentKey::Coordinator,
        Agent::Lane(n) => AgentKey::Lane(n),
    }
}

/// The engine's stream status, as the model spells it for the badge (§9.7).
fn stream_status(status: tab_engine::StreamStatus) -> session::StreamStatus {
    match status {
        tab_engine::StreamStatus::Connected => session::StreamStatus::Connected,
        tab_engine::StreamStatus::Reconnecting { retry_in } => {
            session::StreamStatus::Reconnecting { retry_in }
        }
    }
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

    /// A tab showing a page of a swarm that is not there: what the composer's own
    /// refusals need, and nothing more.
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
                    TabContent::new(TabId::new(1), Arc::new(SwarmConfig::default()), window, cx)
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

    /// A POST came back refused.
    fn refused(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        tab: &Entity<TabContent>,
        status: u16,
        message: &str,
    ) {
        let error = tab_engine::PostError {
            status: Some(status),
            not_now: status == 409,
            message: message.to_string(),
        };
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| tab.finish_post(1, Err(error), window, cx));
        })
        .expect("the refused post");
    }

    /// A POST that never reached a server: the transport's own words are not what
    /// a person needs, so the line says what happened and the raw error waits on
    /// hover (§4).
    fn transport_failed(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        tab: &Entity<TabContent>,
        raw: &str,
    ) {
        let error = tab_engine::PostError {
            status: None,
            not_now: false,
            message: raw.to_string(),
        };
        cx.update_window(window, |_, window, cx| {
            tab.update(cx, |tab, cx| tab.finish_post(1, Err(error), window, cx));
        })
        .expect("the failed post");
    }

    /// §4, §9.2: a refusal is a dim line above the composer, a failure is an
    /// error-coloured one, both in the server's own words, and neither is a modal:
    /// the page keeps its place and the line leaves on its own.
    #[gpui_kit::test]
    fn a_refused_post_is_a_line_above_the_composer(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);

        // `409 not now`: the server cannot take this right now (§4).
        refused(cx, window, &tab, 409, "no goal to pause");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.find("tab-page").visible(),
                "a refusal is not a modal: the page stays"
            );
            assert_eq!(
                window.find("composer-notice-dim").label().as_deref(),
                Some("no goal to pause"),
                "the notice carries the server's own words"
            );
            assert!(
                window.try_find("composer-notice-error").is_none(),
                "and is not dressed as a failure"
            );
        })
        .expect("the dim notice frame");

        // `422`: the command ran and failed. Same place, the reply's `error` (§4).
        refused(cx, window, &tab, 422, "unreadable code");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.find("composer-notice-error").visible(),
                "a failure is shown as one"
            );
            assert!(
                window.try_find("composer-notice-dim").is_none(),
                "and replaces the refusal it followed"
            );
        })
        .expect("the error notice frame");

        // A POST the server never answered (§9.7): the socket's `io:` line is not
        // what a person reads, so the line says what it means and the raw error
        // is what hovers over it.
        transport_failed(cx, window, &tab, "io: Connection refused (os error 61)");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("composer-notice-error").label().as_deref(),
                Some("Can't reach the swarm — it may be restarting."),
                "an unanswered POST says what happened, not what the socket said"
            );
        })
        .expect("the transport notice frame");
        assert_eq!(
            cx.update(|cx| tab.read(cx).notice_detail().map(str::to_owned)),
            Some("io: Connection refused (os error 61)".to_string()),
            "and the raw error is not thrown away: it is the notice's tooltip"
        );

        // It goes away by itself, on the app clock (§4).
        let mut gone = false;
        for _ in 0..5 {
            // The clock moves outside an app update: a task that wakes up while
            // the app is already borrowed cannot update its own entity.
            cx.executor()
                .advance_clock(NOTICE_LIFETIME + Duration::from_secs(1));
            cx.run_until_parked();
            gone = cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    assert!(window.find("tab-page").visible());
                    window.try_find("composer-notice-error").is_none()
                })
                .expect("the frame after the notice");
            if gone {
                break;
            }
        }
        assert!(gone, "the notice ages out on its own");
    }

    /// §7.3: the header's thinking control appears only when the shown agent has
    /// thinking text, and it drives that transcript's own state.
    #[gpui_kit::test]
    fn the_thinking_toggle_follows_the_shown_transcript(cx: &mut TestAppContext) {
        let (window, tab) = running_tab(cx);

        // Nothing to reveal: a control that reveals nothing is noise.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("transcript-thinking").is_none());
        })
        .expect("the header without thinking");

        // The coordinator's view, as a tab with a swarm has one.
        let view = cx.update(|cx| {
            let view = cx.new(TranscriptView::new);
            view.update(cx, |view, cx| view.set_agent(AgentKey::Coordinator, cx));
            tab.update(cx, |tab, _cx| {
                tab.transcripts.insert(AgentKey::Coordinator, view.clone());
            });
            view
        });

        // An assistant row that carries thinking: the control appears.
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.replace(
                    1,
                    vec![session::Row {
                        id: 1,
                        version: 1,
                        kind: session::RowKind::Assistant {
                            markdown: "an answer".into(),
                            thinking: "because".into(),
                            streaming: false,
                            error: None,
                        },
                    }],
                    cx,
                );
            });
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("transcript-thinking").label().as_deref(),
                Some("Show thinking"),
                "thinking is hidden until it is asked for"
            );
            window.click("transcript-thinking", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("transcript-thinking").label().as_deref(),
                Some("Hide thinking"),
                "and the button says what it will do next"
            );
        })
        .expect("the header with thinking");

        cx.update(|cx| {
            assert!(
                view.read(cx).is_showing_thinking(cx),
                "the click reached the transcript's own state"
            );
        });
    }

    /// §4: a POST the server never answered has no reply to quote, so the line
    /// says what happened in plain words — and keeps the raw error for the hover,
    /// where `io: Connection refused (os error 61)` is the evidence.
    #[test]
    fn a_post_with_no_reply_is_said_in_plain_words() {
        for raw in [
            "io: Connection refused (os error 61)",
            "connection closed",
            "timeout: POST /prompt after 30s",
            "protocol: expected an HTTP status line",
        ] {
            let error = tab_engine::PostError {
                status: None,
                not_now: false,
                message: raw.to_owned(),
            };
            let (text, detail) = notice_words(&error);
            assert_eq!(text, "Can't reach the swarm — it may be restarting.");
            assert_eq!(detail.as_deref(), Some(raw), "the raw text is kept");
        }

        // Nothing was said, so there is nothing to hover.
        let silent = tab_engine::PostError {
            status: None,
            not_now: false,
            message: "  ".into(),
        };
        assert_eq!(notice_words(&silent).1, None);
    }

    /// §4: `503` is a reply — the server saying it is going away — and the line
    /// says so instead of quoting the shutdown text.
    #[test]
    fn a_shutting_down_server_says_so() {
        let error = tab_engine::PostError {
            status: Some(503),
            not_now: false,
            message: "the server is shutting down".into(),
        };
        let (text, detail) = notice_words(&error);
        assert_eq!(text, "The swarm is shutting down.");
        assert_eq!(detail.as_deref(), Some("the server is shutting down"));

        // Already the same words: nothing to add on hover.
        let same = tab_engine::PostError {
            status: Some(503),
            not_now: false,
            message: "The swarm is shutting down.".into(),
        };
        assert_eq!(notice_words(&same).1, None);
    }

    /// A reply is the server's own words, verbatim and with nothing hidden behind
    /// a hover: `409`, `422`, `400` are the swarm talking about the request (§8).
    #[test]
    fn a_reply_is_shown_as_the_server_wrote_it() {
        for status in [409u16, 422, 400, 404, 500] {
            let error = tab_engine::PostError {
                status: Some(status),
                not_now: status == 409,
                message: "unreadable code".into(),
            };
            let (text, detail) = notice_words(&error);
            assert_eq!(text, "unreadable code", "{status}");
            assert_eq!(detail, None, "{status}");
        }
    }

    /// The notice decision is the server's, not ours (§8): `not_now` — tab_engine's
    /// reading of `409` — is the only status that is not a failure.
    #[test]
    fn only_a_not_now_refusal_is_dim() {
        let not_now = tab_engine::PostError {
            status: Some(409),
            not_now: true,
            message: "no run to steer".into(),
        };
        assert_eq!(notice_tone(&not_now), NoticeTone::Dim);

        for status in [Some(422), Some(503), Some(400), Some(500), None] {
            let error = tab_engine::PostError {
                status,
                not_now: false,
                message: "…".into(),
            };
            assert_eq!(
                notice_tone(&error),
                NoticeTone::Error,
                "{status:?} is a failure"
            );
        }
    }
}
