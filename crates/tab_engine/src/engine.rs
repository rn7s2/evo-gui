//! The engine thread: one tab's whole life, off the UI thread.
//!
//! It boots the server (§3), runs §9.1's assembly, then turns the server's HTTP
//! and SSE into [`Update`]s until the UI asks it to [`Command::Shutdown`] — the
//! ladder, then [`Update::Exited`]. Nothing here blocks the UI: every
//! [`Command`] is a channel send, and every [`Update`] arrives on a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use serde_json::Value;

use swarm_client::{
    Client, EventStream, Server, ShutdownOutcome, SseParser, StreamConfig, StreamMsg, StreamTarget,
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

    /// `POST /steer`.
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
    Command(Command),
    Stream { agent: Agent, msg: StreamMsg },
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
    streams: Streams,
    revisions: Revisions,
    coordinator_connected: bool,
    lane_connected: bool,
    want_shutdown: bool,
}

fn run(spec: TabSpec, mailbox: Sender<Inbound>, inbox: Receiver<Inbound>, updates: Sender<Update>) {
    let mut engine = Engine {
        updates,
        streams: Streams::new(mailbox),
        revisions: Revisions::default(),
        coordinator_connected: false,
        lane_connected: false,
        want_shutdown: false,
    };

    engine.send(Update::Booting);
    let config = spec.server_config();
    let mut server = match Server::start(&config) {
        Ok(server) => server,
        Err(error) => {
            engine.send(Update::BootFailed {
                message: error.to_string(),
                log_tail: error.log_tail().unwrap_or_default().to_owned(),
            });
            return;
        }
    };
    engine.send(Update::Ready {
        health: server.health().clone(),
        pid: server.pid(),
        port: server.port(),
    });

    let client = server.client().clone();
    engine.assemble(&client, server.health().cursor);
    engine.serve(&mut server, &client, &inbox);
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
        if let Ok(lanes) = client.lanes() {
            self.send(Update::Lanes { raw: lanes.raw });
        }
        self.resync(client, Agent::Coordinator, false);
        self.seed_cache(client);
        self.streams.start_coordinator(client, cursor);
    }

    /// The main loop: commands from the UI and messages from the streams, until
    /// shutdown or the server's death.
    fn serve(&mut self, server: &mut Server, client: &Client, inbox: &Receiver<Inbound>) {
        while let Ok(inbound) = inbox.recv_blocking() {
            let alive = match inbound {
                Inbound::Command(Command::Shutdown) => {
                    self.want_shutdown = true;
                    // Stop the streams first, so nothing more arrives while the
                    // ladder runs.
                    self.streams.stop_all();
                    let outcome = self.ladder(server);
                    return self.exit(outcome);
                }
                Inbound::Command(command) => {
                    self.command(client, command);
                    Liveness::Alive
                }
                Inbound::Stream { agent, msg } => self.stream_message(server, client, agent, msg),
            };
            if alive == Liveness::Gone {
                self.want_shutdown = true;
                self.streams.stop_all();
                self.send(Update::ServerGone);
                return self.exit(ShutdownOutcome::AlreadyGone);
            }
        }
        // The mailbox closed: the handle went away without a shutdown command.
        let outcome = self.ladder(server);
        self.exit(outcome);
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
    fn resync(&mut self, client: &Client, agent: Agent, lanes: bool) {
        match agent {
            Agent::Coordinator => {
                let revision = self.revisions.next(agent);
                if let Ok(transcript) = client.transcript(None) {
                    self.send(Update::Transcript { agent, revision, raw: transcript.raw });
                }
                if let Ok(state) = client.state() {
                    self.send(Update::State { revision, raw: state.raw });
                }
                if lanes {
                    if let Ok(lanes) = client.lanes() {
                        self.send(Update::Lanes { raw: lanes.raw });
                    }
                }
            }
            Agent::Lane(n) => {
                let revision = self.revisions.next(agent);
                if let Ok(transcript) = client.lane_transcript(n, None) {
                    self.send(Update::Transcript { agent, revision, raw: transcript.raw });
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
                let resync = match kind.as_str() {
                    "hello" => Some(true),
                    "gap" | "session-switched" | "settled" => Some(false),
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
