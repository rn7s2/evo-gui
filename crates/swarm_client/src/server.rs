//! Starting, watching and stopping a `serve` process (docs/PROMPT.md §3).
//!
//! One tab is one `evo-swarm serve` in the chosen folder. This module owns the
//! exact argv, the environment, the readiness rule, and the shutdown ladder —
//! the whole lifecycle, so nothing else in the app has to know how a server is
//! started or stopped.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{Client, Health};
use crate::error::{BootFailure, Error, Result};
use crate::http::Token;

/// Environment a spawned server must not inherit.
///
/// The token comes from the `--token-file` the server writes, never from the
/// environment (§3), and a server this app starts must be a fresh top-level
/// process: the app itself may be running inside an evo lane (`EVO_SESSIONS_DIR`,
/// `EVO_SUPERVISED_CHILD`, a heartbeat file), and its coordinator's journal must
/// not land in that lane's session directory.
pub const SCRUB_ENV: &[&str] = &[
    "EVO_SERVE_TOKEN",
    "EVO_SERVE_WATCH_PID",
    "EVO_SESSIONS_DIR",
    "EVO_SUPERVISED_CHILD",
    "EVO_NO_SUPERVISOR",
    "EVO_HEARTBEAT_FILE",
    "EVO_PID",
    "EVO_IDE_CONTEXT",
];

/// How `--resume` is passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Resume {
    /// A fresh swarm.
    #[default]
    No,
    /// `--resume` with no path: the last session for this folder.
    Last,
    /// `--resume <path>`.
    Path(PathBuf),
}

/// What a ready server must say it is (`/health`). The swarm and the agent
/// differ only here, which is why one spawner serves both (§9.4's probe).
#[derive(Clone, Debug)]
pub struct Readiness {
    /// `/health.name` — `evo-swarm` or `evo-agent`.
    pub name: Option<String>,
    /// `/health.features` must contain all of these.
    pub features: Vec<String>,
}

impl Readiness {
    pub fn swarm() -> Readiness {
        Readiness { name: Some("evo-swarm".into()), features: vec!["swarm".into()] }
    }

    pub fn agent() -> Readiness {
        Readiness { name: Some("evo-agent".into()), features: Vec::new() }
    }

    fn matches(&self, health: &Health) -> bool {
        if !health.ok {
            return false;
        }
        if let Some(name) = &self.name {
            if health.name.as_deref() != Some(name.as_str()) {
                return false;
            }
        }
        self.features.iter().all(|feature| health.has_feature(feature))
    }
}

/// How patiently a cancellation waits for the process to leave on its own,
/// before `SIGTERM`, and then `SIGKILL` — a boot a caller aborted has to be
/// over in about a second (§3, and the app's quit path).
const CANCEL_SHUTDOWN_GRACE: Duration = Duration::from_millis(400);
const CANCEL_TERM_GRACE: Duration = Duration::from_millis(200);
/// How long one readiness probe may take. A server that has written its token is
/// listening, so `/health` answers at once; a short patience keeps an abort from
/// waiting out a wedged probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// A flag a caller raises to abort a boot in progress.
///
/// [`Server::start_cancellable`] checks it once per poll, so a cancelled boot is
/// over within about a second: it runs the same shutdown ladder as a normal
/// stop, only with the short graces above.
#[derive(Clone, Debug, Default)]
pub struct BootCancel(Arc<AtomicBool>);

impl BootCancel {
    pub fn new() -> BootCancel {
        BootCancel::default()
    }

    /// Raise it. The boot in progress stops within one poll interval.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Everything about one server that is decided before it starts.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The binary: `evo-swarm` (or `evo-agent` for the probe).
    pub bin: PathBuf,
    /// The folder the server runs in.
    pub cwd: PathBuf,
    /// Where the server writes its bearer token (0600), created by it.
    pub token_file: PathBuf,
    /// stdout + stderr go here, appended: the log a boot failure shows.
    pub log_path: PathBuf,
    /// `None` picks a free port; `Some(0)` lets the server pick and prints it.
    pub port: Option<u16>,
    pub workers: Option<u16>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub resume: Resume,
    /// The `evo-agent` binary the lanes run (`--evo`).
    pub evo_bin: Option<PathBuf>,
    pub no_userspace: bool,
    /// Anything else, verbatim, after the flags above.
    pub extra_args: Vec<String>,
    /// Set in the child's environment (after the scrub list).
    pub extra_env: Vec<(String, String)>,
    /// Extra variables to drop from the child's environment.
    pub env_remove: Vec<String>,
    pub readiness: Readiness,
    /// Readiness deadline (§3: 90 s).
    pub ready_timeout: Duration,
    /// Readiness poll interval (§3: 100 ms).
    pub poll: Duration,
    /// How long a clean `POST /shutdown` gets before `SIGTERM` (§3: 10 s).
    pub shutdown_grace: Duration,
    /// How long `SIGTERM` gets before `SIGKILL` (§3: 5 s).
    pub term_grace: Duration,
}

impl ServerConfig {
    /// A tab's swarm in `tab_dir`: `token` and `swarm.log` live there (§6).
    pub fn swarm(bin: impl Into<PathBuf>, cwd: impl Into<PathBuf>, tab_dir: &Path) -> ServerConfig {
        ServerConfig {
            bin: bin.into(),
            cwd: cwd.into(),
            token_file: tab_dir.join("token"),
            log_path: tab_dir.join("swarm.log"),
            port: None,
            workers: None,
            model: None,
            thinking: None,
            resume: Resume::No,
            evo_bin: None,
            no_userspace: false,
            extra_args: Vec::new(),
            extra_env: Vec::new(),
            env_remove: Vec::new(),
            readiness: Readiness::swarm(),
            ready_timeout: Duration::from_secs(90),
            poll: Duration::from_millis(100),
            shutdown_grace: Duration::from_secs(10),
            term_grace: Duration::from_secs(5),
        }
    }

    /// The throwaway `evo-agent serve` the model catalog is learned from (§9.4).
    pub fn agent(bin: impl Into<PathBuf>, cwd: impl Into<PathBuf>, dir: &Path) -> ServerConfig {
        ServerConfig {
            bin: bin.into(),
            cwd: cwd.into(),
            token_file: dir.join("token"),
            log_path: dir.join("probe.log"),
            readiness: Readiness::agent(),
            ..ServerConfig::swarm(PathBuf::new(), PathBuf::new(), dir)
        }
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

    pub fn with_resume(mut self, resume: Resume) -> Self {
        self.resume = resume;
        self
    }

    pub fn with_evo(mut self, evo_bin: impl Into<PathBuf>) -> Self {
        self.evo_bin = Some(evo_bin.into());
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
        self.extra_env.push((key.into(), value.into()));
        self
    }

    pub fn with_env_removed(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// The argv after the binary: `serve --port … --token-file …`.
    pub fn argv(&self, port: u16) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--port".to_owned(),
            port.to_string(),
            "--token-file".to_owned(),
            self.token_file.to_string_lossy().into_owned(),
        ];
        if let Some(workers) = self.workers {
            args.push("--workers".into());
            args.push(workers.to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(thinking) = &self.thinking {
            args.push("--thinking".into());
            args.push(thinking.clone());
        }
        match &self.resume {
            Resume::No => {}
            Resume::Last => args.push("--resume".into()),
            Resume::Path(path) => {
                args.push("--resume".into());
                args.push(path.to_string_lossy().into_owned());
            }
        }
        if let Some(evo) = &self.evo_bin {
            args.push("--evo".into());
            args.push(evo.to_string_lossy().into_owned());
        }
        if self.no_userspace {
            args.push("--no-userspace".into());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// How a server stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// It was already gone.
    AlreadyGone,
    /// `POST /shutdown` was answered and it exited on its own.
    Graceful,
    /// It needed `SIGTERM`.
    Terminated,
    /// It needed `SIGKILL`.
    Killed,
}

/// The result of the shutdown ladder.
#[derive(Clone, Copy, Debug)]
pub struct Shutdown {
    pub outcome: ShutdownOutcome,
    pub exit_code: Option<i32>,
    pub waited: Duration,
}

/// A running `serve` process, its token, and a client for it.
#[derive(Debug)]
pub struct Server {
    child: Child,
    pid: u32,
    port: u16,
    token: Token,
    token_file: PathBuf,
    log_path: PathBuf,
    client: Client,
    health: Health,
    shutdown_grace: Duration,
    term_grace: Duration,
    reaped: bool,
}

impl Server {
    /// Start a server and wait until it is ready (§3): the token file is
    /// non-empty and `/health` answers 200 naming what
    /// [`Readiness`](ServerConfig::readiness) expects.
    pub fn start(cfg: &ServerConfig) -> Result<Server> {
        Server::start_cancellable(cfg, &BootCancel::new())
    }

    /// Start a server, with a flag a caller can raise to abort the wait for
    /// readiness — the app's quit path, which must not sit out a slow boot.
    ///
    /// A cancelled boot is stopped the ladder's way, only faster: `POST
    /// /shutdown` when a token and a port are known, then `SIGTERM` after
    /// [`CANCEL_SHUTDOWN_GRACE`], then `SIGKILL` after [`CANCEL_TERM_GRACE`]. With
    /// no token or port yet the server is not serving, so there is nothing to
    /// ask over HTTP — but `SIGTERM` is still an ask, and `SIGKILL` only follows
    /// if the process ignores it. Nothing is "killed first".
    pub fn start_cancellable(cfg: &ServerConfig, cancel: &BootCancel) -> Result<Server> {
        let port = match cfg.port {
            Some(port) => port,
            None => free_port()?,
        };
        if !cfg.cwd.is_dir() {
            return Err(Error::Config(format!("{} is not a directory", cfg.cwd.display())));
        }
        let mut child = spawn(cfg, port)?;
        let pid = child.id();

        let fixed_port = cfg.port != Some(0);
        let mut port = if fixed_port { Some(port) } else { None };
        let deadline = Instant::now() + cfg.ready_timeout;
        let mut token: Option<Token> = None;
        let mut client: Option<Client> = None;

        loop {
            if cancel.is_cancelled() {
                let mut child = child;
                // Nothing to ask over HTTP until the server has written its token
                // and we know its port; then `SIGTERM` is the first thing, and
                // `SIGKILL` only follows if it is ignored.
                let impatient = client.as_ref().map(|c| c.with_timeout(CANCEL_SHUTDOWN_GRACE));
                let grace = if impatient.is_some() { CANCEL_SHUTDOWN_GRACE } else { Duration::ZERO };
                let outcome =
                    ladder(impatient.as_ref(), &mut child, grace, CANCEL_TERM_GRACE);
                return Err(Error::Cancelled(outcome.outcome));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let tail = log_tail(&cfg.log_path, 40);
                    return Err(Error::Boot(Box::new(BootFailure {
                        message: format!("the server exited during startup ({})", status),
                        log_tail: tail,
                        log_path: Some(cfg.log_path.clone()),
                        exit_code: status.code(),
                    })));
                }
                Ok(None) => {}
                Err(e) => return Err(e.into()),
            }
            if token.is_none() {
                // The server creates the file empty and fills it a moment later.
                if let Ok(read) = Token::from_file(&cfg.token_file) {
                    token = Some(read);
                }
            }
            if port.is_none() {
                // `--port 0`: the server prints the port it chose.
                port = port_from_log(&log_tail(&cfg.log_path, 400));
            }
            if let (Some(token), Some(port)) = (token.clone(), port) {
                let probe = client.get_or_insert_with(|| Client::loopback(port, token.clone()));
                // A short patience for the probe alone: the client kept for the
                // app answers with its normal timeout.
                if let Ok(health) = probe.with_timeout(PROBE_TIMEOUT).health() {
                    if cfg.readiness.matches(&health.typed) {
                        return Ok(Server {
                            child,
                            pid,
                            port,
                            token: token.clone(),
                            token_file: cfg.token_file.clone(),
                            log_path: cfg.log_path.clone(),
                            client: probe.clone(),
                            health: health.typed,
                            shutdown_grace: cfg.shutdown_grace,
                            term_grace: cfg.term_grace,
                            reaped: false,
                        });
                    }
                }
            }
            if Instant::now() >= deadline {
                let mut child = child;
                ladder(client.as_ref(), &mut child, cfg.shutdown_grace, cfg.term_grace);
                return Err(Error::Boot(Box::new(BootFailure {
                    message: format!(
                        "no ready server after {:?} (name {:?}, features {:?})",
                        cfg.ready_timeout, cfg.readiness.name, cfg.readiness.features
                    ),
                    log_tail: log_tail(&cfg.log_path, 40),
                    log_path: Some(cfg.log_path.clone()),
                    exit_code: None,
                })));
            }
            std::thread::sleep(cfg.poll);
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `/health` this server answered with when it became ready.
    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    pub fn token_file(&self) -> &Path {
        &self.token_file
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn is_running(&mut self) -> bool {
        if self.reaped {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.reaped = true;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// The last `lines` lines of the log — what a tab shows when boot fails.
    pub fn log_tail(&self, lines: usize) -> String {
        log_tail(&self.log_path, lines)
    }

    /// Wait up to `timeout` for the process to exit; the code, or `None`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.reaped = true;
                    return Some(status.code().unwrap_or(-1));
                }
                Ok(None) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The ladder (§3): `POST /shutdown`, wait ≤10 s, `SIGTERM`, 5 s, `SIGKILL`.
    /// Never kills first. Safe to run for every tab at once — each call touches
    /// only this server's process.
    pub fn shutdown(&mut self) -> Result<Shutdown> {
        let started = Instant::now();
        if !self.is_running() {
            return Ok(Shutdown {
                outcome: ShutdownOutcome::AlreadyGone,
                exit_code: None,
                waited: started.elapsed(),
            });
        }
        let outcome = ladder(
            Some(&self.client.with_timeout(Duration::from_secs(3))),
            &mut self.child,
            self.shutdown_grace,
            self.term_grace,
        );
        self.reaped = true;
        Ok(Shutdown { waited: started.elapsed(), ..outcome })
    }

    /// `SIGKILL` now, for a caller that has given up on the ladder (a panic path).
    pub fn kill(&mut self) {
        if self.is_running() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if self.is_running() {
            // Shortened graces: a dropped server is a leaked process otherwise.
            ladder(
                Some(&self.client.with_timeout(Duration::from_secs(2))),
                &mut self.child,
                Duration::from_secs(5),
                Duration::from_secs(2),
            );
            self.reaped = true;
        }
    }
}

/// Stop a child the ladder way. `client` is `None` while boot is still failing.
fn ladder(
    client: Option<&Client>,
    child: &mut Child,
    shutdown_grace: Duration,
    term_grace: Duration,
) -> Shutdown {
    if let Some(client) = client {
        // Never kill first: ask, and give it the grace to leave on its own.
        let _ = client.shutdown();
    }
    if let Some(code) = wait_for_exit(child, shutdown_grace) {
        return Shutdown { outcome: ShutdownOutcome::Graceful, exit_code: Some(code), waited: Duration::ZERO };
    }
    signal_group(child.id(), libc::SIGTERM);
    if let Some(code) = wait_for_exit(child, term_grace) {
        return Shutdown { outcome: ShutdownOutcome::Terminated, exit_code: Some(code), waited: Duration::ZERO };
    }
    signal_group(child.id(), libc::SIGKILL);
    let code = child.wait().ok().and_then(|status| status.code());
    Shutdown { outcome: ShutdownOutcome::Killed, exit_code: code, waited: Duration::ZERO }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A signal to the child's process group (it is spawned as its own group
/// leader), so the swarm's supervisor and every lane it started hear it too.
fn signal_group(pid: u32, signal: i32) {
    let target = pid as libc::pid_t;
    let sent = unsafe { libc::killpg(target, signal) };
    if sent != 0 {
        unsafe { libc::kill(target, signal) };
    }
}

/// Whether a process is still there (`kill(pid, 0)`), for tests and for a tab
/// that wants to be sure its server left.
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    if alive {
        return true;
    }
    // EPERM means it exists and belongs to somebody else.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub(crate) fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn spawn(cfg: &ServerConfig, port: u16) -> Result<Child> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_path)
        .map_err(|e| Error::Config(format!("cannot open {}: {e}", cfg.log_path.display())))?;
    let mut command = Command::new(&cfg.bin);
    command
        .args(cfg.argv(port))
        .current_dir(&cfg.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    apply_environment(&mut command, cfg);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: one signal reaches the supervisor and the lanes.
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|e| Error::Config(format!("cannot run {}: {e}", cfg.bin.display())))
}

/// The child's environment (§3).
///
/// The order here is the whole point. Everything that would tie a server to
/// *our* evo session is removed first, the caller's variables come next, and the
/// watch pid is set **last**: `Command::env_remove` overrides an earlier
/// `env` for the same name whatever the order of the calls looks like, and
/// `SCRUB_ENV` names `EVO_SERVE_WATCH_PID` — so setting it before the scrub
/// silently unsets it, and a server whose driver dies then idles forever instead
/// of stopping (§3). The token is never handed over in the environment: the
/// server writes it to `--token-file`, and nothing else knows it.
pub(crate) fn apply_environment(command: &mut Command, cfg: &ServerConfig) {
    for name in SCRUB_ENV.iter().copied().chain(cfg.env_remove.iter().map(String::as_str)) {
        command.env_remove(name);
    }
    for (key, value) in &cfg.extra_env {
        command.env(key, value);
    }
    command.env("EVO_SERVE_WATCH_PID", std::process::id().to_string());
}

/// `evo-swarm serve: listening on http://127.0.0.1:56750/ (token in …)` — the
/// port, when `--port 0` let the server choose it.
fn port_from_log(text: &str) -> Option<u16> {
    let marker = "http://127.0.0.1:";
    let start = text.rfind(marker)? + marker.len();
    let digits: String = text[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The last `lines` lines of a file, read from the end so a long log stays cheap.
pub fn log_tail(path: &Path, lines: usize) -> String {
    const WINDOW: u64 = 256 * 1024;
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(WINDOW);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return String::new();
    }
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    all[from..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_the_documented_one() {
        let cfg = ServerConfig::swarm("/usr/local/bin/evo-swarm", "/tmp/proj", Path::new("/tmp/tab"))
            .with_workers(3)
            .with_model("m-1")
            .with_resume(Resume::Path(PathBuf::from("/tmp/s.sexp")));
        assert_eq!(
            cfg.argv(8421),
            vec![
                "serve", "--port", "8421", "--token-file", "/tmp/tab/token",
                "--workers", "3", "--model", "m-1", "--resume", "/tmp/s.sexp",
            ]
        );
        // --allow-remote is never passed (§3).
        assert!(!cfg.argv(8421).iter().any(|arg| arg == "--allow-remote"));
    }

    #[test]
    fn the_watch_pid_survives_the_scrub_list() {
        // `/usr/bin/env` prints the environment it was handed, which is the one
        // way to see what a child really gets. `SCRUB_ENV` names
        // EVO_SERVE_WATCH_PID (it must not be inherited from the evo session
        // that started us), and `env_remove` overrides an earlier `env`, so the
        // assignment has to come after both.
        let cfg = ServerConfig::swarm("/usr/bin/env", "/tmp", Path::new("/tmp/tab"));
        let mut command = Command::new("/usr/bin/env");
        apply_environment(&mut command, &cfg);
        let output = command.output().expect("/usr/bin/env runs");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(&format!("EVO_SERVE_WATCH_PID={}", std::process::id())),
            "the child must be told to watch us: {text}"
        );
        assert!(!text.contains("EVO_SERVE_TOKEN="), "the token never comes through the environment: {text}");
        assert!(!text.contains("EVO_SUPERVISED_CHILD="), "and the evo session is scrubbed: {text}");
    }

    #[test]
    fn a_port_is_read_back_from_the_log() {
        assert_eq!(
            port_from_log("evo-swarm serve: listening on http://127.0.0.1:56750/ (token in /x)"),
            Some(56750)
        );
        assert_eq!(port_from_log("nothing here"), None);
    }
}
