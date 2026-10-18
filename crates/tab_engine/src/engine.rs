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

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use serde_json::{json, Value};

use session::{Op, OpRequest, OpSink};
// The protocol half of the client: the same names on both targets, because
// `swarm_client` selects its transport by target and the typed methods match.
use swarm_client::{Client, Cursor, ErrorCode, OpError, OpReply, Snapshot, StreamFrame};

// The stream half, named the same on both targets so nothing below has to care:
// a host reads on a thread of its own (`stream::EventStream`), a browser on the
// page's event loop (`fetch::WasmEventStream`), and `StreamConfig`/`StreamMsg`
// are the same two types either way — the wasm build spells them under `fetch`
// because that is the module that has them.
#[cfg(target_family = "wasm")]
use swarm_client::fetch::{StreamConfig, StreamMsg, WasmEventStream as EventStream};
#[cfg(not(target_family = "wasm"))]
use swarm_client::{EventStream, StreamConfig, StreamMsg};

// The launch half. `Server` is a process, `StdinClose` a pipe, `BootCancel` a boot
// in progress, `ShutdownOutcome` how a process went: a browser has none of them,
// and none of them is reachable from the wasm entry point.
#[cfg(not(target_family = "wasm"))]
use std::path::Path;
#[cfg(not(target_family = "wasm"))]
use std::thread::{self, JoinHandle};
#[cfg(not(target_family = "wasm"))]
use swarm_client::{BootCancel, Server, ServerConfig, ShutdownOutcome, StdinClose};

/// What [`engine_loop`] hands back when it ends by itself: how the server went.
///
/// A host has a process and can say; a browser has none — the process belongs to
/// the server that served the page — so there is nothing to report and the stream
/// ending is the whole story.
#[cfg(not(target_family = "wasm"))]
type LoopOutcome = ShutdownOutcome;
#[cfg(target_family = "wasm")]
type LoopOutcome = ();

/// A borrowed liveness check, named so [`engine_loop`] can take one on both
/// targets: a host's [`Liveness`] borrows the process it asks, and a browser's
/// borrows nothing, so the same parameter cannot be spelled the same way twice.
#[cfg(not(target_family = "wasm"))]
type LivenessRef<'server, 'process> = &'server mut Liveness<'process>;
#[cfg(target_family = "wasm")]
type LivenessRef<'server, 'process> = &'server mut Liveness;

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

/// A client call, awaited where awaiting is a thing.
///
/// A host's `Client` blocks: `client.snapshot(…)` *is* the answer, and `.await`
/// on it would not compile. A browser's is `fetch`-backed, so the same call is a
/// future that has to be awaited. Everything below is otherwise identical on the
/// two targets — the same arms, the same retries, the same order — so the
/// difference is spelled once, here, rather than split into two loops that would
/// drift apart. What it costs is one word at each call: `awaited!(…)`.
#[cfg(not(target_family = "wasm"))]
macro_rules! awaited {
    ($call:expr) => {
        $call
    };
}
#[cfg(target_family = "wasm")]
macro_rules! awaited {
    ($call:expr) => {
        $call.await
    };
}

/// Run one future to completion on this thread.
///
/// A host has no executor and needs none: the engine's future is driven by the
/// thread that used to run the loop synchronously, and the only thing it ever
/// waits on is `async-channel`, whose waker unparks this thread. So the whole of
/// the executor is a park/unpark pair — no runtime invented, and no dependency
/// added to get one.
///
/// A browser has an executor (the page's) and cannot block on it, so this does
/// not exist there; [`TabEngine::connect`] hands the same future to
/// `spawn_local`.
#[cfg(not(target_family = "wasm"))]
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            // `unpark` sets a token, so a wake that lands between the poll and
            // the park is not lost: the park returns at once.
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Wait `total` without holding anything.
///
/// A browser has no thread to sleep and no blocking sleep at all, so it hands the
/// page back and asks to be woken — which is the only kind of wait a page can
/// make. A host does not need this: its retries are threads ([`ask_again`]).
#[cfg(target_family = "wasm")]
async fn sleep_for(total: Duration) {
    use wasm_bindgen::JsCast as _;

    let millis = total.as_millis().min(i32::MAX as u128) as i32;
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                resolve.unchecked_ref(),
                millis,
            );
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

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
    /// Re-read a snapshot (§5.2): every topic, or the one `topic` names. The UI
    /// asks for the whole thing when it dropped updates it could not keep up with,
    /// and for one topic when it wants that topic read back as it stands now.
    Snapshot {
        /// The one topic to read, `None` for every topic the tab subscribes to.
        topic: Option<String>,
    },
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

/// Hand one message to a channel, on a target that has no blocking send.
///
/// On a host this is `async_channel`'s own `send_blocking` — it waits for room,
/// and there is no reason for this crate to spell that any other way. On wasm
/// `send_blocking` does not exist (a browser has no thread to park), so the
/// offer is made in a bounded loop instead: every channel here is generously
/// bounded (1024 in, 4096 out) and every message is small, so a full channel
/// means the reader is behind rather than gone. Giving up after a moment is what
/// keeps a wedged reader from hanging the engine, and `Err` is the message back,
/// exactly as `send_blocking`'s error carries it.
#[cfg(not(target_family = "wasm"))]
fn offer<T>(sender: &Sender<T>, message: T) -> Result<(), T> {
    sender
        .send_blocking(message)
        .map_err(|error| error.into_inner())
}

/// [`offer`], without a blocking send to lean on.
#[cfg(target_family = "wasm")]
fn offer<T>(sender: &Sender<T>, message: T) -> Result<(), T> {
    /// How many times the loop below looks for room before it gives up.
    const TRIES: u32 = 32;
    let mut message = message;
    for _ in 0..TRIES {
        match sender.try_send(message) {
            Ok(()) => return Ok(()),
            Err(async_channel::TrySendError::Full(back)) => {
                message = back;
                // TODO(wasm): a browser has no sleep that does not hand the
                // thread back, and this is a synchronous function on the caller's
                // stack — so there is no pause here. A full channel is a reader
                // that is behind, and the loop gives up rather than spinning on
                // it. The real answer is for `engine_loop` to be `async` and
                // `await` the send; that restructures the loop (see
                // `run_connected`) and is not this change.
            }
            Err(async_channel::TrySendError::Closed(back)) => return Err(back),
        }
    }
    Err(message)
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
    /// `GET /catalog` — the session's own registrations, as they stand now.
    Catalog,
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
            Fetch::Catalog => "the catalog".to_owned(),
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
    ///
    /// One body for both targets: a host's client has answered by the time the
    /// call returns and a browser's is a fetch, and [`awaited!`] is the one word
    /// that difference costs. The requests are the same either way.
    async fn run(self, client: &Client) -> Result<Update, swarm_client::Error> {
        match self {
            Fetch::Page {
                topic,
                before,
                limit,
            } => {
                let body = awaited!(client.items(&topic, before.as_deref(), limit))?;
                Ok(Update::ItemsBefore { topic, body })
            }
            Fetch::Item { topic, id } => {
                let body = awaited!(client.item(&topic, &id))?;
                Ok(Update::Item { topic, body })
            }
            Fetch::Media { topic, id, n } => {
                let (bytes, content_type) = awaited!(client.media(&topic, &id, n))?;
                Ok(Update::Media {
                    topic,
                    id,
                    n,
                    content_type,
                    bytes,
                })
            }
            Fetch::Catalog => Ok(Update::Catalog {
                body: awaited!(client.catalog()).ok(),
            }),
        }
    }
}

/// A tab: one server, one stream, one thread.
pub struct TabEngine;

#[cfg(not(target_family = "wasm"))]
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
                stdin: Some(stdin),
                cancel: Some(cancel),
                thread: Some(thread),
                folder,
            },
            receiver,
        )
    }
}

#[cfg(target_family = "wasm")]
impl TabEngine {
    /// Join a tab whose server is already running somewhere else.
    ///
    /// A browser cannot spawn `evo-swarm`: the `evo-web` server it was served from
    /// owns the process, and the page reaches it through that server's same-origin
    /// prefix ([`Client::at_base`]). So there is no argv, no ready file and no
    /// child — the only thing missing from a running tab is a connection, and this
    /// is one: the same [`Live::connect`], the same [`engine_loop`], the same
    /// [`Update`]s the native side sends, so a tab is the same tab either way.
    ///
    /// What is not here is the launch: no `Update::Ready` (nothing to announce —
    /// the server was ready before this page loaded) and no `Update::Exited`
    /// (there is no process to have exited; the stream ending is the whole story).
    ///
    /// Returns at once: the loop is spawned on the page's executor and runs until
    /// the tab is over, so a caller builds the tab and draws while the first
    /// snapshot is still in flight — which is what `Update::Booting` is for.
    pub fn connect(client: Client, folder: PathBuf) -> (EngineHandle, Receiver<Update>) {
        let (inbox, mailbox) = async_channel::bounded(1024);
        let (updates, receiver) = async_channel::bounded(4096);
        let commands = inbox.clone();
        wasm_bindgen_futures::spawn_local(run_connected(
            client, folder, mailbox, commands, updates,
        ));
        (EngineHandle { inbox }, receiver)
    }
}

/// A tab's handle: ops in, and the engine's own end of the story.
pub struct EngineHandle {
    inbox: Sender<Inbound>,
    /// The child's stdin: closing it is the server's own signal to stop, and it
    /// needs neither the engine thread nor a ladder. `None` when this engine has
    /// no child — a tab joined over a connection, where stopping is the stream's.
    #[cfg(not(target_family = "wasm"))]
    stdin: Option<StdinClose>,
    /// Raised when the tab is dropped during a boot that has not finished.
    /// Native only, with the boot it aborts.
    #[cfg(not(target_family = "wasm"))]
    cancel: Option<BootCancel>,
    /// The engine's own thread. `None` when the engine runs on the caller's
    /// thread, which is what a browser does: there is no thread to hold.
    #[cfg(not(target_family = "wasm"))]
    thread: Option<JoinHandle<()>>,
    /// The folder the tab's server runs in, from the launch that named it.
    /// Native only: a joined tab is told its folder by whoever opened it, and
    /// carries it in the session's own snapshot.
    #[cfg(not(target_family = "wasm"))]
    folder: PathBuf,
}

impl EngineHandle {
    /// The folder this tab's server runs in.
    ///
    /// Known on the native side, where the launch named it. A joined tab is told
    /// the folder by whoever opened it, and carries it in the session's own
    /// snapshot; this handle has none to report.
    #[cfg(not(target_family = "wasm"))]
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
        offer(&self.inbox, Inbound::Snapshot { topic: None }).is_ok()
    }

    /// Re-read one topic — what a UI asks for when it wants the transcript it
    /// shows read back as it stands now, without disturbing any other topic.
    ///
    /// The answer arrives as [`Update::Snapshot`] with that topic; one that does not
    /// carry it is a failed read, retried on the tab's own clock.
    pub fn refetch_topic(&self, topic: &str) -> bool {
        self.send(Inbound::Snapshot {
            topic: Some(topic.to_owned()),
        })
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

    /// `GET /catalog`: what this session has registered now — after a `/reload`, the
    /// models the saved config file added. The answer arrives as [`Update::Catalog`].
    pub fn catalog(&self) -> bool {
        self.send(Inbound::Fetch(Fetch::Catalog))
    }

    /// Hand the engine one inbound message.
    fn send(&self, message: Inbound) -> bool {
        offer(&self.inbox, message).is_ok()
    }

    /// Stop the server and end the engine. Returns at once: the child is told by
    /// its stdin closing, and the ladder runs on the engine's thread.
    ///
    /// A tab with no child — the browser's — has no server of its own to stop:
    /// dropping the handle ends the stream, and the `evo-web` server that owns the
    /// process is the one that stops it.
    pub fn shutdown(&self) {
        #[cfg(not(target_family = "wasm"))]
        {
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            if let Some(stdin) = &self.stdin {
                stdin.close();
            }
        }
        let _ = offer(&self.inbox, Inbound::Shutdown);
    }

    pub fn is_running(&self) -> bool {
        !self.inbox.is_closed()
    }

    /// Stop the server and wait for the engine thread to end — for a test, or a
    /// caller that wants to be sure nothing is left.
    #[cfg(not(target_family = "wasm"))]
    pub fn join(mut self) {
        self.shutdown();
        self.wait();
    }

    /// Wait for the engine thread to end, without asking it to.
    #[cfg(not(target_family = "wasm"))]
    fn wait(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Hand the thread over to nobody: it ends when its server does.
    #[cfg(not(target_family = "wasm"))]
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

/// The engine thread: the launch, then the loop.
///
/// The loop is `async` (see [`engine_loop`]) because that is what a browser needs,
/// and a host runs the very same future here — [`block_on`] drives it on this
/// thread, parking whenever the loop has nothing to do. So there is one loop, and
/// this function is the launch in front of it rather than a second copy of it.
#[cfg(not(target_family = "wasm"))]
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
        fences: BTreeMap::new(),
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

    let Some((mut live, snapshot)) = block_on(Live::connect(
        server.client(),
        &engine.topics,
        &commands,
    )) else {
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

    let mut liveness = Liveness::of(&mut server);
    let outcome = block_on(engine_loop(
        &mut engine,
        &mut liveness,
        &mut live,
        &mailbox,
        &commands,
    ));
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

/// The engine, on a tab whose server somebody else is running: the browser's
/// entry point, and the same loop as [`run`] with the launch taken out.
///
/// A browser has no thread, so this is a task on the page's executor rather than a
/// function that returns: `connect` spawns it and hands back the handle and the
/// receiver at once, and every step inside is awaited — the snapshot, the stream's
/// messages, each fetch and each op. The page is handed back at each of them, so a
/// slow read never freezes the tab.
#[cfg(target_family = "wasm")]
async fn run_connected(
    client: Client,
    _folder: PathBuf,
    mailbox: Receiver<Inbound>,
    commands: Sender<Inbound>,
    updates: Sender<Update>,
) {
    let mut engine = Engine {
        updates: Updates::new(updates),
        topics: tab_topics(),
        fences: BTreeMap::new(),
    };
    engine.send(Update::Booting);

    let Some((mut live, snapshot)) = Live::connect(&client, &engine.topics, &commands).await else {
        engine.send(Update::BootFailed {
            reason: "the tab's server did not answer a snapshot".into(),
            log_tail: String::new(),
        });
        return;
    };

    // The ready file the native path reads off its own child, reconstructed from
    // the snapshot: a browser attaches to a server that was ready before the page
    // loaded, so there is no file to wait on and nothing announced it.
    //
    // This is not cosmetic. The window only leaves the empty tab on this update
    // (`crates/workspace/src/tab.rs`'s `apply`, `Update::Ready` →
    // `TabState::Running`), so a browser that never sent it drew the empty tab
    // over a session that was really running — the folder field still on screen,
    // the transcript and composer never shown. Everything below is the answer to
    // "what would that file have said": the epoch and the cursor are the
    // snapshot's, and the session is the one the `session` topic names.
    engine.send(Update::Ready {
        epoch: snapshot.epoch.clone(),
        // No process of the page's to point at, and no port of its own: the
        // server is on the machine that served the page, reached through the
        // proxy. Zero is the honest answer, and nothing on this path draws them.
        pid: 0,
        port: 0,
        session: session_ref_of(&snapshot).unwrap_or(swarm_client::SessionRef {
            // A snapshot with no `session` topic is a server that answered
            // something unexpected; the tab then records no session, which is
            // what an empty id and path mean here.
            id: String::new(),
            path: String::new(),
        }),
    });

    engine.topics_of(&snapshot);

    engine_loop(&mut engine, &mut Liveness, &mut live, &mailbox, &commands).await;
    live.stop();
}

/// The session a snapshot names, from the `session` topic's own state.
///
/// The topic carries it (`crates/session/src/state.rs`: `session.id` and
/// `session.path`), which is what a tab needs to record the session it is on
/// (§2, §9.5) — the one thing `ReadyFile.session` is read for.
fn session_ref_of(snapshot: &swarm_client::Snapshot) -> Option<swarm_client::SessionRef> {
    let session = snapshot.topic("session")?.get("state")?.get("session")?;
    Some(swarm_client::SessionRef {
        id: session.get("id")?.as_str()?.to_owned(),
        path: session.get("path")?.as_str()?.to_owned(),
    })
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
    /// The thread that forwards what the stream sees. `None` on wasm, where the
    /// stream is read on the page's own task and there is no thread to hold.
    #[cfg(not(target_family = "wasm"))]
    forwarder: Option<JoinHandle<()>>,
}

impl Live {
    /// Snapshot, then one stream from that snapshot's cursor: there is no window
    /// between the two in which an op could be missed.
    ///
    /// The stream's own messages reach the mailbox the same way on both targets
    /// now: a task of its own, driven by the host's thread pool
    /// ([`forward_stream`]) or by the page's executor ([`forward_stream_on_page`]).
    /// Either way this loop does not read the stream directly, so it is never
    /// blocked by it — which is what the two forwarding tasks exist for.
    async fn connect(
        client: &Client,
        topics: &[String],
        commands: &Sender<Inbound>,
    ) -> Option<(Live, Snapshot)> {
        let client = client.clone();
        // A whole render window's worth, not the server's smaller default (§5.2): the
        // tab's live transcript holds `PAGE_ITEMS` items before anything is streamed.
        let snapshot = awaited!(client.snapshot(topics, Some(session::PAGE_ITEMS))).ok()?;
        let stream = EventStream::start(
            client.clone(),
            StreamConfig::new(topics.to_vec()).from(snapshot.cursor()),
        );
        #[cfg(not(target_family = "wasm"))]
        let forwarder = Some(forward_stream(stream.clone(), commands));
        #[cfg(target_family = "wasm")]
        forward_stream_on_page(stream.clone(), commands.clone());
        Some((
            Live {
                client,
                stream,
                #[cfg(not(target_family = "wasm"))]
                forwarder,
            },
            snapshot,
        ))
    }

    /// Stop reading, and wait for the forwarding thread to end.
    fn stop(&mut self) {
        self.stream.stop();
        #[cfg(not(target_family = "wasm"))]
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
    }
}

/// Whether the server behind a tab is still there.
///
/// A process the engine spawned is asked directly ([`Server::is_running`]). A tab
/// joined over a connection has no process here to ask — the server that owns it
/// is on the machine the page was served from — so the honest answer is that this
/// client cannot see it, and the stream's own end is what says so.
#[cfg(not(target_family = "wasm"))]
enum Liveness<'a> {
    Spawned(&'a mut Server),
}

#[cfg(not(target_family = "wasm"))]
impl<'a> Liveness<'a> {
    fn of(server: &'a mut Server) -> Liveness<'a> {
        Liveness::Spawned(server)
    }

    fn is_running(&mut self) -> bool {
        match self {
            Liveness::Spawned(server) => server.is_running(),
        }
    }
}

#[cfg(target_family = "wasm")]
struct Liveness;

#[cfg(target_family = "wasm")]
impl Liveness {
    /// The server behind a joined tab is not this process's to ask. It is on the
    /// machine that served the page, and this client cannot see it; the stream
    /// ending is what says it is gone.
    fn is_running(&mut self) -> bool {
        true
    }
}

/// Why a boot failed, in one line, for a tab's caption.
///
/// Native only: a boot is a spawn, and a browser does not spawn anything.
#[cfg(not(target_family = "wasm"))]
fn boot_message(error: &swarm_client::Error) -> String {
    match error {
        swarm_client::Error::Boot(failure) => failure.reason.clone(),
        other => other.to_string(),
    }
}

/// Copy the stream's messages into the engine's mailbox, so the engine has one
/// thing to read. Ends when the stream does.
#[cfg(not(target_family = "wasm"))]
fn forward_stream(stream: EventStream, mailbox: &Sender<Inbound>) -> JoinHandle<()> {
    let mailbox = mailbox.clone();
    thread::Builder::new()
        .name("evo-tab-forward".into())
        .spawn(move || {
            while let Some(message) = stream.recv() {
                if offer(&mailbox, Inbound::Stream(message)).is_err() {
                    break;
                }
            }
        })
        .expect("the forwarding thread starts")
}

/// The same forwarding, as a task on the page's executor.
///
/// A browser has no thread to read the stream on, and the stream's own reader is
/// already a task (`WasmEventStream::start`), so this is a second task that moves
/// what that one produces into the engine's mailbox — the same job
/// [`forward_stream`] does with a thread. Nothing here blocks: `recv` is awaited
/// and `send` is awaited, so the page is handed back at every step.
#[cfg(target_family = "wasm")]
fn forward_stream_on_page(stream: EventStream, mailbox: Sender<Inbound>) {
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(message) = stream.recv().await {
            if mailbox.send(Inbound::Stream(message)).await.is_err() {
                break;
            }
        }
    });
}

/// The loop: ops from the UI, frames from the stream, until one of them says the
/// tab is over.
///
/// `None` means the server is gone or was stopped; `Some(outcome)` means the
/// ladder already ran.
///
/// `async` on both targets, and that is the point of the whole change: a browser
/// cannot block, so the wait for the next message and every `fetch` inside an arm
/// have to be awaited. A host drives this same future on its own thread
/// ([`block_on`]), so what the two targets share is not merely the logic but the
/// one function — the arms below are identical either way.
async fn engine_loop(
    engine: &mut Engine,
    server: LivenessRef<'_, '_>,
    live: &mut Live,
    mailbox: &Receiver<Inbound>,
    commands: &Sender<Inbound>,
) -> Option<LoopOutcome> {
    loop {
        // The one wait in the loop. On a host `recv` parks the engine thread
        // until the channel's waker unparks it; in a browser it hands the page
        // back and the same message arrives as a task wake-up.
        let message = match mailbox.recv().await {
            Ok(message) => message,
            // Every sender is gone: the handle was dropped (which asks for a
            // shutdown first) and the stream has ended.
            Err(_) => return None,
        };
        match message {
            Inbound::Shutdown => return None,
            Inbound::Snapshot { topic } => {
                // An explicit topic read the server does not answer for is retried
                // like a reset's; the whole-tab read is a point in time, not one.
                if engine.snapshot(&live.client, topic.clone()).await.is_none() {
                    if let Some(topic) = topic {
                        ask_again(engine, commands, Some(topic), RESET_RETRY_FIRST).await;
                    }
                }
            }
            Inbound::Request { rid, request } => {
                post(engine, &live.client, rid, *request).await;
            }
            Inbound::Fetch(fetch) => {
                fetch_off_loop(engine, &live.client, fetch).await;
            }
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
                    if engine
                        .snapshot(&live.client, Some(topic.clone()))
                        .await
                        .is_none()
                    {
                        ask_again(engine, commands, Some(topic), RESET_RETRY_FIRST).await;
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
                match engine.snapshot(&live.client, None).await {
                    Some(snapshot) => live.stream.resume_from(snapshot.cursor()),
                    None => {
                        ask_again(engine, commands, None, RESET_RETRY_FIRST).await;
                    }
                }
            }
            Inbound::Retry { topic, waited } => {
                // The wait is over: the snapshot that failed is asked for again.
                // Only success ends this — a tab catching up is worth telling the
                // reader about (`Reconnecting`), and a process that is gone is the
                // one way out.
                match engine.snapshot(&live.client, topic.clone()).await {
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
                        ask_again(engine, commands, topic, next).await;
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
/// The retry is a timer, not a wait here: the engine still takes ops and frames
/// while a tab is behind, which is the only reason a reset can be retried without
/// freezing the very tab it is trying to bring back.
///
/// Awaited rather than handed to a thread, on both targets: the caller is the loop
/// task, and a page has no thread to hand it to. The timer is the platform's own —
/// a parked engine thread on a host ([`block_on`] unparks when the channel's waker
/// fires), the page's timer in a browser ([`sleep_for`]) — so what this costs is
/// that the loop is *idle* for `next`, not that it is blocked.
async fn ask_again(
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
    #[cfg(not(target_family = "wasm"))]
    thread::sleep(next);
    #[cfg(target_family = "wasm")]
    sleep_for(next).await;
    // A dropped send means the handle is gone, which the loop finds out for itself
    // on its next receive: nothing to do about it here.
    let _ = commands
        .send(Inbound::Retry {
            topic,
            waited: next,
        })
        .await;
}

/// One read, off the loop: a page of items, an item, or an image can be large, and
/// none of them may hold up the stream. A failure is an update, not a panic and not
/// a retry.
///
/// Off the loop on both targets, by the means each has: a host's client blocks, so
/// the read runs on a thread of its own — exactly as it did before this was
/// `async`, which is what keeps a 36-second image from delaying a frame — and a
/// browser's is a fetch, so awaiting it *is* being off the loop. The read itself is
/// [`read_once`], shared.
async fn fetch_off_loop(engine: &Engine, client: &Client, fetch: Fetch) {
    #[cfg(not(target_family = "wasm"))]
    {
        let client = client.clone();
        let updates = engine.updates.clone();
        let _ = thread::Builder::new()
            .name("evo-tab-fetch".into())
            .spawn(move || {
                updates.send(block_on(read_once(&client, fetch)));
            });
    }
    #[cfg(target_family = "wasm")]
    {
        engine.updates.send(read_once(client, fetch).await);
    }
}

/// One read, answered: the update it becomes, failure included.
async fn read_once(client: &Client, fetch: Fetch) -> Update {
    let what = fetch.what();
    let page = fetch.page_topic();
    match fetch.run(client).await {
        Ok(update) => update,
        Err(error) => Update::FetchFailed {
            what,
            reason: error.to_string(),
            page,
        },
    }
}

/// POST one op. A slow or refused op must not hold up the stream, and the rid makes
/// the client's one retry free (§5.5).
///
/// Off the loop on both targets, as [`fetch_off_loop`] is: a host keeps the thread
/// it always had, and a browser awaits the fetch. [`op_reply`] is the shared half.
async fn post(engine: &Engine, client: &Client, rid: String, request: OpRequest) {
    #[cfg(not(target_family = "wasm"))]
    {
        let client = client.clone();
        let updates = engine.updates.clone();
        let op = request.op.clone();
        let _ = thread::Builder::new()
            .name("evo-tab-op".into())
            .spawn(move || {
                let reply = block_on(op_reply(&client, &rid, &request));
                updates.send(Update::OpReply {
                    rid,
                    op,
                    reply: Box::new(reply),
                });
            });
    }
    #[cfg(target_family = "wasm")]
    {
        let op = request.op.clone();
        let reply = op_reply(client, &rid, &request).await;
        engine.updates.send(Update::OpReply {
            rid,
            op,
            reply: Box::new(reply),
        });
    }
}

/// The request itself, answered — the half both targets share.
///
/// Sent under the rid the handle minted, so the reply the UI gets is the one its
/// request is waiting for.
async fn op_reply(client: &Client, rid: &str, request: &OpRequest) -> OpReply {
    let op = request.op.clone();
    let args = request.args.clone();
    let sent_at_ms = now_millis();
    match awaited!(client.op_with_rid(rid, &op, args.clone())) {
        Ok(reply) => reply,
        Err(error) => lost_reply(client, rid, &op, &args, sent_at_ms, error).await,
    }
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
async fn lost_reply(
    client: &Client,
    rid: &str,
    op: &str,
    args: &Value,
    sent_at_ms: u64,
    error: swarm_client::Error,
) -> OpReply {
    if op == "input.send" {
        if let Some(item_id) = the_turn_in_the_session(client, args, sent_at_ms).await {
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
async fn the_turn_in_the_session(client: &Client, args: &Value, sent_at_ms: u64) -> Option<String> {
    let snapshot = awaited!(client.snapshot(&["session".to_string()], Some(SESSION_PAGE))).ok()?;
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
///
/// Split by target for the same reason `workspace::host::now_millis` is: `std::time`
/// is unimplemented on `wasm32-unknown-unknown`, where `SystemTime::now()`
/// **panics** rather than returning an error (`time not implemented on this
/// platform`). This is reached from [`op_reply`], so leaving it ungated made every
/// op posted from a browser panic the tab's engine task. A page has a clock of its
/// own — `Date.now()`, already milliseconds since the epoch, which is this
/// function's own unit — so the reading is taken per target and callers see the
/// same number either way.
#[cfg(not(target_family = "wasm"))]
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// [`now_millis`] from the page's clock.
#[cfg(target_family = "wasm")]
fn now_millis() -> u64 {
    js_sys::Date::now() as u64
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
        let update = match offer(&self.updates, update) {
            Ok(()) => return true,
            Err(update) => update,
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
        Update::Catalog { .. } => "catalog",
        Update::Media { .. } => "image",
        Update::FetchFailed { .. } => "failed read",
        Update::ServerGone => "server gone",
        #[cfg(not(target_family = "wasm"))]
        #[cfg(not(target_family = "wasm"))]
        Update::Exited { .. } => "exit",
    }
}

/// The engine's own state: where updates go, what to read, and what the latest
/// snapshot of each topic covered.
struct Engine {
    updates: Updates,
    topics: Vec<String>,
    fences: BTreeMap<String, SnapshotFence>,
}

struct SnapshotFence {
    cursor: Cursor,
    items: HashSet<String>,
}

impl Engine {
    fn send(&mut self, update: Update) -> bool {
        self.updates.send(update)
    }

    /// Hand a snapshot's topics over, one update each: the UI applies them topic by
    /// topic (`TabModel::on_snapshot`), and each is fenced at the read's cursor.
    fn topics_of(&mut self, snapshot: &Snapshot) {
        let cursor = snapshot.cursor();
        for topic in snapshot.topic_names() {
            if let Some(body) = snapshot.topic(topic) {
                let body = body.clone();
                let items = body["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned))
                    .collect();
                self.fences.insert(
                    topic.to_owned(),
                    SnapshotFence {
                        cursor: cursor.clone(),
                        items,
                    },
                );
                self.send(Update::Snapshot {
                    topic: topic.to_owned(),
                    body,
                });
            }
        }
    }

    /// One frame, parsed: the UI's `TabModel::on_op`. A frame this build does not
    /// know, or one the topic's own read already carries, is dropped.
    fn frame(&mut self, frame: StreamFrame) {
        let topic = frame.topic().unwrap_or_default().to_owned();
        if let Some(op) = Op::from_json(&frame.op, &frame.data) {
            if !self.covered(&topic, frame.cursor.as_ref(), &op) {
                self.send(Update::Op { topic, op });
            }
        }
    }

    /// Only the read's epoch is ordered against it. A bounded read carries whole
    /// state, but not mutations of older items the model may have paged in.
    fn covered(&self, topic: &str, cursor: Option<&Cursor>, op: &Op) -> bool {
        let (Some(fence), Some(cursor)) = (self.fences.get(topic), cursor) else {
            return false;
        };
        if fence.cursor.epoch != cursor.epoch || cursor.seq > fence.cursor.seq {
            return false;
        }
        match op {
            Op::ItemAppend { id, .. } | Op::ItemPatch { id, .. } | Op::ItemRemove { id } => {
                fence.items.contains(id)
            }
            // Adds at this cursor already exist in the read or its older pages.
            Op::ItemAdd { .. } | Op::StatePatch { .. } => true,
            _ => false,
        }
    }

    /// Take a snapshot of everything, or of one topic after a `topic.reset` (or for
    /// a UI's explicit refresh of it).
    ///
    /// A snapshot that fails — including one whose body does not carry the topic
    /// asked for, which is what a server that does not have the name answers — is
    /// `None`, and the retry reads it again. The stream keeps running either way.
    ///
    /// The read, awaited where awaiting is a thing — the one body both targets
    /// share, because the work (read, fence, hand the topics over) is the same
    /// and only the call is spelled differently.
    async fn snapshot(&mut self, client: &Client, topic: Option<String>) -> Option<Snapshot> {
        let topics = match &topic {
            Some(topic) => vec![topic.clone()],
            None => self.topics.clone(),
        };
        let snapshot =
            awaited!(client.snapshot(&topics, Some(session::PAGE_ITEMS))).ok()?;
        if let Some(wanted) = &topic {
            snapshot.topic(wanted)?;
        }
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
        // the session refusing anything. `lost_reply` is `async` now — it awaits
        // the read that looks for the turn — so it is driven here the way the
        // engine drives it.
        let nowhere = Client::loopback(1, "t").expect("a client with no server behind it");
        let lost = block_on(lost_reply(
            &nowhere,
            "r1",
            "input.send",
            &args,
            now_millis(),
            error(),
        ));
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
