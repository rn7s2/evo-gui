//! The browser transport: `reqwest` for one request, `eventsource-stream` for the
//! op stream (CONTRACT.md §5).
//!
//! This is the wasm half of the transport seam — the other is [`crate::http`],
//! which opens a socket. What the two share is the protocol, the paths and the
//! types; what differs is the platform underneath, so this file is the browser's
//! whole transport and native never compiles it.
//!
//! # Why not a hand-rolled reader
//!
//! This module used to frame SSE itself: a `ReadableStreamDefaultReader`, a
//! `Vec<u8>` buffer, a `take_line` that drained up to the next `\n`, and a loop
//! feeding [`crate::sse::SseParser`] a line at a time. It compiled, and it was
//! wrong: a live session showed `reconnecting…` for ever and sent turns that were
//! answered but never appeared, while `curl` against the same URL read the frames
//! and the native half of this crate read them too. Framing a streaming protocol
//! by hand is a place to be wrong quietly — a mis-split frame is a turn that never
//! arrives — so the framing is now done by crates that exist for it:
//!
//! * [`reqwest`] is the request: one `fetch` on this target, with the timeout,
//!   the headers and the status already handled;
//! * [`eventsource_stream`] is the framing: `bytes_stream().eventsource()` turns
//!   the reply body into a `Stream<Item = Result<Event, _>>` that splits frames
//!   on the blank line, joins a frame's several `data:` lines with `\n`, and drops
//!   `:` comments — the same rules [`crate::sse`] implements, from a crate that is
//!   tested against them.
//!
//! Nothing here is per-target: the request, the framing and the loop below are the
//! same code on both. What *is* per-target is small and named where it appears: a
//! same-origin prefix has to be resolved against the page's origin before
//! `reqwest` sees it (the browser's `fetch` refuses a relative URL), and a browser
//! needs a cancellation handle where a host has a socket it can shut down.
//!
//! # Where it points
//!
//! A browser cannot spawn `evo-swarm`, so it does not talk to serve directly. It
//! talks to the axum server that served the bundle ([`Target::BasePath`] — a
//! same-origin prefix like `/api/tabs/7f3a`), which holds the bearer token and
//! adds `Authorization` to every request it forwards. That is [`Client::at_base`],
//! and it is the shape the web frontend uses.
//!
//! A [`Target::Loopback`] is also accepted — an absolute `http://host:port/…` URL
//! with the token in a header — for a page served by the very machine that runs
//! serve. It needs that server to send CORS headers, which the real `serve` does
//! not, so it is a capability and not the intended route.
//!
//! # Why not `EventSource`
//!
//! The browser's own SSE client would have been less code still. It cannot be used
//! here, for two reasons that are both fatal:
//!
//! * it cannot set an `Authorization` header, and this protocol is bearer-token on
//!   every route;
//! * it hides the response status — a 401 and a closed stream look the same to it
//!   — so a tab could not tell "wrong token" from "the server restarted".
//!
//! An authenticated request whose body is read as a stream has neither problem:
//! the status is in hand before a frame is parsed, and the request carries whatever
//! headers the protocol needs.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{Error, Result, StatusError};
use crate::protocol::{Cursor, OpReply, OpRequest, Snapshot};
use crate::sse::SseEvent;
use crate::transport::{request_path, HttpResponse, Target, Token};

/// How much patience a body of `bytes` bytes buys, on top of the base timeout.
///
/// The same model as the native transport's, and for the same reason: serve reads
/// a POST body a few kilobytes a second, so the reply to a turn carrying a pasted
/// screenshot is tens of seconds behind its request, and a client that gives up
/// there reports a refusal for a turn the session has already taken. See
/// [`crate::http`] for the measurements this is drawn from.
const BODY_RATE_FLOOR: u64 = 8 * 1024;

/// The most extra patience one body may earn, whatever it carries.
const BODY_PATIENCE_CAP: Duration = Duration::from_secs(240);

/// How often the reader looks at the connection's own clock while it waits for a
/// frame. Short enough that a dead connection is noticed promptly, long enough that
/// an idle tab is not polling a timer hard. See [`Tap`].
const IDLE_TICK: Duration = Duration::from_secs(1);

/// One request, on the platform's own client.
#[derive(Clone, Debug)]
pub struct FetchClient {
    target: Target,
    client: reqwest::Client,
    timeout: Duration,
    stream_timeout: Duration,
}

impl FetchClient {
    pub fn new(target: Target) -> FetchClient {
        FetchClient {
            target,
            // No timeout on the client: every request carries its own, because a
            // stream's is different from a POST's and a client-wide one would
            // apply to both.
            client: reqwest::Client::new(),
            timeout: Duration::from_secs(30),
            stream_timeout: Duration::from_secs(45),
        }
    }

    /// A client for a same-origin path prefix, with no token of its own — what a
    /// browser uses, because the proxy that served it holds the credential.
    pub fn at_base(prefix: &str) -> Result<FetchClient> {
        Ok(FetchClient::new(Target::base_path(prefix)?))
    }

    /// A client for a server on this machine, with its bearer token. Needs that
    /// server to answer CORS; the intended browser route is [`Self::at_base`].
    pub fn loopback(port: u16, token: impl Into<String>) -> Result<FetchClient> {
        Ok(FetchClient::new(Target::loopback(port, token)?))
    }

    /// The same server, with a different patience — a shutdown request gets a
    /// short one, so a wedged server reaches the next rung of the ladder.
    pub fn with_timeout(&self, timeout: Duration) -> FetchClient {
        FetchClient {
            timeout,
            ..self.clone()
        }
    }

    /// The patience for an idle long-lived stream: serve keeps one alive with a
    /// `: keepalive` comment every 15 s, so a silence past this is a dead socket.
    pub fn with_stream_timeout(&self, timeout: Duration) -> FetchClient {
        FetchClient {
            stream_timeout: timeout,
            ..self.clone()
        }
    }

    /// The patience for an idle stream — what [`Client::open_stream`]'s reader
    /// gives a silent connection before it calls it dead.
    pub fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// How long a request may take, body and reply together — the same model as
    /// the native transport's [`crate::http::HttpClient::patience_for_body`].
    pub fn patience_for_body(&self, body: usize) -> Duration {
        let extra = Duration::from_secs(body as u64 / BODY_RATE_FLOOR);
        self.timeout + extra.min(BODY_PATIENCE_CAP)
    }

    /// One request, its whole reply read.
    pub async fn request(
        &self,
        method: &str,
        route: &str,
        body: Option<&Value>,
    ) -> Result<HttpResponse> {
        let payload = body.map(serde_json::to_string).transpose()?;
        let timeout = self.patience_for_body(payload.as_ref().map_or(0, String::len));
        let mut request = self
            .client
            .request(method.parse().map_err(|_| {
                Error::Config(format!("{method:?} is not an HTTP method"))
            })?, self.url(route))
            .timeout(timeout)
            .header("accept", "application/json");
        if let Some(token) = self.token() {
            request = request.header("authorization", format!("Bearer {}", token.as_str()));
        }
        if let Some(payload) = &payload {
            request = request
                .header("content-type", "application/json")
                .body(payload.clone());
        }
        let response = request.send().await.map_err(request_error)?;
        let status = response.status().as_u16();
        let headers = read_headers(response.headers());
        let body = response
            .bytes()
            .await
            .map_err(|e| Error::Protocol(format!("cannot read the body: {e}")))?
            .to_vec();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    pub async fn get(&self, route: &str) -> Result<HttpResponse> {
        self.request("GET", route, None).await
    }

    pub async fn post(&self, route: &str, body: &Value) -> Result<HttpResponse> {
        self.request("POST", route, Some(body)).await
    }

    /// Open an SSE stream. A refusal (no token, a shut-down server) is written
    /// before any stream header, so it arrives as an ordinary HTTP error here —
    /// which is the whole reason this is not an `EventSource`.
    ///
    /// The reply is *not* read here: an SSE stream does not end, so a caller that
    /// awaited the body would wait for the tab to be over. What is read is the
    /// status, which is what tells a refusal from a stream; the frames come from
    /// [`FetchSseConnection::next_event`] as the server writes them.
    pub async fn open_sse(&self, route: &str) -> Result<FetchSseConnection> {
        let mut request = self
            .client
            .get(self.url(route))
            .header("accept", "text/event-stream")
            // A proxy that buffers would defeat an SSE stream; this asks it not
            // to. Harmless where there is no proxy.
            .header("cache-control", "no-cache");
        if let Some(token) = self.token() {
            request = request.header("authorization", format!("Bearer {}", token.as_str()));
        }
        let response = request.send().await.map_err(request_error)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.bytes().await.map_err(|e| {
                Error::Protocol(format!("cannot read the refusal: {e}"))
            })?;
            let text = String::from_utf8_lossy(&body).into_owned();
            let raw = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
            return Err(Error::Status(StatusError::from_reply(status, raw, &text)));
        }
        let last_activity = Rc::new(Cell::new(js_sys::Date::now()));
        let tap = Tap {
            inner: response.bytes_stream(),
            last_activity: Rc::clone(&last_activity),
        };
        Ok(FetchSseConnection {
            stream: Box::pin(tap.eventsource()),
            done: false,
            last_activity,
        })
    }

    /// The token this client holds, if it holds one: a base-path client holds none,
    /// because the proxy that forwards it adds one.
    pub fn token(&self) -> Option<&Token> {
        match &self.target {
            Target::Loopback { token, .. } => Some(token),
            Target::BasePath(_) => None,
        }
    }

    /// The absolute URL one route is requested at.
    ///
    /// A browser's `fetch` refuses a relative URL — `reqwest` builds the request
    /// itself and answers `builder error` rather than resolving it — so a
    /// same-origin prefix is resolved against the page's origin here, and what
    /// reaches the transport is always absolute.
    fn url(&self, route: &str) -> String {
        let path = request_path(&self.target, route);
        match &self.target {
            // A loopback target is already absolute: host and port are in it.
            Target::Loopback { .. } => path,
            Target::BasePath(_) => format!("{}{}", origin(), path),
        }
    }
}

/// The page's own origin — `https://host:port` — for resolving a same-origin path.
///
/// This is the one thing the browser needs that a host does not, and it is the
/// *only* reason a base-path client cannot be built without a page: a path has no
/// origin of its own.
fn origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

/// A rejected request as an [`Error`].
///
/// A network failure and a timeout are the same shape to a caller — the connection
/// (rather than the request) is what went wrong — and that is the case a stream
/// reconnects on, so they are transport errors and not protocol ones.
fn request_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        return Error::Timeout(error.to_string());
    }
    if error.is_builder() {
        // A URL `reqwest` would not build: a config mistake, and the message says
        // which — `builder error` alone is not worth showing anyone.
        return Error::Config(error.to_string());
    }
    // Everything else a request can fail with is the connection rather than the
    // request: the case a stream reconnects on. `is_connect` is not asked — on wasm
    // there is no connection to have failed, and the method does not exist there.
    if error.is_request() || error.is_body() || error.is_decode() {
        return Error::Closed;
    }
    Error::Protocol(error.to_string())
}

/// A reply's headers, lower-cased, as the native transport's `HttpResponse` has
/// them.
fn read_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A byte stream that notes when it last produced anything.
///
/// The liveness of an SSE connection is *any* byte arriving, and the server proves
/// it with a `: keepalive` comment every 15 s. But a comment is not an event, so
/// [`eventsource_stream`] drops it — which means the frame stream below cannot see
/// the very thing that says the connection is alive. Timing the *frames* therefore
/// measures nothing useful: a healthy idle stream looks exactly like a dead one,
/// and a reader that reconnects on frame-silence tears down good connections for
/// ever (measured: two reconnects in a hundred seconds, which is the "reconnecting…"
/// the reader sees).
///
/// So the tap sits *under* the framing, where the bytes still are: it records the
/// last moment one arrived, keepalives included, and the reader's patience is
/// measured against that instead of against the last frame.
struct Tap<S> {
    inner: S,
    last_activity: Rc<Cell<f64>>,
}

impl<S, T> futures_util::Stream for Tap<S>
where
    S: futures_util::Stream<Item = T> + Unpin,
{
    type Item = T;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_next(cx);
        if matches!(polled, std::task::Poll::Ready(Some(_))) {
            self.last_activity.set(js_sys::Date::now());
        }
        polled
    }
}

/// A live SSE connection, read one frame at a time.
///
/// The framing is [`eventsource_stream`]'s, not this crate's: what it hands back
/// is already a whole event, with a frame's several `data:` lines joined and its
/// `:` comments dropped. This type's job is the two things a stream of frames
/// still needs — a way to ask for the next one, and a way to stop.
pub struct FetchSseConnection {
    /// The frame stream. Boxed because the underlying reader is a `reqwest` body
    /// of an unnamed type, and pinned because a stream is polled in place.
    ///
    /// Not `Send`: a browser's body is a `ReadableStream` on the one thread there
    /// is, and `reqwest`'s wasm stream is `!Send` for exactly that reason. Nothing
    /// here crosses a thread — the whole stream task is one `spawn_local` — so the
    /// bound would only be a lie about a platform that has no second thread.
    stream: std::pin::Pin<
        Box<
            dyn futures_util::Stream<
                Item = std::result::Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<reqwest::Error>,
                >,
            >,
        >,
    >,
    done: bool,
    /// When a byte last arrived on this connection, keepalives included — the
    /// clock the reader's idle patience is measured against. See [`Tap`].
    last_activity: Rc<Cell<f64>>,
}

impl FetchSseConnection {
    /// The next frame, or `None` at the end of the stream.
    ///
    /// This is the browser's [`crate::http::SseConnection::read_line`]: a caller
    /// reads frames until the connection ends. A `stream.reset` and a `hello` are
    /// frames like any other — the caller is what decides what they mean.
    pub async fn next_event(&mut self) -> Option<Result<SseEvent>> {
        if self.done {
            return None;
        }
        match self.stream.next().await {
            Some(Ok(event)) => Some(Ok(SseEvent::from_wire(&event.event, &event.id, &event.data))),
            Some(Err(error)) => {
                self.done = true;
                Some(Err(Error::Protocol(format!("the stream broke: {error}"))))
            }
            None => {
                self.done = true;
                None
            }
        }
    }

    /// Unblock a reader: end the stream, so a `next_event` in flight comes back
    /// `None` instead of waiting out the server's next keepalive.
    ///
    /// The browser has no socket to shut down, so this closes the stream from this
    /// side — which is what a `Drop` on the reader would do anyway, and is
    /// idempotent.
    pub fn close(&mut self) {
        self.done = true;
    }

    /// Whether this connection has gone quiet for longer than `patience`.
    ///
    /// Measured against the last *byte*, not the last frame: a stream that is idle
    /// but alive is saying so with a `: keepalive` every 15 s, and only the tap
    /// under the framing can see those. See [`Tap`].
    pub fn is_idle(&self, patience: Duration) -> bool {
        let idle_ms = js_sys::Date::now() - self.last_activity.get();
        idle_ms > patience.as_millis() as f64
    }
}

impl fmt::Debug for FetchSseConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchSseConnection")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// A client bound to one server, at one address, with one token — the wasm
/// counterpart of [`crate::client::Client`], and the type a wasm consumer names as
/// `swarm_client::Client`.
///
/// Everything is `async`: a browser has no thread to block.
#[derive(Clone, Debug)]
pub struct Client {
    fetch: FetchClient,
}

impl Client {
    /// A client for a same-origin path prefix with no token — the browser's shape.
    /// Every request goes to `<prefix>/snapshot`, `<prefix>/stream`, … and the
    /// server behind that prefix adds the bearer token itself.
    pub fn at_base(prefix: &str) -> Result<Client> {
        Ok(Client {
            fetch: FetchClient::at_base(prefix)?,
        })
    }

    /// A client for a server on this machine, with its bearer token. The server
    /// must answer CORS; [`Client::at_base`] is the intended browser route.
    pub fn loopback(port: u16, token: impl Into<String>) -> Result<Client> {
        Ok(Client {
            fetch: FetchClient::loopback(port, token)?,
        })
    }

    /// A client for a named host, which must be loopback.
    pub fn at(host: &str, port: u16, token: impl Into<String>) -> Result<Client> {
        Ok(Client {
            fetch: FetchClient::new(Target::at(host, port, token)?),
        })
    }

    pub fn with_timeout(&self, timeout: Duration) -> Client {
        Client {
            fetch: self.fetch.with_timeout(timeout),
        }
    }

    pub fn with_stream_timeout(&self, timeout: Duration) -> Client {
        Client {
            fetch: self.fetch.with_stream_timeout(timeout),
        }
    }

    /// The transport underneath, for a caller that needs a route the typed methods
    /// do not cover.
    pub fn fetch(&self) -> &FetchClient {
        &self.fetch
    }

    pub fn target(&self) -> &Target {
        self.fetch.target()
    }

    /// The patience for an idle stream — the `with_stream_timeout` this client was
    /// built with, and what its reader calls a silent connection dead after.
    pub fn stream_timeout(&self) -> Duration {
        self.fetch.stream_timeout()
    }

    /// `GET /snapshot?topics=…&items=N` — the whole view, atomically.
    pub async fn snapshot(&self, topics: &[String], items: Option<u32>) -> Result<Snapshot> {
        let mut route = format!("/snapshot?topics={}", encode_list(topics));
        if let Some(items) = items {
            route.push_str(&format!("&items={items}"));
        }
        let body = self.get_json(&route).await?;
        Ok(serde_json::from_value(body)?)
    }

    /// Open the op stream (§5.3) from `since`, or from now when it is `None`.
    ///
    /// The caller reads the frames; [`WasmEventStream`] is the usual one, and it is
    /// the one that knows how to reconnect.
    pub async fn open_stream(
        &self,
        topics: &[String],
        since: Option<&Cursor>,
    ) -> Result<FetchSseConnection> {
        let mut route = format!("/stream?topics={}", encode_list(topics));
        if let Some(cursor) = since {
            route.push_str(&format!("&since={}", cursor.encode()));
        }
        self.fetch.open_sse(&route).await
    }

    /// `POST /ops` (§5.5) — one transparent retry on a connection failure.
    ///
    /// The retry is safe because `rid` is idempotent: the server remembers the last
    /// 256 replies by rid, so a request that was answered into a connection that
    /// died is answered again rather than acted on twice.
    pub async fn op(&self, op: &str, args: Value) -> Result<OpReply> {
        let rid = crate::transport::new_rid();
        self.op_with_rid(&rid, op, args).await
    }

    /// The same, with a caller-chosen rid.
    pub async fn op_with_rid(&self, rid: &str, op: &str, args: Value) -> Result<OpReply> {
        let request = serde_json::to_value(OpRequest::new(rid, op, args))?;
        let body = match self.fetch.post("/ops", &request).await {
            Ok(response) => response,
            Err(e) if e.is_transport() => self.fetch.post("/ops", &request).await?,
            Err(e) => return Err(e),
        };
        if !body.is_success() {
            return Err(Error::Status(body.error()));
        }
        Ok(serde_json::from_slice(&body.body)?)
    }

    /// `server.shutdown` — the first rung of the ladder. A refusal is not an
    /// error: something is already stopping.
    pub async fn shutdown(&self) -> Result<()> {
        self.op("server.shutdown", Value::Object(serde_json::Map::new()))
            .await?;
        Ok(())
    }

    /// `GET /catalog` (§5.6) — the live registry, untyped.
    pub async fn catalog(&self) -> Result<Value> {
        self.get_json("/catalog").await
    }

    /// `GET /items?topic=&before=&limit=` (§5.4) — older items, untyped.
    pub async fn items(&self, topic: &str, before: Option<&str>, limit: u32) -> Result<Value> {
        let mut route = format!("/items?topic={}&limit={limit}", encode_value(topic));
        if let Some(before) = before {
            route.push_str(&format!("&before={}", encode_value(before)));
        }
        self.get_json(&route).await
    }

    /// `GET /items/<id>?topic=` — one item, whole.
    pub async fn item(&self, topic: &str, id: &str) -> Result<Value> {
        self.get_json(&format!(
            "/items/{}?topic={}",
            encode_value(id),
            encode_value(topic)
        ))
        .await
    }

    /// `GET /media/<id>/<n>?topic=` — image bytes and their content type.
    pub async fn media(&self, topic: &str, id: &str, n: u32) -> Result<(Vec<u8>, String)> {
        let response = self
            .fetch
            .get(&format!(
                "/media/{}/{n}?topic={}",
                encode_value(id),
                encode_value(topic)
            ))
            .await?;
        if !response.is_success() {
            return Err(Error::Status(response.error()));
        }
        let content_type = response
            .header("content-type")
            .unwrap_or("application/octet-stream")
            .to_owned();
        Ok((response.body, content_type))
    }

    /// One `GET`, its body as JSON.
    pub async fn get_json(&self, route: &str) -> Result<Value> {
        let response = self.fetch.get(route).await?;
        if !response.is_success() {
            return Err(Error::Status(response.error()));
        }
        Ok(serde_json::from_slice(&response.body)?)
    }

    /// One `GET`, the whole reply.
    pub async fn get(&self, route: &str) -> Result<HttpResponse> {
        let response = self.fetch.get(route).await?;
        if !response.is_success() {
            return Err(Error::Status(response.error()));
        }
        Ok(response)
    }
}

/// A comma-separated topic list, percent-encoded, `lane:*` included.
fn encode_list(topics: &[String]) -> String {
    topics
        .iter()
        .map(|topic| encode_value(topic))
        .collect::<Vec<_>>()
        .join(",")
}

/// Percent-encode everything that is not unreserved.
fn encode_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// How long a reconnect waits, doubling up to `max`. The native
/// [`crate::stream::Backoff`], in a build that has no `stream` module.
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
    pub fn wait(&self, attempt: u32) -> Duration {
        let shift = attempt.min(16);
        let millis = self.initial.as_millis() as u64 * (1u64 << shift);
        Duration::from_millis(millis).min(self.max)
    }
}

/// Which topics to stream, and from where.
#[derive(Clone, Debug)]
pub struct StreamConfig {
    pub topics: Vec<String>,
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

/// What the stream tells its consumer — the native [`crate::stream::StreamMsg`],
/// in a build that has no `stream` module.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamMsg {
    Connected { cursor: Cursor },
    Frame(crate::protocol::StreamFrame),
    Reset {
        reason: crate::protocol::StreamResetReason,
    },
    Reconnecting { retry_in: Duration },
    Stopped,
}

/// An op stream on the page's event loop, reconnecting with a backoff.
///
/// The native [`crate::stream::EventStream`] reads on a thread of its own; a
/// browser has no thread, so this is a task on the event loop, and `recv` is
/// `async` rather than blocking. What it does is the same: one connection at a
/// time, resuming from the last cursor it saw, parking when the server asks for a
/// full re-read.
///
/// Cheap to clone, and every handle is the same stream — which is the native
/// guarantee, and the reason the state is behind an `Rc`: it is the **last**
/// handle that stops the reader, not whichever one happens to be dropped first.
#[derive(Clone)]
pub struct WasmEventStream(Rc<StreamHandle>);

/// An order from the consumer to the stream task.
enum Order {
    Resume(Cursor),
    Stop,
}

/// The stream itself: one per `start`, shared by every handle to it. When the last
/// handle goes, the reader stops.
pub struct StreamHandle {
    messages: async_channel::Receiver<StreamMsg>,
    orders: async_channel::Sender<Order>,
    /// The cursor last seen, shared so a consumer can read it without asking the
    /// task.
    cursor: Rc<RefCell<Option<Cursor>>>,
    /// Whether the reader has been told to stop. The stream task checks it between
    /// frames, which is how `stop` ends a read that would otherwise wait for the
    /// server's next keepalive.
    stopped: Rc<Cell<bool>>,
}

impl StreamHandle {
    /// Stop reading: the flag ends a backoff and a read in flight, and the order
    /// ends a parked stream.
    fn stop_stream(&self) {
        self.stopped.set(true);
        let _ = self.orders.try_send(Order::Stop);
    }

    /// The next message, awaited; `None` once the stream has stopped and its last
    /// message has been read.
    pub async fn recv(&self) -> Option<StreamMsg> {
        self.messages.recv().await.ok()
    }

    /// The next message, if one is already waiting.
    pub fn try_recv(&self) -> Option<StreamMsg> {
        self.messages.try_recv().ok()
    }

    /// Where this stream has read up to, as far as it knows.
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor.borrow().clone()
    }

    /// Reconnect from `cursor` — the consumer's answer to a [`StreamMsg::Reset`],
    /// after it has taken the snapshot that cursor is the position of.
    pub fn resume_from(&self, cursor: Cursor) {
        let _ = self.orders.try_send(Order::Resume(cursor));
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop_stream();
    }
}

impl WasmEventStream {
    /// Start reading. The task ends when the stream does — on `stop`, or when the
    /// last handle is dropped.
    pub fn start(client: Client, config: StreamConfig) -> WasmEventStream {
        let (messages_tx, messages) = async_channel::bounded(1024);
        let (orders, orders_rx) = async_channel::bounded(8);
        let cursor = Rc::new(RefCell::new(config.cursor.clone()));
        let stopped = Rc::new(Cell::new(false));
        let stream = WasmEventStream(Rc::new(StreamHandle {
            messages,
            orders,
            cursor: Rc::clone(&cursor),
            stopped: Rc::clone(&stopped),
        }));
        wasm_bindgen_futures::spawn_local(run(
            client, config, messages_tx, orders_rx, cursor, stopped,
        ));
        stream
    }

    /// Stop reading and end the task. Idempotent.
    pub fn stop(&self) {
        self.0.stop_stream();
    }
}

impl std::ops::Deref for WasmEventStream {
    type Target = StreamHandle;
    fn deref(&self) -> &StreamHandle {
        &self.0
    }
}

impl fmt::Debug for WasmEventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmEventStream")
            .field("cursor", &self.cursor())
            .finish_non_exhaustive()
    }
}

/// The stream task: connect, read, and decide what a broken connection means.
async fn run(
    client: Client,
    config: StreamConfig,
    messages: async_channel::Sender<StreamMsg>,
    orders: async_channel::Receiver<Order>,
    cursor: Rc<RefCell<Option<Cursor>>>,
    stopped: Rc<Cell<bool>>,
) {
    let mut attempt: u32 = 0;
    loop {
        if stopped.get() {
            break;
        }
        let since = cursor.borrow().clone();
        match read(&client, &config.topics, since, &messages, &cursor, &stopped).await {
            // A re-read: park until the consumer says where to resume.
            Ok(Ended::Reset(reason)) => {
                if messages.send(StreamMsg::Reset { reason }).await.is_err() {
                    return;
                }
                match orders.recv().await {
                    Ok(Order::Resume(from)) => {
                        *cursor.borrow_mut() = Some(from);
                        attempt = 0;
                    }
                    Ok(Order::Stop) | Err(_) => {
                        let _ = messages.send(StreamMsg::Stopped).await;
                        return;
                    }
                }
            }
            // A connect that failed is a connection that is not there *yet*.
            Ok(Ended::Disconnected) | Err(_) => {
                if stopped.get() {
                    break;
                }
                let retry_in = config.backoff.wait(attempt);
                attempt = attempt.saturating_add(1);
                if messages
                    .send(StreamMsg::Reconnecting { retry_in })
                    .await
                    .is_err()
                {
                    return;
                }
                if !sleep(retry_in, &stopped).await {
                    break;
                }
            }
        }
    }
    let _ = messages.send(StreamMsg::Stopped).await;
}

/// Why one connection ended.
enum Ended {
    Reset(crate::protocol::StreamResetReason),
    Disconnected,
}

/// One connection, start to end.
async fn read(
    client: &Client,
    topics: &[String],
    since: Option<Cursor>,
    messages: &async_channel::Sender<StreamMsg>,
    cursor: &Rc<RefCell<Option<Cursor>>>,
    stopped: &Rc<Cell<bool>>,
) -> Result<Ended> {
    let mut connection = client.open_stream(topics, since.as_ref()).await?;
    let ended = loop {
        if stopped.get() || messages.is_closed() {
            break Ended::Disconnected;
        }
        // The next frame, or a tick. The tick is what lets an idle-but-alive stream
        // be told apart from a dead one: frames can be minutes apart on a quiet
        // tab, while the server keeps saying it is there every 15 s — but those
        // keepalives are comments, which the framing drops. Only the tap under it
        // sees them, so the patience is measured against the last byte rather than
        // against the last frame (see `Tap`).
        //
        // The clock is cloned out before the `select`, because the borrow of the
        // connection that `next_event` takes lives as long as the future does.
        let activity = Rc::clone(&connection.last_activity);
        let next = futures_util::future::select(
            Box::pin(connection.next_event()),
            Box::pin(sleep(IDLE_TICK, stopped)),
        );
        let event = match next.await {
            // A frame arrived first: `eventsource-stream` has already joined its
            // `data:` lines and dropped the `:` keepalives, so this is the same
            // shape the native reader's parser produces.
            futures_util::future::Either::Left((event, _)) => match event {
                Some(Ok(event)) => event,
                // Closed, or broke: either way this connection is over.
                Some(Err(_)) | None => break Ended::Disconnected,
            },
            // Nothing arrived in a tick. Not a dead stream by itself — the server's
            // own keepalives are 15 s apart — so the connection's clock decides,
            // and only a silence past the patience ends it.
            futures_util::future::Either::Right(_) => {
                let idle = js_sys::Date::now() - activity.get();
                if stopped.get() || idle > client.stream_timeout().as_millis() as f64 {
                    break Ended::Disconnected;
                }
                continue;
            }
        };
        let frame = crate::protocol::StreamFrame::parse(
            event.kind.as_deref(),
            event.id.as_deref(),
            &event.data,
        );
        if let Some(from) = &frame.cursor {
            *cursor.borrow_mut() = Some(from.clone());
        }
        if frame.is_hello() {
            let hello = hello_cursor(&frame).unwrap_or_else(|| {
                frame
                    .cursor
                    .clone()
                    .unwrap_or_else(|| Cursor::new(String::new(), 0))
            });
            *cursor.borrow_mut() = Some(hello.clone());
            let _ = messages.send(StreamMsg::Connected { cursor: hello }).await;
            continue;
        }
        if let Some(reason) = frame.stream_reset() {
            break Ended::Reset(reason);
        }
        if messages.send(StreamMsg::Frame(frame)).await.is_err() {
            break Ended::Disconnected;
        }
    };
    connection.close();
    Ok(ended)
}

/// The cursor a hello frame announces: its `epoch`, and its `seq`.
fn hello_cursor(frame: &crate::protocol::StreamFrame) -> Option<Cursor> {
    let epoch = frame.get("epoch")?.as_str()?.to_owned();
    let seq = frame.get("seq")?.as_u64()?;
    Some(Cursor::new(epoch, seq))
}

/// Wait, in slices, until the stream is stopped. `false` when it was stopped.
///
/// A browser has no `thread::sleep`; the page's timer is the only clock, so this
/// waits on a promise the browser resolves. `setTimeout` is called through
/// `js_sys`'s `Promise`, which is what a wasm sleep is made of.
async fn sleep(total: Duration, stopped: &Rc<Cell<bool>>) -> bool {
    const SLICE: u64 = 20;
    let mut left = total.as_millis() as u64;
    while left > 0 {
        if stopped.get() {
            return false;
        }
        let step = left.min(SLICE);
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            if let Some(window) = web_sys::window() {
                // `resolve` is already a `js_sys::Function`: `setTimeout` takes the
                // callback itself, so there is nothing to cast.
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    &resolve,
                    step as i32,
                );
            }
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        left -= step;
    }
    !stopped.get()
}
