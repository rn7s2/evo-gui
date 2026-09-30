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

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::server::{Server, ServerConfig};

/// The fake server's script, beside this crate.
pub fn fake_serve_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_serve.py")
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
    /// Start one in `dir` (the tab directory), with the fake server's binary.
    pub fn start(dir: impl Into<PathBuf>) -> Result<FakeSwarm> {
        FakeSwarm::with_config(dir, |bin, cwd, tab| ServerConfig::swarm(bin, cwd, tab))
    }

    /// Start one, letting the caller shape the config — the cwd, the workers,
    /// the timeouts.
    pub fn with_config<F>(dir: impl Into<PathBuf>, shape: F) -> Result<FakeSwarm>
    where
        F: FnOnce(PathBuf, &Path, &Path) -> ServerConfig,
    {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd)?;
        let is_fake = std::env::var_os("EVO_SWARM_BIN").is_none();
        let bin = fake_swarm_bin(&dir)?;
        let config = shape(bin.clone(), &cwd, &dir);
        let server = Server::start(&config)?;
        Ok(FakeSwarm {
            server,
            dir,
            bin,
            is_fake,
        })
    }

    /// Whether the real binary answered, rather than the fake.
    pub fn is_fake(&self) -> bool {
        self.is_fake
    }

    pub fn server(&self) -> &Server {
        &self.server
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
        let reply = self.server.client().http().post(path, &body)?;
        if !reply.is_success() {
            return Err(Error::Status(reply.error()));
        }
        Ok(serde_json::from_slice(&reply.body)?)
    }
}
