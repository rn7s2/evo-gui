//! swarm_client — the evo-desktop client for one `serve` process, pure Rust, no
//! gpui (CONTRACT.md §1, §5).
//!
//! One tab of the app is one `evo-swarm serve` in a chosen folder. This crate
//! owns that process's whole lifecycle and the protocol in front of it, and
//! nothing about what the bytes mean — items, states and mirrors belong to the
//! `session` crate:
//!
//! - [`protocol`] — the wire types: the ready file, the cursor, a stream frame,
//!   the snapshot envelope, the `POST /ops` request/reply, the catalog.
//! - [`client`] — snapshot, stream, ops (with `rid`, so a retry is free).
//! - [`stream`] — the SSE reader on its own thread, reconnecting with a backoff
//!   and parking when the server asks for a full re-read.
//! - [`server`] — spawn (stdin a pipe we hold, `--port 0`), the ready file, and
//!   the shutdown ladder.
//! - [`http`] / [`sse`] / [`redact`] — the transport, its framing, and the
//!   masking every error and log tail goes through.
//! - `harness` (feature `test-harness`) — a fake serve, so this crate and its
//!   dependents can drive a real client against the protocol without evo-agent.
//!
//! ```no_run
//! use swarm_client::{Client, EventStream, Server, ServerConfig, StreamConfig, StreamMsg};
//!
//! let server = Server::start(&ServerConfig::swarm(
//!     "/usr/local/bin/evo-swarm", "/path/to/project", std::path::Path::new("/path/to/tab"),
//! ))?;
//! let client = server.client();
//! let snapshot = client.snapshot(&["session".to_owned(), "swarm".to_owned()], None)?;
//! println!("{} topics from epoch {}", snapshot.topics.as_object().map_or(0, |t| t.len()), snapshot.epoch);
//!
//! let stream = EventStream::start(client.clone(), StreamConfig::new(["session", "swarm"]));
//! while let Some(message) = stream.recv() {
//!     if let StreamMsg::Frame(frame) = &message {
//!         println!("{}", frame.op);
//!     }
//! }
//! # Ok::<(), swarm_client::Error>(())
//! ```

pub mod client;
pub mod error;
pub mod http;
pub mod protocol;
pub mod redact;
pub mod server;
pub mod sse;
pub mod stream;

#[cfg(feature = "test-harness")]
pub mod harness;

pub use client::{new_rid, Client};
pub use error::{BootFailure, Error, RequestError, Result, StatusError};
pub use http::{is_loopback, HttpClient, HttpResponse, SseConnection, Token};
pub use protocol::{
    lane_number, lane_topic, op_name, Cursor, ErrorCode, OpError, OpReply, OpRequest, ReadyFile,
    SessionRef, Snapshot, StreamFrame, StreamResetReason, TopicResetReason, TOPIC_LANE_WILDCARD,
    TOPIC_SESSION, TOPIC_SWARM,
};
pub use server::{
    log_tail, process_alive, BootCancel, Server, ServerConfig, Shutdown, ShutdownOutcome,
    StdinClose, SCRUB_ENV,
};
pub use sse::{SseEvent, SseParser};
pub use stream::{Backoff, EventStream, StreamConfig, StreamMsg};

/// The installed swarm binary, unless `EVO_SWARM_BIN` says otherwise.
pub fn default_swarm_bin() -> std::path::PathBuf {
    std::env::var_os("EVO_SWARM_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/evo-swarm"))
}
