//! What the UI [asks a tab](EngineHandle) and what a [tab tells the UI](Update).
//!
//! The UI never runs an op itself: it builds a [`session::OpRequest`] from the
//! tab's own model (`TabModel::send_input`, `interrupt_lane`, …) and hands it to
//! an [`session::OpSink`], which the [`EngineHandle`](crate::EngineHandle) is.
//! In the other direction a tab hands over the protocol's own JSON — one topic's
//! snapshot body, one parsed [`session::Op`] — and the `session` crate is the
//! only thing that knows what any of it means.

use std::path::PathBuf;
use std::time::Duration;

use swarm_client::{OpReply, SessionRef};

/// The stream's state, for the "reconnecting" badge: session's own vocabulary,
/// so the UI has one enum and not two.
pub use session::StreamStatus;

/// Everything about one tab that is decided before it starts.
#[derive(Clone, Debug)]
pub struct TabSpec {
    /// `evo-swarm`.
    pub swarm_bin: PathBuf,
    /// The `evo-agent` the lanes run (`--evo`); `None` lets the swarm find one.
    pub agent_bin: Option<PathBuf>,
    /// The folder the server runs in (its cwd).
    pub folder: PathBuf,
    /// Where the server keeps its ready file and its log; created if missing.
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

/// What a tab tells the UI. Everything here goes straight into the `session` crate.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// The server is being started.
    Booting,
    /// The server is ready: its epoch, its process id, its port, its session.
    Ready {
        epoch: String,
        pid: u32,
        port: u16,
        session: SessionRef,
    },
    /// The server never came up; `log_tail` is what the tab shows.
    BootFailed { message: String, log_tail: String },
    /// One topic of a snapshot (§5.2), exactly as the server sent it: the body
    /// of `topics[name]` — `{state, items, has_more}`. `TabModel::on_snapshot`.
    Snapshot { topic: String, body: serde_json::Value },
    /// One stream frame (§5.3). `TabModel::on_op`.
    Op {
        topic: String,
        op: session::Op,
    },
    /// The stream's state, for the "reconnecting" line.
    Stream { status: StreamStatus },
    /// The reply to an op the UI sent (§5.5).
    OpReply(Box<OpReply>),
    /// The server process died on its own; the engine has stopped.
    ServerGone,
    /// The engine has stopped, and how the server went.
    Exited {
        outcome: swarm_client::ShutdownOutcome,
    },
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
