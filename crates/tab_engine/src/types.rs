//! What the UI says to a tab, and what a tab says back.
//!
//! One tab is one `evo-swarm serve` (§3). The engine thread owns it and turns
//! the server's HTTP/SSE into a stream of [`Update`]s; the UI sends [`Command`]s
//! back. Everything crosses an `async-channel`, so the UI thread never blocks.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use swarm_client::{Envelope, Health, Resume, ServerConfig, StatusError};

/// Which agent a piece of state belongs to: the coordinator, or a watched lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    Coordinator,
    Lane(u32),
}

impl Agent {
    pub fn lane_number(self) -> Option<u32> {
        match self {
            Agent::Lane(n) => Some(n),
            Agent::Coordinator => None,
        }
    }
}

/// A request id the UI assigns to a `POST`, echoed back on its [`Update::PostResult`]
/// so a reply is matched to the click that caused it.
pub type ReqId = u64;

/// Everything about one tab that is decided before it starts (§3, §6).
#[derive(Clone, Debug)]
pub struct TabSpec {
    /// `evo-swarm`.
    pub swarm_bin: PathBuf,
    /// The `evo-agent` the lanes run (`--evo`); `None` lets the swarm find one.
    pub agent_bin: Option<PathBuf>,
    /// The folder the swarm runs in (cwd).
    pub folder: PathBuf,
    /// Where the server keeps its `token` and `swarm.log` (§6).
    pub tab_dir: PathBuf,
    /// Lanes to start (`--workers`); `None` uses the swarm's own default.
    pub workers: Option<u16>,
    /// The coordinator's model (`--model`). The swarm takes the model **id**
    /// alone — the provider comes from how the model was registered — so there
    /// is no provider flag to carry.
    pub model: Option<String>,
    /// `--thinking low|medium|high|xhigh|max`.
    pub thinking: Option<String>,
    /// `--resume <path>`, resuming a specific session instead of the last one.
    pub resume: Option<PathBuf>,
    /// `--no-userspace`: boot without init files or extensions.
    pub no_userspace: bool,
    /// Extra argv, after the flags above.
    pub extra_args: Vec<String>,
    /// Extra environment variables the child gets.
    pub env: Vec<(String, String)>,
    /// Variables to drop from the child's environment.
    pub env_remove: Vec<String>,
}

impl TabSpec {
    /// A tab: the swarm binary, a folder, and a tab directory.
    pub fn new(
        swarm_bin: impl Into<PathBuf>,
        folder: impl Into<PathBuf>,
        tab_dir: impl Into<PathBuf>,
    ) -> TabSpec {
        TabSpec {
            swarm_bin: swarm_bin.into(),
            agent_bin: None,
            folder: folder.into(),
            tab_dir: tab_dir.into(),
            workers: None,
            model: None,
            thinking: None,
            resume: None,
            no_userspace: false,
            extra_args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    pub fn with_agent_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.agent_bin = Some(bin.into());
        self
    }

    pub fn with_workers(mut self, workers: u16) -> Self {
        self.workers = Some(workers);
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_thinking(mut self, level: impl Into<String>) -> Self {
        self.thinking = Some(level.into());
        self
    }

    pub fn with_resume(mut self, session: impl Into<PathBuf>) -> Self {
        self.resume = Some(session.into());
        self
    }

    pub fn with_no_userspace(mut self, yes: bool) -> Self {
        self.no_userspace = yes;
        self
    }

    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_args.push(arg.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_env_removed(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// The [`ServerConfig`] this tab spawns (§3): the argv, the cwd, the tab
    /// directory, and the environment, exactly as [`swarm_client::Server`] wants.
    pub fn server_config(&self) -> ServerConfig {
        let mut config = ServerConfig::swarm(&self.swarm_bin, &self.folder, &self.tab_dir)
            .with_no_userspace(self.no_userspace);
        config.workers = self.workers;
        config.model = self.model.clone();
        config.thinking = self.thinking.clone();
        config.evo_bin = self.agent_bin.clone();
        if let Some(session) = &self.resume {
            config.resume = Resume::Path(session.clone());
        }
        config.extra_args = self.extra_args.clone();
        config.extra_env = self.env.clone();
        config.env_remove = self.env_remove.clone();
        config
    }
}

/// What the UI asks a tab to do. Sending is non-blocking; a `false` means the
/// engine is gone.
#[derive(Clone, Debug)]
pub enum Command {
    /// `POST /prompt` — the user's turn.
    Prompt { req_id: ReqId, text: String },
    /// `POST /steer` — mid-run input, landing at the running turn's next
    /// boundary. It is the one command serve answers `409 Not now` when nothing
    /// runs, so its reply is how the composer shows a soft refusal.
    Steer { req_id: ReqId, text: String },
    /// `POST /interrupt` — the TUI's esc.
    Interrupt { req_id: ReqId },
    /// Refetch a view for AGENT (rows + state); what a UI asks for after it has
    /// dropped updates it could not keep up with.
    Refetch(Agent),
    /// Watch one lane's events, or none: `WatchLane(Some(n))` closes any other
    /// lane stream first, so at most one is ever open.
    WatchLane(Option<u32>),
    /// Stop the server the ladder way (§3) and exit the engine.
    Shutdown,
}

/// How a stream is doing, so the UI can show "reconnecting in N s".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatus {
    Connected,
    Reconnecting { retry_in: Duration },
}

/// A refusal from a `POST`, flattened to something `Clone`, so it can ride in an
/// [`Update`]: the HTTP status (or `None` for a transport failure), whether it is
/// the `409 Not now` the composer shows as a soft refusal, and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostError {
    pub status: Option<u16>,
    pub not_now: bool,
    pub message: String,
}

impl From<swarm_client::Error> for PostError {
    fn from(error: swarm_client::Error) -> PostError {
        match &error {
            swarm_client::Error::Status(status) => PostError {
                status: Some(status.status()),
                not_now: status.is_not_now(),
                message: status.message().to_owned(),
            },
            other => PostError { status: None, not_now: false, message: other.to_string() },
        }
    }
}

impl From<&StatusError> for PostError {
    fn from(status: &StatusError) -> PostError {
        PostError {
            status: Some(status.status()),
            not_now: status.is_not_now(),
            message: status.message().to_owned(),
        }
    }
}

/// What a tab tells the UI. Applied to `session` models by the workspace crate.
#[derive(Clone, Debug)]
pub enum Update {
    /// The server is being started (§3).
    Booting,
    /// The server is ready: what `/health` said, its process id and its port.
    Ready { health: Health, pid: u32, port: u16 },
    /// The server never came up; `log_tail` is what the tab shows (§3).
    BootFailed { message: String, log_tail: String },
    /// A whole transcript, as `GET /transcript` returned it: rows are rebuilt
    /// from it (§9.1). `revision` is monotonic per agent — a UI request stamped
    /// with the revision it started at can drop its own late answer.
    Transcript { agent: Agent, revision: u64, raw: Value },
    /// `GET /state`, as it arrived (the coordinator only; a lane's state comes
    /// from its events).
    State { revision: u64, raw: Value },
    /// `GET /registry`, once per tab — the UI refreshes its model cache from it.
    Registry { raw: Value },
    /// `GET /lanes` — the swarm and its lanes.
    Lanes { raw: Value },
    /// One SSE event, in stream order.
    Event { agent: Agent, id: Option<i64>, kind: String, data: Value },
    /// The state of an agent's stream.
    Stream { agent: Agent, status: StreamStatus },
    /// The journal's newest `cache-stats` custom entry, or `None` when the walk
    /// of `/journal?limit=20,100,400` found none (§7.3).
    CacheSeed { entry: Option<Value> },
    /// A `POST` finished: the reply envelope, or the typed refusal.
    PostResult { req_id: ReqId, result: Result<Envelope, PostError> },
    /// The server process died on its own (§3); the engine has stopped.
    ServerGone,
    /// The engine has stopped, and why the server did (§3's ladder).
    Exited { outcome: swarm_client::ShutdownOutcome },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refusal(status: u16, message: &str) -> swarm_client::Error {
        let raw = json!({ "ok": false, "error": message, "status": status });
        swarm_client::Error::Status(swarm_client::StatusError::from_reply(status, raw, message))
    }

    #[test]
    fn a_not_now_maps_to_the_soft_refusal() {
        let post = PostError::from(refusal(409, "no run to steer — POST /prompt starts one"));
        assert_eq!(post.status, Some(409));
        assert!(post.not_now);
        assert!(post.message.contains("no run to steer"));
    }

    #[test]
    fn other_statuses_are_not_not_now() {
        for code in [400u16, 401, 404, 422, 503] {
            let post = PostError::from(refusal(code, "nope"));
            assert_eq!(post.status, Some(code), "{code}");
            assert!(!post.not_now, "{code}");
            assert_eq!(post.message, "nope", "{code}");
        }
    }

    #[test]
    fn a_transport_failure_has_no_status() {
        let post = PostError::from(swarm_client::Error::Closed);
        assert_eq!(post.status, None);
        assert!(!post.not_now);
    }

    #[test]
    fn the_spec_becomes_the_spawn_argv() {
        let spec = TabSpec::new("/bin/evo-swarm", "/proj", "/tab")
            .with_agent_bin("/bin/evo-agent")
            .with_workers(3)
            .with_model("m-1")
            .with_resume("/s.sexp");
        let config = spec.server_config();
        assert_eq!(
            config.argv(8421),
            vec![
                "serve", "--port", "8421", "--token-file", "/tab/token",
                "--workers", "3", "--model", "m-1", "--resume", "/s.sexp",
                "--evo", "/bin/evo-agent",
            ]
        );
        assert_eq!(config.cwd, std::path::PathBuf::from("/proj"));
        assert_eq!(config.log_path, std::path::PathBuf::from("/tab/swarm.log"));
    }
}
