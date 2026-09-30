//! The op stream, on its own thread (CONTRACT.md §5.3).
//!
//! One [`EventStream`] holds one connection at a time and delivers what it reads
//! as [`StreamMsg`]s. It reconnects on its own with a backoff, resuming from the
//! last cursor it saw, and it says so when the server asks for a full re-read:
//!
//! * `stream.reset` — the server cannot continue from the cursor (it restarted,
//!   or the cursor is older than the retention). The stream **parks**: it stops
//!   reading, tells the consumer, and waits for [`EventStream::resume_from`],
//!   which the consumer calls with the cursor of the snapshot it just took. That
//!   is what makes the re-read gapless — snapshot first, then resume from the
//!   position that snapshot was atomic at.
//! * a dropped connection — reconnecting from the last cursor is enough, and
//!   sounds ([`StreamMsg::Reconnecting`]) while it is away.
//!
//! Nothing here parses what an op means: a frame goes to the consumer as it
//! arrived.

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::{Receiver, Sender};

use crate::client::Client;
use crate::protocol::{Cursor, StreamFrame, StreamResetReason};
use crate::sse::SseParser;

/// How long a reconnect waits, doubling up to `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(5),
        }
    }
}

impl Backoff {
    /// The wait before attempt `n` (0-based).
    fn wait(&self, attempt: u32) -> Duration {
        let shift = attempt.min(16);
        let millis = self.initial.as_millis() as u64 * (1u64 << shift);
        Duration::from_millis(millis).min(self.max)
    }
}

/// Which topics to stream, and from where.
#[derive(Clone, Debug)]
pub struct StreamConfig {
    /// `session`, `swarm`, `lane:*`, …
    pub topics: Vec<String>,
    /// Resume from here; `None` starts at now (the hello frame says where).
    pub cursor: Option<Cursor>,
    pub backoff: Backoff,
}

impl StreamConfig {
    pub fn new(topics: impl IntoIterator<Item = impl Into<String>>) -> StreamConfig {
        StreamConfig {
            topics: topics.into_iter().map(Into::into).collect(),
            cursor: None,
            backoff: Backoff::default(),
        }
    }

    pub fn from(mut self, cursor: Cursor) -> StreamConfig {
        self.cursor = Some(cursor);
        self
    }
}

/// What the stream tells its consumer.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamMsg {
    /// A connection is open, from this cursor (the hello frame's).
    Connected { cursor: Cursor },
    /// One frame, in stream order.
    Frame(StreamFrame),
    /// The server cannot continue from the cursor, and the stream has parked
    /// until [`EventStream::resume_from`]. Re-snapshot everything first.
    Reset { reason: StreamResetReason },
    /// The connection dropped; the next attempt is in `retry_in`.
    Reconnecting { attempt: u32, retry_in: Duration },
    /// The stream has stopped and nothing more will arrive.
    Stopped,
}

/// An order from the consumer to the stream thread.
enum Order {
    Resume(Cursor),
    Stop,
}

/// A live op stream, reading on a thread of its own. Cheap to clone: every
/// handle is the same stream.
#[derive(Clone)]
pub struct EventStream(Arc<StreamHandle>);

impl EventStream {
    /// Start reading. The thread ends when the stream does — on `stop`, or when
    /// the consumer drops the `EventStream`.
    pub fn start(client: Client, config: StreamConfig) -> EventStream {
        let (messages_tx, messages) = async_channel::bounded(1024);
        let (orders, orders_rx) = async_channel::bounded(8);
        let cursor = Arc::new(Mutex::new(config.cursor.clone()));
        let socket = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread = {
            let cursor = Arc::clone(&cursor);
            let socket = Arc::clone(&socket);
            let stopped = Arc::clone(&stopped);
            thread::Builder::new()
                .name("evo-op-stream".into())
                .spawn(move || {
                    run(
                        client,
                        config,
                        messages_tx,
                        orders_rx,
                        cursor,
                        socket,
                        stopped,
                    )
                })
                .expect("the stream thread starts")
        };
        EventStream(Arc::new(StreamHandle {
            messages,
            orders,
            cursor,
            socket,
            stopped,
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// The next message; blocks until there is one, `None` once the stream has
    /// stopped and its last message has been read.
    pub fn recv(&self) -> Option<StreamMsg> {
        self.messages.recv_blocking().ok()
    }

    /// The next message, if one is already waiting.
    pub fn try_recv(&self) -> Option<StreamMsg> {
        self.messages.try_recv().ok()
    }

    /// Where this stream has read up to, as far as it knows.
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor.lock().ok().and_then(|cursor| cursor.clone())
    }

    /// Reconnect from `cursor` — the consumer's answer to a [`StreamMsg::Reset`],
    /// after it has taken the snapshot that cursor is the position of.
    pub fn resume_from(&self, cursor: Cursor) {
        let _ = self.orders.send_blocking(Order::Resume(cursor));
    }

    /// Stop reading and end the thread. Idempotent.
    pub fn stop(&self) {
        self.0.stop_stream();
    }
}

/// The stream itself: one per `start`, shared by every handle to it. When the
/// last handle goes, the reader stops and its thread is joined.
pub struct StreamHandle {
    messages: Receiver<StreamMsg>,
    orders: Sender<Order>,
    /// The cursor last seen, shared so a consumer can read it without asking the
    /// thread.
    cursor: Arc<Mutex<Option<Cursor>>>,
    /// The socket of the connection in flight, so `stop` can unblock the reader
    /// (a read timeout is seconds away, and a shutdown must not wait it out).
    socket: Arc<Mutex<Option<TcpStream>>>,
    stopped: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl StreamHandle {
    /// Stop reading: the flag ends a backoff, the order ends a parked stream, and
    /// shutting the socket down ends a read that would otherwise wait out its
    /// timeout.
    fn stop_stream(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.orders.send_blocking(Order::Stop);
        if let Ok(socket) = self.socket.lock() {
            if let Some(socket) = socket.as_ref() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop_stream();
        if let Ok(mut thread) = self.thread.lock() {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

impl std::ops::Deref for EventStream {
    type Target = StreamHandle;
    fn deref(&self) -> &StreamHandle {
        &self.0
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("cursor", &self.cursor())
            .finish_non_exhaustive()
    }
}

/// Why one connection ended.
enum Ended {
    /// The server asked for a re-read, and the stream is parking.
    Reset(StreamResetReason),
    /// The connection dropped, or was dropped by `stop`.
    Disconnected,
}

/// The stream thread: connect, read, and decide what a broken connection means.
fn run(
    client: Client,
    config: StreamConfig,
    messages: Sender<StreamMsg>,
    orders: Receiver<Order>,
    cursor: Arc<Mutex<Option<Cursor>>>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    stopped: Arc<AtomicBool>,
) {
    let mut attempt: u32 = 0;
    loop {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let since = cursor.lock().ok().and_then(|cursor| cursor.clone());
        match read(&client, &config.topics, since, &messages, &cursor, &socket) {
            // A re-read: park until the consumer says where to resume.
            Ok(Ended::Reset(reason)) => {
                let _ = messages.send_blocking(StreamMsg::Reset { reason });
                // Parked until the consumer has re-snapshotted and says where to
                // resume: that is what keeps the re-read gapless.
                match orders.recv_blocking() {
                    Ok(Order::Resume(from)) => {
                        if let Ok(mut slot) = cursor.lock() {
                            *slot = Some(from);
                        }
                        attempt = 0;
                    }
                    Ok(Order::Stop) | Err(_) => {
                        let _ = messages.send_blocking(StreamMsg::Stopped);
                        return;
                    }
                }
            }
            Ok(Ended::Disconnected) => {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let retry_in = config.backoff.wait(attempt);
                attempt = attempt.saturating_add(1);
                let _ = messages.send_blocking(StreamMsg::Reconnecting { attempt, retry_in });
                if !sleep_until(retry_in, &stopped) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = messages.send_blocking(StreamMsg::Stopped);
}

/// One connection, start to end. Sends `Connected` on hello and `Frame` for
/// every other op; a `stream.reset` ends the connection and says why.
fn read(
    client: &Client,
    topics: &[String],
    since: Option<Cursor>,
    messages: &Sender<StreamMsg>,
    cursor: &Arc<Mutex<Option<Cursor>>>,
    socket: &Arc<Mutex<Option<TcpStream>>>,
) -> Result<Ended, crate::error::Error> {
    let mut connection = client.open_stream(topics, since.as_ref())?;
    if let Ok(handle) = connection.socket().try_clone() {
        if let Ok(mut slot) = socket.lock() {
            *slot = Some(handle);
        }
    }
    let mut parser = SseParser::new();
    let ended = loop {
        if messages.is_closed() {
            break Ended::Disconnected;
        }
        let line = match connection.read_line() {
            Ok(Some(line)) => line,
            // Closed, or the read timed out: either way this connection is over,
            // and a reconnect resumes from where it left off.
            Ok(None) | Err(_) => break Ended::Disconnected,
        };
        let Some(event) = parser.feed(&line) else {
            continue;
        };
        let frame = StreamFrame::parse(event.kind.as_deref(), event.id.as_deref(), &event.data);
        if let Some(from) = &frame.cursor {
            if let Ok(mut slot) = cursor.lock() {
                *slot = Some(from.clone());
            }
        }
        if frame.is_hello() {
            let hello = hello_cursor(&frame).unwrap_or_else(|| {
                frame
                    .cursor
                    .clone()
                    .unwrap_or_else(|| Cursor::new(String::new(), 0))
            });
            if let Ok(mut slot) = cursor.lock() {
                *slot = Some(hello.clone());
            }
            let _ = messages.send_blocking(StreamMsg::Connected { cursor: hello });
            continue;
        }
        if let Some(reason) = frame.stream_reset() {
            break Ended::Reset(reason);
        }
        if messages.send_blocking(StreamMsg::Frame(frame)).is_err() {
            break Ended::Disconnected;
        }
    };
    if let Ok(mut slot) = socket.lock() {
        *slot = None;
    }
    connection.close();
    Ok(ended)
}

/// The cursor a hello frame announces: its `epoch`, and its `seq`.
fn hello_cursor(frame: &StreamFrame) -> Option<Cursor> {
    let epoch = frame.get("epoch")?.as_str()?.to_owned();
    let seq = frame.get("seq")?.as_u64()?;
    Some(Cursor::new(epoch, seq))
}

/// Sleep, in slices, until the stream is stopped. `false` when it was stopped.
fn sleep_until(total: Duration, stopped: &AtomicBool) -> bool {
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if stopped.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !stopped.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_backoff_doubles_and_stops_at_the_maximum() {
        let backoff = Backoff::default();
        assert_eq!(backoff.wait(0), Duration::from_millis(250));
        assert_eq!(backoff.wait(1), Duration::from_millis(500));
        assert_eq!(backoff.wait(2), Duration::from_millis(1000));
        assert_eq!(backoff.wait(3), Duration::from_millis(2000));
        assert_eq!(
            backoff.wait(4),
            Duration::from_millis(5000).min(Duration::from_millis(4000))
        );
        assert_eq!(backoff.wait(20), Duration::from_secs(5));
    }

    #[test]
    fn a_hello_frame_names_the_cursor_the_stream_starts_at() {
        let frame = StreamFrame::parse(
            Some("op"),
            Some("7f3a.9"),
            &json!({"op": "hello", "epoch": "7f3a", "seq": 9}).to_string(),
        );
        assert_eq!(hello_cursor(&frame), Some(Cursor::new("7f3a", 9)));

        let without = StreamFrame::parse(Some("op"), None, r#"{"op":"hello"}"#);
        assert_eq!(hello_cursor(&without), None);
    }
}
