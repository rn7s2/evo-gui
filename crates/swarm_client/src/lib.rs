//! swarm_client — the evo-desktop client for a serve process, pure Rust, no gpui.
//!
//! One tab of the app is one `evo-swarm serve` in a chosen folder. This crate
//! owns that server's whole lifecycle and the HTTP/SSE protocol in front of it
//! (docs/PROMPT.md §3–§5, §9.4):
//!
//! - [`server`] — spawn (exact argv, environment, port), readiness, the shutdown
//!   ladder, and the log tail a boot failure shows.
//! - [`http`] — a blocking HTTP/1.1 client: one request per connection
//!   (`Connection: close`), loopback only, bearer token, redacted in `Debug`.
//! - [`api`] — typed payloads for `/health`, `/state`, `/transcript`, `/registry`,
//!   `/journal`, `/lanes`, a lane's transcript, and the `POST` reply envelope —
//!   each with the raw `serde_json::Value` beside it.
//! - [`stream`] — the resumable SSE reader on its own thread, delivering
//!   `Connected`/`Disconnected`/`Event`/`Reset`/`Ended` into an `async-channel`.
//! - [`probe`] — `learn_registry`, the throwaway `evo-agent serve` of §9.4.
//! - [`redact`] — the credentials a server's own text must not carry into a log
//!   or onto the screen; applied to every error and log tail below.
//! - `harness` (feature `test-harness`) — a temp `HOME` with a stub provider and
//!   a live swarm, for this crate's tests and any other crate's.
//!
//! ```no_run
//! use swarm_client::{Client, EventStream, Server, ServerConfig, StreamConfig, StreamMsg, StreamTarget};
//!
//! let mut server = Server::start(&ServerConfig::swarm(
//!     "/usr/local/bin/evo-swarm", "/path/to/project", std::path::Path::new("/path/to/tab"),
//! ))?;
//! let client = server.client().clone();
//! println!("{:?}", client.state()?.status);
//!
//! let mut stream = EventStream::start(StreamTarget::coordinator(&client, Some(0)), StreamConfig::default());
//! while let Some(message) = stream.recv_blocking() {
//!     if let StreamMsg::Event { kind, .. } = &message {
//!         println!("{kind}");
//!     }
//! }
//! server.shutdown()?;
//! # Ok::<(), swarm_client::Error>(())
//! ```

pub mod api;
pub mod error;
pub mod http;
pub mod probe;
pub mod redact;
pub mod server;
pub mod sse;
pub mod stream;

#[cfg(feature = "test-harness")]
pub mod harness;

pub use api::{
    Activity, Choice, Choices, Client, Command, Envelope, Goal, Health, Journal, Lane, LaneState,
    Lanes, Language, Model, OutputLine, OutputStyle, Payload, Provider, Registry, Skill, State,
    SwarmSummary, TaskInfo, Todo, TodoKind, Tool, Transcript,
};
pub use error::{BootFailure, Error, RequestError, Result, StatusError};
pub use http::{HttpClient, HttpResponse, SseConnection, Token, is_loopback};
pub use probe::{learn_registry, learn_registry_with};
pub use redact::{MASK, redact, redact_json};
pub use server::{
    BootCancel, Readiness, Resume, SCRUB_ENV, Server, ServerConfig, Shutdown, ShutdownOutcome,
    log_tail, process_alive,
};
pub use sse::{SseEvent, SseParser};
pub use stream::{
    CursorProbe, EventStream, ResetReason, StreamConfig, StreamMsg, StreamTarget,
};

/// The installed swarm binary, unless `EVO_SWARM_BIN` says otherwise.
pub fn default_swarm_bin() -> std::path::PathBuf {
    std::env::var_os("EVO_SWARM_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/evo-swarm"))
}

/// The installed agent binary, unless `EVO_AGENT_BIN` says otherwise.
pub fn default_agent_bin() -> std::path::PathBuf {
    std::env::var_os("EVO_AGENT_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/evo-agent"))
}
