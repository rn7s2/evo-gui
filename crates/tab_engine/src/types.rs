//! What the UI [asks a tab](Command) and what a [tab tells the UI](Update).
//!
//! There is no session state in either direction. A tab carries the protocol's
//! own JSON — a [`Snapshot`] because it asked for one, a [`StreamFrame`] because
//! the server sent one, an [`OpReply`] because the UI asked for something — and
//! the `session` crate is the only thing that knows what any of it means.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use swarm_client::{OpReply, Snapshot, StreamFrame, StreamResetReason, TopicResetReason};

/// Everything about one tab that is decided before it starts.
#[derive(Clone, Debug)]
pub struct TabSpec {
    /// `evo-swarm`.
    pub swarm_bin: PathBuf,
    /// The `evo-agent` the lanes run (`--evo`); `None` lets the swarm find one.
    pub agent_bin: Option<PathBuf>,
    /// The folder the server runs in (its cwd).
    pub folder: PathBuf,
    /// Where the server keeps its ready file, its log; created if missing.
    pub tab_dir: PathBuf,
    /// Lanes to start (`--workers`); `None` uses the swarm's own default.
    pub workers: Option<u16>,
    /// The coordinator's model (`--model ID[@PROVIDER]`).
    pub model: Option<String>,
    /// The lanes' model (`--lane-model ID[@PROVIDER]`).
    pub lane_model: Option<String>,
    /// `--thinking off|low|medium|high|xhigh`.
    pub thinking: Option<String>,
    /// `--resume <path>`: a session to come back to.
    pub resume: Option<PathBuf>,
    /// `--no-userspace`: boot without init files or extensions.
    pub no_userspace: bool,
    /// Extra argv, after the flags above.
    pub extra_args: Vec<String>,
    /// Extra environment variables the child gets.
    pub env: Vec<(String, String)>,
    /// Variables to drop from the child's environment.
    pub env_remove: Vec<String>,
    /// The client's patience for an ordinary request, and for a stream gone
    /// silent. A test that *stops* a server sets them short, so that the
    /// patience itself is what it asserts on.
    pub request_timeout: Duration,
    pub stream_timeout: Duration,
}

impl TabSpec {
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
            lane_model: None,
            thinking: None,
            resume: None,
            no_userspace: false,
            extra_args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            request_timeout: Duration::from_secs(30),
            stream_timeout: Duration::from_secs(45),
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

    pub fn with_lane_model(mut self, model: impl Into<String>) -> Self {
        self.lane_model = Some(model.into());
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

    /// The client's patience, for a test that wants to see it run out.
    pub fn with_http_timeouts(mut self, request: Duration, stream: Duration) -> Self {
        self.request_timeout = request;
        self.stream_timeout = stream;
        self
    }

    /// The [`ServerConfig`](swarm_client::ServerConfig) this tab spawns: the
    /// argv, the cwd, the tab directory and the environment.
    pub fn server_config(&self) -> swarm_client::ServerConfig {
        let mut config =
            swarm_client::ServerConfig::swarm(&self.swarm_bin, &self.folder, &self.tab_dir)
                .with_no_userspace(self.no_userspace);
        config.workers = self.workers;
        config.model = self.model.clone();
        config.lane_model = self.lane_model.clone();
        config.thinking = self.thinking.clone();
        config.evo_bin = self.agent_bin.clone();
        if let Some(session) = &self.resume {
            config.resume = swarm_client::Resume::Path(session.clone());
        }
        config.extra_args = self.extra_args.clone();
        config.extra_env = self.env.clone();
        config.env_remove = self.env_remove.clone();
        config.request_timeout = self.request_timeout;
        config.stream_timeout = self.stream_timeout;
        config
    }
}

/// What the UI asks a tab to do. Sending is non-blocking; a `false` means the
/// engine is gone.
#[derive(Clone, Debug)]
pub enum Command {
    /// `POST /ops` with these arguments (§5.5). The rid is the engine's: the
    /// server dedupes by rid, so a retry after a dropped connection is free.
    Op { op: String, args: Value },
    /// Re-read the whole view (§5.2) — what a UI asks for when it dropped
    /// updates it could not keep up with.
    Snapshot,
    /// Stop the server the ladder's way (§8) and end the engine.
    Shutdown,
}

impl Command {
    /// The common case: one op, its arguments.
    pub fn op(op: impl Into<String>, args: Value) -> Command {
        Command::Op {
            op: op.into(),
            args,
        }
    }
}

/// How a stream is doing, so the UI can say "reconnecting in N s".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatus {
    Live,
    Reconnecting { attempt: u32, retry_in: Duration },
}

/// What a tab tells the UI. The `session` crate applies these; nothing here is
/// interpreted on the way.
#[derive(Clone, Debug)]
pub enum Update {
    /// The server is being started.
    Booting,
    /// The server is ready: its epoch, its process id, its port, its session.
    Ready {
        epoch: String,
        pid: u32,
        port: u16,
        session: swarm_client::SessionRef,
    },
    /// The server never came up; `log_tail` is what the tab shows.
    BootFailed { message: String, log_tail: String },
    /// A snapshot (§5.2), exactly as it arrived: `topics`, `epoch` and `seq`.
    /// The topics present are the ones this snapshot is authoritative for.
    Snapshot(Box<Snapshot>),
    /// One frame of the op stream (§5.3), in stream order.
    Frame(Box<StreamFrame>),
    /// The server cannot continue the stream from our cursor; everything is
    /// being re-read, and the stream will resume from the new snapshot.
    StreamReset { reason: StreamResetReason },
    /// One topic is stale (`topic.reset`); a snapshot of it is on its way.
    TopicReset { topic: String, reason: TopicResetReason },
    /// The stream's state, for the "reconnecting" line.
    Stream { status: StreamStatus },
    /// The reply to an op the UI sent (§5.5).
    OpReply(Box<OpReply>),
    /// The server process died on its own; the engine has stopped.
    ServerGone,
    /// The engine has stopped, and how the server went.
    Exited { outcome: swarm_client::ShutdownOutcome },
}

/// The topics a tab always reads: the coordinator's own, the swarm's, and every
/// lane's. `lane:*` is expanded by the server, so a lane that starts or restarts
/// needs no new subscription.
pub fn tab_topics() -> Vec<String> {
    vec![
        swarm_client::TOPIC_SESSION.to_owned(),
        swarm_client::TOPIC_SWARM.to_owned(),
        swarm_client::TOPIC_LANE_WILDCARD.to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_becomes_the_spawn_argv() {
        let spec = TabSpec::new("/bin/evo-swarm", "/proj", "/tab")
            .with_agent_bin("/bin/evo-agent")
            .with_workers(3)
            .with_model("m-1@anthropic")
            .with_lane_model("l-1")
            .with_resume("/s.sexp");
        let config = spec.server_config();
        assert_eq!(
            config.argv(),
            vec![
                "serve",
                "--port",
                "0",
                "--ready-file",
                "/tab/ready.json",
                "--watch-stdin",
                "--workers",
                "3",
                "--model",
                "m-1@anthropic",
                "--lane-model",
                "l-1",
                "--resume",
                "/s.sexp",
                "--evo",
                "/bin/evo-agent",
            ]
        );
        assert_eq!(config.cwd, PathBuf::from("/proj"));
        assert_eq!(config.log_path, PathBuf::from("/tab/swarm.log"));
    }

    #[test]
    fn the_topics_are_the_coordinators_the_swarms_and_every_lane() {
        assert_eq!(tab_topics(), vec!["session", "swarm", "lane:*"]);
    }
}
