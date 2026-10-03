//! One tab's I/O, off the UI thread.
//!
//! ```text
//!   UI thread ──OpRequest──▶ [ tab-engine thread ] ──snapshot/stream/ops──▶ evo-swarm serve
//!   UI thread ◀──Update──── [ tab-engine thread ] ◀──frames──────────────  (coordinator, lanes)
//! ```
//!
//! The whole engine is one thread and four steps:
//!
//! 1. **spawn** — [`Server::start`], which waits for the ready file, and report
//!    [`Update::BootFailed`] with the log tail when the server never comes up.
//! 2. **snapshot** — one `GET /snapshot` for `session`, `swarm` and `lane:*`, split
//!    into one [`Update::Snapshot`] per topic.
//! 3. **stream** — one `GET /stream` from that snapshot's cursor. Every frame is
//!    parsed into a [`session::Op`] and handed over; a `topic.reset` makes the
//!    engine re-snapshot that topic, and a `stream.reset` makes it re-snapshot
//!    everything and resume the stream from the snapshot's cursor, so the re-read
//!    is gapless. A snapshot that failed is asked for again — half a second,
//!    doubling to ten — because the resumed stream is the only thing that can wake
//!    a parked one; while that lasts the tab says it is catching up
//!    ([`crate::types::StreamStatus::Reconnecting`]).
//! 4. **ops** — `POST /ops` with an engine-made rid, on a thread of its own so a
//!    slow answer never holds up the stream.
//!
//! There is no second stream, no per-lane bookkeeping, no revision counter and no
//! polling: a restart is an epoch, a stale view is a reset, and a dead server is a
//! stream that cannot reconnect (checked against the process, once).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use serde_json::{json, Value};

use session::{Op, OpRequest, OpSink};
use swarm_client::{
    BootCancel, Client, ErrorCode, EventStream, OpError, OpReply, Server, ServerConfig,
    ShutdownOutcome, Snapshot, StdinClose, StreamConfig, StreamFrame, StreamMsg,
};

/// How many items of the session topic a lost send is checked against: the newest
/// page, which is where a turn that has just been taken is.
const SESSION_PAGE: u32 = 50;

/// How long the first re-snapshot after a reset waits, and the longest it grows to.
///
/// A reset parks the stream until the snapshot that resumes it lands
/// ([`EventStream::resume_from`]), and nothing else in this process can wake it.
/// What failed is the *moment* — a session thread taken by a long turn, a process
/// still coming back after a restart — so the snapshot is asked for again, at half a
/// second and doubling: long enough to stop hammering a session that is busy, short
/// enough that a tab which came back is live again before a reader misses it.
const RESET_RETRY_FIRST: Duration = Duration::from_millis(500);
const RESET_RETRY_LONGEST: Duration = Duration::from_secs(10);

use crate::types::Update;

use crate::types::tab_topics;

/// What the engine thread reads: a command from the UI, or something the stream
/// saw. One channel for both, so the loop never blocks on two things at once.
enum Inbound {
    /// An op the UI asked for (session built it; the engine POSTs it, under the
    /// rid the handle minted so the UI can recognise its reply).
    Request {
        rid: String,
        request: Box<OpRequest>,
    },
    /// Re-read every topic.
    Snapshot,
    /// A read the UI asked for (§5.4): older items, one whole item, image bytes.
    Fetch(Fetch),
    /// Stop the server the ladder's way and end the engine.
    Shutdown,
    /// Ask again for a snapshot that failed, once the retry timer says the wait is
    /// over. Sent by the timer thread, never by the UI.
    Retry {
        /// The one topic a `topic.reset` made stale; `None` for a `stream.reset`,
        /// which makes every topic stale and parks the stream until it is answered.
        topic: Option<String>,
        /// What was waited before this attempt: what the next one doubles from.
        waited: Duration,
    },
    Stream(StreamMsg),
}

/// One read the UI asked for. GETs, so they are neither ops nor snapshots.
#[derive(Clone, Debug)]
enum Fetch {
    /// `GET /items?topic=&before=&limit=` — older items, paged in at the front.
    Page {
        topic: String,
        before: Option<String>,
        limit: u32,
    },
    /// `GET /items/<id>?topic=` — one item, whole.
    Item { topic: String, id: String },
    /// `GET /media/<id>/<n>?topic=` — image bytes.
    Media { topic: String, id: String, n: u32 },
}

impl Fetch {
    /// The fetch, named for a person — and for a failure the UI has to place.
    fn what(&self) -> String {
        match self {
            Fetch::Page {
                topic,
                before: Some(before),
                ..
            } => format!("items before {before} in {topic}"),
            Fetch::Page { topic, .. } => format!("items in {topic}"),
            Fetch::Item { topic, id } => format!("item {id} in {topic}"),
            Fetch::Media { topic, id, n } => format!("media {n} of {id} in {topic}"),
        }
    }

    /// The topic a page of older items was about, `None` for the other reads: a page is
    /// the one read whose failure must release the transcript's in-flight state (§5.4).
    fn page_topic(&self) -> Option<String> {
        match self {
            Fetch::Page { topic, .. } => Some(topic.clone()),
            _ => None,
        }
    }

    /// Do it. The update carries the server's own body, unread.
    fn run(self, client: &Client) -> Result<Update, swarm_client::Error> {
        match self {
            Fetch::Page {
                topic,
                before,
                limit,
            } => {
                let body = client.items(&topic, before.as_deref(), limit)?;
                Ok(Update::ItemsBefore { topic, body })
            }
            Fetch::Item { topic, id } => {
                let body = client.item(&topic, &id)?;
                Ok(Update::Item { topic, body })
            }
            Fetch::Media { topic, id, n } => {
                let (bytes, content_type) = client.media(&topic, &id, n)?;
                Ok(Update::Media {
                    topic,
                    id,
                    n,
                    content_type,
                    bytes,
                })
            }
        }
    }
}

/// A tab: one server, one stream, one thread.
pub struct TabEngine;

impl TabEngine {
    /// Start a tab from the config its launcher built. The handle takes ops; the
    /// receiver carries updates.
    pub fn start(config: ServerConfig) -> (EngineHandle, Receiver<Update>) {
        let (inbox, mailbox) = async_channel::bounded(1024);
        let (updates, receiver) = async_channel::bounded(4096);
        let folder = config.cwd.clone();
        let commands = inbox.clone();
        // Both are shared with the handle: a quit closes the child's stdin and
        // cancels a boot still in progress, without waiting for the thread.
        let stdin = StdinClose::new();
        let cancel = BootCancel::new();
        let thread = thread::Builder::new()
            .name("evo-tab-engine".into())
            .spawn({
                let stdin = stdin.clone();
                let cancel = cancel.clone();
                move || run(config, mailbox, commands, updates, stdin, cancel)
            })
            .expect("the engine thread starts");
        (
            EngineHandle {
                inbox,
                stdin,
                cancel,
                thread: Some(thread),
                folder,
            },
            receiver,
        )
    }
}

/// A tab's handle: ops in, and the thread's end of the story.
pub struct EngineHandle {
    inbox: Sender<Inbound>,
    /// The child's stdin: closing it is the server's own signal to stop, and it
    /// needs neither the engine thread nor a ladder.
    stdin: StdinClose,
    /// Raised when the tab is dropped during a boot that has not finished.
    cancel: BootCancel,
    thread: Option<JoinHandle<()>>,
    folder: PathBuf,
}

impl EngineHandle {
    /// The folder this tab's server runs in.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Send one op (`TabModel::send_input`, `interrupt_lane`, …), and say which
    /// rid it was sent under: the reply arrives as
    /// [`Update::OpReply`](crate::Update::OpReply) with that rid and the op's
    /// name, so the UI can match it to the request it made. `None` when the
    /// engine is already gone.
    pub fn request(&self, request: OpRequest) -> Option<String> {
        let rid = swarm_client::new_rid();
        let sent = self.send(Inbound::Request {
            rid: rid.clone(),
            request: Box::new(request),
        });
        sent.then_some(rid)
    }

    /// Re-read every topic — what a UI asks for when it dropped updates it could
    /// not keep up with.
    pub fn refetch(&self) -> bool {
        self.inbox.send_blocking(Inbound::Snapshot).is_ok()
    }

    /// `GET /items?topic=&before=&limit=`: a page of items older than `before`
    /// (`None` pages from the newest). The answer arrives as
    /// [`Update::ItemsBefore`], or [`Update::FetchFailed`].
    pub fn page(&self, topic: &str, before: Option<&str>, limit: u32) -> bool {
        self.send(Inbound::Fetch(Fetch::Page {
            topic: topic.to_owned(),
            before: before.map(str::to_owned),
            limit,
        }))
    }

    /// `GET /items/<id>`: one item whole — a tool row's full output, the whole
    /// thinking. The answer arrives as [`Update::Item`].
    pub fn item(&self, topic: &str, id: &str) -> bool {
        self.send(Inbound::Fetch(Fetch::Item {
            topic: topic.to_owned(),
            id: id.to_owned(),
        }))
    }

    /// `GET /media/<id>/<n>`: image bytes and their content type. The answer
    /// arrives as [`Update::Media`].
    pub fn media(&self, topic: &str, id: &str, n: u32) -> bool {
        self.send(Inbound::Fetch(Fetch::Media {
            topic: topic.to_owned(),
            id: id.to_owned(),
            n,
        }))
    }

    /// Hand the engine one inbound message.
    fn send(&self, message: Inbound) -> bool {
        self.inbox.send_blocking(message).is_ok()
    }

    /// Stop the server and end the engine. Returns at once: the child is told by
    /// its stdin closing, and the ladder runs on the engine's thread.
    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.stdin.close();
        let _ = self.inbox.send_blocking(Inbound::Shutdown);
    }

    pub fn is_running(&self) -> bool {
        !self.inbox.is_closed()
    }

    /// Stop the server and wait for the engine thread to end — for a test, or a
    /// caller that wants to be sure nothing is left.
    pub fn join(mut self) {
        self.shutdown();
        self.wait();
    }

    /// Wait for the engine thread to end, without asking it to.
    fn wait(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Hand the thread over to nobody: it ends when its server does.
    pub fn detach(mut self) {
        self.thread.take();
    }
}

/// The UI builds an op; the transport POSTs it. This is the transport. The
/// fire-and-forget form: a caller that wants to recognise its reply uses
/// [`EngineHandle::request`] and keeps the rid.
impl OpSink for EngineHandle {
    fn send(&self, request: OpRequest) {
        let _ = self.request(request);
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // A quit must not block the UI thread: the server is told to stop (stdin
        // EOF, at once), and the engine thread finishes in its own time.
        self.shutdown();
    }
}

/// The engine thread.
fn run(
    config: ServerConfig,
    mailbox: Receiver<Inbound>,
    commands: Sender<Inbound>,
    updates: Sender<Update>,
    stdin: StdinClose,
    cancel: BootCancel,
) {
    let mut engine = Engine {
        updates: Updates::new(updates),
        topics: tab_topics(),
    };
    engine.send(Update::Booting);

    let mut server = match Server::start_with(&config, &cancel, stdin) {
        Ok(server) => server,
        Err(error) => {
            engine.send(Update::BootFailed {
                reason: boot_message(&error),
                log_tail: error.log_tail().unwrap_or_default().to_owned(),
            });
            return;
        }
    };
    let ready = server.ready().clone();
    engine.send(Update::Ready {
        epoch: ready.epoch.clone(),
        pid: ready.pid,
        port: ready.port,
        session: ready.session.clone(),
    });

    let Some((mut live, snapshot)) = Live::connect(&server, &engine.topics, &commands) else {
        engine.send(Update::BootFailed {
            reason: "the server answered the ready file but not a snapshot".into(),
            log_tail: server.log_tail(40),
        });
        let outcome = server
            .shutdown()
            .map(|shutdown| shutdown.outcome)
            .unwrap_or(ShutdownOutcome::Killed);
        engine.send(Update::Exited { outcome });
        return;
    };
    engine.topics_of(&snapshot);

    let outcome = engine_loop(&mut engine, &mut server, &mut live, &mailbox, &commands);
    live.stop();
    let outcome = match outcome {
        Some(outcome) => outcome,
        None => match server.shutdown() {
            Ok(shutdown) => shutdown.outcome,
            Err(_) => ShutdownOutcome::Killed,
        },
    };
    engine.send(Update::Exited { outcome });
}

/// The tab's connection to one process lifetime: the client, the stream it reads,
/// and the thread that forwards what the stream sees.
///
/// A supervisor restart is a *new* lifetime, but not a new address: the port and
/// the token are the launch's (§1), so the stream reconnects to the connection it
/// already had and the server answers hello + `stream.reset{restarted}` — which
/// the tab takes as "read me again", not as a new server. There is no second
/// ready-file path, and nothing in the UI needs to be told: the snapshots carry
/// the session and the epoch, which is everything a tab draws.
struct Live {
    client: Client,
    stream: EventStream,
    forwarder: Option<JoinHandle<()>>,
}

impl Live {
    /// Snapshot, then one stream from that snapshot's cursor: there is no window
    /// between the two in which an op could be missed.
    fn connect(
        server: &Server,
        topics: &[String],
        commands: &Sender<Inbound>,
    ) -> Option<(Live, Snapshot)> {
        let client = server.client().clone();
        // A whole render window's worth, not the server's smaller default (§5.2): the
        // tab's live transcript holds `PAGE_ITEMS` items before anything is streamed.
        let snapshot = client.snapshot(topics, Some(session::PAGE_ITEMS)).ok()?;
        let stream = EventStream::start(
            client.clone(),
            StreamConfig::new(topics.to_vec()).from(snapshot.cursor()),
        );
        let forwarder = forward_stream(stream.clone(), commands);
        Some((
            Live {
                client,
                stream,
                forwarder: Some(forwarder),
            },
            snapshot,
        ))
    }

    /// Stop reading, and wait for the forwarding thread to end.
    fn stop(&mut self) {
        self.stream.stop();
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
    }
}

/// Why a boot failed, in one line, for a tab's caption.
fn boot_message(error: &swarm_client::Error) -> String {
    match error {
        swarm_client::Error::Boot(failure) => failure.reason.clone(),
        other => other.to_string(),
    }
}

/// Copy the stream's messages into the engine's mailbox, so the engine thread has
/// one thing to read. Ends when the stream does.
fn forward_stream(stream: EventStream, mailbox: &Sender<Inbound>) -> JoinHandle<()> {
    let mailbox = mailbox.clone();
    thread::Builder::new()
        .name("evo-tab-forward".into())
        .spawn(move || {
            while let Some(message) = stream.recv() {
                if mailbox.send_blocking(Inbound::Stream(message)).is_err() {
                    break;
                }
            }
        })
        .expect("the forwarding thread starts")
}

/// The loop: ops from the UI, frames from the stream, until one of them says the
/// tab is over.
///
/// `None` means the server is gone or was stopped; `Some(outcome)` means the
/// ladder already ran.
fn engine_loop(
    engine: &mut Engine,
    server: &mut Server,
    live: &mut Live,
    mailbox: &Receiver<Inbound>,
    commands: &Sender<Inbound>,
) -> Option<ShutdownOutcome> {
    loop {
        let message = match mailbox.recv_blocking() {
            Ok(message) => message,
            // Every sender is gone: the handle was dropped (which asks for a
            // shutdown first) and the stream has ended.
            Err(_) => return None,
        };
        match message {
            Inbound::Shutdown => return None,
            Inbound::Snapshot => {
                engine.snapshot(&live.client, None);
            }
            Inbound::Request { rid, request } => post(engine, &live.client, rid, *request),
            Inbound::Fetch(fetch) => fetch_off_loop(engine, &live.client, fetch),
            Inbound::Stream(StreamMsg::Connected { .. }) => {
                engine.send(Update::Stream {
                    status: crate::types::StreamStatus::Connected,
                });
            }
            Inbound::Stream(StreamMsg::Reconnecting { retry_in }) => {
                // A supervisor restart is a new process lifetime, but not a new
                // address: §1 keeps the port and the token, so the reconnect ends
                // in a `stream.reset` on this same connection, and the reset path
                // above reads the tab again from the new epoch. All there is to do
                // here is say so, and give up if the process itself is gone.
                // A stream that cannot come back is a server that is not there.
                if !server.is_running() {
                    engine.send(Update::ServerGone);
                    return None;
                }
                engine.send(Update::Stream {
                    status: crate::types::StreamStatus::Reconnecting { retry_in },
                });
            }
            Inbound::Stream(StreamMsg::Frame(frame)) => {
                if frame.topic_reset().is_some() {
                    let topic = frame.topic().unwrap_or_default().to_owned();
                    engine.frame(frame);
                    // That one topic is stale; everything else stands. Asking again
                    // does not hold this loop up — the stream is not parked — so a
                    // snapshot that failed is retried on its own clock.
                    if engine.snapshot(&live.client, Some(topic.clone())).is_none() {
                        ask_again(engine, commands, Some(topic), RESET_RETRY_FIRST);
                    }
                } else {
                    engine.frame(frame);
                }
            }
            Inbound::Stream(StreamMsg::Reset { reason }) => {
                // The server cannot continue from our cursor. Everything is stale
                // — the session crate decides what that means — and the engine
                // snapshots first so it can resume the stream from the position
                // that snapshot was atomic at: no op lost, none applied twice.
                engine.send(Update::Op {
                    topic: swarm_client::TOPIC_SESSION.to_owned(),
                    op: Op::StreamReset {
                        reason: reason.as_str().to_owned(),
                    },
                });
                match engine.snapshot(&live.client, None) {
                    Some(snapshot) => live.stream.resume_from(snapshot.cursor()),
                    None => ask_again(engine, commands, None, RESET_RETRY_FIRST),
                }
            }
            Inbound::Retry { topic, waited } => {
                // The wait is over: the snapshot that failed is asked for again.
                // Only success ends this — a tab catching up is worth telling the
                // reader about (`Reconnecting`), and a process that is gone is the
                // one way out.
                match engine.snapshot(&live.client, topic.clone()) {
                    Some(snapshot) => {
                        if topic.is_none() {
                            // Every topic is re-read and the parked stream resumes
                            // from the position that read was atomic at.
                            live.stream.resume_from(snapshot.cursor());
                        }
                        // The tab is live again, and stops saying otherwise — if
                        // this read was one that made it say so.
                        if holds_the_tab(topic.as_deref()) {
                            engine.send(Update::Stream {
                                status: crate::types::StreamStatus::Connected,
                            });
                        }
                    }
                    None if server.is_running() => {
                        let next = waited
                            .saturating_mul(2)
                            .clamp(RESET_RETRY_FIRST, RESET_RETRY_LONGEST);
                        ask_again(engine, commands, topic, next);
                    }
                    None => {
                        engine.send(Update::ServerGone);
                        return None;
                    }
                }
            }
            Inbound::Stream(StreamMsg::Stopped) => {
                if !server.is_running() {
                    engine.send(Update::ServerGone);
                    return None;
                }
            }
        }
    }
}

/// Whether a re-read that has not landed leaves the tab itself behind: the whole
/// tab's (the stream is parked until it lands) or the coordinator's own topic.
///
/// A lane's or the swarm's topic is not: the stream is live, the lane keeps the
/// items it had and folds every op after them, and the badge this would raise is
/// the coordinator's — a lane whose snapshot the server cannot answer is not a
/// coordinator that is reconnecting.
fn holds_the_tab(topic: Option<&str>) -> bool {
    topic.is_none_or(|topic| topic == swarm_client::TOPIC_SESSION)
}

/// Ask again for a snapshot that failed, once `next` has gone by — and, when the
/// read is one the tab is waiting on ([`holds_the_tab`]), say the tab is catching
/// up until it lands.
///
/// The retry is a timer, not a wait here: the engine thread still takes ops and
/// frames while a tab is behind, which is the only reason a reset can be retried
/// without freezing the very tab it is trying to bring back.
fn ask_again(
    engine: &mut Engine,
    commands: &Sender<Inbound>,
    topic: Option<String>,
    next: Duration,
) {
    if holds_the_tab(topic.as_deref()) {
        engine.send(Update::Stream {
            status: crate::types::StreamStatus::Reconnecting { retry_in: next },
        });
    }
    let commands = commands.clone();
    let _ = thread::Builder::new()
        .name("evo-tab-reset".into())
        .spawn(move || {
            thread::sleep(next);
            let _ = commands.send_blocking(Inbound::Retry {
                topic,
                waited: next,
            });
        });
}

/// One read, on a thread of its own: a page of items, an item, or an image can be
/// large, and none of them may hold up the stream. A failure is an update, not a
/// panic and not a retry.
fn fetch_off_loop(engine: &Engine, client: &Client, fetch: Fetch) {
    let client = client.clone();
    let updates = engine.updates.clone();
    let what = fetch.what();
    let page = fetch.page_topic();
    let _ = thread::Builder::new()
        .name("evo-tab-fetch".into())
        .spawn(move || {
            let message = match fetch.run(&client) {
                Ok(update) => update,
                Err(error) => Update::FetchFailed {
                    what,
                    reason: error.to_string(),
                    page,
                },
            };
            updates.send(message);
        });
}

/// POST one op, on a thread of its own: a slow or refused op must not hold up the
/// stream, and the rid makes the client's one retry free (§5.5).
fn post(engine: &Engine, client: &Client, rid: String, request: OpRequest) {
    let client = client.clone();
    let updates = engine.updates.clone();
    let op = request.op.clone();
    let args = request.args.clone();
    let sent_at_ms = now_millis();
    let _ = thread::Builder::new()
        .name("evo-tab-op".into())
        .spawn(move || {
            // Sent under the rid the handle minted, so the reply the UI gets is
            // the one its request is waiting for.
            let reply = match client.op_with_rid(&rid, &op, args.clone()) {
                Ok(reply) => reply,
                Err(error) => lost_reply(&client, &rid, &op, &args, sent_at_ms, error),
            };
            updates.send(Update::OpReply {
                rid,
                op,
                reply: Box::new(reply),
            });
        });
}

/// What to answer when the server never answered.
///
/// A request that was written and not answered is *not* a refusal: the server may
/// have taken it and lost the reply (a connection reset, a read timeout), and a
/// client that reads that as "the session refused this" is how a reader ends up
/// sending the same turn again — measured 2026-10-02: a turn carrying a 640 KB
/// screenshot is 854 KB of base64, which this server reads and journals in 35.7 s,
/// and the 30 s patience in force then reported it as refused while the session
/// held it.
///
/// An `input.send` is the one op whose whole truth a client can look up: the turn
/// it asked for *is* the session topic's newest user item, with the same words and
/// the same number of pictures, made when the send went out. Finding it answers the
/// send the way the server would have — `ok` with the item's own id — so the
/// composer clears its draft and the turn is not sent twice. Not finding it says so
/// in the reply's own words: the outcome of this send is unknown, and the reader is
/// told that rather than told the session refused it.
fn lost_reply(
    client: &Client,
    rid: &str,
    op: &str,
    args: &Value,
    sent_at_ms: u64,
    error: swarm_client::Error,
) -> OpReply {
    if op == "input.send" {
        if let Some(item_id) = the_turn_in_the_session(client, args, sent_at_ms) {
            return OpReply {
                rid: rid.to_owned(),
                ok: true,
                seq: 0,
                result: json!({ "item_id": item_id, "queued": false, "blocked": Value::Null }),
                error: None,
            };
        }
    }
    OpReply {
        rid: rid.to_owned(),
        ok: false,
        seq: 0,
        result: Value::Null,
        error: Some(OpError {
            code: ErrorCode::Unknown,
            message: if op == "input.send" {
                format!(
                    "the server did not answer this send ({error}); the session does not \
                     hold it yet — it may still arrive"
                )
            } else {
                format!("the server did not answer this {op} ({error})")
            },
            // A marker the UI reads, not a code: an absent answer is not the
            // session refusing anything, and it may not be shown as a refusal.
            detail: json!({ "outcome": "unknown" }),
        }),
    }
}

/// The item the session holds for the turn `args` asked for, if it is there: same
/// words, same number of pictures, made no earlier than the send went out.
///
/// A read, and a cheap one: the newest page of the session topic. It cannot make a
/// duplicate — it asks the session, it does not ask it again.
fn the_turn_in_the_session(client: &Client, args: &Value, sent_at_ms: u64) -> Option<String> {
    let snapshot = client
        .snapshot(&["session".to_string()], Some(SESSION_PAGE))
        .ok()?;
    the_turn_in(snapshot.topic("session")?, args, sent_at_ms)
}

/// The same question of one snapshot body, so the rule is testable without a
/// server (`tests` below).
fn the_turn_in(topic: &Value, args: &Value, sent_at_ms: u64) -> Option<String> {
    let text = args.get("text").and_then(Value::as_str)?;
    let images = args
        .get("images")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let items = topic.get("items")?.as_array()?;
    for item in items.iter().rev() {
        if item.get("kind").and_then(Value::as_str) != Some("user") {
            continue;
        }
        if item.get("text").and_then(Value::as_str) != Some(text) {
            continue;
        }
        if item
            .get("images")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
            != images
        {
            continue;
        }
        // The reader's own history is not evidence about this send: a turn of the
        // same words from before it went out is a different turn. Two seconds of
        // slack for the server's own clock against ours.
        let ts = item.get("ts").and_then(Value::as_u64).unwrap_or(0);
        if ts + 2_000 < sent_at_ms {
            continue;
        }
        return item.get("id").and_then(Value::as_str).map(str::to_owned);
    }
    None
}

/// Epoch milliseconds, for matching a turn's own `ts` against when the send went out.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// The engine's end of the update channel, and the one thing to say when nobody is
/// taking what it sends.
///
/// Every send here is fire-and-forget on purpose — a tab that fell behind must not
/// hold the stream up — so an update nobody takes vanishes without a trace, and that
/// is how a dead reader hides: the engine writes into a closed channel for the rest
/// of the tab's life and nothing anywhere says so. The first one is said once, with
/// what it was. It is a bug in this client, not something a reader can act on.
#[derive(Clone)]
struct Updates {
    updates: Sender<Update>,
    /// How many have been dropped, and whether that has been said.
    dropped: Arc<AtomicUsize>,
    said: Arc<AtomicBool>,
}

impl Updates {
    fn new(updates: Sender<Update>) -> Updates {
        Updates {
            updates,
            dropped: Arc::new(AtomicUsize::new(0)),
            said: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Hand one over. `false` when there was nobody left to take it.
    fn send(&self, update: Update) -> bool {
        let update = match self.updates.send_blocking(update) {
            Ok(()) => return true,
            Err(error) => error.into_inner(),
        };
        let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.said.swap(true, Ordering::Relaxed) {
            eprintln!(
                "evo-desktop: the tab stopped taking updates — {dropped} dropped, the first was a {}",
                what(&update)
            );
        }
        false
    }
}

/// One update, named for the one line said when it could not be handed over.
fn what(update: &Update) -> &'static str {
    match update {
        Update::Booting => "boot",
        Update::Ready { .. } => "ready",
        Update::BootFailed { .. } => "boot failure",
        Update::Snapshot { .. } => "snapshot",
        Update::Op { .. } => "frame",
        Update::Stream { .. } => "stream status",
        Update::OpReply { .. } => "reply",
        Update::ItemsBefore { .. } => "page of older items",
        Update::Item { .. } => "item",
        Update::Media { .. } => "image",
        Update::FetchFailed { .. } => "failed read",
        Update::ServerGone => "server gone",
        Update::Exited { .. } => "exit",
    }
}

/// The engine's own state: where updates go, and what to read.
struct Engine {
    updates: Updates,
    topics: Vec<String>,
}

impl Engine {
    fn send(&mut self, update: Update) -> bool {
        self.updates.send(update)
    }

    /// Hand a snapshot's topics over, one update each: the UI applies them topic by
    /// topic (`TabModel::on_snapshot`).
    fn topics_of(&mut self, snapshot: &Snapshot) {
        for topic in snapshot.topic_names() {
            if let Some(body) = snapshot.topic(topic) {
                let body = body.clone();
                self.send(Update::Snapshot {
                    topic: topic.to_owned(),
                    body,
                });
            }
        }
    }

    /// One frame, parsed: the UI's `TabModel::on_op`. A frame this build does not
    /// know is not the UI's to draw, and is dropped.
    fn frame(&mut self, frame: StreamFrame) {
        let topic = frame.topic().unwrap_or_default().to_owned();
        if let Some(op) = Op::from_json(&frame.op, &frame.data) {
            self.send(Update::Op { topic, op });
        }
    }

    /// Take a snapshot of everything, or of one topic after a `topic.reset`.
    ///
    /// A failed snapshot is not fatal: the stream keeps running, and the next
    /// reset asks again.
    fn snapshot(&mut self, client: &Client, topic: Option<String>) -> Option<Snapshot> {
        let topics = match &topic {
            Some(topic) => vec![topic.clone()],
            None => self.topics.clone(),
        };
        let snapshot = client.snapshot(&topics, Some(session::PAGE_ITEMS)).ok()?;
        self.topics_of(&snapshot);
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_boot_exit_is_read_back_as_a_message() {
        let error = swarm_client::Error::Boot(Box::new(swarm_client::BootFailure {
            reason: "the server exited during startup (exit status: 3)".into(),
            log_tail: "boom".into(),
            log_path: None,
            exit_code: Some(3),
        }));
        assert!(boot_message(&error).contains("exited during startup"));
        assert!(boot_message(&swarm_client::Error::Closed).contains("closed"));
    }

    #[test]
    fn a_frame_becomes_the_session_crate_its_own_op() {
        let frame = StreamFrame::parse(
            Some("op"),
            Some("7f3a.3"),
            &json!({"op": "item.append", "topic": "session", "id": "e_1", "field": "text", "text": "hi"})
                .to_string(),
        );
        assert_eq!(frame.topic(), Some("session"));
        assert!(matches!(
            Op::from_json(&frame.op, &frame.data),
            Some(Op::ItemAppend { .. })
        ));
    }

    /// §5.5: a send whose reply was lost is looked up in the session before it is
    /// called a failure. The turn is the one with its words and its pictures, made
    /// when the send went out — the reader's own history is not evidence about
    /// this send.
    #[test]
    fn a_lost_send_is_found_by_its_own_words_pictures_and_time() {
        let sent = 1_790_000_000_000u64;
        let turn = |id: &str, text: &str, images: usize, ts: u64| {
            json!({
                "id": id, "kind": "user", "ts": ts, "text": text, "status": "queued",
                "images": (0..images).map(|n| json!({"name": format!("{n}.png")})).collect::<Vec<_>>(),
            })
        };
        let args = |text: &str, images: usize| {
            json!({
                "text": text, "queue": "now", "topic": "session",
                "images": (0..images).map(|_| json!({"data": "…"})).collect::<Vec<_>>(),
            })
        };
        let topic = |items: Vec<serde_json::Value>| json!({ "state": {}, "items": items });

        // There it is: the reader's turn with the picture, made as the send went out.
        let body = topic(vec![
            turn("e_1", "an older turn", 0, sent - 60_000),
            turn("e_2", "look at this", 1, sent + 120),
        ]);
        assert_eq!(
            the_turn_in(&body, &args("look at this", 1), sent).as_deref(),
            Some("e_2")
        );

        // A turn of the same words from before the send is not this send — and a
        // turn with the words but not the picture is not this send either.
        let older = topic(vec![turn("e_1", "look at this", 1, sent - 60_000)]);
        assert_eq!(the_turn_in(&older, &args("look at this", 1), sent), None);
        let text_only = topic(vec![turn("e_1", "look at this", 0, sent + 120)]);
        assert_eq!(
            the_turn_in(&text_only, &args("look at this", 1), sent),
            None
        );

        // And a session that does not hold it says so, rather than answering with
        // somebody else's turn.
        let other = topic(vec![turn("e_9", "something else", 1, sent + 120)]);
        assert_eq!(the_turn_in(&other, &args("look at this", 1), sent), None);
        assert_eq!(
            the_turn_in(&json!({ "state": {} }), &args("", 0), sent),
            None
        );
    }

    /// The same, end to end of the failure path: what the UI is told when the reply
    /// was lost. `ok` when the session holds the turn (so the composer clears its
    /// draft and nothing is sent twice), and an *unknown* outcome — never a refusal
    /// the reader would read as "that did not happen" — when it does not.
    #[test]
    fn a_lost_send_is_not_reported_as_a_refusal() {
        let args = json!({"text": "look at this", "images": [{"data": "…"}], "queue": "now"});
        let error = || swarm_client::Error::Protocol("io: Broken pipe (os error 32)".into());
        let held = json!({"state": {}, "items": [
            {"id": "e_7", "kind": "user", "ts": now_millis(), "text": "look at this",
             "images": [{"name": "pasted image.png"}]}
        ]});

        // The session holds it: the reply is the one the server would have sent.
        let found = {
            let turn = the_turn_in(&held, &args, now_millis() - 60_000).expect("the turn");
            OpReply {
                rid: "r1".into(),
                ok: true,
                seq: 0,
                result: json!({"item_id": turn}),
                error: None,
            }
        };
        assert!(found.ok);
        assert_eq!(found.result["item_id"], json!("e_7"));

        // And the words for the two ends of the failure path: a lost send is not
        // the session refusing anything.
        let nowhere = Client::loopback(1, "t").expect("a client with no server behind it");
        let lost = lost_reply(&nowhere, "r1", "input.send", &args, now_millis(), error());
        assert!(!lost.ok);
        assert_eq!(
            lost.error.as_ref().map(|error| error.code),
            Some(ErrorCode::Unknown)
        );
        let lost = lost.error.as_ref().expect("a reason");
        assert!(
            lost.message.contains("did not answer this send"),
            "{lost:?}"
        );
        assert_eq!(
            lost.detail,
            json!({"outcome": "unknown"}),
            "and marks the outcome as unknown, which is what the UI reads"
        );
    }

    /// A tab whose UI stopped taking updates is the one failure with no other
    /// witness: every send is fire-and-forget, so the count and the one line are
    /// all there is to see from the outside.
    #[test]
    fn updates_nobody_takes_are_counted_and_the_first_is_named() {
        let (sender, receiver) = async_channel::bounded(1);
        let updates = Updates::new(sender);
        // The tab is gone: its receiver went with it.
        drop(receiver);
        assert!(!updates.send(Update::Booting));
        assert!(!updates.send(Update::Snapshot {
            topic: "session".into(),
            body: json!({}),
        }));
        assert_eq!(updates.dropped.load(Ordering::Relaxed), 2);
        assert!(
            updates.said.load(Ordering::Relaxed),
            "said once, and only once"
        );

        // The name is one line's worth, and it is the update's own.
        assert_eq!(what(&Update::Booting), "boot");
        assert_eq!(
            what(&Update::Op {
                topic: "session".into(),
                op: Op::StreamReset {
                    reason: "restarted".into()
                },
            }),
            "frame"
        );
        assert_eq!(what(&Update::ServerGone), "server gone");
    }
}
