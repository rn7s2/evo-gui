//! The transport seam: what a client needs from a platform, and the types that
//! are the same on every platform (CONTRACT.md §5).
//!
//! A `serve` speaks one small HTTP/1.1 protocol. Talking to it needs exactly
//! three things from the platform underneath:
//!
//! 1. **one request, its reply** — [`HttpResponse`], from `HttpClient` on a
//!    native build and `FetchClient` on a wasm one;
//! 2. **a stream read incrementally** — an SSE connection whose next line can be
//!    asked for as it arrives;
//! 3. **a handle that can be closed** — a stream must be cancellable from
//!    somewhere other than the thread reading it, or a stop waits out a read
//!    timeout.
//!
//! Those three are what a *transport* is here, and the two implementations live
//! beside this module: [`crate::http`] (native: a blocking client on
//! `std::net::TcpStream`, read on a thread of its own) and [`crate::fetch`]
//! (wasm: `fetch` for a request, a `ReadableStream` read chunk by chunk for a
//! stream). They are deliberately **not** one trait: a browser cannot block, so
//! the wasm half is `async` where the native half is not, and a single trait
//! would have to be one or the other and a lie either way. What they do share is
//! this module's types, the paths they are given, the errors they raise, and the
//! contract in the paragraphs above.
//!
//! # Where a client points
//!
//! The protocol is reachable in two shapes, and a [`Target`] is which:
//!
//! * **loopback** — `127.0.0.1:<port>` and the bearer token the server minted in
//!   its ready file. What the desktop app's tabs use, and the only shape the
//!   native transport can speak: the protocol has no TLS, so its trust boundary
//!   is the loopback interface and any other host is refused.
//! * **a base path** — `/api/tabs/7f3a`, same origin, **no token**. What a
//!   browser uses: it cannot spawn a server, so it talks to the axum server that
//!   served it, which holds the credential and adds `Authorization: Bearer …`
//!   to everything it forwards.
//!
//! The native transport can only speak the first, and says so rather than
//! pretending — see `HttpClient::new`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::error::{Error, Result, StatusError};

/// A bearer token. Its `Debug` never prints the secret (§2.4: never log it).
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(text: impl Into<String>) -> Token {
        Token(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where a client points: the two shapes the protocol is reachable in.
#[derive(Clone, Debug)]
pub enum Target {
    /// A server on this machine, at the port its ready file announced, with the
    /// bearer token it minted. The desktop app's shape.
    Loopback {
        host: String,
        port: u16,
        token: Token,
    },
    /// A same-origin path prefix — `/api/tabs/7f3a` — with no token of its own.
    /// The browser's shape: a proxy holds the credential and adds it to every
    /// request it forwards.
    BasePath(String),
}

impl Target {
    /// A target for a server on the loopback interface, refusing any other host.
    pub fn loopback(port: u16, token: impl Into<String>) -> Result<Target> {
        Target::at("127.0.0.1", port, token)
    }

    /// A target for a named host, which must be loopback.
    pub fn at(host: &str, port: u16, token: impl Into<String>) -> Result<Target> {
        if !is_loopback(host) {
            return Err(Error::Config(format!(
                "refusing to talk to {host:?}: only loopback servers are supported"
            )));
        }
        Ok(Target::Loopback {
            host: host.to_owned(),
            port,
            token: Token::new(token),
        })
    }

    /// A target for a same-origin path prefix, with no token.
    ///
    /// The path is normalised: a missing leading slash is added and a trailing
    /// one dropped, so `/api/tabs/1/` and `api/tabs/1` are one prefix. A prefix
    /// that is not a path — an absolute URL, a scheme, a `..` step — is refused:
    /// this target is only ever *this* origin, and a value that could address
    /// another one is a value this transport will not take.
    pub fn base_path(prefix: &str) -> Result<Target> {
        let trimmed = prefix.trim();
        if trimmed.contains("://") || trimmed.starts_with("//") {
            return Err(Error::Config(format!(
                "refusing {prefix:?}: a base path is same-origin, not a URL"
            )));
        }
        if trimmed.split('/').any(|segment| segment == "..") {
            return Err(Error::Config(format!(
                "refusing {prefix:?}: a base path may not step out of itself"
            )));
        }
        let path = trimmed.trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        if path == "/" || path.is_empty() {
            return Err(Error::Config(
                "a base path needs a prefix, not the origin root".into(),
            ));
        }
        Ok(Target::BasePath(path))
    }

    /// The path prefix this target requests under, or `None` for a loopback
    /// target, whose requests go to the server root.
    pub fn base_path_prefix(&self) -> Option<&str> {
        match self {
            Target::Loopback { .. } => None,
            Target::BasePath(path) => Some(path),
        }
    }
}

/// Whether a host names this machine. Everything else is refused: there is no
/// TLS and serve's trust boundary is the loopback interface.
pub fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// A request id, unique to this process and this instant: the server dedupes
/// retries by rid, so it only has to be *unique*, not a real uuid.
///
/// Public because a caller that wants to recognise its own reply mints the rid
/// itself and uses `Client::op_with_rid`. Both platforms mint one the same way;
/// only the clock underneath differs, and a browser has `Date.now` where a host
/// has a system clock.
pub fn new_rid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{n:x}", process_tag(), now_nanos())
}

/// A number that tells this process from another. A browser has one process and
/// no pid to read, so it says so with a zero.
#[cfg(not(target_family = "wasm"))]
fn process_tag() -> u32 {
    std::process::id()
}

#[cfg(target_family = "wasm")]
fn process_tag() -> u32 {
    0
}

/// Nanoseconds since the Unix epoch, from whatever clock the platform has.
#[cfg(not(target_family = "wasm"))]
fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `Date.now()` — milliseconds, which is the resolution the page has.
#[cfg(target_family = "wasm")]
fn now_nanos() -> u128 {
    (js_sys::Date::now() as u128) * 1_000_000
}

/// The path one protocol route is requested at, given a target.
///
/// A loopback target asks the server root — `/snapshot` — because it is talking
/// to serve itself. A base-path target asks the same route under its prefix —
/// `/api/tabs/7f3a/snapshot` — because it is talking to the proxy that forwards
/// it. `route` is a protocol path (`/snapshot`, `/ops`, `/media/<id>/0?topic=`)
/// and is never a full URL, so this is a concatenation and not a resolution.
pub fn request_path(target: &Target, route: &str) -> String {
    match target {
        Target::Loopback { .. } => route.to_owned(),
        Target::BasePath(prefix) => format!("{prefix}{route}"),
    }
}

/// One reply, whole.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == want)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(Error::from)
    }

    /// This reply as a typed error, for a status the client did not expect.
    pub fn error(&self) -> StatusError {
        let text = self.text();
        let raw = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        StatusError::from_reply(self.status, raw, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_host_is_refused() {
        assert!(Target::at("10.0.0.1", 9, "t").is_err());
        assert!(Target::loopback(9, "t").is_ok());
        assert!(Target::at("localhost", 9, "t").is_ok());
    }

    #[test]
    fn a_base_path_is_normalised_to_one_prefix() {
        assert_eq!(
            Target::base_path("/api/tabs/1").unwrap().base_path_prefix(),
            Some("/api/tabs/1")
        );
        assert_eq!(
            Target::base_path("api/tabs/1/").unwrap().base_path_prefix(),
            Some("/api/tabs/1")
        );
        assert_eq!(
            Target::base_path("  /api/tabs/1  ")
                .unwrap()
                .base_path_prefix(),
            Some("/api/tabs/1")
        );
    }

    #[test]
    fn a_base_path_that_could_address_another_origin_is_refused() {
        assert!(Target::base_path("https://elsewhere/api").is_err());
        assert!(Target::base_path("//elsewhere/api").is_err());
        assert!(Target::base_path("/api/../../etc").is_err());
        assert!(Target::base_path("/").is_err());
        assert!(Target::base_path("").is_err());
    }

    #[test]
    fn a_loopback_target_has_no_prefix() {
        let target = Target::loopback(8421, "t").unwrap();
        assert_eq!(target.base_path_prefix(), None);
    }

    /// A browser asks the proxy under its prefix; the desktop app asks serve
    /// itself. Same route, same protocol, two shapes — and the shape is the only
    /// thing that differs.
    #[test]
    fn a_route_goes_under_the_prefix_or_to_the_root() {
        let proxied = Target::base_path("/api/tabs/7f3a").unwrap();
        assert_eq!(
            request_path(&proxied, "/snapshot?topics=session"),
            "/api/tabs/7f3a/snapshot?topics=session"
        );
        assert_eq!(
            request_path(&proxied, "/media/e_1/0?topic=session"),
            "/api/tabs/7f3a/media/e_1/0?topic=session"
        );

        let local = Target::loopback(8421, "t").unwrap();
        assert_eq!(request_path(&local, "/snapshot"), "/snapshot");
        assert_eq!(request_path(&local, "/ops"), "/ops");
    }

    #[test]
    fn rids_are_unique() {
        let first = new_rid();
        let second = new_rid();
        assert_ne!(first, second);
        assert!(!first.contains(' '));
    }

    #[test]
    fn a_token_never_prints_itself() {
        let token = Token::new("Zm9vYmFy");
        assert!(!format!("{token:?}").contains("Zm9vYmFy"));
        assert!(!format!("{token}").contains("Zm9vYmFy"));
        assert_eq!(format!("{token:?}"), "Token(<redacted>)");
    }
}
