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
        let handle = EngineHandle {
            mailbox,
            thread,
            folder,
            shutdown_requested: false,
        };
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
        self.mailbox
            .send_blocking(Inbound::Command(command))
            .is_ok()
    }

    /// `POST /prompt`.
    pub fn prompt(&self, req_id: ReqId, text: impl Into<String>) -> bool {
        self.send(Command::Prompt {
            req_id,
            text: text.into(),
        })
    }

    /// `POST /interrupt`.
    pub fn interrupt(&self, req_id: ReqId) -> bool {
        self.send(Command::Interrupt { req_id })
    }

    /// `POST /steer`. Tests and diagnostics only — a UI turn goes through
    /// [`EngineHandle::prompt`] (§14.7), which the server queues itself.
    pub fn steer(&self, req_id: ReqId, text: impl Into<String>) -> bool {
        self.send(Command::Steer {
            req_id,
            text: text.into(),
        })
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
        self.thread
            .as_ref()
            .map(|thread| !thread.is_finished())
            .unwrap_or(false)
    }

    /// Stop the tab and wait for the engine thread to finish — for a caller that
    /// wants to see [`Update::Exited`] before it returns.
    pub fn join(mut self) {
        if !self.shutdown_requested {
            let _ = self
                .mailbox
                .send_blocking(Inbound::Command(Command::Shutdown));
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
            let _ = self
                .mailbox
                .send_blocking(Inbound::Command(Command::Shutdown));
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
    Stream {
        agent: Agent,
        msg: StreamMsg,
    },
    /// A view read came back — or gave up — on the thread that asked for it
    /// ([`Engine::resync`]). Off the loop for the same reason a `POST` is: a server
    /// that is there but silent answers nothing for a whole request timeout, and the
    /// stream's own news (a dropped connection, a lane that moved) must reach the UI
    /// while that is still going on.
    Resynced {
        agent: Agent,
        /// The number of the read this answers ([`Reads`]).
        ticket: u64,
        /// Whether this read also wants the lane list asked again once it has
        /// answered (see [`Engine::resync`]).
        lanes: bool,
        view: View,
    },
    /// §9.1's assembly read off the loop ([`Engine::assemble`]): the coordinator's
    /// view and the cache seed, in the order the first frame wants them. The stream
    /// opens when this lands, so the frame is still assembled whole — rows, state,
    /// the cache figure, then the stream.
    Assembled {
        /// The cursor `/health` named, which the stream resumes from.
        cursor: Option<i64>,
        ticket: u64,
        view: View,
        /// The newest `cache-stats` journal entry, or `None` when the walk of
        /// `/journal?limit=20,100,400` found none (§7.3).
        seed: Option<Value>,
    },
    /// A lane's rows came back ([`Engine::watch_lane`]): what its column shows, and
    /// what says the lane's stream may open.
    LaneSeeded {
        n: u32,
        /// Which `WatchLane` asked for this: a newer one supersedes it.
        episode: u64,
        ticket: u64,
        /// `/lanes/N/transcript`, or `None` for a lane whose server did not answer.
        rows: Option<Value>,
    },
    /// The bounded replay that seeds a shown lane's TODO panel came back
    /// ([`Engine::watch_lane`]). Its own message, and *after* the stream opened: the
    /// replay reads the lane's log while the lane may already be working, and nothing
    /// it reads is forwarded as rows (see [`lane_todo`]).
    LaneTodo {
        n: u32,
        episode: u64,
        /// The newest `todo-changed` in the retained log, with the id it carried.
        todo: Option<(Option<i64>, Value)>,
    },
    /// The deferred lane-list read is due ([`Engine::arm_lanes_refetch`]). It
    /// carries no state: whether it is still wanted is the engine's to decide when
    /// it arrives, so a timer that outlived its episode is simply ignored.
    LanesDue,
    /// A `POST` came back — or gave up — on the thread it was issued from
    /// ([`Engine::post_off_loop`]). The engine's loop never waits for one: a server
    /// that is there but silent answers nothing for a whole request timeout, and the
    /// stream's own news (a dropped connection, a lane that moved) must reach the UI
    /// while that is still going on.
    Posted {
        req_id: ReqId,
        result: Result<swarm_client::Envelope, PostError>,
    },
    /// A lane list came back (or did not) from the thread that asked for it
    /// ([`Engine::ask_lanes`]): `None` is a read that failed, which says nothing
    /// about the lanes and must not be mistaken for one that did.
    LanesFetched {
        raw: Option<Value>,
    },
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
    /// fetch that failed (which spends no revision — see [`Engine::apply_view`]) has
    /// not been made good yet.
    fn get(&self, agent: Agent) -> u64 {
        match agent {
            Agent::Coordinator => self.coordinator,
            Agent::Lane(n) => self.lanes.get(&n).copied().unwrap_or(0),
        }
    }
}

/// A whole view of one agent, as an off-loop read found it: the rows a transcript
/// read returned, and `/state`. `None` is a read that failed — which says nothing
/// about the view and must not be mistaken for one that did.
#[derive(Debug, Default)]
struct View {
    transcript: Option<Value>,
    /// `/state` is the coordinator's alone: a lane's state comes from its events
    /// (§9.1).
    state: Option<Value>,
}

impl View {
    /// Read AGENT's view from CLIENT, the read §9.1's resync is made of.
    fn read(client: &Client, agent: Agent) -> View {
        match agent {
            Agent::Coordinator => View {
                transcript: client.transcript(None).ok().map(|reply| reply.raw),
                state: client.state().ok().map(|reply| reply.raw),
            },
            Agent::Lane(n) => View {
                transcript: client.lane_transcript(n, None).ok().map(|reply| reply.raw),
                state: None,
            },
        }
    }

    /// Whether anything came back: a read that answered nothing spends no revision.
    fn came_back(&self) -> bool {
        self.transcript.is_some() || self.state.is_some()
    }
}

/// Which view reads are out, and which have landed: the off-loop resync's own
/// bookkeeping, for §9.1's "late results from an old revision are dropped".
///
/// A read takes a ticket when it is issued and carries it back. A result older than
/// the newest one already applied is an answer that overtook a newer one, and the
/// view on screen is the newer one's, so it is dropped. The comparison is against
/// what *landed*, not what was issued, because a read that failed answers nothing:
/// dropping on issue order would throw the only view there is away when the newer
/// read is the one that failed.
#[derive(Default)]
struct Reads {
    issued: HashMap<Agent, u64>,
    landed: HashMap<Agent, u64>,
}

impl Reads {
    /// The number of the next view read of AGENT.
    fn issue(&mut self, agent: Agent) -> u64 {
        let next = self.issued.entry(agent).or_insert(0);
        *next += 1;
        *next
    }

    /// Note that TICKET's view is about to be applied. `false` means a newer view is
    /// already on screen, so this one is stale.
    fn land(&mut self, agent: Agent, ticket: u64) -> bool {
        if ticket <= self.landed.get(&agent).copied().unwrap_or(0) {
            return false;
        }
        self.landed.insert(agent, ticket);
        true
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
        if mailbox
            .send_blocking(Inbound::Stream { agent, msg })
            .is_err()
        {
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
        Streams {
            mailbox,
            coordinator: None,
            lane: None,
        }
    }

    fn start_coordinator(&mut self, client: &Client, since: Option<i64>) {
        self.stop_coordinator();
        let target = StreamTarget::coordinator(client, since);
        self.coordinator = Some(LiveStream::start(
            target,
            Agent::Coordinator,
            self.mailbox.clone(),
        ));
    }

    fn start_lane(&mut self, client: &Client, n: u32, since: Option<i64>) {
        self.stop_lane();
        let target = StreamTarget::lane(client, n, since);
        self.lane = Some((
            n,
            LiveStream::start(target, Agent::Lane(n), self.mailbox.clone()),
        ));
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
    /// Which view reads are in flight and which have landed ([`Reads`]).
    reads: Reads,
    coordinator_connected: bool,
    lane_connected: bool,
    want_shutdown: bool,
    /// The lane the tab is showing, with the number of the `WatchLane` that asked
    /// for it. The seed reads that open its stream carry that number back, and one
    /// that comes back after a newer request is dropped: its stream would belong to
    /// a lane the tab is no longer showing (§9.3).
    watch: Option<(u64, u32)>,
    /// How many `WatchLane`s this tab has taken — nothing but the next one's number.
    watch_requests: u64,
    /// What each lane's state last was, as the UI was told it (every `/lanes`
    /// snapshot and every `lane-state` event). It is what decides whether a
    /// `lane-state` names a lane that is coming up.
    lane_states: HashMap<u32, String>,
    /// The process each lane's events come from, as `/lanes` and `lane-state` named it.
    /// A *different* pid is a new process, with a new event log (ids begin again at 1),
    /// so the watched lane's stream has to be opened again ([`Engine::reopen_lane`]).
    lane_pids: HashMap<u32, u64>,
    /// When the deferred lane-list refetch is due — `None` when none is pending —
    /// and how many follow-ups the current "a lane is starting" episode has spent.
    lanes_due: Option<Instant>,
    lanes_tries: u32,
    /// Whether a lane-list read is out with the server already: one at a time, so a
    /// server that is slow to answer is not asked again on top of itself.
    lanes_fetching: bool,
}

fn run(spec: TabSpec, mailbox: Sender<Inbound>, inbox: Receiver<Inbound>, updates: Sender<Update>) {
    let mut engine = Engine {
        updates,
        mailbox: mailbox.clone(),
        streams: Streams::new(mailbox.clone()),
        revisions: Revisions::default(),
        reads: Reads::default(),
        coordinator_connected: false,
        lane_connected: false,
        want_shutdown: false,
        watch: None,
        watch_requests: 0,
        lane_states: HashMap::new(),
        lane_pids: HashMap::new(),
        lanes_due: None,
        lanes_tries: 0,
        lanes_fetching: false,
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
    ///
    /// The last three are read on a thread of their own, in the order they are
    /// emitted, and the stream opens when they land ([`Engine::assembled`]): the
    /// loop stays free while they go — a stop during a slow journal walk is honoured
    /// at once — and the first frame is still assembled whole, with nothing live
    /// arriving before the rows it extends.
    fn assemble(&mut self, client: &Client, cursor: Option<i64>) {
        if let Ok(registry) = client.registry() {
            self.send(Update::Registry { raw: registry.raw });
        }
        // The assembly's own read, before the transcript: the boot has just waited
        // out a server that is up, and the lane list belongs in the same first frame
        // (the tab draws it above the transcript). Everything after boot goes through
        // `ask_lanes` instead — a read the loop does not wait for.
        self.fetch_lanes_now(client);
        // The snapshot is read while the lanes are booting (`starting`), and what
        // they come up as is a transition no event carries (see `LANES_DEBOUNCE`):
        // schedule the read that says.
        if self.lanes_coming_up() {
            self.lanes_tries = 1;
            self.arm_lanes_refetch();
        }
        let ticket = self.reads.issue(Agent::Coordinator);
        let mailbox = self.mailbox.clone();
        let client = client.clone();
        let _ = thread::Builder::new()
            .name("tab-engine-assemble".to_owned())
            .spawn(move || {
                let view = View::read(&client, Agent::Coordinator);
                let seed = walk_journal(&client);
                let _ = mailbox.send_blocking(Inbound::Assembled {
                    cursor,
                    ticket,
                    view,
                    seed,
                });
            });
    }

    /// The assembly's reads landed: emit the first frame's rows, state and cache
    /// figure, and only then open the stream.
    fn assembled(
        &mut self,
        client: &Client,
        cursor: Option<i64>,
        ticket: u64,
        view: View,
        seed: Option<Value>,
    ) {
        self.apply_view(Agent::Coordinator, ticket, view);
        self.send(Update::CacheSeed { entry: seed });
        self.streams.start_coordinator(client, cursor);
    }

    /// `GET /lanes` here and now, for [`Engine::assemble`] alone: the boot's own
    /// first frame, where the lane list is drawn with the transcript it was read
    /// beside.
    fn fetch_lanes_now(&mut self, client: &Client) {
        if let Ok(lanes) = client.lanes() {
            self.remember_lane_states(client, &lanes.raw);
            self.send(Update::Lanes { raw: lanes.raw });
        }
    }

    /// Ask `GET /lanes` on a thread of its own; the answer comes back as
    /// [`Inbound::LanesFetched`] and is folded there.
    ///
    /// A read on the engine's own loop would hold everything else — the stream's
    /// `Disconnected`, a lane's `settled` — for as long as the server made it wait,
    /// and a server that has stopped answering makes it wait out the whole request
    /// timeout. That is the difference between the tab noticing within its 45 s
    /// stream patience and the tab noticing half a minute later (§9.7). One read is
    /// in flight at a time: an answer that is already on its way is as fresh as the
    /// one being asked for.
    fn ask_lanes(&mut self, client: &Client) {
        if self.lanes_fetching {
            return;
        }
        self.lanes_fetching = true;
        let mailbox = self.mailbox.clone();
        let client = client.clone();
        let _ = thread::Builder::new()
            .name("tab-engine-lanes-fetch".to_owned())
            .spawn(move || {
                let raw = client.lanes().ok().map(|lanes| lanes.raw);
                let _ = mailbox.send_blocking(Inbound::LanesFetched { raw });
            });
    }

    /// A lane list came back. What each lane's state was is what later tells a
    /// `lane-state` whether the lane it names is coming up; and while some lane is
    /// still `starting`, one more look is armed ([`LANES_FOLLOWUPS`] of them) — but
    /// only after a read that *answered*: a failed one says nothing about the lanes,
    /// so following it up would be asking again with nothing new to go on.
    fn lanes_fetched(&mut self, client: &Client, raw: Option<Value>) {
        self.lanes_fetching = false;
        let Some(raw) = raw else { return };
        self.remember_lane_states(client, &raw);
        self.send(Update::Lanes { raw });
        if self.lanes_coming_up() && self.lanes_tries < LANES_FOLLOWUPS {
            self.lanes_tries += 1;
            self.arm_lanes_refetch();
        } else {
            self.lanes_tries = 0;
        }
    }

    fn remember_lane_states(&mut self, client: &Client, raw: &Value) {
        for lane in raw["lanes"].as_array().into_iter().flatten() {
            let Some(n) = lane["n"].as_u64().map(|n| n as u32) else {
                continue;
            };
            if let Some(state) = lane["state"].as_str() {
                self.lane_states.insert(n, state.to_owned());
            }
            if self.note_lane_pid(n, lane["pid"].as_u64()) {
                self.reopen_lane(client, n);
            }
        }
    }

    /// Remember the process a lane's events come from, as `/lanes` or a `lane-state`
    /// named it. Returns whether this is a *new* process behind the lane the tab is
    /// showing, which is a stream to open again ([`Engine::reopen_lane`]).
    ///
    /// A lane's event log is its process's: a restarted lane's ids begin again at 1
    /// (`serve/events.lisp`), so a stream resuming from the dead process's cursor — or
    /// reconnecting with a `Last-Event-ID` pointing into it — would sit silent until the
    /// new log grew past that number. Nothing else says so: the lane's own serve is only
    /// reachable through the swarm's relay (§9.3 forbids reading its token or URL), so
    /// the pid the lane list reports is what tells the two processes apart. A null pid (a
    /// lane that is starting or down reports none) says nothing about the process and is
    /// ignored, so the change is still visible when the lane comes back.
    fn note_lane_pid(&mut self, n: u32, pid: Option<u64>) -> bool {
        let Some(pid) = pid else {
            return false;
        };
        let replaced = matches!(self.lane_pids.insert(n, pid), Some(previous) if previous != pid);
        replaced && self.watch.is_some_and(|(_, lane)| lane == n)
    }

    /// Open the watched lane's stream again — from no cursor at all — and read its rows
    /// again: the process behind it is a new one (§9.1's refetch after a restart, seen
    /// through the lane list rather than through a reset).
    fn reopen_lane(&mut self, client: &Client, n: u32) {
        self.lane_connected = false;
        self.resync(client, Agent::Lane(n), false);
        self.streams.start_lane(client, n, None);
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
    fn note_lane_state(&mut self, client: &Client, data: &Value) {
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
        // A lane that comes back is a new process under the same number: the watched
        // lane's stream belongs to the one that is gone.
        if self.note_lane_pid(n, data.get("pid").and_then(Value::as_u64)) {
            self.reopen_lane(client, n);
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
        let _ = thread::Builder::new()
            .name("tab-engine-lanes".to_owned())
            .spawn(move || {
                thread::sleep(LANES_DEBOUNCE);
                let _ = mailbox.send_blocking(Inbound::LanesDue);
            });
    }

    /// The deferred read, now due: ask for the list. Whether another look follows is
    /// [`Engine::lanes_fetched`]'s decision, once this one has answered.
    fn lanes_followup(&mut self, client: &Client) {
        self.ask_lanes(client);
    }

    /// The main loop: commands from the UI and messages from the streams, until
    /// shutdown or the server's death.
    /// The engine's whole life: wait for the boot, then for commands and stream
    /// messages, until shutdown or the server's death. Nothing here polls — the one
    /// read this loop schedules for later (the deferred lane-list read, see
    /// [`Engine::arm_lanes_refetch`]) arrives as a message of its own.
    fn run_loop(
        &mut self,
        inbox: &Receiver<Inbound>,
        cancel: BootCancel,
        boot: Option<JoinHandle<()>>,
    ) {
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
                        Ok(mut started) => {
                            // A stop that arrived *while* this was booting, and lost
                            // the race to the boot itself: the command was taken, the
                            // boot thread had already finished and sent its server, so
                            // the flag is all that is left of it. The server is stopped
                            // again, the ladder way, instead of being assembled and left
                            // running for a caller that is gone — which is a tab whose
                            // swarm nobody will ever ask to stop (its lanes included,
                            // since they are the supervisor's to take down).
                            if self.want_shutdown {
                                let outcome = self.ladder(&mut started);
                                return self.exit(outcome);
                            }
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
                    let Some(server) = server.as_mut() else {
                        continue;
                    };
                    // Stop the streams first, so nothing more arrives while the
                    // ladder runs.
                    self.streams.stop_all();
                    let outcome = self.ladder(server);
                    return self.exit(outcome);
                }
                Inbound::Command(command) => {
                    let Some(server) = server.as_mut() else {
                        continue;
                    };
                    let client = server.client().clone();
                    self.command(&client, command);
                }
                Inbound::Posted { req_id, result } => {
                    self.post_result(req_id, result);
                }
                Inbound::LanesFetched { raw } => {
                    let Some(server) = server.as_ref() else {
                        continue;
                    };
                    let client = server.client().clone();
                    self.lanes_fetched(&client, raw);
                }
                Inbound::Resynced {
                    agent,
                    ticket,
                    lanes,
                    view,
                } => {
                    let Some(server) = server.as_ref() else {
                        continue;
                    };
                    let client = server.client().clone();
                    self.resynced(&client, agent, ticket, lanes, view);
                }
                Inbound::Assembled {
                    cursor,
                    ticket,
                    view,
                    seed,
                } => {
                    let Some(server) = server.as_ref() else {
                        continue;
                    };
                    let client = server.client().clone();
                    self.assembled(&client, cursor, ticket, view, seed);
                }
                Inbound::LaneSeeded {
                    n,
                    episode,
                    ticket,
                    rows,
                } => {
                    let Some(server) = server.as_ref() else {
                        continue;
                    };
                    let client = server.client().clone();
                    self.lane_seeded(&client, n, episode, ticket, rows);
                }
                Inbound::LaneTodo { n, episode, todo } => {
                    self.lane_todo(n, episode, todo);
                }
                Inbound::LanesDue => {
                    // A deferred lane-list read came due. `due` says whether it is
                    // still the one this episode wants; it is cleared either way.
                    let due = self.lanes_due.take();
                    let Some(server) = server.as_ref() else {
                        continue;
                    };
                    if due.is_some_and(|at| Instant::now() >= at) {
                        let client = server.client().clone();
                        self.lanes_followup(&client);
                    }
                }
                Inbound::Stream { agent, msg } => {
                    let Some(server) = server.as_mut() else {
                        continue;
                    };
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
                let client = client.clone();
                self.post_off_loop(req_id, move || client.prompt(&text));
            }
            Command::Steer { req_id, text } => {
                let client = client.clone();
                self.post_off_loop(req_id, move || client.steer(&text));
            }
            Command::Interrupt { req_id } => {
                let client = client.clone();
                self.post_off_loop(req_id, move || client.interrupt());
            }
            Command::Refetch(agent) => self.resync(client, agent, false),
            Command::WatchLane(lane) => self.watch_lane(client, lane),
            Command::Shutdown => {}
        }
    }

    /// Issue a `POST` on a thread of its own and let its answer come back as
    /// [`Inbound::Posted`].
    ///
    /// The loop must not wait for the server: while a request is outstanding — and
    /// the server may be silent for the whole request timeout — the stream could be
    /// telling the tab that the connection is gone, or that a lane moved, and none of
    /// that would reach anyone until the loop came back (§9.2, §9.7: the tab's
    /// "sending" state and its reconnect notice are the same view of a server that
    /// stopped answering).
    ///
    /// The thread is short-lived and joined by nobody: a tab closed while a request
    /// is in flight drops the mailbox, and the thread ends when the request does.
    fn post_off_loop<F>(&self, req_id: ReqId, call: F)
    where
        F: FnOnce() -> Result<swarm_client::Envelope, swarm_client::Error> + Send + 'static,
    {
        let mailbox = self.mailbox.clone();
        let _ = thread::Builder::new()
            .name("tab-engine-post".to_owned())
            .spawn(move || {
                let result = call().map_err(PostError::from);
                let _ = mailbox.send_blocking(Inbound::Posted { req_id, result });
            });
    }

    fn post_result(&self, req_id: ReqId, result: Result<swarm_client::Envelope, PostError>) {
        self.send(Update::PostResult { req_id, result });
    }

    /// Refetch an agent's view and emit it with a fresh revision — on a thread of
    /// its own, the answer coming back as [`Inbound::Resynced`].
    ///
    /// A resync on the engine's own loop would hold everything else — the stream's
    /// `Disconnected`, a lane's `settled` — for as long as the server made it wait,
    /// and a server that has stopped answering makes it wait out the whole request
    /// timeout. That is the difference between the tab noticing within its 45 s
    /// stream patience and the tab noticing half a minute later (§9.7). The
    /// coordinator's resync reads its transcript *and* its state; a lane's reads its
    /// transcript alone. `lanes` asks the lane list again once the view has answered:
    /// a run ending moves what `/lanes` reports — whether a lane is busy, and for how
    /// long it has been — and the list carries what the stream does not.
    fn resync(&mut self, client: &Client, agent: Agent, lanes: bool) {
        let ticket = self.reads.issue(agent);
        let mailbox = self.mailbox.clone();
        let client = client.clone();
        let _ = thread::Builder::new()
            .name("tab-engine-fetch".to_owned())
            .spawn(move || {
                let view = View::read(&client, agent);
                let _ = mailbox.send_blocking(Inbound::Resynced {
                    agent,
                    ticket,
                    lanes,
                    view,
                });
            });
    }

    /// A resync came back: emit the view it read, and ask the lane list if the read
    /// was one that wanted it. A read that failed says nothing about the lanes, so
    /// following it up would be reading again with nothing new to go on.
    fn resynced(&mut self, client: &Client, agent: Agent, ticket: u64, lanes: bool, view: View) {
        let answered = view.came_back();
        self.apply_view(agent, ticket, view);
        if answered && lanes {
            self.ask_lanes(client);
        }
    }

    /// Emit a view that came back, with a fresh revision (§9.1).
    ///
    /// A revision is spent only by a view that actually came back. A fetch that
    /// failed — a row clicked before the lane behind it was up, a connection that
    /// dropped in between — must not consume a number, or the *next* read would
    /// arrive as revision 2 and every reader would have to wonder what revision 1
    /// said; [`Revisions::get`] would also stop saying whether a lane's rows were
    /// ever read, which is what tells the first connect to read them. And a view
    /// older than one already applied is a read that overtook a newer one: it is
    /// dropped rather than replacing the newer view with an older one.
    fn apply_view(&mut self, agent: Agent, ticket: u64, view: View) {
        if !view.came_back() || !self.reads.land(agent, ticket) {
            return;
        }
        let revision = self.revisions.next(agent);
        if let Some(transcript) = view.transcript {
            self.send(Update::Transcript {
                agent,
                revision,
                raw: transcript,
            });
        }
        if let Some(state) = view.state {
            self.send(Update::State {
                revision,
                raw: state,
            });
        }
    }

    /// Watch one lane, or none: the previous lane stream is closed first, so at
    /// most one is ever open.
    ///
    /// A lane's column is seeded with two reads on a thread of their own: its rows
    /// (the lane's transcript is authoritative), and a bounded replay of its retained
    /// events for the newest `todo-changed`. Only the rows hold the stream back
    /// ([`Engine::lane_seeded`]); the replay is read afterwards and reported apart
    /// ([`Engine::lane_todo`]) because it reads a *live* lane's log — a lane put to
    /// work while the watch was still reading would have its first run swallowed by a
    /// replay the tab never sees the events of.
    fn watch_lane(&mut self, client: &Client, lane: Option<u32>) {
        self.lane_connected = false;
        self.streams.stop_lane();
        self.watch_requests += 1;
        let episode = self.watch_requests;
        let Some(n) = lane else {
            self.watch = None;
            return;
        };
        self.watch = Some((episode, n));
        let ticket = self.reads.issue(Agent::Lane(n));
        let mailbox = self.mailbox.clone();
        let client = client.clone();
        let _ = thread::Builder::new()
            .name("tab-engine-lane-seed".to_owned())
            .spawn(move || {
                let rows = client.lane_transcript(n, None).ok().map(|reply| reply.raw);
                if mailbox
                    .send_blocking(Inbound::LaneSeeded {
                        n,
                        episode,
                        ticket,
                        rows,
                    })
                    .is_err()
                {
                    return;
                }
                let todo = lane_todo(&client, n);
                let _ = mailbox.send_blocking(Inbound::LaneTodo { n, episode, todo });
            });
    }

    /// A lane's rows came back: put them on screen, and open its stream. A seed that
    /// arrives after a newer `WatchLane` is dropped — its stream would belong to a lane
    /// the tab is no longer showing (§9.3).
    ///
    /// The stream tails from *now* rather than resuming from a cursor: the lane may have
    /// been working all along, and an event the tab has not seen yet is worth more than
    /// one replayed twice. Only the read above, and the moment it took, sit between the
    /// rows and the stream.
    fn lane_seeded(
        &mut self,
        client: &Client,
        n: u32,
        episode: u64,
        ticket: u64,
        rows: Option<Value>,
    ) {
        if self.watch != Some((episode, n)) {
            return;
        }
        self.apply_view(
            Agent::Lane(n),
            ticket,
            View {
                transcript: rows,
                state: None,
            },
        );
        self.streams.start_lane(client, n, None);
    }

    /// The replay that seeds a shown lane's TODO panel came back: one event, and only
    /// if the tab is still showing that lane.
    fn lane_todo(&mut self, n: u32, episode: u64, todo: Option<(Option<i64>, Value)>) {
        if self.watch != Some((episode, n)) {
            return;
        }
        if let Some((id, data)) = todo {
            self.send(Update::Event {
                agent: Agent::Lane(n),
                id,
                kind: "todo-changed".to_owned(),
                data,
            });
        }
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
                self.send(Update::Stream {
                    agent,
                    status: StreamStatus::Connected,
                });
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
                    self.note_lane_state(client, &data);
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
                self.send(Update::Event {
                    agent,
                    id,
                    kind,
                    data,
                });
                if let Some(lanes) = resync {
                    self.resync(client, agent, lanes);
                }
                Liveness::Alive
            }
            StreamMsg::Ended => Liveness::Alive,
        }
    }
}

/// §7.3's cache seed: `GET /journal?limit=20`, then 100, then 400 — growing only
/// while the newest `cache-stats` entry is missing, so a long session is never
/// re-sent in full (the same walk `session::cache` reads).
fn walk_journal(client: &Client) -> Option<Value> {
    for limit in CACHE_LIMITS {
        match client.journal(Some(limit)) {
            Ok(journal) => {
                if let Some(entry) = journal.newest_custom(CACHE_STATS_KEY) {
                    return Some(entry.clone());
                }
            }
            Err(_) => break,
        }
    }
    None
}

/// One bounded replay of a lane's retained events (`?since=0`): the newest
/// `todo-changed` in it, with the id it carried.
///
/// Only that one event is forwarded. The replay is not forwarded as rows: the lane's
/// serve keeps up to 20 000 events, so `?since=0` would re-send the lane's whole
/// session, duplicating every row the transcript has already given. The replay is
/// bounded by a short silence — the log replays as a burst, so a pause means the live
/// edge — and whatever the lane does while it reads belongs to the lane's own stream,
/// which is already open by then (§9.3).
fn lane_todo(client: &Client, n: u32) -> Option<(Option<i64>, Value)> {
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
    let mut todo = None;
    let mut seen = 0usize;
    while let Ok(Some(line)) = connection.read_line() {
        let Some(event) = parser.feed(&line) else {
            continue;
        };
        seen += 1;
        if event.kind.as_deref() == Some("todo-changed") {
            let data = serde_json::from_str(&event.data).unwrap_or(Value::Null);
            todo = Some((event.id, data));
        }
        if seen >= SEED_CAP {
            break;
        }
    }
    todo
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An engine with nothing running behind it: enough to ask what a `lane-state`
    /// event decides, which is all these tests are about. The deferred read the
    /// decisions schedule finds a dropped receiver and gives up quietly.
    fn dormant() -> Engine {
        dormant_engine().0
    }

    /// The same, with the channel its [`Update`]s land on kept, for the tests that
    /// look at what the engine emitted.
    fn dormant_engine() -> (Engine, Receiver<Update>) {
        let (updates, updates_rx) = async_channel::unbounded();
        let (mailbox, _inbox) = async_channel::unbounded();
        let mut engine = Engine {
            updates,
            mailbox: mailbox.clone(),
            streams: Streams::new(mailbox),
            revisions: Revisions::default(),
            reads: Reads::default(),
            coordinator_connected: false,
            lane_connected: false,
            want_shutdown: false,
            watch: None,
            watch_requests: 0,
            lane_states: HashMap::new(),
            lane_pids: HashMap::new(),
            lanes_due: None,
            lanes_tries: 0,
            lanes_fetching: false,
        };
        engine.lane_states.insert(1, "idle".to_owned());
        engine.lane_states.insert(2, "idle".to_owned());
        (engine, updates_rx)
    }

    /// The row's glyph follows the event, but the header above it and the row's
    /// clocks are `/lanes`' — so a lane whose state moves is a moment to read the
    /// list, not only a lane that is still `starting`.
    #[test]
    fn a_lane_state_that_moves_asks_for_the_list() {
        let mut engine = dormant();
        engine.note_lane_state(&nowhere(), &json!({ "lane": 1, "state": "working" }));
        assert!(
            engine.lanes_due.is_some(),
            "a lane going working asks for the list"
        );
        assert_eq!(
            engine.lane_states.get(&1).map(String::as_str),
            Some("working")
        );

        let mut engine = dormant();
        engine.note_lane_state(&nowhere(), &json!({ "lane": 2, "state": "idle" }));
        assert!(engine.lanes_due.is_some(), "so does one going idle");
    }

    /// A launch announcement is what the follow-up episode exists for: the swarm
    /// never announces the state a lane comes up *as* (`sync-lane-state :announce
    /// nil`), so the read has to be repeated while a lane is still `starting`.
    #[test]
    fn a_launch_announcement_starts_the_follow_ups() {
        let mut engine = dormant();
        engine.lanes_tries = 7;
        engine.note_lane_state(&nowhere(), &json!({ "lane": 2, "state": "starting" }));
        assert_eq!(engine.lanes_tries, 0, "a new launch is a new episode");
        assert!(engine.lanes_due.is_some());
        assert_eq!(
            engine.lane_states.get(&2).map(String::as_str),
            Some("starting")
        );

        // …and the follow-ups stop once no lane is starting.
        let mut engine = dormant();
        engine.note_lane_state(&nowhere(), &json!({ "lane": 2, "state": "starting" }));
        assert!(engine.lanes_coming_up());
        engine.note_lane_state(&nowhere(), &json!({ "lane": 2, "state": "idle" }));
        assert!(
            !engine.lanes_coming_up(),
            "the episode is over when the lane is up"
        );
    }

    /// One read per debounce window: a boot's per-lane announcements — or a lane
    /// settling and the coordinator's run ending at the same moment — must not
    /// become a burst of reads.
    #[test]
    fn one_read_per_debounce_window() {
        let mut engine = dormant();
        engine.note_lane_state(&nowhere(), &json!({ "lane": 1, "state": "starting" }));
        let due = engine.lanes_due.expect("a read is pending");
        engine.note_lane_state(&nowhere(), &json!({ "lane": 2, "state": "starting" }));
        engine.arm_lanes_refetch();
        assert_eq!(
            engine.lanes_due,
            Some(due),
            "the pending read is the one that happens"
        );
    }

    /// A client for a server that is not there: what the lane-list decisions are asked
    /// about never reads anything (no lane in these tests is the watched one).
    fn nowhere() -> Client {
        Client::loopback(1, swarm_client::Token::new("test"))
    }

    /// Everything the engine has sent so far.
    fn drain(updates: &Receiver<Update>) -> Vec<Update> {
        let mut all = Vec::new();
        while let Ok(update) = updates.try_recv() {
            all.push(update);
        }
        all
    }

    /// A lane's event log belongs to its process: a *different* pid behind the lane the
    /// tab is showing is the one stream to open again (§9.3), and a lane that reports no
    /// pid — one that is starting or down — says nothing about the process, so the change
    /// is still visible when the lane comes back.
    #[test]
    fn a_lane_that_comes_back_as_a_new_process_is_a_stream_to_open_again() {
        let (mut engine, _updates) = dormant_engine();
        assert!(
            !engine.note_lane_pid(1, Some(400)),
            "the first pid is no change"
        );
        assert!(
            !engine.note_lane_pid(1, Some(400)),
            "nor is the same one again"
        );
        assert!(
            !engine.note_lane_pid(2, Some(500)),
            "a lane the tab is not showing has no stream to reopen"
        );

        engine.watch = Some((1, 2));
        assert!(
            engine.note_lane_pid(2, Some(501)),
            "a new process behind it"
        );
        assert!(!engine.note_lane_pid(2, None), "a lane with no pid yet");
        assert!(
            engine.note_lane_pid(2, Some(502)),
            "…and the change still shows when it is back"
        );
        assert!(
            !engine.note_lane_pid(1, Some(401)),
            "lane 1 is not the watched one"
        );
    }

    /// A view that came back is emitted at one fresh revision — the rows and the
    /// state of one resync share it — and a read that answered nothing emits
    /// nothing, because a revision means a view was read (§9.1).
    #[test]
    fn a_view_that_came_back_is_emitted_at_one_revision() {
        let (mut engine, updates) = dormant_engine();
        let ticket = engine.reads.issue(Agent::Coordinator);

        engine.apply_view(Agent::Coordinator, ticket, View::default());
        assert!(
            updates.try_recv().is_err(),
            "a read that answered nothing says nothing"
        );
        assert_eq!(engine.revisions.get(Agent::Coordinator), 0);

        engine.apply_view(
            Agent::Coordinator,
            ticket,
            View {
                transcript: Some(json!({ "messages": [] })),
                state: Some(json!({ "status": "idle" })),
            },
        );
        let emitted = drain(&updates);
        assert_eq!(emitted.len(), 2, "rows and state, or neither");
        match (&emitted[0], &emitted[1]) {
            (
                Update::Transcript { revision, .. },
                Update::State {
                    revision: state_revision,
                    ..
                },
            ) => assert_eq!(revision, state_revision, "one resync, one revision"),
            other => panic!("expected a transcript and a state, got {other:?}"),
        }
    }

    /// An answer that arrives after a newer one is a late result from an old
    /// revision: the view on screen is the newer read's, so the older one is
    /// dropped rather than replacing it (§9.1).
    #[test]
    fn a_late_answer_does_not_replace_a_newer_view() {
        let (mut engine, updates) = dormant_engine();
        let first = engine.reads.issue(Agent::Coordinator);
        let second = engine.reads.issue(Agent::Coordinator);
        let rows = |text: &str| View {
            transcript: Some(json!({ "messages": [text] })),
            state: None,
        };

        // The newer read answers first; the older one must not take it back.
        engine.apply_view(Agent::Coordinator, second, rows("newer"));
        engine.apply_view(Agent::Coordinator, first, rows("older"));
        assert_eq!(engine.revisions.get(Agent::Coordinator), 1);

        // A read that failed answered nothing: it cannot make an older answer stale
        // either, or the only view there is would be thrown away when the newer read
        // is the one that failed.
        let third = engine.reads.issue(Agent::Coordinator);
        engine.apply_view(Agent::Coordinator, third, View::default());
        engine.apply_view(Agent::Coordinator, second, rows("newer"));
        assert!(!engine.reads.land(Agent::Coordinator, second));
        assert!(
            engine.reads.land(Agent::Coordinator, third),
            "the failed read is not a view, so the older one still lands after it"
        );

        // Tickets are per agent, so a busy lane does not stale the coordinator's.
        let lane = engine.reads.issue(Agent::Lane(1));
        assert!(engine.reads.land(Agent::Lane(1), lane));
        let sent: Vec<&str> = drain(&updates)
            .iter()
            .map(|update| match update {
                Update::Transcript { .. } => "Transcript",
                Update::State { .. } => "State",
                _ => "other",
            })
            .collect();
        assert_eq!(sent, ["Transcript"], "only the newer view was emitted");
    }
}
