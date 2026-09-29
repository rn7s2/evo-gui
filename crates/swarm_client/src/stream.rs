//! The live event stream (§5): a resumable SSE reader on its own thread, feeding
//! an `async-channel` the UI can await without blocking on the socket.
//!
//! One handle owns one connection at a time — never two streams to the same
//! server or lane. It reconnects with `Last-Event-ID` and exponential backoff,
//! and it says when the server it is talking to has been restarted.

use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use serde_json::Value;

use crate::api::Client;
use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::sse::SseParser;

/// Whether a reconnect can learn that the server was restarted.
///
/// `/health.cursor` is the coordinator's own log cursor, so it is the right
/// probe for `/events` — and the wrong one for a lane's stream, whose ids belong
/// to the lane's log, not the coordinator's. A lane that was restarted is seen
/// through the coordinator (`lane-state`, `restarts`) instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorProbe {
    Health,
    None,
}

/// One SSE stream: which server, which route, from where.
#[derive(Clone, Debug)]
pub struct StreamTarget {
    pub http: HttpClient,
    /// `/events` (the coordinator) or `/lanes/N/events`.
    pub path: String,
    /// The first cursor: `None` tails from now, `Some(0)` replays what the log
    /// still holds, `Some(n)` starts right after event n.
    pub since: Option<i64>,
    pub probe: CursorProbe,
}

impl StreamTarget {
    /// The coordinator's own event stream — every kernel, serve and swarm event.
    pub fn coordinator(client: &Client, since: Option<i64>) -> StreamTarget {
        StreamTarget {
            http: client.http().clone(),
            path: "/events".to_owned(),
            since,
            probe: CursorProbe::Health,
        }
    }

    /// One lane's event stream, relayed read-only by the coordinator (§D21).
    pub fn lane(client: &Client, n: u32, since: Option<i64>) -> StreamTarget {
        StreamTarget {
            http: client.http().clone(),
            path: format!("/lanes/{n}/events"),
            since,
            probe: CursorProbe::None,
        }
    }
}

/// How patiently a stream reconnects.
#[derive(Clone, Debug)]
pub struct StreamConfig {
    /// The first wait after a drop (§5: 0.5 s).
    pub initial_backoff: Duration,
    /// The longest wait (§5: 10 s).
    pub max_backoff: Duration,
    /// How many messages may wait unread before the reader blocks (backpressure).
    pub capacity: usize,
    /// Whether to reconnect at all; `false` ends the stream at the first drop.
    pub reconnect: bool,
}

impl Default for StreamConfig {
    fn default() -> StreamConfig {
        StreamConfig {
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            capacity: 1024,
            reconnect: true,
        }
    }
}

/// Why the stream says the session was reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    /// The server announced `hello`: a new process, its ids start again at 1.
    Hello,
    /// An id went backwards: same thing, seen from the numbers.
    IdRegression,
}

/// What a consumer receives. Consume until [`StreamMsg::Ended`], or until the
/// channel closes.
#[derive(Clone, Debug)]
pub enum StreamMsg {
    /// A connection was established (again).
    Connected {
        /// The cursor it resumed from.
        cursor: Option<i64>,
    },
    /// The connection dropped; another attempt follows after `retry_in`.
    Disconnected {
        error: String,
        retry_in: Duration,
    },
    /// One event: `id` is the resume cursor, `kind` the event's type, `data` its
    /// JSON payload.
    Event { id: Option<i64>, kind: String, data: Value },
    /// The session behind the stream restarted: refetch state and transcript
    /// (§3, §9.1).
    Reset { reason: ResetReason },
    /// The stream is over; nothing more will arrive.
    Ended,
}

/// A running stream. Dropping it stops the thread.
#[derive(Debug)]
pub struct EventStream {
    rx: Receiver<StreamMsg>,
    stop_flag: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
    path: String,
}

impl EventStream {
    /// Open a stream. The thread starts immediately and reconnects on its own.
    pub fn start(target: StreamTarget, cfg: StreamConfig) -> EventStream {
        let (tx, rx) = async_channel::bounded(cfg.capacity.max(1));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
        let path = target.path.clone();
        let thread = {
            let stop_flag = Arc::clone(&stop_flag);
            let socket = Arc::clone(&socket);
            thread::Builder::new()
                .name(format!("sse {path}"))
                .spawn(move || run(target, cfg, tx, stop_flag, socket))
                .ok()
        };
        // A spawn that failed drops `tx`, which closes the channel — a consumer
        // sees the stream end rather than waiting for events that cannot come.
        EventStream { rx, stop_flag, socket, thread, path }
    }

    /// The route this stream reads (`/events`, `/lanes/1/events`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// A second handle on the same stream — what a UI task awaits.
    pub fn receiver(&self) -> Receiver<StreamMsg> {
        self.rx.clone()
    }

    /// The next message, without blocking.
    pub fn try_recv(&self) -> Option<StreamMsg> {
        self.rx.try_recv().ok()
    }

    /// The next message, blocking the calling thread. `None` once the stream is
    /// over and drained.
    pub fn recv_blocking(&self) -> Option<StreamMsg> {
        self.rx.recv_blocking().ok()
    }

    pub fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::SeqCst)
    }

    /// Stop now: signal, unblock the socket, and join the thread. Idempotent.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.socket.lock() {
            if let Some(socket) = slot.take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.stop();
    }
}

// A spawn failure leaves the channel closed, which reads as "ended"; there is
// nothing else to do about it, and this keeps the intent visible.
fn run(
    target: StreamTarget,
    cfg: StreamConfig,
    tx: Sender<StreamMsg>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
) {
    let mut cursor = target.since;
    let mut backoff = cfg.initial_backoff;
    let mut first = true;

    'outer: loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if !first && target.probe == CursorProbe::Health {
            match probe_cursor(&target.http) {
                Ok(Some(server_cursor)) => {
                    // A cursor ahead of the server's own log means the process
                    // we were reading restarted (its ids begin again at 1).
                    if let Some(ours) = cursor {
                        if server_cursor < ours {
                            cursor = Some(0);
                            if !send(&tx, StreamMsg::Reset { reason: ResetReason::IdRegression }) {
                                break;
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    if !send(
                        &tx,
                        StreamMsg::Disconnected { error: error.to_string(), retry_in: backoff },
                    ) {
                        break;
                    }
                    if !cfg.reconnect || !sleep_or_stop(backoff, &stop) {
                        break;
                    }
                    backoff = bump(backoff, cfg.max_backoff);
                    continue;
                }
            }
        }
        first = false;

        match target.http.open_sse(&target.path, cursor) {
            Err(error) => {
                if !send(
                    &tx,
                    StreamMsg::Disconnected { error: error.to_string(), retry_in: backoff },
                ) {
                    break;
                }
                if !cfg.reconnect || !sleep_or_stop(backoff, &stop) {
                    break;
                }
                backoff = bump(backoff, cfg.max_backoff);
            }
            Ok(mut connection) => {
                if let Ok(handle) = connection.socket().try_clone() {
                    if let Ok(mut slot) = socket.lock() {
                        *slot = Some(handle);
                    }
                }
                if !send(&tx, StreamMsg::Connected { cursor }) {
                    break;
                }
                backoff = cfg.initial_backoff;
                let mut parser = SseParser::new();
                let mut announced_reset = false;
                while !stop.load(Ordering::SeqCst) {
                    let line = match connection.read_line() {
                        Ok(Some(line)) => line,
                        // The server closed, or the socket timed out or died.
                        Ok(None) | Err(_) => break,
                    };
                    let Some(event) = parser.feed(&line) else { continue };
                    if let Some(id) = event.id {
                        if let Some(previous) = cursor {
                            if id < previous && !announced_reset {
                                announced_reset = true;
                                if !send(&tx, StreamMsg::Reset { reason: ResetReason::IdRegression })
                                {
                                    break 'outer;
                                }
                            }
                        }
                        cursor = Some(id);
                    }
                    let kind = event.kind.clone().unwrap_or_default();
                    let data = serde_json::from_str(&event.data)
                        .unwrap_or_else(|_| Value::String(event.data.clone()));
                    let hello = kind == "hello";
                    if !send(&tx, StreamMsg::Event { id: event.id, kind, data }) {
                        break 'outer;
                    }
                    if hello && !announced_reset {
                        announced_reset = true;
                        if !send(&tx, StreamMsg::Reset { reason: ResetReason::Hello }) {
                            break 'outer;
                        }
                    }
                }
                if let Ok(mut slot) = socket.lock() {
                    *slot = None;
                }
                if stop.load(Ordering::SeqCst) || !cfg.reconnect {
                    break;
                }
                if !send(
                    &tx,
                    StreamMsg::Disconnected {
                        error: "the event stream ended".to_owned(),
                        retry_in: backoff,
                    },
                ) {
                    break;
                }
                if !sleep_or_stop(backoff, &stop) {
                    break;
                }
                backoff = bump(backoff, cfg.max_backoff);
            }
        }
    }

    if let Ok(mut slot) = socket.lock() {
        *slot = None;
    }
    let _ = tx.send_blocking(StreamMsg::Ended);
    tx.close();
}

fn send(tx: &Sender<StreamMsg>, message: StreamMsg) -> bool {
    tx.send_blocking(message).is_ok()
}

fn probe_cursor(http: &HttpClient) -> Result<Option<i64>> {
    let reply = http.get("/health")?;
    if !reply.is_success() {
        return Err(Error::Status(reply.error()));
    }
    Ok(reply.json()?.get("cursor").and_then(Value::as_i64))
}

fn bump(backoff: Duration, max: Duration) -> Duration {
    (backoff * 2).min(max)
}

/// Sleep in slices so a stop is noticed at once.
fn sleep_or_stop(total: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + total;
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(25)));
    }
}
