//! The client for one serve process: one snapshot, one stream, and ops
//! (CONTRACT.md §5).
//!
//! There is no session state here — no transcript, no lanes, no revisions. The
//! client sends bytes and hands back what came; the `session` crate decides what
//! they mean.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::{HttpClient, HttpResponse, SseConnection, Token};
use crate::protocol::{Cursor, OpReply, OpRequest, Snapshot};

/// A client bound to one server, at one address, with one token.
#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
}

impl Client {
    /// A client for a server on the loopback interface. Any other host is
    /// refused: the protocol has no TLS and its trust boundary is the loopback
    /// interface.
    pub fn loopback(port: u16, token: impl Into<String>) -> Result<Client> {
        Client::at("127.0.0.1", port, token)
    }

    /// A client for a named host, which must be loopback.
    pub fn at(host: &str, port: u16, token: impl Into<String>) -> Result<Client> {
        Ok(Client {
            http: HttpClient::new(host, port, Token::new(token))?,
        })
    }

    /// The same server, with a different patience — a shutdown request gets a
    /// short one, so a wedged server reaches the next rung of the ladder.
    pub fn with_timeout(&self, timeout: Duration) -> Client {
        Client {
            http: self.http.with_timeout(timeout),
        }
    }

    /// The patience for a silent stream.
    pub fn with_stream_timeout(&self, timeout: Duration) -> Client {
        Client {
            http: self.http.with_stream_timeout(timeout),
        }
    }

    /// The transport underneath, for a caller that needs a route the typed
    /// methods do not cover (the test harness's control endpoints).
    #[cfg_attr(not(feature = "test-harness"), allow(dead_code))]
    pub(crate) fn http(&self) -> &HttpClient {
        &self.http
    }

    pub fn port(&self) -> u16 {
        self.http.port()
    }

    /// `GET /snapshot?topics=…&items=N` — the whole view, atomically.
    ///
    /// `items` of `None` leaves the server's own default (200).
    pub fn snapshot(&self, topics: &[String], items: Option<u32>) -> Result<Snapshot> {
        let mut path = format!("/snapshot?topics={}", encode_list(topics));
        if let Some(items) = items {
            path.push_str(&format!("&items={items}"));
        }
        let body = self.get_json(&path)?;
        Ok(serde_json::from_value(body)?)
    }

    /// Open the op stream (§5.3) from `since`, or from now when it is `None`.
    ///
    /// The caller reads the lines; [`crate::stream::EventStream`] is the usual
    /// one, and it is the one that knows how to reconnect.
    pub fn open_stream(&self, topics: &[String], since: Option<&Cursor>) -> Result<SseConnection> {
        let mut path = format!("/stream?topics={}", encode_list(topics));
        if let Some(cursor) = since {
            path.push_str(&format!("&since={}", cursor.encode()));
        }
        self.http.open_sse(&path)
    }

    /// `POST /ops` (§5.5) — one transparent retry on a connection failure.
    ///
    /// The retry is safe because `rid` is idempotent: the server remembers the
    /// last 256 replies by rid, so a request that was answered into a socket
    /// that died is answered again rather than acted on twice.
    pub fn op(&self, op: &str, args: Value) -> Result<OpReply> {
        let rid = new_rid();
        self.op_with_rid(&rid, op, args)
    }

    /// The same, with a caller-chosen rid — a caller retrying its own request
    /// after something other than an HTTP failure.
    pub fn op_with_rid(&self, rid: &str, op: &str, args: Value) -> Result<OpReply> {
        let request = serde_json::to_value(OpRequest::new(rid, op, args))?;
        let body = match self.http.post("/ops", &request) {
            Ok(response) => response,
            Err(e) if e.is_transport() => {
                // The connection died; the request may or may not have arrived.
                // The rid makes asking again free.
                self.http.post("/ops", &request)?
            }
            Err(e) => return Err(e),
        };
        if !body.is_success() {
            return Err(Error::Status(body.error()));
        }
        Ok(serde_json::from_slice(&body.body)?)
    }

    /// `server.shutdown` — the first rung of the ladder. A refusal is not an
    /// error: something is already stopping.
    pub fn shutdown(&self) -> Result<()> {
        self.op("server.shutdown", Value::Object(serde_json::Map::new()))?;
        Ok(())
    }

    /// `GET /catalog` (§5.6) — the live registry, untyped.
    pub fn catalog(&self) -> Result<Value> {
        self.get_json("/catalog")
    }

    /// `GET /items?topic=&before=&limit=` (§5.4) — older items, untyped.
    pub fn items(&self, topic: &str, before: Option<&str>, limit: u32) -> Result<Value> {
        let mut path = format!("/items?topic={}&limit={limit}", encode_value(topic));
        if let Some(before) = before {
            path.push_str(&format!("&before={}", encode_value(before)));
        }
        self.get_json(&path)
    }

    /// `GET /items/<id>?topic=` — one item, whole (thinking and tool output
    /// included).
    pub fn item(&self, topic: &str, id: &str) -> Result<Value> {
        self.get_json(&format!(
            "/items/{}?topic={}",
            encode_value(id),
            encode_value(topic)
        ))
    }

    /// `GET /media/<id>/<n>?topic=` — image bytes and their content type.
    pub fn media(&self, topic: &str, id: &str, n: u32) -> Result<(Vec<u8>, String)> {
        let response = self.http.get(&format!(
            "/media/{}/{n}?topic={}",
            encode_value(id),
            encode_value(topic)
        ))?;
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
    pub fn get_json(&self, path: &str) -> Result<Value> {
        let response = self.http.get(path)?;
        if !response.is_success() {
            return Err(Error::Status(response.error()));
        }
        Ok(serde_json::from_slice(&response.body)?)
    }

    /// One `GET`, the whole reply.
    pub fn get(&self, path: &str) -> Result<HttpResponse> {
        let response = self.http.get(path)?;
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

/// Percent-encode everything that is not unreserved. Topic names and item ids
/// are terse (`session`, `lane:3`, `e_9c1f`), so this costs nothing and no query
/// can be broken by a name.
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

/// A request id, unique to this process and this instant: the server dedupes
/// retries by rid, so it only has to be *unique*, not a real uuid.
///
/// Public because a caller that wants to recognise its own reply mints the rid
/// itself and uses [`Client::op_with_rid`].
pub fn new_rid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{nanos:x}-{n:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_host_is_refused() {
        assert!(Client::at("10.0.0.1", 9, "t").is_err());
        assert!(Client::loopback(9, "t").is_ok());
    }

    #[test]
    fn rids_are_unique() {
        let first = new_rid();
        let second = new_rid();
        assert_ne!(first, second);
        assert!(!first.contains(' '));
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(encode_value("lane:3"), "lane%3A3");
        assert_eq!(encode_value("session"), "session");
        assert_eq!(
            encode_list(&["session".into(), "lane:1".into()]),
            "session,lane%3A1"
        );
    }
}
