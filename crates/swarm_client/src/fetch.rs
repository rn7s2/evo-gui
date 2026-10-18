//! The browser transport: `fetch` for one request, a `ReadableStream` read chunk
//! by chunk for the op stream (CONTRACT.md §5).
//!
//! This is the wasm half of the transport seam — the other is [`crate::http`],
//! which opens a socket. A browser can do neither of the things the native half
//! is made of: it cannot open a raw TCP connection, and it cannot block a thread
//! on a read. So everything here is `async`, driven by the page's own event loop
//! through `wasm-bindgen-futures`, and the client's methods are `async` with it.
//!
//! # Where it points
//!
//! A browser cannot spawn `evo-swarm`, so it does not talk to serve directly. It
//! talks to the axum server that served the bundle ([`Target::BasePath`] — a
//! same-origin prefix like `/api/tabs/7f3a`), which holds the bearer token and
//! adds `Authorization` to every request it forwards. That is
//! [`Client::at_base`], and it is the shape the web frontend uses.
//!
//! A [`Target::Loopback`] is also accepted — an absolute `http://host:port/…`
//! URL with the token in a header — for a page served by the very machine that
//! runs serve. It needs that server to send CORS headers, which the real
//! `serve` does not, so it is a capability and not the intended route.
//!
//! # Why not `EventSource`
//!
//! `EventSource` is the browser's own SSE client and would have been less code.
//! It cannot be used here, for two reasons that are both fatal:
//!
//! * it cannot set an `Authorization` header, and this protocol is bearer-token
//!   on every route;
//! * it hides the response status — a 401 and a closed stream look the same to
//!   it — so a tab could not tell "wrong token" from "the server restarted".
//!
//! An authenticated `fetch` whose body is read incrementally has neither
//! problem: the status is in hand before a byte of the body is read, and the
//! request carries whatever headers the protocol needs.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Reflect, Uint8Array};
use serde_json::Value;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, AbortSignal, Headers, ReadableStreamDefaultReader,
    ReadableStreamReadResult, RequestCredentials, RequestInit, Response,
};

use crate::error::{Error, Result, StatusError};
use crate::protocol::{Cursor, OpReply, OpRequest, Snapshot};
use crate::sse::SseParser;
use crate::transport::{request_path, HttpResponse, Target, Token};

/// How much patience a body of `bytes` bytes buys, on top of the base timeout.
///
/// The same model as the native transport's, and for the same reason: serve
/// reads a POST body a few kilobytes a second, so the reply to a turn carrying a
/// pasted screenshot is tens of seconds behind its request, and a client that
/// gives up there reports a refusal for a turn the session has already taken.
/// See [`crate::http`] for the measurements this is drawn from.
const BODY_RATE_FLOOR: u64 = 8 * 1024;

/// The most extra patience one body may earn, whatever it carries.
const BODY_PATIENCE_CAP: Duration = Duration::from_secs(240);

/// One `fetch`, on the page's event loop.
#[derive(Clone, Debug)]
pub struct FetchClient {
    target: Target,
    timeout: Duration,
    stream_timeout: Duration,
}

impl FetchClient {
    pub fn new(target: Target) -> FetchClient {
        FetchClient {
            target,
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
        let url = self.url(route);
        let init = RequestInit::new();
        init.set_method(method);
        // Same-origin: a cookie the proxy set travels, and nothing is sent
        // cross-origin by default. A loopback target is a different origin and
        // the token in the header is what authenticates it.
        init.set_credentials(RequestCredentials::SameOrigin);
        if let Some(payload) = &payload {
            init.set_body(&JsValue::from_str(payload));
        }
        init.set_headers(&self.headers(payload.is_some(), false)?);
        // A deadline, not a delay: the fetch is aborted if it outlives its
        // patience, which is what the native transport's read timeout does.
        let signal = AbortSignal::timeout_with_u32(timeout.as_millis().min(u32::MAX as u128) as u32);
        init.set_signal(Some(&signal));

        let response = fetch(&url, &init).await?;
        let status = response.status();
        let headers = read_headers(&response);
        let body = read_body(&response).await?;
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
    /// The connection is left with an `AbortController` armed: [`FetchSseConnection::close`]
    /// aborts the fetch, which is what unblocks a `read` that is waiting on a
    /// stream nobody is going to write to again.
    pub async fn open_sse(&self, route: &str) -> Result<FetchSseConnection> {
        let url = self.url(route);
        let controller = AbortController::new()
            .map_err(|e| Error::Protocol(format!("no AbortController: {}", describe(&e))))?;
        let init = RequestInit::new();
        init.set_method("GET");
        init.set_credentials(RequestCredentials::SameOrigin);
        init.set_headers(&self.headers(false, true)?);
        init.set_signal(Some(&controller.signal()));

        let response = fetch(&url, &init).await?;
        let status = response.status();
        if !(200..300).contains(&status) {
            let body = read_body(&response).await?;
            let text = String::from_utf8_lossy(&body).into_owned();
            let raw = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
            return Err(Error::Status(StatusError::from_reply(status, raw, &text)));
        }
        let stream = response
            .body()
            .ok_or_else(|| Error::Protocol("the stream has no body".into()))?;
        let reader = stream
            .get_reader()
            .dyn_into::<ReadableStreamDefaultReader>()
            .map_err(|_| Error::Protocol("the stream is not a byte reader".into()))?;
        Ok(FetchSseConnection {
            reader,
            controller,
            buffer: Vec::new(),
            ended: false,
            stream_timeout: self.stream_timeout,
        })
    }

    /// The absolute or same-origin URL one route is fetched at.
    fn url(&self, route: &str) -> String {
        match &self.target {
            // A relative URL: same origin as the page, whatever that is. The
            // prefix is the proxy's, and the route is the protocol's.
            Target::BasePath(_) => request_path(&self.target, route),
            Target::Loopback { host, port, .. } => {
                let host = if host.contains(':') && !host.starts_with('[') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                format!("http://{host}:{port}{route}")
            }
        }
    }

    /// The headers every request carries. `Accept: text/event-stream` for a
    /// stream; `Content-Type` when there is a body; and `Authorization` only
    /// when this client holds a token of its own — a base-path client holds
    /// none, because the proxy that forwards it adds one.
    fn headers(&self, has_body: bool, sse: bool) -> Result<JsValue> {
        let headers = Headers::new()
            .map_err(|e| Error::Protocol(format!("no Headers: {}", describe(&e))))?;
        if let Target::Loopback { token, .. } = &self.target {
            set_header(&headers, "authorization", &format!("Bearer {}", token.as_str()))?;
        }
        if sse {
            set_header(&headers, "accept", "text/event-stream")?;
            // A proxy that buffers would defeat an SSE stream; this asks it not
            // to. Harmless where there is no proxy.
            set_header(&headers, "cache-control", "no-cache")?;
        }
        if has_body {
            set_header(&headers, "content-type", "application/json")?;
        }
        Ok(headers.into())
    }
}

/// Set one header, as an error rather than a panic: a browser refuses some
/// names outright, and a refusal here is a config error, not a crash.
fn set_header(headers: &Headers, name: &str, value: &str) -> Result<()> {
    headers
        .set(name, value)
        .map_err(|e| Error::Protocol(format!("cannot set {name}: {}", describe(&e))))
}

/// `window.fetch`, as a future.
async fn fetch(url: &str, init: &RequestInit) -> Result<Response> {
    let window = web_sys::window()
        .ok_or_else(|| Error::Config("no window: this is not a browser context".into()))?;
    let promise = window.fetch_with_str_and_init(url, init);
    let value = JsFuture::from(promise).await.map_err(fetch_error)?;
    value
        .dyn_into::<Response>()
        .map_err(|_| Error::Protocol("fetch answered something that is not a Response".into()))
}

/// A rejected fetch as an [`Error`].
///
/// A network failure and an abort are the same shape here — a `TypeError`, with
/// the reason on `signal.reason` — and both are transport failures: the
/// connection (rather than the request) is what went wrong. That is the case a
/// stream reconnects on, so it is [`Error::Closed`] and not a protocol error.
fn fetch_error(value: JsValue) -> Error {
    let text = describe(&value);
    if value.is_instance_of::<js_sys::TypeError>() {
        return Error::Closed;
    }
    Error::Protocol(format!("fetch failed: {text}"))
}

/// One JavaScript value as text, for an error message.
fn describe(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            value
                .dyn_ref::<js_sys::Error>()
                .map(|e| String::from(e.message()))
        })
        .unwrap_or_else(|| format!("{value:?}"))
}

/// A reply's headers, lower-cased, as the native transport's `HttpResponse` has
/// them.
fn read_headers(response: &Response) -> Vec<(String, String)> {
    let headers = response.headers();
    let mut out = Vec::new();
    // `entries()` is an iterator of `[name, value]` pairs; a browser folds the
    // name to lower case itself, so there is nothing to normalise.
    if let Ok(iter) = Reflect::get(&headers, &JsValue::from_str("entries")) {
        if let Ok(func) = iter.dyn_into::<js_sys::Function>() {
            if let Ok(entries) = func.call0(&headers) {
                if let Ok(iterator) = entries.dyn_into::<js_sys::Iterator>() {
                    for entry in iterator {
                        let Ok(entry) = entry else { break };
                        let Ok(pair) = entry.dyn_into::<js_sys::Array>() else {
                            continue;
                        };
                        let (Some(name), Some(value)) =
                            (pair.get(0).as_string(), pair.get(1).as_string())
                        else {
                            continue;
                        };
                        out.push((name.to_ascii_lowercase(), value));
                    }
                }
            }
        }
    }
    out
}

/// A reply's whole body.
async fn read_body(response: &Response) -> Result<Vec<u8>> {
    let promise = response
        .array_buffer()
        .map_err(|e| Error::Protocol(format!("cannot read the body: {}", describe(&e))))?;
    let value = JsFuture::from(promise)
        .await
        .map_err(|e| Error::Protocol(format!("the body failed: {}", describe(&e))))?;
    let buffer: js_sys::ArrayBuffer = value
        .dyn_into()
        .map_err(|_| Error::Protocol("the body is not an ArrayBuffer".into()))?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// A live SSE connection, read a chunk at a time.
///
/// The body of an SSE reply arrives in whatever pieces the network and the proxy
/// hand over, and a frame is not aligned to a chunk: one `read()` can end in the
/// middle of a line, or carry three frames and half of a fourth. So the bytes go
/// through a buffer and [`read_line`](Self::read_line) hands back one line at a
/// time, holding the remainder — which is exactly what the native transport gets
/// for free from a `BufReader`.
pub struct FetchSseConnection {
    reader: ReadableStreamDefaultReader,
    controller: AbortController,
    /// Bytes read and not yet handed out as a line.
    buffer: Vec<u8>,
    ended: bool,
    /// The patience for an idle stream: serve keeps one alive with a
    /// `: keepalive` comment every 15 s, so a silence past this is a dead
    /// socket, and the stream reconnects. The native transport gets this from
    /// its socket's read timeout; here it is a timer that aborts the fetch.
    stream_timeout: Duration,
}

impl FetchSseConnection {
    /// The next line, without its terminator; `Ok(None)` at end of stream.
    pub async fn read_line(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(line) = self.take_line() {
                return Ok(Some(line));
            }
            if self.ended {
                // The last line of a body that did not end in a newline.
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.buffer);
                return Ok(Some(lossy(&line)));
            }
            self.read_chunk().await?;
        }
    }

    /// One complete line out of the buffer, if there is one.
    fn take_line(&mut self) -> Option<String> {
        let at = self.buffer.iter().position(|byte| *byte == b'\n')?;
        let mut line: Vec<u8> = self.buffer.drain(..=at).collect();
        while line.last().is_some_and(|byte| *byte == b'\n' || *byte == b'\r') {
            line.pop();
        }
        Some(lossy(&line))
    }

    /// Read one chunk into the buffer, and note the end of the stream.
    ///
    /// The read is bounded: a chunk that never comes is a connection that is
    /// gone, and the timer below aborts the fetch when the patience runs out —
    /// which is what the native transport's socket read timeout does, and what
    /// makes a stream reconnect rather than hang forever on a dead socket.
    async fn read_chunk(&mut self) -> Result<()> {
        let _deadline = self.arm_idle_timeout();
        let promise = self.reader.read();
        let value = JsFuture::from(promise)
            .await
            .map_err(|e| Error::Protocol(format!("the stream failed: {}", describe(&e))))?;
        let result: ReadableStreamReadResult = value
            .dyn_into()
            .map_err(|_| Error::Protocol("a stream read gave no result".into()))?;
        if result.get_done().unwrap_or(true) {
            self.ended = true;
            return Ok(());
        }
        let value = result.get_value();
        let chunk: Uint8Array = value
            .dyn_into()
            .map_err(|_| Error::Protocol("a stream chunk is not bytes".into()))?;
        self.buffer.extend_from_slice(&chunk.to_vec());
        Ok(())
    }

    /// Arm the idle deadline for one read: if no chunk arrives within the
    /// stream's patience, abort the fetch — which ends the stream, and is
    /// reported as a transport failure for the reader to reconnect from.
    fn arm_idle_timeout(&self) -> Option<IdleTimeout> {
        let window = web_sys::window()?;
        let fired = Rc::new(Cell::new(false));
        let controller = self.controller.clone();
        let expired = Rc::clone(&fired);
        let closure = Closure::once(move || {
            if !expired.get() {
                controller.abort();
            }
        });
        let millis = self.stream_timeout.as_millis().min(i32::MAX as u128) as i32;
        let handle = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                millis,
            )
            .ok()?;
        Some(IdleTimeout {
            handle,
            fired,
            _closure: closure,
        })
    }

    /// Unblock a reader on another task: abort the fetch, which ends the stream.
    /// Idempotent, and safe to call from anywhere the handle is.
    pub fn close(&self) {
        self.controller.abort();
    }

    /// The controller whose `signal` is this stream's fetch — a handle that can
    /// be aborted from elsewhere.
    pub fn controller(&self) -> &AbortController {
        &self.controller
    }
}

impl fmt::Debug for FetchSseConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchSseConnection")
            .field("ended", &self.ended)
            .field("buffered", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

/// One read's idle deadline: a timer that aborts the fetch if the read it was
/// armed for has not come back. Dropped when the read does come back, which
/// cancels the timer.
struct IdleTimeout {
    handle: i32,
    fired: Rc<Cell<bool>>,
    /// Held so the JS callback outlives the timer, and released with it.
    _closure: Closure<dyn FnMut()>,
}

impl Drop for IdleTimeout {
    fn drop(&mut self) {
        // Before the timer is cleared, so a callback already queued behind this
        // drop does not abort a fetch that just answered.
        self.fired.set(true);
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(self.handle);
        }
    }
}

/// Bytes as text, lossily — a frame is UTF-8 JSON, and a chunk boundary can fall
/// inside a multi-byte character, which is why the buffer holds bytes and not a
/// `String`.
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A client bound to one server, at one address, with one token — the wasm
/// counterpart of [`crate::client::Client`], and the type a wasm consumer names
/// as `swarm_client::Client`.
///
/// Everything is `async`: a browser has no thread to block.
#[derive(Clone, Debug)]
pub struct Client {
    fetch: FetchClient,
}

impl Client {
    /// A client for a same-origin path prefix with no token — the browser's
    /// shape. Every request goes to `<prefix>/snapshot`, `<prefix>/stream`, …
    /// and the server behind that prefix adds the bearer token itself.
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

    /// The transport underneath, for a caller that needs a route the typed
    /// methods do not cover.
    pub fn fetch(&self) -> &FetchClient {
        &self.fetch
    }

    pub fn target(&self) -> &Target {
        self.fetch.target()
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
    /// The caller reads the lines; [`WasmEventStream`] is the usual one, and it
    /// is the one that knows how to reconnect.
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
    /// The retry is safe because `rid` is idempotent: the server remembers the
    /// last 256 replies by rid, so a request that was answered into a connection
    /// that died is answered again rather than acted on twice.
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
/// time, resuming from the last cursor it saw, parking when the server asks for
/// a full re-read.
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

/// The stream itself: one per `start`, shared by every handle to it. When the
/// last handle goes, the reader stops.
pub struct StreamHandle {
    messages: async_channel::Receiver<StreamMsg>,
    orders: async_channel::Sender<Order>,
    /// The cursor last seen, shared so a consumer can read it without asking the
    /// task.
    cursor: Rc<RefCell<Option<Cursor>>>,
    /// The connection in flight, so `stop` can abort it: a read waiting on a
    /// stream nobody will write to again would otherwise wait out its idle
    /// timeout. This is the native handle's socket shutdown, on a platform that
    /// has no socket.
    abort: Rc<RefCell<Option<AbortController>>>,
    stopped: Rc<Cell<bool>>,
}

impl StreamHandle {
    /// Stop reading: the flag ends a backoff, the order ends a parked stream, and
    /// aborting the fetch ends a read that is in flight.
    fn stop_stream(&self) {
        self.stopped.set(true);
        let _ = self.orders.try_send(Order::Stop);
        // The borrow ends here, before the call: `if let Some(c) = self.abort.borrow().as_ref()`
        // holds the `Ref` for the whole body, so `abort()` would run while `abort`
        // was still borrowed — and a re-entrant `abort()` panics with
        // `already borrowed`. Cloning the controller first is what scopes it.
        let controller = self.abort.borrow().clone();
        if let Some(controller) = controller {
            controller.abort();
        }
    }

    /// The next message, awaited; `None` once the stream has stopped and its
    /// last message has been read.
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
    /// Start reading. The task ends when the stream does — on `stop`, or when
    /// the last handle is dropped.
    pub fn start(client: Client, config: StreamConfig) -> WasmEventStream {
        let (messages_tx, messages) = async_channel::bounded(1024);
        let (orders, orders_rx) = async_channel::bounded(8);
        let cursor = Rc::new(RefCell::new(config.cursor.clone()));
        let abort = Rc::new(RefCell::new(None));
        let stopped = Rc::new(Cell::new(false));
        let stream = WasmEventStream(Rc::new(StreamHandle {
            messages,
            orders,
            cursor: Rc::clone(&cursor),
            abort: Rc::clone(&abort),
            stopped: Rc::clone(&stopped),
        }));
        wasm_bindgen_futures::spawn_local(run(
            client, config, messages_tx, orders_rx, cursor, abort, stopped,
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
    abort: Rc<RefCell<Option<AbortController>>>,
    stopped: Rc<Cell<bool>>,
) {
    let mut attempt: u32 = 0;
    loop {
        if stopped.get() {
            break;
        }
        let since = cursor.borrow().clone();
        match read(
            &client,
            &config.topics,
            since,
            &messages,
            &cursor,
            &abort,
            &stopped,
        )
        .await
        {
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
    abort: &Rc<RefCell<Option<AbortController>>>,
    stopped: &Rc<Cell<bool>>,
) -> Result<Ended> {
    let mut connection = client.open_stream(topics, since.as_ref()).await?;
    // The handle `stop` aborts from anywhere; cleared when this connection ends.
    *abort.borrow_mut() = Some(connection.controller().clone());
    let mut parser = SseParser::new();
    let ended = loop {
        if stopped.get() || messages.is_closed() {
            break Ended::Disconnected;
        }
        let line = match connection.read_line().await {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => break Ended::Disconnected,
        };
        let Some(event) = parser.feed(&line) else {
            continue;
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
    *abort.borrow_mut() = None;
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
            let window = web_sys::window();
            if let Some(window) = window {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    resolve.unchecked_ref(),
                    step as i32,
                );
            }
        });
        let _ = JsFuture::from(promise).await;
        left -= step;
    }
    !stopped.get()
}

/// A token this client holds, or `None` when it holds none.
impl FetchClient {
    pub fn token(&self) -> Option<&Token> {
        match &self.target {
            Target::Loopback { token, .. } => Some(token),
            Target::BasePath(_) => None,
        }
    }
}
