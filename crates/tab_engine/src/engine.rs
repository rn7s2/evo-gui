//! The engine thread: one tab's whole life, off the UI thread.
//!
//! It boots the server (§3), runs §9.1's assembly, then turns the server's HTTP
//! and SSE into [`Update`]s until the UI asks it to [`Command::Shutdown`] — the
//! ladder, then [`Update::Exited`]. Nothing here blocks the UI: every
//! [`Command`] is a channel send, and every [`Update`] arrives on a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use serde_json::Value;

use swarm_client::{
    BootCancel, Client, EventStream, Server, ShutdownOutcome, SseParser, StreamConfig, StreamMsg,
    StreamTarget,
};

use crate::types::{Agent, Command, PostError, ReqId, StreamStatus, TabSpec, Update};

/// The `:custom` journal key the cache totals persist under
/// (`340-cache-stats.lisp`), the same one `session::cache` reads.
const CACHE_STATS_KEY: &str = "cache-stats";
/// `GET /journal?limit=N` limits to try, in order, growing only while the entry
/// is missing (§7.3, matching `session::cache::cache_seed_limits`).
const CACHE_LIMITS: [u32; 3] = [20, 100, 400];
/// How long the lane todo seed waits in silence before it calls the replay done.
const SEED_SILENCE: Duration = Duration::from_millis(400);
/// A cap on the lane todo seed, so a huge retained log cannot stall a switch.
const SEED_CAP: usize = 4000;
/// How long after a lane reports `starting` the engine asks `GET /lanes` again.
///
/// The swarm publishes `starting` when it *launches* a lane and then, when that
/// lane is initialized, calls `sync-lane-state` with `:announce nil`
/// (`swarm/lanes.lisp`) — deliberately, "so the machine-readable stream tells a
/// lane's work cycle rather than an idle event for a lane never given work". So a
/// lane that comes up idle without ever having worked is a transition no event
/// carries, and a client that only folds events shows it `starting` (◌) forever.
/// A moment after the launch announcement is the read that says otherwise; the
/// delay also coalesces the per-lane announcements of one boot into one read.
const LANES_DEBOUNCE: Duration = Duration::from_millis(500);
/// How many such deferred reads a lane that is *still* starting re-arms.
///
/// A lane's boot is seconds, so the first look can still catch it coming up. Each
/// look re-arms the next while some lane is starting, and gives up after this many
/// (about ten seconds) rather than asking a swarm that never finishes a boot
/// forever — anything that happens to that lane after that (it works, it goes
/// down) is an event again.
const LANES_FOLLOWUPS: u32 = 20;

/// One tab's engine. [`TabEngine::start`] hands back a handle to command it and
/// the channel its updates arrive on.
pub struct TabEngine;

impl TabEngine {
    /// Start a tab's engine thread. Returns the handle that commands it and the
    /// receiver its [`Update`]s arrive on. The thread runs until
    /// [`Command::Shutdown`], the server dying, or the handle being dropped.
    pub fn start(spec: TabSpec) -> (EngineHandle, Receiver<Update>) {
        let (updates, updates_rx) = async_channel::unbounded();
        let (mailbox, inbox) = async_channel::unbounded();
        let folder = spec.folder.clone();
        let engine_mailbox = mailbox.clone();
        let thread = thread::Builder::new()
            .name(format!("tab-engine {}", folder.display()))
            .spawn(move || run(spec, engine_mailbox, inbox, updates))
            .ok();
        let handle = EngineHandle { mailbox, thread, folder, shutdown_requested: false };
        (handle, updates_rx)
    }
}

/// The UI's side of a tab: send commands, read updates, and stop the server.
///
/// Dropping the handle is enough to stop the tab: the drop runs the shutdown
/// ladder (through the engine thread, which it joins). Dropping every tab's
/// handle at once runs their ladders in parallel — each engine thread is already
/// working while the joins wait.
pub struct EngineHandle {
    mailbox: Sender<Inbound>,
    thread: Option<JoinHandle<()>>,
    folder: PathBuf,
    shutdown_requested: bool,
}

impl EngineHandle {
    /// The folder the tab's swarm runs in.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Send a command. `false` means the engine is already gone.
    pub fn send(&self, command: Command) -> bool {
        self.mailbox.send_blocking(Inbound::Command(command)).is_ok()
    }

    /// `POST /prompt`.
    pub fn prompt(&self, req_id: ReqId, text: impl Into<String>) -> bool {
        self.send(Command::Prompt { req_id, text: text.into() })
    }

    /// `POST /interrupt`.
    pub fn interrupt(&self, req_id: ReqId) -> bool {
        self.send(Command::Interrupt { req_id })
    }

    /// `POST /steer`. Tests and diagnostics only — a UI turn goes through
    /// [`EngineHandle::prompt`] (§14.7), which the server queues itself.
    pub fn steer(&self, req_id: ReqId, text: impl Into<String>) -> bool {
        self.send(Command::Steer { req_id, text: text.into() })
    }

    /// Refetch a view (rows + state).
    pub fn refetch(&self, agent: Agent) -> bool {
        self.send(Command::Refetch(agent))
    }

    /// Watch one lane's events, or none.
    pub fn watch_lane(&self, lane: Option<u32>) -> bool {
        self.send(Command::WatchLane(lane))
    }

    /// Ask the tab to stop (§3's ladder) and return at once; the waiting happens
    /// when the handle is dropped. Idempotent.
    pub fn shutdown(&mut self) -> bool {
        self.shutdown_requested = true;
        self.send(Command::Shutdown)
    }

    /// Whether the engine thread is still running.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().map(|thread| !thread.is_finished()).unwrap_or(false)
    }

    /// Stop the tab and wait for the engine thread to finish — for a caller that
    /// wants to see [`Update::Exited`] before it returns.
    pub fn join(mut self) {
        if !self.shutdown_requested {
            let _ = self.mailbox.send_blocking(Inbound::Command(Command::Shutdown));
        }
        self.shutdown_requested = true;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Give up waiting: the engine keeps stopping on its own thread, and this
    /// handle stops caring. Only [`shutdown_all`] uses it, when the deadline has
    /// passed and the caller has to return anyway.
    pub fn detach(mut self) {
        self.shutdown_requested = true;
        self.thread = None;
    }
}

/// What [`shutdown_all`] managed, by folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Tabs whose engine thread finished within the deadline.
    pub exited: Vec<PathBuf>,
    /// Tabs still running when the deadline passed. Their handles were detached,
    /// so they keep stopping on their own threads; nothing blocks on them.
    pub pending: Vec<PathBuf>,
}

impl ShutdownReport {
    pub fn all_exited(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Stop every tab at once (§3, §9.8) and wait up to `deadline` for all of them.
///
/// Every handle is asked to stop first, so the tabs run their ladders **in
/// parallel** — each on its own engine thread, which is also where the waiting
/// happens. Returns as soon as the last tab is gone, or at the deadline with the
/// folders that did not make it.
///
/// `std` has no timed join, so the wait is a bounded poll of the handles'
/// threads; there is no event to wait on for a thread finishing.
pub fn shutdown_all(handles: Vec<EngineHandle>, deadline: Duration) -> ShutdownReport {
    let mut remaining: Vec<EngineHandle> = handles;
    for handle in &mut remaining {
        handle.shutdown();
    }

    let end = Instant::now() + deadline;
    let mut report = ShutdownReport::default();
    loop {
        let mut still_running = Vec::new();
        for handle in remaining.drain(..) {
            if handle.is_running() {
                still_running.push(handle);
            } else {
                report.exited.push(handle.folder().to_path_buf());
                handle.join();
            }
        }
        remaining = still_running;
        if remaining.is_empty() || Instant::now() >= end {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    for handle in remaining {
        report.pending.push(handle.folder().to_path_buf());
        handle.detach();
    }
    report
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if !self.shutdown_requested {
            // Never leave a server behind: the engine never stops on its own.
            let _ = self.mailbox.send_blocking(Inbound::Command(Command::Shutdown));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------- internals

enum Inbound {
    /// The boot thread's one message: the server, or why there is none. Boxed,
    /// because the other variants are small and a `Server` is not.
    Booted(Box<std::result::Result<Server, swarm_client::Error>>),
    Command(Command),
    Stream { agent: Agent, msg: StreamMsg },
    /// The deferred lane-list read is due ([`Engine::arm_lanes_refetch`]). It
    /// carries no state: whether it is still wanted is the engine's to decide when
    /// it arrives, so a timer that outlived its episode is simply ignored.
    LanesDue,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Liveness {
    Alive,
    Gone,
}

/// Monotonic revisions, one counter per agent (§9.1): a UI request stamped with
/// the revision it began at can drop its own stale answer.
#[derive(Default)]
struct Revisions {
    coordinator: u64,
    lanes: HashMap<u32, u64>,
}

impl Revisions {
    fn next(&mut self, agent: Agent) -> u64 {
        match agent {
            Agent::Coordinator => {
                self.coordinator += 1;
                self.coordinator
            }
            Agent::Lane(n) => {
                let counter = self.lanes.entry(n).or_insert(0);
                *counter += 1;
                *counter
            }
        }
    }

    /// How many views of this agent have been read: zero means none ever was, so a
    /// fetch that failed (which spends no revision — see [`Engine::resync`]) has not
    /// been made good yet.
    fn get(&self, agent: Agent) -> u64 {
        match agent {
            Agent::Coordinator => self.coordinator,
            Agent::Lane(n) => self.lanes.get(&n).copied().unwrap_or(0),
        }
    }
}

/// A live SSE stream and the thread that tags its messages with their agent.
struct LiveStream {
    stream: EventStream,
    forwarder: Option<JoinHandle<()>>,
}

impl LiveStream {
    fn start(target: StreamTarget, agent: Agent, mailbox: Sender<Inbound>) -> LiveStream {
        let stream = EventStream::start(target, StreamConfig::default());
        let receiver = stream.receiver();
        let forwarder = thread::Builder::new()
            .name(format!("tab-engine-fwd {agent:?}"))
            .spawn(move || forward(receiver, mailbox, agent))
            .ok();
        LiveStream { stream, forwarder }
    }

    /// Stop the reader and the forwarder; both threads are joined, so nothing
    /// outlives this call.
    fn stop(&mut self) {
        self.stream.stop();
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
    }
}

impl Drop for LiveStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Tag every message from one stream with its agent and hand it to the engine.
fn forward(receiver: Receiver<StreamMsg>, mailbox: Sender<Inbound>, agent: Agent) {
    while let Ok(msg) = receiver.recv_blocking() {
        if mailbox.send_blocking(Inbound::Stream { agent, msg }).is_err() {
            break;
        }
    }
}

/// At most one coordinator stream and one lane stream, ever (§5, §D21).
struct Streams {
    mailbox: Sender<Inbound>,
    coordinator: Option<LiveStream>,
    lane: Option<(u32, LiveStream)>,
}

impl Streams {
    fn new(mailbox: Sender<Inbound>) -> Streams {
        Streams { mailbox, coordinator: None, lane: None }
    }

    fn start_coordinator(&mut self, client: &Client, since: Option<i64>) {
        self.stop_coordinator();
        let target = StreamTarget::coordinator(client, since);
        self.coordinator = Some(LiveStream::start(target, Agent::Coordinator, self.mailbox.clone()));
    }

    fn start_lane(&mut self, client: &Client, n: u32, since: Option<i64>) {
        self.stop_lane();
        let target = StreamTarget::lane(client, n, since);
        self.lane = Some((n, LiveStream::start(target, Agent::Lane(n), self.mailbox.clone())));
    }

    fn stop_coordinator(&mut self) {
        if let Some(mut stream) = self.coordinator.take() {
            stream.stop();
        }
    }

    fn stop_lane(&mut self) {
        if let Some((_, mut stream)) = self.lane.take() {
            stream.stop();
        }
    }

    fn stop_all(&mut self) {
        self.stop_lane();
        self.stop_coordinator();
    }
}

struct Engine {
    updates: Sender<Update>,
    /// The engine's own mailbox, for the one deferred read it schedules (a
    /// lane-list refetch; see [`Engine::arm_lanes_refetch`]).
    mailbox: Sender<Inbound>,
    streams: Streams,
    revisions: Revisions,
    coordinator_connected: bool,
    lane_connected: bool,
    want_shutdown: bool,
    /// What each lane's state last was, as the UI was told it (every `/lanes`
    /// snapshot and every `lane-state` event). It is what decides whether a
    /// `lane-state` names a lane that is coming up.
    lane_states: HashMap<u32, String>,
    /// When the deferred lane-list refetch is due — `None` when none is pending —
    /// and how many follow-ups the current "a lane is starting" episode has spent.
    lanes_due: Option<Instant>,
    lanes_tries: u32,
}

fn run(spec: TabSpec, mailbox: Sender<Inbound>, inbox: Receiver<Inbound>, updates: Sender<Update>) {
    let mut engine = Engine {
        updates,
        mailbox: mailbox.clone(),
        streams: Streams::new(mailbox.clone()),
        revisions: Revisions::default(),
        coordinator_connected: false,
        lane_connected: false,
        want_shutdown: false,
        lane_states: HashMap::new(),
        lanes_due: None,
        lanes_tries: 0,
    };

    engine.send(Update::Booting);

    // Boot on its own thread, so the wait for readiness can be aborted: the
    // engine's own loop stays free to hear a shutdown, and cancelling makes the
    // boot thread run the ladder and come back within about a second (§3).
    let cancel = BootCancel::new();
    let boot_cancel = cancel.clone();
    let config = spec.server_config();
    let boot_mailbox = mailbox.clone();
    let boot = thread::Builder::new()
        .name(format!("tab-engine boot {}", spec.folder.display()))
        .spawn(move || {
            let result = Server::start_cancellable(&config, &boot_cancel);
            let _ = boot_mailbox.send_blocking(Inbound::Booted(Box::new(result)));
        })
        .ok();

    engine.run_loop(&inbox, cancel, boot);
}

impl Engine {
    fn send(&self, update: Update) -> bool {
        self.updates.send_blocking(update).is_ok()
    }

    /// §9.1's assembly, right after boot: the registry once, the lane list, the
    /// coordinator's transcript and state, the cache seed (§7.3), then the live
    /// stream from the health cursor.
    fn assemble(&mut self, client: &Client, cursor: Option<i64>) {
        if let Ok(registry) = client.registry() {
            self.send(Update::Registry { raw: registry.raw });
        }
        self.fetch_lanes(client);
        // The snapshot is read while the lanes are booting (`starting`), and what
        // they come up as is a transition no event carries (see `LANES_DEBOUNCE`):
        // schedule the read that says.
        if self.lanes_coming_up() {
            self.lanes_tries = 1;
            self.arm_lanes_refetch();
        }
        self.resync(client, Agent::Coordinator, false);
        self.seed_cache(client);
        self.streams.start_coordinator(client, cursor);
    }

    /// `GET /lanes`, sent to the UI and remembered: what each lane's state was is
    /// what later tells a `lane-state` whether the lane it names is coming up.
    fn fetch_lanes(&mut self, client: &Client) {
        if let Ok(lanes) = client.lanes() {
            self.remember_lane_states(&lanes.raw);
            self.send(Update::Lanes { raw: lanes.raw });
        }
    }

    fn remember_lane_states(&mut self, raw: &Value) {
        for lane in raw["lanes"].as_array().into_iter().flatten() {
            if let (Some(n), Some(state)) = (lane["n"].as_u64(), lane["state"].as_str()) {
                self.lane_states.insert(n as u32, state.to_owned());
            }
        }
    }

    /// Whether the list the UI holds has a lane that is still coming up — the one
    /// lane state the swarm never announces on its way out of.
    fn lanes_coming_up(&self) -> bool {
        self.lane_states.values().any(|state| state == "starting")
    }

    /// A `lane-state` event's lane, remembered — and the list asked again shortly.
    ///
    /// Two reasons a lane's state moving means a read:
    ///
    /// * the header's count and the row's clocks (`swarm.busy`, `step_age`,
    ///   `reports`) are `/lanes`' alone, so a list read before the event leaves the
    ///   header counting a lane the row below it has already stopped counting;
    /// * a lane that comes up `idle` is announced by nobody at all (see
    ///   `LANES_DEBOUNCE`), so a `starting` event needs the read that says what it
    ///   became.
    fn note_lane_state(&mut self, data: &Value) {
        let (Some(n), Some(state)) = (
            data.get("lane").and_then(Value::as_u64).map(|n| n as u32),
            data.get("state").and_then(Value::as_str),
        ) else {
            return;
        };
        if state == "starting" && self.lanes_due.is_none() {
            // A lane is being launched: follow it up (see `LANES_FOLLOWUPS`) as well
            // as asking once.
            self.lanes_tries = 0;
        }
        self.arm_lanes_refetch();
        self.lane_states.insert(n, state.to_owned());
    }

    /// Ask `/lanes` again a moment from now, unless one such read is already
    /// pending — the debounce that keeps a boot's per-lane announcements, a lane's
    /// state moving, a watched lane connecting, and a `settled` from becoming a
    /// burst of reads (throttle, not polling: every read here has an event behind
    /// it, §2.5).
    fn arm_lanes_refetch(&mut self) {
        if self.lanes_due.is_some() {
            return;
        }
        self.lanes_due = Some(Instant::now() + LANES_DEBOUNCE);
        let mailbox = self.mailbox.clone();
        // One thread, one wake-up: the engine's own loop stays a blocking receive,
        // so a deferred read costs no polling (§2.5).
        let _ = thread::Builder::new().name("tab-engine-lanes".to_owned()).spawn(move || {
            thread::sleep(LANES_DEBOUNCE);
            let _ = mailbox.send_blocking(Inbound::LanesDue);
        });
    }

    /// The deferred read, now due: send the list, and — while some lane is still
    /// `starting` — arm one more look, up to [`LANES_FOLLOWUPS`] of them. Anything
    /// else (every lane up, or a lane that went down instead) ends the episode.
    fn lanes_followup(&mut self, client: &Client) {
        self.fetch_lanes(client);
        if self.lanes_coming_up() && self.lanes_tries < LANES_FOLLOWUPS {
            self.lanes_tries += 1;
            self.arm_lanes_refetch();
        } else {
            self.lanes_tries = 0;
        }
    }

    /// The main loop: commands from the UI and messages from the streams, until
    /// shutdown or the server's death.
    /// The engine's whole life: wait for the boot, then for commands and stream
    /// messages, until shutdown or the server's death. Nothing here polls — the one
    /// read this loop schedules for later (the deferred lane-list read, see
    /// [`Engine::arm_lanes_refetch`]) arrives as a message of its own.
    fn run_loop(&mut self, inbox: &Receiver<Inbound>, cancel: BootCancel, boot: Option<JoinHandle<()>>) {
        let mut boot = boot;
        let mut server: Option<Server> = None;
        let mut booting = true;

        while let Ok(inbound) = inbox.recv_blocking() {
            match inbound {
                Inbound::Booted(result) => {
                    // The boot thread sends exactly one message; join it so no
                    // thread outlives the engine.
                    if let Some(handle) = boot.take() {
                        let _ = handle.join();
                    }
                    booting = false;
                    match *result {
                        Ok(started) => {
                            self.send(Update::Ready {
                                health: started.health().clone(),
                                pid: started.pid(),
                                port: started.port(),
                            });
                            let client = started.client().clone();
                            self.assemble(&client, started.health().cursor);
                            server = Some(started);
                        }
                        Err(error) => {
                            // A boot we aborted is not a failure to show: the
                            // caller asked for the tab to stop, so it stops.
                            return match error.cancelled() {
                                Some(outcome) => self.exit(outcome),
                                None => {
                                    let message = error.to_string();
                                    // A boot that never got as far as a running
                                    // server has no log worth reading — a binary
                                    // that is not there, a folder that cannot be
                                    // written. The reason itself is then what the
                                    // failure screen has to show (§9.7): a screen
                                    // with an empty log box tells the user nothing.
                                    let log_tail = match error.log_tail() {
                                        Some(tail) if !tail.trim().is_empty() => tail.to_owned(),
                                        _ => message.clone(),
                                    };
                                    self.send(Update::BootFailed { message, log_tail });
                                }
                            };
                        }
                    }
                }
                Inbound::Command(Command::Shutdown) => {
                    self.want_shutdown = true;
                    if booting {
                        // Abort the wait for readiness; the boot thread runs the
                        // ladder and reports how the process went.
                        cancel.cancel();
                        continue;
                    }
                    let Some(server) = server.as_mut() else { continue };
                    // Stop the streams first, so nothing more arrives while the
                    // ladder runs.
                    self.streams.stop_all();
                    let outcome = self.ladder(server);
                    return self.exit(outcome);
                }
                Inbound::Command(command) => {
                    let Some(server) = server.as_mut() else { continue };
                    let client = server.client().clone();
                    self.command(&client, command);
                }
                Inbound::LanesDue => {
                    // A deferred lane-list read came due. `due` says whether it is
                    // still the one this episode wants; it is cleared either way.
                    let due = self.lanes_due.take();
                    let Some(server) = server.as_ref() else { continue };
                    if due.is_some_and(|at| Instant::now() >= at) {
                        let client = server.client().clone();
                        self.lanes_followup(&client);
                    }
                }
                Inbound::Stream { agent, msg } => {
                    let Some(server) = server.as_mut() else { continue };
                    let client = server.client().clone();
                    if self.stream_message(server, &client, agent, msg) == Liveness::Gone {
                        self.want_shutdown = true;
                        self.streams.stop_all();
                        self.send(Update::ServerGone);
                        return self.exit(ShutdownOutcome::AlreadyGone);
                    }
                }
            }
        }

        // The mailbox closed: the handle went away without a shutdown command.
        if booting {
            cancel.cancel();
        }
        if let Some(server) = server.as_mut() {
            let outcome = self.ladder(server);
            self.exit(outcome);
        } else if let Some(handle) = boot.take() {
            // Wait for the aborted boot to finish and stop its process.
            let _ = handle.join();
        }
        self.exit(ShutdownOutcome::AlreadyGone);
    }

    /// Stop every stream and tell the UI the tab is over.
    fn exit(&mut self, outcome: ShutdownOutcome) {
        self.streams.stop_all();
        self.send(Update::Exited { outcome });
    }

    /// §3's ladder.
    fn ladder(&mut self, server: &mut Server) -> ShutdownOutcome {
        match server.shutdown() {
            Ok(report) => report.outcome,
            Err(_) => ShutdownOutcome::AlreadyGone,
        }
    }

    fn command(&mut self, client: &Client, command: Command) {
        match command {
            Command::Prompt { req_id, text } => {
                let result = client.prompt(&text).map_err(PostError::from);
                self.post_result(req_id, result);
            }
            Command::Steer { req_id, text } => {
                let result = client.steer(&text).map_err(PostError::from);
                self.post_result(req_id, result);
            }
            Command::Interrupt { req_id } => {
                let result = client.interrupt().map_err(PostError::from);
                self.post_result(req_id, result);
            }
            Command::Refetch(agent) => self.resync(client, agent, false),
            Command::WatchLane(lane) => self.watch_lane(client, lane),
            Command::Shutdown => {}
        }
    }

    fn post_result(&self, req_id: ReqId, result: Result<swarm_client::Envelope, PostError>) {
        self.send(Update::PostResult { req_id, result });
    }

    /// Refetch an agent's view and emit it with a fresh revision. The coordinator
    /// gets its transcript and state; a lane gets its transcript. `lanes` also
    /// refetches the lane list (a restart can move it).
    ///
    /// A revision is spent only by a view that actually came back. A fetch that
    /// failed — a row clicked before the lane behind it was up, a connection that
    /// dropped in between — must not consume a number, or the *next* read would
    /// arrive as revision 2 and every reader would have to wonder what revision 1
    /// said; [`Revisions::get`] would also stop saying whether a lane's rows were
    /// ever read, which is what tells the first connect to read them.
    fn resync(&mut self, client: &Client, agent: Agent, lanes: bool) {
        match agent {
            Agent::Coordinator => {
                let transcript = client.transcript(None).ok();
                let state = client.state().ok();
                if transcript.is_none() && state.is_none() {
                    return;
                }
                let revision = self.revisions.next(agent);
                if let Some(transcript) = transcript {
                    self.send(Update::Transcript { agent, revision, raw: transcript.raw });
                }
                if let Some(state) = state {
                    self.send(Update::State { revision, raw: state.raw });
                }
                if lanes {
                    self.fetch_lanes(client);
                }
            }
            Agent::Lane(n) => {
                let Ok(transcript) = client.lane_transcript(n, None) else { return };
                let revision = self.revisions.next(agent);
                self.send(Update::Transcript { agent, revision, raw: transcript.raw });
                if lanes {
                    // A lane's run ending (`settled` on the lane's own stream, which
                    // is the one the tab watches) moves what `/lanes` reports about
                    // it: whether it is busy, and for how long it has been.
                    self.fetch_lanes(client);
                }
            }
        }
    }

    /// §7.3's cache seed: `/journal?limit=20`, then 100, then 400 — growing only
    /// while the newest `cache-stats` entry is missing.
    fn seed_cache(&mut self, client: &Client) {
        for limit in CACHE_LIMITS {
            match client.journal(Some(limit)) {
                Ok(journal) => {
                    if let Some(entry) = journal.newest_custom(CACHE_STATS_KEY) {
                        self.send(Update::CacheSeed { entry: Some(entry.clone()) });
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        self.send(Update::CacheSeed { entry: None });
    }

    /// Watch one lane, or none: the previous lane stream is closed first, so at
    /// most one is ever open.
    fn watch_lane(&mut self, client: &Client, lane: Option<u32>) {
        self.lane_connected = false;
        self.streams.stop_lane();
        let Some(n) = lane else { return };
        // Rows first: the lane's transcript is authoritative.
        self.resync(client, Agent::Lane(n), false);
        // Then the todo seed, whose returned cursor the live stream resumes from.
        let since = self.lane_todo_seed(client, n);
        self.streams.start_lane(client, n, since);
    }

    /// One bounded replay of a lane's retained events (`?since=0`) to seed the
    /// TODO panel, returning the cursor the live stream should resume from.
    ///
    /// Only the newest `todo-changed` is forwarded. The replay is not forwarded
    /// as rows: the lane's serve keeps up to 20 000 events, so `?since=0` would
    /// re-send the lane's whole session, duplicating every row the transcript has
    /// already given. The replay is bounded by a short silence — the log replays
    /// as a burst, so a pause means the live edge — and the live stream then
    /// resumes at the last id seen, so no event between the two is lost.
    fn lane_todo_seed(&mut self, client: &Client, n: u32) -> Option<i64> {
        let path = format!("/lanes/{n}/events?since=0");
        let mut connection = client.http().open_sse(&path, None).ok()?;
        if connection
            .socket()
            .set_read_timeout(Some(SEED_SILENCE))
            .is_err()
        {
            return None;
        }
        let mut parser = SseParser::new();
        let mut last_id = None;
        let mut todo: Option<(Option<i64>, Value)> = None;
        let mut seen = 0usize;
        while let Ok(Some(line)) = connection.read_line() {
            let Some(event) = parser.feed(&line) else { continue };
            if let Some(id) = event.id {
                last_id = Some(id);
            }
            seen += 1;
            if event.kind.as_deref() == Some("todo-changed") {
                let data = serde_json::from_str(&event.data).unwrap_or(Value::Null);
                todo = Some((event.id, data));
            }
            if seen >= SEED_CAP {
                break;
            }
        }
        drop(connection);
        if let Some((id, data)) = todo {
            self.send(Update::Event {
                agent: Agent::Lane(n),
                id,
                kind: "todo-changed".to_owned(),
                data,
            });
        }
        last_id
    }

    /// Fold one stream message for one agent, resyncing where §9.1 says to.
    fn stream_message(
        &mut self,
        server: &mut Server,
        client: &Client,
        agent: Agent,
        msg: StreamMsg,
    ) -> Liveness {
        match msg {
            StreamMsg::Connected { .. } => {
                self.send(Update::Stream { agent, status: StreamStatus::Connected });
                let again = match agent {
                    Agent::Coordinator => std::mem::replace(&mut self.coordinator_connected, true),
                    Agent::Lane(_) => std::mem::replace(&mut self.lane_connected, true),
                };
                if again {
                    // A reconnect resumed from a cursor: make the view match, and
                    // for the coordinator the lane list too.
                    self.resync(client, agent, agent == Agent::Coordinator);
                } else if agent != Agent::Coordinator {
                    // The lane that is being watched answered for the first time: it
                    // is up at last, and the lane list was read while it was still
                    // coming up. Its rows are read here too if showing it never got
                    // them — a row clicked while the lane was down asks the swarm for
                    // a transcript with nothing behind it — because the stream
                    // connecting is the first moment the lane can answer. The list is
                    // asked again either way (debounced, so a boot's own
                    // announcements coalesce with this).
                    if self.revisions.get(agent) == 0 {
                        self.resync(client, agent, false);
                    }
                    self.arm_lanes_refetch();
                }
                Liveness::Alive
            }
            StreamMsg::Disconnected { retry_in, .. } => {
                self.send(Update::Stream {
                    agent,
                    status: StreamStatus::Reconnecting { retry_in },
                });
                // The coordinator's stream drops when the session behind it goes
                // away. If the whole process is gone, so is the tab's server.
                if agent == Agent::Coordinator && !self.want_shutdown && !server.is_running() {
                    return Liveness::Gone;
                }
                Liveness::Alive
            }
            StreamMsg::Reset { .. } => {
                // The server restarted; its ids begin again at 1 (§5).
                self.resync(client, agent, agent == Agent::Coordinator);
                Liveness::Alive
            }
            StreamMsg::Event { id, kind, data } => {
                // A lane-state event is the lane list's news: remember it, and if
                // it says a lane is starting, ask `/lanes` again shortly (see
                // `LANES_DEBOUNCE` — a lane's coming up idle is never announced).
                if agent == Agent::Coordinator && kind == "lane-state" {
                    self.note_lane_state(&data);
                }
                // `settled` is a run finishing — the coordinator's or the watched
                // lane's — and a natural moment to re-read the lane list: `/lanes`
                // carries what the stream does not (the task and report counts, the
                // step clock, whether a lane is up at all), and it is what the left
                // column's header counts and its rows' clocks come from.
                let resync = match kind.as_str() {
                    "hello" | "settled" => Some(true),
                    "gap" | "session-switched" => Some(false),
                    _ => None,
                };
                self.send(Update::Event { agent, id, kind, data });
                if let Some(lanes) = resync {
                    self.resync(client, agent, lanes);
                }
                Liveness::Alive
            }
            StreamMsg::Ended => Liveness::Alive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An engine with nothing running behind it: enough to ask what a `lane-state`
    /// event decides, which is all these tests are about. The deferred read the
    /// decisions schedule finds a dropped receiver and gives up quietly.
    fn dormant() -> Engine {
        let (updates, _) = async_channel::unbounded();
        let (mailbox, _inbox) = async_channel::unbounded();
        let mut engine = Engine {
            updates,
            mailbox: mailbox.clone(),
            streams: Streams::new(mailbox),
            revisions: Revisions::default(),
            coordinator_connected: false,
            lane_connected: false,
            want_shutdown: false,
            lane_states: HashMap::new(),
            lanes_due: None,
            lanes_tries: 0,
        };
        engine.lane_states.insert(1, "idle".to_owned());
        engine.lane_states.insert(2, "idle".to_owned());
        engine
    }

    /// The row's glyph follows the event, but the header above it and the row's
    /// clocks are `/lanes`' — so a lane whose state moves is a moment to read the
    /// list, not only a lane that is still `starting`.
    #[test]
    fn a_lane_state_that_moves_asks_for_the_list() {
        let mut engine = dormant();
        engine.note_lane_state(&json!({ "lane": 1, "state": "working" }));
        assert!(engine.lanes_due.is_some(), "a lane going working asks for the list");
        assert_eq!(engine.lane_states.get(&1).map(String::as_str), Some("working"));

        let mut engine = dormant();
        engine.note_lane_state(&json!({ "lane": 2, "state": "idle" }));
        assert!(engine.lanes_due.is_some(), "so does one going idle");
    }

    /// A launch announcement is what the follow-up episode exists for: the swarm
    /// never announces the state a lane comes up *as* (`sync-lane-state :announce
    /// nil`), so the read has to be repeated while a lane is still `starting`.
    #[test]
    fn a_launch_announcement_starts_the_follow_ups() {
        let mut engine = dormant();
        engine.lanes_tries = 7;
        engine.note_lane_state(&json!({ "lane": 2, "state": "starting" }));
        assert_eq!(engine.lanes_tries, 0, "a new launch is a new episode");
        assert!(engine.lanes_due.is_some());
        assert_eq!(engine.lane_states.get(&2).map(String::as_str), Some("starting"));

        // …and the follow-ups stop once no lane is starting.
        let mut engine = dormant();
        engine.note_lane_state(&json!({ "lane": 2, "state": "starting" }));
        assert!(engine.lanes_coming_up());
        engine.note_lane_state(&json!({ "lane": 2, "state": "idle" }));
        assert!(!engine.lanes_coming_up(), "the episode is over when the lane is up");
    }

    /// One read per debounce window: a boot's per-lane announcements — or a lane
    /// settling and the coordinator's run ending at the same moment — must not
    /// become a burst of reads.
    #[test]
    fn one_read_per_debounce_window() {
        let mut engine = dormant();
        engine.note_lane_state(&json!({ "lane": 1, "state": "starting" }));
        let due = engine.lanes_due.expect("a read is pending");
        engine.note_lane_state(&json!({ "lane": 2, "state": "starting" }));
        engine.arm_lanes_refetch();
        assert_eq!(engine.lanes_due, Some(due), "the pending read is the one that happens");
    }
}
