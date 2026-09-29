//! Errors: transport, protocol, and the typed HTTP status mapping of docs/serve.md.

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde_json::Value;

use crate::api::Envelope;
use crate::redact::{redact, redact_json};
use crate::server::ShutdownOutcome;

pub type Result<T> = std::result::Result<T, Error>;

/// Anything that can go wrong talking to a server, starting one, or stopping it.
#[derive(Debug)]
pub enum Error {
    /// The socket, the process, the file.
    Io(io::Error),
    /// A reply that is not the HTTP/1.1 serve writes.
    Protocol(String),
    /// A reply body that is not the JSON it claimed to be.
    Json(serde_json::Error),
    /// A non-2xx reply, typed by status.
    Status(StatusError),
    /// A deadline passed.
    Timeout(String),
    /// A server that never became ready, with the tail of its log.
    Boot(Box<BootFailure>),
    /// A boot a caller aborted, and how the process was stopped.
    Cancelled(ShutdownOutcome),
    /// The thing we were talking to went away.
    Closed,
    /// Our own call, with something wrong in it.
    Config(String),
}

impl Error {
    /// The typed status, for a reply the server refused.
    pub fn status(&self) -> Option<&StatusError> {
        match self {
            Error::Status(s) => Some(s),
            _ => None,
        }
    }

    /// Whether a caller's own [`BootCancel`](crate::server::BootCancel) stopped
    /// this boot, and how the process went.
    pub fn cancelled(&self) -> Option<ShutdownOutcome> {
        match self {
            Error::Cancelled(outcome) => Some(*outcome),
            _ => None,
        }
    }

    /// `409 Not now` — the session is busy and nothing happened (docs/serve.md).
    pub fn is_not_now(&self) -> bool {
        matches!(self, Error::Status(StatusError::NotNow(_)))
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Status(StatusError::Unauthorized(_)))
    }

    /// Whether the connection (rather than the request) failed: the case a
    /// stream reconnects on.
    pub fn is_transport(&self) -> bool {
        matches!(self, Error::Io(_) | Error::Protocol(_) | Error::Timeout(_) | Error::Closed)
    }

    /// The log tail a boot failure carries.
    pub fn log_tail(&self) -> Option<&str> {
        match self {
            Error::Boot(b) => Some(&b.log_tail),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Protocol(m) => write!(f, "protocol: {m}"),
            Error::Json(e) => write!(f, "json: {e}"),
            Error::Status(s) => write!(f, "{s}"),
            Error::Timeout(m) => write!(f, "timeout: {m}"),
            Error::Boot(b) => write!(f, "boot failed: {}", b.message),
            Error::Cancelled(outcome) => write!(f, "boot cancelled ({outcome:?})"),
            Error::Closed => write!(f, "connection closed"),
            Error::Config(m) => write!(f, "config: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::Json(e)
    }
}

/// A server that did not come up.
#[derive(Clone, Debug)]
pub struct BootFailure {
    /// What was wrong, phrased for a person.
    pub message: String,
    /// The last lines of the server's log — what the tab shows (§3).
    pub log_tail: String,
    pub log_path: Option<PathBuf>,
    /// The exit code, when it exited on its own.
    pub exit_code: Option<i32>,
}

/// What a non-2xx reply was, with the reply itself when it parsed.
#[derive(Clone, Debug)]
pub struct RequestError {
    pub status: u16,
    /// The reply's `error` field, else its body.
    pub message: String,
    /// The POST envelope, when the body was one (serve writes one for a
    /// refusal; GET errors carry only `ok`/`error`).
    pub envelope: Option<Envelope>,
    /// The body as it arrived, always.
    pub raw: Value,
}

/// The statuses of docs/serve.md, one variant each.
///
/// The payload is boxed: a `RequestError` carries the whole reply, and an
/// unboxed one makes every `Result<_, Error>` in the crate 300+ bytes wide.
#[derive(Clone, Debug)]
pub enum StatusError {
    /// 400 — bad request: malformed JSON, a missing field, a bad argument.
    BadRequest(Box<RequestError>),
    /// 401 — missing or wrong bearer token.
    Unauthorized(Box<RequestError>),
    /// 404 — no such endpoint, command, session or entry.
    NotFound(Box<RequestError>),
    /// 409 — **not now**: the session is busy; nothing happened.
    NotNow(Box<RequestError>),
    /// 405 — the path exists, the method is wrong.
    MethodNotAllowed(Box<RequestError>),
    /// 422 — the command ran and failed.
    Unprocessable(Box<RequestError>),
    /// 503 — the server is shutting down.
    ShuttingDown(Box<RequestError>),
    /// Anything else, the status kept.
    Other(Box<RequestError>),
}

impl StatusError {
    /// Type a reply by its status: the mapping of docs/serve.md §Statuses.
    ///
    /// The reply is **redacted first**, message and body alike: a refusal quotes
    /// what caused it, and what caused it can be the user's own configuration —
    /// the real `/registry` 500 echoes an MCP bearer token (docs/proofs-real.md
    /// R1). Everything downstream (the app's log, the empty tab's caption, a
    /// boot failure) reads these two fields, so this is the one place that has
    /// to get it right.
    pub fn from_reply(status: u16, raw: Value, text: &str) -> StatusError {
        let mut raw = raw;
        redact_json(&mut raw);
        let message = raw
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let clipped: String = text.trim().chars().take(400).collect();
                redact(&clipped).into_owned()
            });
        let envelope = serde_json::from_value::<Envelope>(raw.clone()).ok();
        let reply = Box::new(RequestError { status, message, envelope, raw });
        match status {
            400 => StatusError::BadRequest(reply),
            401 => StatusError::Unauthorized(reply),
            404 => StatusError::NotFound(reply),
            405 => StatusError::MethodNotAllowed(reply),
            409 => StatusError::NotNow(reply),
            422 => StatusError::Unprocessable(reply),
            503 => StatusError::ShuttingDown(reply),
            _ => StatusError::Other(reply),
        }
    }

    pub fn status(&self) -> u16 {
        self.request().status
    }

    pub fn request(&self) -> &RequestError {
        match self {
            StatusError::BadRequest(r)
            | StatusError::Unauthorized(r)
            | StatusError::NotFound(r)
            | StatusError::NotNow(r)
            | StatusError::MethodNotAllowed(r)
            | StatusError::Unprocessable(r)
            | StatusError::ShuttingDown(r)
            | StatusError::Other(r) => r,
        }
    }

    /// What the server said, or the body when it said nothing.
    pub fn message(&self) -> &str {
        &self.request().message
    }

    pub fn is_not_now(&self) -> bool {
        matches!(self, StatusError::NotNow(_))
    }
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http {}: {}", self.status(), self.request().message)
    }
}

impl std::error::Error for StatusError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Result<_, Error>` in the crate is this wide, so the refusal
    /// payload is boxed: clippy's `result_large_err` thresholds at 128 bytes.
    #[test]
    fn the_error_stays_small() {
        assert!(
            std::mem::size_of::<Error>() <= 128,
            "Error is {} bytes",
            std::mem::size_of::<Error>()
        );
        assert!(std::mem::size_of::<StatusError>() <= 64, "{}", std::mem::size_of::<StatusError>());
    }

    #[test]
    fn a_reply_is_typed_by_its_status() {
        let raw = serde_json::json!({ "ok": false, "error": "no run to steer" });
        let not_now = StatusError::from_reply(409, raw.clone(), "no run to steer");
        assert!(not_now.is_not_now());
        assert_eq!(not_now.status(), 409);
        assert_eq!(not_now.message(), "no run to steer");
        assert_eq!(not_now.request().status, 409);

        let missing = StatusError::from_reply(404, raw, "no run to steer");
        assert!(!missing.is_not_now());
        assert_eq!(missing.status(), 404);
    }

    #[test]
    fn a_cancelled_boot_says_how_it_went() {
        let error = Error::Cancelled(ShutdownOutcome::Terminated);
        assert_eq!(error.cancelled(), Some(ShutdownOutcome::Terminated));
        assert_eq!(Error::Closed.cancelled(), None);
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn a_refusal_is_redacted_before_it_is_stored() {
        // The real /registry 500, its token replaced by a squib of the same
        // shape. Both things a caller can read — `message()` and the raw body —
        // must come out masked, because the app logs one and shows the other.
        let text = "{\"ok\":false,\"error\":\"The value\\n  \\\"Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2\\\"\\nis not of type\\n  LIST\"}";
        let raw: Value = serde_json::from_str(text).unwrap();
        let error = StatusError::from_reply(500, raw, text);
        assert_eq!(error.status(), 500);
        assert!(error.message().contains("<redacted>"), "{}", error.message());
        assert!(!error.message().contains("Zm9vYmFy"), "{}", error.message());
        assert!(!error.request().raw.to_string().contains("Zm9vYmFy"));
        assert!(!error.to_string().contains("Zm9vYmFy"));
        assert!(error.message().contains("is not of type"));
    }

    #[test]
    fn a_body_that_is_not_json_is_redacted_too() {
        let text = "Internal Server Error: Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2 is not of type LIST";
        let error = StatusError::from_reply(500, Value::String(text.to_string()), text);
        assert!(error.message().contains("<redacted>"), "{}", error.message());
        assert!(!error.message().contains("Zm9vYmFy"));
    }
}
