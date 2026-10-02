//! A fake `serve` to test against (feature `test-harness`).
//!
//! [`FakeSwarm`] starts `tests/fake_serve.py` the way the app starts the real
//! thing — same argv, same ready file, same held stdin pipe — and lets a test
//! drive it: publish ops to the streams, script an op's reply, read back what the
//! client asked for. The server interprets nothing (`session` owns meaning), so
//! what a test publishes is exactly what a client receives.
//!
//! `EVO_SWARM_BIN` swaps the real binary in for the fake, for a run against
//! evo-agent; the `/_…` control endpoints then answer nothing, so a test that
//! needs them asks [`FakeSwarm::is_fake`] first.
//!
//! The argv is the caller's — `store::launch` is the one place that knows evo's
//! launch flags — so the harness only offers [`serving_argv`], the shape of a
//! `serve` command, for a test to fill in.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::server::{Server, ServerConfig};

/// The fake server's script, beside this crate.
pub fn fake_serve_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_serve.py")
}

/// The argv that starts a `serve`: `serve --port 0 --ready-file <path>
/// --watch-stdin …`. `store::launch` builds the real one; this is the shape the
/// fake and the tests use.
pub fn serving_argv(ready_file: &Path, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "serve".to_owned(),
        "--port".to_owned(),
        "0".to_owned(),
        "--ready-file".to_owned(),
        ready_file.display().to_string(),
        "--watch-stdin".to_owned(),
    ];
    argv.extend(extra.iter().map(|arg| (*arg).to_owned()));
    argv
}

/// Hand a real server the stub home it needs to answer without a key.
///
/// `EVO_TEST_HOME` is a `scripts/stub_home.sh` directory; it becomes the child's
/// `HOME`/`EVO_HOME` (`EVO_TEST_EVO_HOME` overrides the latter), and the
/// variables that would tie the child to *our* evo session are dropped.
///
/// There is no fallback to the real `HOME`: a server started there reads the
/// person's own `init.lisp`, calls their own providers — real requests, real
/// spend — and writes its sessions into their history. A test that reached a
/// real binary without a stub home is stopped here, loudly, instead.
pub fn with_stub_home(config: ServerConfig) -> ServerConfig {
    let Some(home) = std::env::var_os("EVO_TEST_HOME") else {
        panic!(
            "EVO_TEST_HOME is not set: a test against a real evo binary never runs in \
             the real HOME (it would call your own providers and write your history). \
             Start a stub home with `scripts/stub_home.sh start DIR` and set \
             EVO_TEST_HOME=DIR"
        );
    };
    let home = PathBuf::from(home);
    let evo_home = std::env::var_os("EVO_TEST_EVO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".evo/"));
    let _ = std::fs::create_dir_all(&evo_home);
    config
        .with_env("HOME", home.to_string_lossy())
        .with_env("EVO_HOME", evo_home.to_string_lossy())
        .with_env_removed("EVO_SESSIONS_DIR")
        .with_env_removed("EVO_SUPERVISED_CHILD")
}

/// How long a server this harness started is given to stop before the ladder
/// escalates. A real swarm stops its lanes one at a time, and these tests run in
/// parallel on a machine that is not idle: ten seconds is enough for one server
/// and not for thirteen at once.
pub const STOP_PATIENCE: Duration = Duration::from_secs(30);

/// A [`ServerConfig`] that runs the fake server in `dir`, with extra argv.
///
/// With `EVO_SWARM_BIN` set the same argv drives the *real* `evo-swarm`, and
/// then three things differ, because a real server is not the fake:
///
/// * it gets `--workers 1` unless the caller asks for more — a swarm with its
///   default workers is a supervisor, six lanes and their sessions, and a test
///   of the protocol does not need them;
/// * it gets `--evo $EVO_AGENT_BIN` when the environment names the agent its
///   lanes run;
/// * its ladder is given [`STOP_PATIENCE`], so a loaded machine still sees the
///   graceful ending rather than the `SIGTERM` after it.
///
/// The stub home is applied here rather than by each caller: a real server needs
/// one to answer without a key.
pub fn fake_config(dir: &Path, extra: &[&str]) -> std::io::Result<ServerConfig> {
    let bin = fake_swarm_bin(dir)?;
    let real = std::env::var_os("EVO_SWARM_BIN").is_some();
    let config = ServerConfig::swarm(bin, dir, dir);
    let mut extra: Vec<String> = extra.iter().map(|arg| (*arg).to_owned()).collect();
    if real {
        if !extra.iter().any(|arg| arg == "--workers") {
            extra.push("--workers".to_owned());
            extra.push("1".to_owned());
        }
        if let Some(agent) = std::env::var_os("EVO_AGENT_BIN") {
            extra.push("--evo".to_owned());
            extra.push(agent.to_string_lossy().into_owned());
        }
    }
    let borrowed: Vec<&str> = extra.iter().map(String::as_str).collect();
    let argv = serving_argv(&config.ready_file, &borrowed);
    // The fake needs no home at all; the real swarm, pointed at by EVO_SWARM_BIN,
    // must never get the person's own (see `with_stub_home`).
    let mut config = if real {
        with_stub_home(config.with_argv(argv))
    } else {
        config.with_argv(argv)
    };
    if real {
        config.shutdown_grace = STOP_PATIENCE;
        config.term_grace = Duration::from_secs(15);
    }
    Ok(config)
}

/// A binary that runs the fake server: a one-line `sh` wrapper, because the
/// spawner takes a path to a program and the protocol's argv is not python's.
///
/// `EVO_SWARM_BIN` (the real `evo-swarm` built by another checkout) wins when it
/// is set, so the same tests can be pointed at the real thing.
pub fn fake_swarm_bin(dir: &Path) -> std::io::Result<PathBuf> {
    if let Some(real) = std::env::var_os("EVO_SWARM_BIN") {
        return Ok(PathBuf::from(real));
    }
    std::fs::create_dir_all(dir)?;
    let wrapper = dir.join("fake-serve");
    let script = fake_serve_script();
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec python3 {} \"$@\"\n", script.display()),
    )?;
    set_executable(&wrapper)?;
    Ok(wrapper)
}

/// A startup script that fails a boot in a way a test can assert on: it exits
/// immediately, or it never writes a ready file.
pub fn script_that(dir: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
    set_executable(&path)?;
    Ok(path)
}

fn set_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Kill a process by pid, for a test that wants a server to die under a tab.
pub fn kill_process(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

/// Kill a spawned server and everything under it: the supervisor is spawned as
/// its own process group leader, so one signal reaches its serving child too.
///
/// `0` is not a pid — it means "my own process group" to `killpg` — so it is
/// refused rather than obeyed.
pub fn kill_tree(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let sent = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    sent == 0 || kill_process(pid)
}

/// A server this test must not outlive.
///
/// A test that drives a server from *another thread* — the tab's engine owns its
/// `Server` — can fail before that thread runs the shutdown ladder, and a leaked
/// server spins a core until somebody notices. This guard kills the tree when the
/// test ends, however it ends.
pub struct OrphanGuard {
    pid: u32,
}

impl OrphanGuard {
    pub fn new(pid: u32) -> OrphanGuard {
        OrphanGuard { pid }
    }

    /// Forget it: the test stopped the server itself, and this pid may belong to
    /// somebody else by now.
    pub fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for OrphanGuard {
    fn drop(&mut self) {
        if self.pid != 0 {
            let _ = kill_tree(self.pid);
        }
    }
}

/// A directory of one's own, removed when it goes out of scope.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A fresh directory under the system temporary directory.
    pub fn new(tag: &str) -> std::io::Result<TempDir> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("evo-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(TempDir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A fake server, spawned and ready.
pub struct FakeSwarm {
    server: Server,
    dir: PathBuf,
    bin: PathBuf,
    is_fake: bool,
}

impl FakeSwarm {
    /// Start one in `dir` (the tab directory), with the fake server's binary and
    /// no argv of its own.
    pub fn start(dir: impl Into<PathBuf>) -> Result<FakeSwarm> {
        FakeSwarm::with_argv(dir, &[])
    }

    /// Start one, with extra argv — `--workers 2`, a shorter patience.
    pub fn with_argv(dir: impl Into<PathBuf>, extra: &[&str]) -> Result<FakeSwarm> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let bin = fake_swarm_bin(&dir)?;
        let server = Server::start(&fake_config(&dir, extra)?)?;
        Ok(FakeSwarm {
            server,
            dir,
            bin,
            is_fake: std::env::var_os("EVO_SWARM_BIN").is_none(),
        })
    }

    /// Whether the real binary answered, rather than the fake.
    pub fn is_fake(&self) -> bool {
        self.is_fake
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// The server, to stop it or ask about its process.
    pub fn server_mut(&mut self) -> &mut Server {
        &mut self.server
    }

    pub fn client(&self) -> &Client {
        self.server.client()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn epoch(&self) -> &str {
        self.server.epoch()
    }

    /// The binary a caller should spawn to get this server (`TabSpec`, say).
    pub fn bin(&self) -> PathBuf {
        self.bin.clone()
    }

    /// A control handle on this server's fake endpoints.
    pub fn control(&self) -> Control {
        Control {
            client: self.server.client().clone(),
            is_fake: self.is_fake,
        }
    }
}

/// The fake server's control endpoints (`/_…`), which a real server does not have.
///
/// [`Control::attach`] finds whatever server is serving a tab directory — the
/// engine's own child, for a test that started a tab rather than a [`FakeSwarm`]
/// — so a test can drive the server the tab is really talking to.
pub struct Control {
    client: Client,
    is_fake: bool,
}

impl Control {
    /// Attach to the server whose ready file is in `dir` (the tab directory).
    pub fn attach(dir: &Path) -> Result<Control> {
        let text = std::fs::read_to_string(dir.join("ready.json"))
            .map_err(|e| Error::Config(format!("no ready file in {}: {e}", dir.display())))?;
        let ready: crate::protocol::ReadyFile = serde_json::from_str(&text)?;
        Ok(Control {
            client: Client::loopback(ready.port, ready.token.as_str())?,
            is_fake: std::env::var_os("EVO_SWARM_BIN").is_none(),
        })
    }

    /// The client the control endpoints answer on.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Replace what `/snapshot` answers for these topics.
    pub fn snapshot_body(&self, topics: Value) -> Result<()> {
        self.control("/_snapshot", json!({"topics": topics}))?;
        Ok(())
    }

    /// Publish one op to every open stream. Its `seq` is assigned here.
    pub fn emit(&self, op: Value) -> Result<()> {
        self.control("/_emit", json!({"op": op}))?;
        Ok(())
    }

    /// Script the reply an op gets, for the refusals a tab must show.
    pub fn reply_for(&self, op: &str, reply: Value) -> Result<()> {
        self.control("/_reply", json!({"op": op, "reply": reply}))?;
        Ok(())
    }

    /// Answer the next `times` requests for this op with nothing at all — the
    /// connection dies with the request written. That is a *lost* reply, not a
    /// refusal, and it is what a slow server does to a `input.send` carrying a
    /// picture.
    pub fn drop_ops(&self, op: &str, times: u32) -> Result<()> {
        self.control("/_drop_op", json!({"op": op, "times": times}))?;
        Ok(())
    }

    /// Fail the next `times` snapshots, the way a server that is not ready yet
    /// does: the one ask a client has to repeat rather than read as "no answer".
    pub fn fail_snapshots(&self, times: u32) -> Result<()> {
        self.control("/_fail_snapshot", json!({"times": times}))?;
        Ok(())
    }

    /// Close every open stream, to see the client reconnect.
    pub fn drop_streams(&self) -> Result<()> {
        self.control("/_drop", json!({}))?;
        Ok(())
    }

    /// Publish a `stream.reset` with this reason.
    pub fn stream_reset(&self, reason: &str) -> Result<()> {
        self.control("/_reset", json!({"reason": reason}))?;
        Ok(())
    }

    /// Re-exec the server: a new epoch, on the port it bound, with the token it
    /// minted — what a supervisor restart looks like from a client's side (§1).
    pub fn restart(&self) -> Result<()> {
        self.control("/_restart", json!({}))?;
        Ok(())
    }

    /// Become a different process lifetime: a new epoch, a fresh sequence.
    pub fn new_epoch(&self) -> Result<String> {
        let body = self.control("/_epoch", json!({}))?;
        Ok(body["epoch"].as_str().unwrap_or_default().to_owned())
    }

    /// Make the next `since` look older than the retention.
    pub fn forget_cursors(&self) -> Result<()> {
        self.control("/_forget", json!({}))?;
        Ok(())
    }

    /// Every request this server has seen, and forget them.
    pub fn requests(&self) -> Vec<Value> {
        match self.control("/_requests", json!({})) {
            Ok(body) => body["requests"].as_array().cloned().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// The requests whose path starts with `prefix` (e.g. `/ops`).
    pub fn requests_on(&self, prefix: &str) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|request| {
                request["path"]
                    .as_str()
                    .map(|path| path.starts_with(prefix))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// One of the fake's `/_…` endpoints. Only the fake answers them.
    fn control(&self, path: &str, body: Value) -> Result<Value> {
        if !self.is_fake {
            return Err(Error::Config(
                "the control endpoints exist only on the fake server".into(),
            ));
        }
        let reply = self.client.http().post(path, &body)?;
        if !reply.is_success() {
            return Err(Error::Status(reply.error()));
        }
        Ok(serde_json::from_slice(&reply.body)?)
    }
}
