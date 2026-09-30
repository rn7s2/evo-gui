//! One tab's I/O, off the UI thread.
//!
//! ```text
//!   UI thread ──Command──▶ [ tab-engine thread ] ──snapshot/stream/ops──▶ evo-swarm serve
//!   UI thread ◀──Update─── [ tab-engine thread ] ◀──frames───────────────  (coordinator, lanes)
//! ```
//!
//! The whole engine is one thread and four steps:
//!
//! 1. **spawn** — [`Server::start`], which waits for the ready file, and report
//!    [`Update::BootFailed`] with the log tail when the server never comes up.
//! 2. **snapshot** — one `GET /snapshot` for `session`, `swarm` and `lane:*`,
//!    handed to the UI as it arrived.
//! 3. **stream** — one `GET /stream` from that snapshot's cursor. Every frame
//!    goes to the UI unchanged; a `topic.reset` makes the engine re-snapshot that
//!    topic, and a `stream.reset` makes it re-snapshot everything and resume the
//!    stream from the snapshot's cursor, so the re-read is gapless.
//! 4. **ops** — `POST /ops` with an engine-made rid, on a thread of its own so a
//!    slow answer never holds up the stream.
//!
//! There is no second stream, no per-lane bookkeeping, no revision counter and
//! no polling: a restart is an epoch, a stale view is a reset, and a dead server
//! is a stream that cannot reconnect (checked against the process, once).

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::{Receiver, Sender};
use serde_json::Value;

use swarm_client::{
    Client, ErrorCode, EventStream, OpError, OpReply, Server, ShutdownOutcome, Snapshot,
    StreamConfig, StreamMsg,
};

use crate::types::{tab_topics, Command, StreamStatus, TabSpec, Update};

/// What the engine thread reads: a command from the UI, or something the stream
/// saw. One channel for both, so the loop never blocks on two things at once.
enum Inbound {
    Command(Command),
    Stream(StreamMsg),
}

/// A tab: one server, one stream, one thread.
pub struct TabEngine;

impl TabEngine {
    /// Start a tab. The handle takes commands; the receiver carries updates.
    pub fn start(spec: TabSpec) -> (EngineHandle, Receiver<Update>) {
        let (inbox, mailbox) = async_channel::bounded(1024);
        let (updates, receiver) = async_channel::bounded(4096);
        let folder = spec.folder.clone();
        let commands = inbox.clone();
        let thread = thread::Builder::new()
            .name("evo-tab-engine".into())
            .spawn(move || run(spec, mailbox, commands, updates))
            .expect("the engine thread starts");
        (
            EngineHandle {
                inbox,
                thread: Some(thread),
                folder,
            },
            receiver,
        )
    }
}

/// A tab's handle: commands in, and the thread's end of the story.
pub struct EngineHandle {
    inbox: Sender<Inbound>,
    thread: Option<JoinHandle<()>>,
    folder: PathBuf,
}

impl EngineHandle {
    /// The folder this tab's server runs in.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Send a command; `false` when the engine is already gone.
    pub fn send(&self, command: Command) -> bool {
        self.inbox.send_blocking(Inbound::Command(command)).is_ok()
    }

    /// One op, its arguments.
    pub fn request(&self, op: impl Into<String>, args: Value) -> bool {
        self.send(Command::op(op, args))
    }

    /// Re-read the whole view.
    pub fn refetch(&self) -> bool {
        self.send(Command::Snapshot)
    }

    /// Stop the server the ladder's way and end the engine.
    pub fn shutdown(&mut self) -> bool {
        self.send(Command::Shutdown)
    }

    pub fn is_running(&self) -> bool {
        !self.inbox.is_closed()
    }

    /// Wait for the engine to stop. Stops it first: a handle that is joined is a
    /// handle that no longer wants a server.
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

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        self.shutdown();
        self.wait();
    }
}

/// What a quit did, tab by tab.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShutdownReport {
    pub tabs: usize,
    pub exited: usize,
}

impl ShutdownReport {
    pub fn all_exited(&self) -> bool {
        self.tabs > 0 && self.tabs == self.exited
    }
}

/// Stop every tab at once: ask them all first, then wait. Asking one at a time
/// would serialize the ladders, and a slow one would hold up the rest.
pub fn shutdown_all(mut handles: Vec<EngineHandle>, _deadline: Duration) -> ShutdownReport {
    let tabs = handles.len();
    for handle in handles.iter_mut() {
        handle.shutdown();
    }
    let mut exited = 0;
    for mut handle in handles.drain(..) {
        if handle.thread.take().map(|t| t.join().is_ok()).unwrap_or(false) {
            exited += 1;
        }
    }
    ShutdownReport { tabs, exited }
}

/// The engine thread.
/// `commands` is a second handle on the mailbox: the forwarding thread needs one
/// to put the stream's messages in, and it must not be the handle's (which goes
/// away as soon as the UI drops it).
fn run(
    spec: TabSpec,
    mailbox: Receiver<Inbound>,
    commands: Sender<Inbound>,
    updates: Sender<Update>,
) {
    let mut engine = Engine {
        updates: updates.clone(),
        topics: tab_topics(),
    };
    engine.send(Update::Booting);

    let mut server = match Server::start(&spec.server_config()) {
        Ok(server) => server,
        Err(error) => {
            engine.send(Update::BootFailed {
                message: boot_message(&error),
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

    let client = server.client().clone();
    let snapshot = match client.snapshot(&engine.topics, None) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            engine.send(Update::BootFailed {
                message: format!("the server answered the ready file but not a snapshot: {error}"),
                log_tail: server.log_tail(40),
            });
            let outcome = server.shutdown().map(|s| s.outcome).unwrap_or(ShutdownOutcome::Killed);
            engine.send(Update::Exited { outcome });
            return;
        }
    };
    let stream = EventStream::start(
        client.clone(),
        StreamConfig::new(engine.topics.clone()).from(snapshot.cursor()),
    );
    let forwarder = forward_stream(stream.clone(), &commands);
    engine.send(Update::Snapshot(Box::new(snapshot)));

    let outcome = engine_loop(&mut engine, &mut server, &client, &stream, &mailbox);
    stream.stop();
    drop(stream);
    let _ = forwarder.join();
    let outcome = match outcome {
        Some(outcome) => outcome,
        None => match server.shutdown() {
            Ok(shutdown) => shutdown.outcome,
            Err(_) => ShutdownOutcome::Killed,
        },
    };
    engine.send(Update::Exited { outcome });
}

/// Why a boot failed, in one line, for a tab's caption.
fn boot_message(error: &swarm_client::Error) -> String {
    match error {
        swarm_client::Error::Boot(failure) => failure.message.clone(),
        other => other.to_string(),
    }
}

/// Copy the stream's messages into the engine's mailbox, so the engine thread
/// has one thing to read. Ends when the stream does.
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

/// The loop: commands from the UI, messages from the stream, until one of them
/// says the tab is over.
///
/// `None` means the server is gone or was stopped; `Some(outcome)` means the
/// ladder already ran.
fn engine_loop(
    engine: &mut Engine,
    server: &mut Server,
    client: &Client,
    stream: &EventStream,
    mailbox: &Receiver<Inbound>,
) -> Option<ShutdownOutcome> {
    loop {
        let message = match mailbox.recv_blocking() {
            Ok(message) => message,
            // Every sender is gone: the handle was dropped (which asks for a
            // shutdown first) and the stream has ended.
            Err(_) => return None,
        };
        match message {
            Inbound::Command(Command::Shutdown) => return None,
            Inbound::Command(Command::Snapshot) => {
                engine.snapshot(client, None);
            }
            Inbound::Command(Command::Op { op, args }) => {
                // Off the loop: a slow or refused op must not hold up the
                // stream, and the rid makes a retry free (§5.5).
                let client = client.clone();
                let updates = engine.updates.clone();
                let _ = thread::Builder::new()
                    .name("evo-tab-op".into())
                    .spawn(move || match client.op(&op, args) {
                        Ok(reply) => {
                            let _ = updates.send_blocking(Update::OpReply(Box::new(reply)));
                        }
                        Err(error) => {
                            let _ = updates.send_blocking(Update::OpReply(Box::new(OpReply {
                                rid: String::new(),
                                ok: false,
                                seq: 0,
                                result: Value::Null,
                                error: Some(OpError {
                                    code: ErrorCode::OpFailed,
                                    message: error.to_string(),
                                    detail: Value::Null,
                                }),
                            })));
                        }
                    });
            }
            Inbound::Stream(StreamMsg::Connected { .. }) => {
                engine.send(Update::Stream {
                    status: StreamStatus::Live,
                });
            }
            Inbound::Stream(StreamMsg::Reconnecting { attempt, retry_in }) => {
                // A stream that cannot come back is a server that is not there.
                if !server.is_running() {
                    engine.send(Update::ServerGone);
                    return None;
                }
                engine.send(Update::Stream {
                    status: StreamStatus::Reconnecting { attempt, retry_in },
                });
            }
            Inbound::Stream(StreamMsg::Frame(frame)) => {
                if let Some(reason) = frame.topic_reset() {
                    let topic = frame.topic().unwrap_or_default().to_owned();
                    engine.send(Update::TopicReset {
                        topic: topic.clone(),
                        reason,
                    });
                    // That one topic is stale; everything else stands.
                    engine.snapshot(client, Some(topic));
                } else {
                    engine.send(Update::Frame(Box::new(frame)));
                }
            }
            Inbound::Stream(StreamMsg::Reset { reason }) => {
                engine.send(Update::StreamReset { reason });
                // Snapshot first, then resume from the position that snapshot
                // was atomic at: no op is lost and none is applied twice.
                if let Some(snapshot) = engine.snapshot(client, None) {
                    stream.resume_from(snapshot.cursor());
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

/// The engine's own state: where to send updates, and what to read.
struct Engine {
    updates: Sender<Update>,
    topics: Vec<String>,
}

impl Engine {
    fn send(&mut self, update: Update) -> bool {
        self.updates.send_blocking(update).is_ok()
    }

    /// Take a snapshot of everything, or of one topic after a `topic.reset`.
    ///
    /// A failed snapshot is not fatal: the stream keeps running, and the next
    /// reset asks again. It stays silent rather than inventing an update.
    fn snapshot(&mut self, client: &Client, topic: Option<String>) -> Option<Snapshot> {
        let topics = match &topic {
            Some(topic) => vec![topic.clone()],
            None => self.topics.clone(),
        };
        let snapshot = client.snapshot(&topics, None).ok()?;
        self.send(Update::Snapshot(Box::new(snapshot.clone())));
        Some(snapshot)
    }
}
