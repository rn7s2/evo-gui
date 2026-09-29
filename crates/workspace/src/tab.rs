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
use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Subscription, Task,
    Window,
};

use async_channel::Receiver;
use composer::{Composer, ComposerEvent};
use session::{AgentKey, Changes, LaunchPlan, RowChanges, TabModel};
use tab_engine::{Agent, EngineHandle, ReqId, Update};
use transcript::TranscriptView;

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
    _subscriptions: Vec<Subscription>,
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
            _subscriptions: vec![history_subscription, composer_subscription],
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
                self.state = TabState::Booting { folder };
                self.attach(started, window, cx);
            }
            Err(error) => {
                // Nothing is running: show why, with what little there is to show.
                self.last_launch = Some(launch);
                self.state = TabState::Failed {
                    folder,
                    log_tail: format!("could not prepare this tab: {error}"),
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
        self.transcripts
            .insert(AgentKey::Coordinator, cx.new(TranscriptView::new));
        self.live = Some(Live {
            engine: started.engine,
            model: TabModel::new(),
            _pump: pump,
            tab_dir: started.tab_dir,
            next_req: 0,
            in_flight: None,
            state_revision: 0,
            recorded: false,
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
            Update::Ready { .. } => {
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    self.state = TabState::Running { folder };
                    cx.notify();
                }
            }
            Update::BootFailed { log_tail, .. } => {
                if let Some(folder) = self.folder().map(Path::to_path_buf) {
                    self.state = TabState::Failed { folder, log_tail };
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
                    self.record_recent(PathBuf::from(session), cx);
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
        cx.notify();
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
        let started = self.model()?.coordinator_step_started()?.started_at_millis?;
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
        self.transcripts
            .entry(agent)
            .or_insert_with(|| cx.new(TranscriptView::new));
        self.push(changes, cx);
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
            self.composer
                .update(cx, |composer, cx| composer.request_finished(false, window, cx));
        }
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
            self.composer
                .update(cx, |composer, cx| composer.request_finished(result.is_ok(), window, cx));
        }
        if let Err(error) = result {
            // `409 Not now` is not a mistake: it is the server saying it cannot
            // take this right now, so it reads as a notice and not an error.
            let notice = if error.not_now {
                Notification::info(error.message)
            } else {
                Notification::error(error.message)
            };
            window.push_notification(notice, cx);
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
        let (bridge, _worker) = crate::bridge::Bridge::spawn(
            crate::bridge::Revision::new(0),
            move |updates| {
                let _ = updates.send(swarm_client::log_tail(&log, 40));
            },
        );
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
    fn record_recent(&mut self, session: PathBuf, cx: &mut Context<Self>) {
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
            .or_else(|| live.model.lanes().swarm.as_ref().map(|swarm| swarm.workers as u32))
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
            self.state = TabState::Failed { folder, log_tail };
            cx.notify();
        }
    }
}

impl Live {
    /// Fold one engine update into the model, answering the [`Changes`] the UI
    /// has to apply — or `None` when the update was not the model's.
    fn absorb(&mut self, update: Update) -> Option<Changes> {
        let changes = match update {
            Update::Transcript { agent, revision, raw } => {
                self.model.on_transcript(agent_key(agent), revision, &raw)
            }
            Update::Lanes { raw } => self.model.on_lanes(&raw),
            Update::Event { agent, id, kind, data } => {
                let id = id.unwrap_or_default().max(0) as u64;
                // The event is stamped with when the UI saw it, which is what the
                // step clock counts from (§7.3).
                self.model
                    .on_event_at(agent_key(agent), id, &kind, &data, now_millis())
            }
            Update::Stream { agent, status } => {
                self.model.on_stream(agent_key(agent), stream_status(status))
            }
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
