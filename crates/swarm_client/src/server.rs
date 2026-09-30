//! Starting, watching and stopping a `serve` process (CONTRACT.md §1, §8).
//!
//! One tab is one `evo-swarm serve` in the chosen folder. This module owns the
//! exact argv, the environment, the readiness rule (the ready file, and nothing
//! else), and the shutdown ladder.
//!
//! The argv is not built here: evo's launch flags are `store::launch`'s to know,
//! and a [`ServerConfig`] carries the argv it was given. Two things are
//! load-bearing:
//!
//! * **stdin is a pipe we hold.** The child is started with `--watch-stdin` and
//!   our end of the pipe; EOF means "the tab is gone", which is immediate and
//!   immune to pid reuse. Dropping the [`Server`] drops the pipe, and
//!   [`Server::stdin_handle`] hands out a closer a caller can use from anywhere —
//!   a quit that must not wait for the ladder still stops the server at once.
//! * **Readiness is one file.** The child writes `<tabdir>/ready.json`
//!   atomically once it is listening, and rewrites it after every supervisor
//!   restart: the port, the token and the epoch all come from there, so nothing
//!   polls `/health`, nothing picks a free port, and nothing compares pids.
//! * **Readiness is one file.** The child writes `<tabdir>/ready.json`
//!   atomically once it is listening, and rewrites it after every supervisor
//!   restart: the port, the token and the epoch all come from there, so nothing
//!   polls `/health`, nothing picks a free port, and nothing compares pids.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::error::{BootFailure, Error, Result};
use crate::protocol::ReadyFile;
use crate::redact::redact;

/// Environment a spawned server must not inherit.
///
/// A server this app starts must be a fresh top-level process: the app itself
/// may be running inside an evo lane (`EVO_SESSIONS_DIR`, `EVO_SUPERVISED_CHILD`,
/// a heartbeat file), and its journal must not land in that lane's session
/// directory. `EVO_SERVE_TOKEN` is scrubbed for the same reason — a test-only
/// token from an outer process must not silently authenticate ours.
pub const SCRUB_ENV: &[&str] = &[
    "EVO_SERVE_TOKEN",
    "EVO_SESSIONS_DIR",
    "EVO_SUPERVISED_CHILD",
    "EVO_NO_SUPERVISOR",
    "EVO_HEARTBEAT_FILE",
    "EVO_PID",
    "EVO_IDE_CONTEXT",
];

/// A handle on the child's stdin, from anywhere.
///
/// Closing it is EOF, which is how a child (and every lane under it) is told its
/// tab is gone — immediately, and without waiting for anything. The app's quit
/// path uses one so it can return before any ladder has run.
#[derive(Clone, Default)]
pub struct StdinClose(Arc<Mutex<Option<ChildStdin>>>);

impl StdinClose {
    pub fn new() -> StdinClose {
        StdinClose::default()
    }

    /// Close the pipe. The child sees EOF; idempotent.
    pub fn close(&self) {
        if let Ok(mut stdin) = self.0.lock() {
            *stdin = None;
        }
    }

    fn put(&self, stdin: Option<ChildStdin>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = stdin;
        }
    }
}

impl std::fmt::Debug for StdinClose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdinClose").finish_non_exhaustive()
    }
}

/// How long a cancelled boot waits for the child to leave on its own before a
/// signal, and then for the signal to be obeyed.
const CANCEL_STOP_GRACE: Duration = Duration::from_secs(5);
const CANCEL_TERM_GRACE: Duration = Duration::from_millis(200);
/// How often the ready file is looked for, while the boot waits.
const READY_POLL: Duration = Duration::from_millis(20);

/// A flag a caller raises to abort a boot in progress.
///
/// [`Server::start_with`] checks it once per poll, so a cancelled boot is over
/// quickly: it runs the same shutdown ladder as a normal stop, only with a
/// shorter patience.
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
///
/// The argv is the caller's: `store::launch` is the one place that knows evo's
/// launch flags, and this struct simply runs what it is handed.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The binary: `evo-swarm`.
    pub bin: PathBuf,
    /// The argv after the binary — `serve --port 0 --ready-file … --watch-stdin …`.
    pub argv: Vec<String>,
    /// The folder the server runs in (its cwd).
    pub cwd: PathBuf,
    /// Where the child writes its ready file (§1). The argv must name the same
    /// path; [`ServerConfig::swarm`] derives both from the tab directory.
    pub ready_file: PathBuf,
    /// stdout + stderr go here, appended: the log a boot failure shows.
    pub log_path: PathBuf,
    /// Set in the child's environment (after the scrub list).
    pub extra_env: Vec<(String, String)>,
    /// Extra variables to drop from the child's environment.
    pub env_remove: Vec<String>,
    /// How long the child gets to write its ready file (§1: 90 s).
    pub ready_timeout: Duration,
    /// How long an ordinary request may take before it is given up on.
    pub request_timeout: Duration,
    /// How long a stream may go with no bytes at all before it is considered
    /// gone (against the server's 15 s ping).
    pub stream_timeout: Duration,
    /// How long a clean shutdown gets before `SIGTERM`.
    pub shutdown_grace: Duration,
    /// How long `SIGTERM` gets before `SIGKILL`.
    pub term_grace: Duration,
}

impl ServerConfig {
    /// A tab's swarm in `tab_dir`: `ready.json` and `swarm.log` live there. The
    /// argv starts empty — [`ServerConfig::with_argv`] fills it.
    pub fn swarm(bin: impl Into<PathBuf>, cwd: impl Into<PathBuf>, tab_dir: &Path) -> ServerConfig {
        ServerConfig {
            bin: bin.into(),
            argv: Vec::new(),
            cwd: cwd.into(),
            ready_file: tab_dir.join("ready.json"),
            log_path: tab_dir.join("swarm.log"),
            extra_env: Vec::new(),
            env_remove: Vec::new(),
            ready_timeout: Duration::from_secs(90),
            request_timeout: Duration::from_secs(30),
            stream_timeout: Duration::from_secs(45),
            shutdown_grace: Duration::from_secs(10),
            term_grace: Duration::from_secs(5),
        }
    }

    /// The argv the child is run with.
    pub fn with_argv<I, S>(mut self, argv: I) -> ServerConfig
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv = argv.into_iter().map(Into::into).collect();
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

    /// The client's patience: how long a request and a silent stream may take.
    pub fn with_http_timeouts(mut self, request: Duration, stream: Duration) -> Self {
        self.request_timeout = request;
        self.stream_timeout = stream;
        self
    }
}

/// How a server stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// It was already gone.
    AlreadyGone,
    /// It left on its own — stdin EOF or `server.shutdown` was enough.
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

/// The process itself: everything that is true before it is ready, and
/// everything the shutdown ladder needs. Split out so that a boot which never
/// became ready still has a way to be stopped.
struct Proc {
    child: Child,
    /// Our end of the child's stdin, shared so a caller can close it from
    /// anywhere. Closing it is EOF, which the child reads as "the tab is gone"
    /// and stops cleanly (lanes included, §8).
    stdin: StdinClose,
    log_path: PathBuf,
    ready_file: PathBuf,
    shutdown_grace: Duration,
    term_grace: Duration,
    reaped: bool,
}

impl Proc {
    /// The ladder (§8): stdin EOF, `server.shutdown`, wait, `SIGTERM`, `SIGKILL`.
    /// Never kills first. `client` is `None` while a boot is still failing: with
    /// nothing listening there is nothing to ask.
    fn stop(
        &mut self,
        client: Option<&Client>,
        shutdown_grace: Duration,
        term_grace: Duration,
    ) -> Shutdown {
        // EOF first: it is the child's own signal, and it reaches the lanes too.
        self.stdin.close();
        if let Some(client) = client {
            // A courtesy: the answer is nice, the stopping is not.
            let _ = client
                .with_timeout(shutdown_grace.min(Duration::from_secs(3)))
                .shutdown();
        }
        if let Some(code) = wait_for_exit(&mut self.child, shutdown_grace) {
            self.reaped = true;
            return Shutdown {
                outcome: ShutdownOutcome::Graceful,
                exit_code: Some(code),
                waited: Duration::ZERO,
            };
        }
        signal_group(self.child.id(), libc::SIGTERM);
        if let Some(code) = wait_for_exit(&mut self.child, term_grace) {
            self.reaped = true;
            return Shutdown {
                outcome: ShutdownOutcome::Terminated,
                exit_code: Some(code),
                waited: Duration::ZERO,
            };
        }
        signal_group(self.child.id(), libc::SIGKILL);
        let code = self.child.wait().ok().and_then(|status| status.code());
        self.reaped = true;
        Shutdown {
            outcome: ShutdownOutcome::Killed,
            exit_code: code,
            waited: Duration::ZERO,
        }
    }

    fn is_running(&mut self) -> bool {
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

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<i32> {
        let code = wait_for_exit(&mut self.child, timeout);
        if code.is_some() {
            self.reaped = true;
        }
        code
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if self.is_running() {
            // Shortened graces: a dropped server is a leaked process otherwise.
            self.stop(None, Duration::from_secs(5), Duration::from_secs(2));
        }
    }
}

/// A running `serve` process, its ready file, and a client for it.
pub struct Server {
    proc: Proc,
    ready: ReadyFile,
    client: Client,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("pid", &self.proc.child.id())
            .field("epoch", &self.ready.epoch)
            .field("port", &self.ready.port)
            .field("restarts", &self.ready.restarts)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// Start a server and wait for its ready file (§1), holding the child's stdin.
    pub fn start(cfg: &ServerConfig) -> Result<Server> {
        Server::start_with(cfg, &BootCancel::new(), StdinClose::new())
    }

    /// Start a server, with a flag a caller can raise to abort the wait — the
    /// app's quit path, which must not sit out a slow boot — and with the stdin
    /// pipe it will hold.
    ///
    /// A cancelled boot is stopped the ladder's way, only faster: EOF on stdin,
    /// then `SIGTERM` after [`CANCEL_STOP_GRACE`], then `SIGKILL` after
    /// [`CANCEL_TERM_GRACE`]. Nothing is "killed first".
    pub fn start_with(
        cfg: &ServerConfig,
        cancel: &BootCancel,
        stdin: StdinClose,
    ) -> Result<Server> {
        if !cfg.cwd.is_dir() {
            return Err(Error::Config(format!(
                "{} is not a directory",
                cfg.cwd.display()
            )));
        }
        // A ready file left over from a previous server in this tab dir is not
        // this server's readiness: drop it and wait for a fresh one.
        let _ = std::fs::remove_file(&cfg.ready_file);
        let mut child = spawn(cfg, &stdin)?;
        let deadline = Instant::now() + cfg.ready_timeout;

        loop {
            if cancel.is_cancelled() {
                let mut proc = Proc::new(child, stdin.clone(), cfg);
                let outcome = proc.stop(None, CANCEL_STOP_GRACE, CANCEL_TERM_GRACE);
                return Err(Error::Cancelled(outcome.outcome));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(Error::Boot(Box::new(BootFailure {
                        reason: format!("the server exited during startup ({status})"),
                        log_tail: log_tail(&cfg.log_path, 40),
                        log_path: Some(cfg.log_path.clone()),
                        exit_code: status.code(),
                    })));
                }
                Ok(None) => {}
                Err(e) => return Err(e.into()),
            }
            if let Some(ready) = read_ready(&cfg.ready_file) {
                let client = client_of(&ready, cfg).ok_or_else(|| {
                    Error::Config(format!(
                        "the ready file at {} names a server we will not talk to",
                        cfg.ready_file.display()
                    ))
                })?;
                return Ok(Server {
                    proc: Proc::new(child, stdin, cfg),
                    ready,
                    client,
                });
            }
            if Instant::now() >= deadline {
                let mut proc = Proc::new(child, stdin.clone(), cfg);
                proc.stop(None, cfg.shutdown_grace, cfg.term_grace);
                return Err(Error::Boot(Box::new(BootFailure {
                    reason: format!(
                        "no ready file at {} after {:?}",
                        cfg.ready_file.display(),
                        cfg.ready_timeout
                    ),
                    log_tail: log_tail(&cfg.log_path, 40),
                    log_path: Some(cfg.log_path.clone()),
                    exit_code: None,
                })));
            }
            std::thread::sleep(READY_POLL);
        }
    }

    /// The ready file this server last wrote — and rewrites on every restart.
    pub fn ready(&self) -> &ReadyFile {
        &self.ready
    }

    pub fn pid(&self) -> u32 {
        self.proc.child.id()
    }

    pub fn port(&self) -> u16 {
        self.ready.port
    }

    pub fn epoch(&self) -> &str {
        &self.ready.epoch
    }

    /// The client this server answers on. Never blocks.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The same, owned — a caller that keeps a handle after the tab is gone.
    pub fn log_path(&self) -> &Path {
        &self.proc.log_path
    }

    pub fn ready_file(&self) -> &Path {
        &self.proc.ready_file
    }

    /// The last `lines` lines of the log — what a tab shows when boot fails.
    pub fn log_tail(&self, lines: usize) -> String {
        log_tail(&self.proc.log_path, lines)
    }

    pub fn is_running(&mut self) -> bool {
        self.proc.is_running()
    }

    /// Whether the process is still there, without surprising it.
    pub fn alive(&self) -> bool {
        process_alive(self.proc.child.id())
    }

    /// Wait up to `timeout` for the process to exit; the code, or `None`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<i32> {
        self.proc.wait_for_exit(timeout)
    }

    /// Close our end of the child's stdin. EOF is how a child is told its tab is
    /// gone, and it needs no HTTP, no signal and no pid.
    pub fn close_stdin(&mut self) {
        self.proc.stdin.close();
    }

    /// A closer for the child's stdin, usable from anywhere — the app's quit path
    /// keeps one so it can stop the server and return without waiting for the
    /// ladder.
    pub fn stdin_handle(&self) -> StdinClose {
        self.proc.stdin.clone()
    }

    /// The ladder (§8): stdin EOF, `server.shutdown`, wait, `SIGTERM`, `SIGKILL`.
    /// Never kills first. Safe to run for every tab at once — each call touches
    /// only this server's process.
    pub fn shutdown(&mut self) -> Result<Shutdown> {
        let started = Instant::now();
        if !self.is_running() {
            self.proc.stdin.close();
            return Ok(Shutdown {
                outcome: ShutdownOutcome::AlreadyGone,
                exit_code: None,
                waited: started.elapsed(),
            });
        }
        let client = self.client.clone();
        let outcome = self.proc.stop(
            Some(&client),
            self.proc.shutdown_grace,
            self.proc.term_grace,
        );
        Ok(Shutdown {
            waited: started.elapsed(),
            ..outcome
        })
    }

    /// `SIGKILL` now, for a caller that has given up on the ladder (a panic path).
    pub fn kill(&mut self) {
        if self.is_running() {
            let _ = self.proc.child.kill();
            let _ = self.proc.child.wait();
            self.proc.reaped = true;
        }
        self.proc.stdin.close();
    }
}

impl Proc {
    fn new(child: Child, stdin: StdinClose, cfg: &ServerConfig) -> Proc {
        Proc {
            child,
            stdin,
            log_path: cfg.log_path.clone(),
            ready_file: cfg.ready_file.clone(),
            shutdown_grace: cfg.shutdown_grace,
            term_grace: cfg.term_grace,
            reaped: false,
        }
    }
}

/// The client a ready file describes: loopback only, never without a token, with
/// the caller's patience.
fn client_of(ready: &ReadyFile, cfg: &ServerConfig) -> Option<Client> {
    if ready.token.is_empty() || ready.port == 0 {
        return None;
    }
    Client::loopback(ready.port, ready.token.as_str())
        .ok()?
        .with_timeout(cfg.request_timeout)
        .with_stream_timeout(cfg.stream_timeout)
        .into()
}

/// Read and parse the ready file, if it is there and complete.
///
/// The child writes it atomically (tmp + rename), so a reader either sees the
/// whole document or no file at all; a torn read would be a server bug, and it
/// is treated as "not ready yet".
fn read_ready(path: &Path) -> Option<ReadyFile> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<ReadyFile>(&text).ok()
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
/// leader), so the supervisor and every lane it started hear it too.
fn signal_group(pid: u32, signal: i32) {
    let target = pid as libc::pid_t;
    let sent = unsafe { libc::killpg(target, signal) };
    if sent != 0 {
        unsafe { libc::kill(target, signal) };
    }
}

/// Whether a process is still **running**, for tests and for a tab that wants to
/// be sure its server left.
///
/// `kill(pid, 0)` alone is not the answer: a child of ours that has exited but
/// has not been reaped is a zombie, and the kernel says it exists while nothing
/// is running behind it. The app spawns its own servers, so that case is the
/// common one — a tab stopping its server would keep reporting it alive until
/// something waited on it.
///
/// So our own children are asked for their status (`waitpid(WNOHANG)`, which
/// also reaps them: "it exited" is the answer, and it is a final one), and
/// anything that is not our child is asked the other way.
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let mut status: libc::c_int = 0;
    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    if waited == pid as libc::pid_t {
        // Our child, and this is where it ended.
        return false;
    }
    if waited == 0 {
        // Our child, still running.
        return true;
    }
    // Not our child (ECHILD), or a pid we may not wait on: the kernel is the only
    // one who can say.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    if alive {
        return true;
    }
    // EPERM means it exists and belongs to somebody else.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn spawn(cfg: &ServerConfig, stdin: &StdinClose) -> Result<Child> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_path)
        .map_err(|e| Error::Config(format!("cannot open {}: {e}", cfg.log_path.display())))?;
    let mut command = Command::new(&cfg.bin);
    command
        .args(&cfg.argv)
        .current_dir(&cfg.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    apply_environment(&mut command, cfg);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: one signal reaches the supervisor and its lanes.
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| Error::Config(format!("cannot run {}: {e}", cfg.bin.display())))?;
    // Our end of the pipe, shared with whoever wants to close it: it is the
    // child's whole death signal, so it must outlive this call.
    stdin.put(child.stdin.take());
    Ok(child)
}

/// The child's environment.
///
/// Everything that would tie a server to *our* evo session is removed first, the
/// caller's variables come next, and nothing else is added: the token travels in
/// the ready file, not the environment, and parent death is stdin EOF, not a pid
/// to watch.
pub(crate) fn apply_environment(command: &mut Command, cfg: &ServerConfig) {
    for name in SCRUB_ENV
        .iter()
        .copied()
        .chain(cfg.env_remove.iter().map(String::as_str))
    {
        command.env_remove(name);
    }
    for (key, value) in &cfg.extra_env {
        command.env(key, value);
    }
}

/// The last `lines` lines of a file, read from the end so a long log stays cheap.
///
/// Redacted on the way out: a server log carries whatever the user's
/// configuration put in front of that server, and this text is shown as a boot
/// failure's tail.
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
    redact(&all[from..].join("\n")).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exited_child_is_not_alive() {
        // A child that has exited and has not been reaped is a zombie: `kill(pid, 0)`
        // says it exists, and nothing is running behind it. That is the shape of a
        // server the app spawned and stopped, so the moment it exits is the moment it
        // has to answer "gone".
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("a child to watch");
        let pid = child.id();
        assert!(process_alive(pid), "a running child is alive");
        child.kill().expect("kill the child");
        let deadline = Instant::now() + Duration::from_secs(10);
        while (!is_zombie(pid)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(is_zombie(pid), "the kill landed");
        assert!(
            !process_alive(pid),
            "an exited child is gone, not a zombie to report as running"
        );
        assert!(!process_alive(pid), "reaped, and still gone");
        let _ = child.wait();

        // Anything that is not our child is asked the kernel instead.
        assert!(process_alive(std::process::id()), "ourselves");
        assert!(!process_alive(u32::MAX), "a pid that does not exist");
        assert!(!process_alive(0), "and 0 is never a process");
    }

    /// Whether the process is a zombie: exited, and not yet reaped.
    fn is_zombie(pid: u32) -> bool {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().starts_with('Z'))
            .unwrap_or(false)
    }

    #[test]
    fn the_config_carries_its_own_argv_and_tab_paths() {
        // evo's launch flags belong to store::launch; this crate runs what it is
        // given, and watches the ready file the tab directory implies.
        let config = ServerConfig::swarm(
            "/usr/local/bin/evo-swarm",
            "/tmp/proj",
            Path::new("/tmp/tab"),
        )
        .with_argv([
            "serve",
            "--port",
            "0",
            "--ready-file",
            "/tmp/tab/ready.json",
        ]);
        assert_eq!(config.argv[0], "serve");
        assert_eq!(config.ready_file, PathBuf::from("/tmp/tab/ready.json"));
        assert_eq!(config.log_path, PathBuf::from("/tmp/tab/swarm.log"));
        assert_eq!(config.cwd, PathBuf::from("/tmp/proj"));
    }

    #[test]
    fn a_log_tail_is_redacted() {
        let dir = std::env::temp_dir().join(format!("swarm-log-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("swarm.log");
        std::fs::write(
            &path,
            "ready\nerror: {\"error\":\"The value \\\"Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2\\\" is not of type LIST\"}\nstill here\n",
        )
        .unwrap();
        let tail = log_tail(&path, 3);
        assert!(!tail.contains("Zm9vYmFy"), "{tail}");
        assert!(tail.contains("Bearer <redacted>"), "{tail}");
        assert!(tail.starts_with("ready\nerror:"), "{tail}");
        assert!(tail.ends_with("still here"), "{tail}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_childs_environment_is_scrubbed() {
        // `/usr/bin/env` prints the environment it was handed, which is the one
        // way to see what a child really gets: the evo session we may be running
        // inside must not leak into the server we start, and the caller's own
        // variables must survive the scrub.
        let cfg = ServerConfig::swarm("/usr/bin/env", "/tmp", Path::new("/tmp/tab"))
            .with_env("EVO_SESSIONS_DIR", "/mine")
            .with_env("KEEP_ME", "yes");
        let mut command = Command::new("/usr/bin/env");
        apply_environment(&mut command, &cfg);
        let output = command.output().expect("/usr/bin/env runs");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("EVO_SUPERVISED_CHILD="), "{text}");
        assert!(!text.contains("EVO_SERVE_TOKEN="), "{text}");
        assert!(text.contains("EVO_SESSIONS_DIR=/mine"), "{text}");
        assert!(text.contains("KEEP_ME=yes"), "{text}");
    }

    #[test]
    fn a_ready_file_that_is_not_there_or_not_complete_is_not_readiness() {
        let dir = std::env::temp_dir().join(format!("swarm-ready-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ready.json");
        let _ = std::fs::remove_file(&path);
        assert!(read_ready(&path).is_none(), "no file, no readiness");
        std::fs::write(&path, "{\"epoch\":\"e\"").unwrap();
        assert!(read_ready(&path).is_none(), "a torn file is not readiness");
        std::fs::write(
            &path,
            serde_json::json!({
                "epoch": "7f3a", "pid": 12, "port": 5300, "url": "http://127.0.0.1:5300/",
                "token": "t", "session": {"id": "s", "path": "/p"}, "program": "evo-swarm",
                "version": "1"
            })
            .to_string(),
        )
        .unwrap();
        let ready = read_ready(&path).expect("a complete file is readiness");
        assert_eq!(ready.port, 5300);
        let config = ServerConfig::swarm("/bin/true", "/tmp", &dir);
        assert!(client_of(&ready, &config).is_some());
        assert!(client_of(
            &ReadyFile {
                token: String::new(),
                ..ready
            },
            &config
        )
        .is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
