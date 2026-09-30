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
//!    is gapless.
//! 4. **ops** — `POST /ops` with an engine-made rid, on a thread of its own so a
//!    slow answer never holds up the stream.
//!
//! There is no second stream, no per-lane bookkeeping, no revision counter and no
//! polling: a restart is an epoch, a stale view is a reset, and a dead server is a
//! stream that cannot reconnect (checked against the process, once).

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

use async_channel::{Receiver, Sender};
use serde_json::Value;

use session::{Op, OpRequest, OpSink};
use swarm_client::{
    BootCancel, Client, ErrorCode, EventStream, OpError, OpReply, Server, ServerConfig,
    ShutdownOutcome, Snapshot, StdinClose, StreamConfig, StreamFrame, StreamMsg,
};

use crate::types::{tab_topics, Update};

/// What the engine thread reads: a command from the UI, or something the stream
/// saw. One channel for both, so the loop never blocks on two things at once.
enum Inbound {
    /// An op the UI asked for (session built it; the engine POSTs it).
    Request(Box<OpRequest>),
    /// Re-read every topic.
    Snapshot,
    /// Stop the server the ladder's way and end the engine.
    Shutdown,
    Stream(StreamMsg),
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

    /// Send one op (`TabModel::send_input`, `interrupt_lane`, …).
    pub fn request(&self, request: OpRequest) -> bool {
        self.inbox
            .send_blocking(Inbound::Request(Box::new(request)))
            .is_ok()
    }

    /// Re-read every topic — what a UI asks for when it dropped updates it could
    /// not keep up with.
    pub fn refetch(&self) -> bool {
        self.inbox.send_blocking(Inbound::Snapshot).is_ok()
    }

    /// Stop the server and end the engine. Returns at once: the child is told by
    /// its stdin closing, and the ladder runs on the engine's thread.
    pub fn shutdown(&mut self) {
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

/// The UI builds an op; the transport POSTs it. This is the transport.
impl OpSink for EngineHandle {
    fn send(&self, request: OpRequest) {
        self.request(request);
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
        updates,
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

    let client = server.client().clone();
    // One snapshot, then one stream from its cursor: there is no window between
    // the two in which an op could be missed.
    let snapshot = match client.snapshot(&engine.topics, None) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            engine.send(Update::BootFailed {
                reason: format!("the server answered the ready file but not a snapshot: {error}"),
                log_tail: server.log_tail(40),
            });
            let outcome = server
                .shutdown()
                .map(|shutdown| shutdown.outcome)
                .unwrap_or(ShutdownOutcome::Killed);
            engine.send(Update::Exited { outcome });
            return;
        }
    };
    let stream = EventStream::start(
        client.clone(),
        StreamConfig::new(engine.topics.clone()).from(snapshot.cursor()),
    );
    let forwarder = forward_stream(stream.clone(), &commands);
    engine.topics_of(&snapshot);

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
            Inbound::Shutdown => return None,
            Inbound::Snapshot => {
                engine.snapshot(client, None);
            }
            Inbound::Request(request) => post(engine, client, *request),
            Inbound::Stream(StreamMsg::Connected { .. }) => {
                engine.send(Update::Stream {
                    status: crate::types::StreamStatus::Connected,
                });
            }
            Inbound::Stream(StreamMsg::Reconnecting { retry_in, .. }) => {
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
                    // That one topic is stale; everything else stands.
                    engine.snapshot(client, Some(topic));
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

/// POST one op, on a thread of its own: a slow or refused op must not hold up the
/// stream, and the rid makes the client's one retry free (§5.5).
fn post(engine: &Engine, client: &Client, request: OpRequest) {
    let client = client.clone();
    let updates = engine.updates.clone();
    let _ = thread::Builder::new()
        .name("evo-tab-op".into())
        .spawn(move || {
            let reply = client
                .op(&request.op, request.args)
                .unwrap_or_else(|error| OpReply {
                    rid: String::new(),
                    ok: false,
                    seq: 0,
                    result: Value::Null,
                    error: Some(OpError {
                        code: ErrorCode::OpFailed,
                        message: error.to_string(),
                        detail: Value::Null,
                    }),
                });
            let _ = updates.send_blocking(Update::OpReply(Box::new(reply)));
        });
}

/// The engine's own state: where updates go, and what to read.
struct Engine {
    updates: Sender<Update>,
    topics: Vec<String>,
}

impl Engine {
    fn send(&mut self, update: Update) -> bool {
        self.updates.send_blocking(update).is_ok()
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
        let snapshot = client.snapshot(&topics, None).ok()?;
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
}
