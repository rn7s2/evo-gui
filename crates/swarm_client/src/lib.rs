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
//!   and parking when the server asks for a full re-read. **Native only.**
//! - [`server`] — spawn (stdin a pipe we hold, `--port 0`), the ready file, and
//!   the shutdown ladder. **Native only**: a browser has no process to spawn.
//! - [`transport`] — the seam: the types and the contract a platform has to
//!   meet, and where a client points ([`Target`]). Neutral.
//! - [`http`] — the native transport, on a socket. [`fetch`] — the browser's,
//!   on `fetch`. One of the two is compiled, by target.
//! - [`sse`] / [`redact`] — the framing, and the masking every error and log
//!   tail goes through. Both pure.
//! - `harness` (feature `test-harness`) — a fake serve, so this crate and its
//!   dependents can drive a real client against the protocol without evo-agent.
//!
//! # Two platforms, one protocol
//!
//! The protocol is the same on both, and the client's typed methods are the same
//! names — but a browser cannot block, so the wasm half of the crate is `async`
//! where the native half is not. The split is by target, not by feature:
//!
//! ```text
//!   native : Client::loopback(port, token)   →  snapshot(&topics, items)   (sync)
//!   wasm   : Client::at_base("/api/tabs/7f3a") → snapshot(&topics, items).await
//! ```
//!
//! A browser cannot spawn `evo-swarm` and cannot open a socket, so it talks to
//! the axum server that served it, which proxies the same paths under a
//! same-origin prefix and adds the bearer token server-side. That is
//! [`Client::at_base`], and it is the only way a wasm build reaches a server.
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

pub mod error;
pub mod protocol;
pub mod redact;
pub mod sse;
pub mod transport;

// The native transport, and the two modules that can only exist beside it: a
// spawn is `std::process`, and the stream reader is a thread on a socket.
#[cfg(not(target_family = "wasm"))]
pub mod client;
#[cfg(not(target_family = "wasm"))]
pub mod http;
#[cfg(not(target_family = "wasm"))]
pub mod server;
#[cfg(not(target_family = "wasm"))]
pub mod stream;

// The browser's transport: `fetch` for one request, a `ReadableStream` read
// chunk by chunk for the op stream.
#[cfg(target_family = "wasm")]
pub mod fetch;

#[cfg(all(feature = "test-harness", not(target_family = "wasm")))]
pub mod harness;

#[cfg(not(target_family = "wasm"))]
pub use client::Client;
#[cfg(target_family = "wasm")]
pub use fetch::Client;

pub use error::{BootFailure, Error, RequestError, Result, StatusError};
pub use protocol::{
    lane_number, lane_topic, op_name, Cursor, ErrorCode, OpError, OpReply, OpRequest, ReadyFile,
    SessionRef, Snapshot, StreamFrame, StreamResetReason, TopicResetReason, TOPIC_LANE_WILDCARD,
    TOPIC_SESSION, TOPIC_SWARM,
};
pub use sse::{SseEvent, SseParser};
pub use transport::{is_loopback, new_rid, request_path, HttpResponse, Target, Token};

#[cfg(not(target_family = "wasm"))]
pub use http::{HttpClient, SseConnection};
#[cfg(not(target_family = "wasm"))]
pub use server::{
    group_alive, log_tail, process_alive, read_ready, reap_spawned, spawned_groups, BootCancel,
    Server, ServerConfig, Shutdown, ShutdownOutcome, StdinClose, SCRUB_ENV,
};
#[cfg(not(target_family = "wasm"))]
pub use stream::{Backoff, EventStream, StreamConfig, StreamMsg};

#[cfg(target_family = "wasm")]
pub use fetch::{FetchClient, FetchSseConnection, StreamHandle, WasmEventStream};

/// The installed swarm binary, unless `EVO_SWARM_BIN` says otherwise.
///
/// Native only: a browser has no binary to name, and no way to run one.
#[cfg(not(target_family = "wasm"))]
pub fn default_swarm_bin() -> std::path::PathBuf {
    std::env::var_os("EVO_SWARM_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/evo-swarm"))
}
