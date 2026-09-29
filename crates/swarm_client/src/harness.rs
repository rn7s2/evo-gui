//! A real swarm, in a temp `HOME`, with a scripted model — the shape of
//! `../evo-agent/tests/swarm-serve-e2e.py`, as a Rust module other crates' tests
//! can reuse (feature `test-harness`).
//!
//! What it gives you: a temp `EVO_HOME` whose `init.lisp` registers the stub
//! provider, a temp project directory, `tests/stub-messages.py` running as the
//! model both the coordinator and the lanes talk to, and a live `evo-swarm
//! serve` with `--evo <the installed evo-agent>`.
//!
//! ```no_run
//! # fn main() -> swarm_client::Result<()> {
//! let h = swarm_client::harness::Harness::start(Default::default())?;
//! println!("{:?}", h.client().health()?.typed.features);
//! println!("{:?}", h.wait_for_lanes_idle(std::time::Duration::from_secs(120))?.lanes);
//! h.client().prompt("SLOW hello")?;
//! # Ok(()) }
//! ```

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::api::{Client, Lane, LaneState, Lanes, Payload, State};
use crate::error::{Error, Result};
use crate::server::{Server, ServerConfig, Shutdown};

/// The secret the stub provider is registered with: a value the test can grep
/// for to prove no key ever reached a journal, a log or a lane file.
pub const STUB_SECRET: &str = "swarm-client-stub-secret-9f31";

/// The model the stub provider serves.
pub const STUB_MODEL: &str = "stub-a";

/// The installed binaries the harness drives.
#[derive(Clone, Debug)]
pub struct Bins {
    pub swarm: PathBuf,
    pub agent: PathBuf,
}

impl Bins {
    /// `/usr/local/bin/evo-swarm` and `evo-agent`, unless `EVO_SWARM_BIN` /
    /// `EVO_AGENT_BIN` say otherwise.
    pub fn installed() -> Bins {
        Bins { swarm: crate::default_swarm_bin(), agent: crate::default_agent_bin() }
    }

    /// Whether both are there — a test that needs them can skip otherwise.
    pub fn available(&self) -> bool {
        self.swarm.is_file() && self.agent.is_file()
    }
}

/// The `init.lisp` the stub provider is registered by.
pub fn stub_init_lisp(stub_port: u16, model: &str) -> String {
    format!(
        "(evo:register-provider :stub :base-url \"http://127.0.0.1:{stub_port}\" \
         :api-key \"{STUB_SECRET}\")\n\
         (evo:register-model \"{model}\" :provider :stub :context-window 200000 \
         :max-output 8000 :effort t)\n\
         (evo:set-setting :model \"{model}\")\n"
    )
}

/// Where `tests/stub-messages.py` lives, unless `EVO_STUB_MESSAGES` says.
pub fn stub_script() -> PathBuf {
    if let Some(path) = std::env::var_os("EVO_STUB_MESSAGES") {
        return PathBuf::from(path);
    }
    let repo = std::env::var_os("EVO_AGENT_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/bytedance/coding/evo-agent"));
    repo.join("tests/stub-messages.py")
}

/// `tests/stub-messages.py`: the scripted model. It answers from the last user
/// turn, so a test scripts the "model" by what it sends — `CALL <tool> {json}`
/// becomes that tool call, `DELAY2 SLOW …` streams 60 deltas a tenth apart.
pub struct StubProvider {
    port: u16,
    child: Child,
    _stdout: ChildStdout,
}

impl StubProvider {
    pub fn start() -> Result<StubProvider> {
        let script = stub_script();
        if !script.is_file() {
            return Err(Error::Config(format!(
                "no stub model at {} (set EVO_STUB_MESSAGES)",
                script.display()
            )));
        }
        let port = crate::server::free_port()?;
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Config(format!("cannot run python3 {}: {e}", script.display())))?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.starts_with("stub listening") {
            let _ = child.kill();
            return Err(Error::Config(format!("the stub model did not start: {line:?}")));
        }
        Ok(StubProvider { port, child, _stdout: reader.into_inner() })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Every request the stub has been sent, as recorded.
    pub fn requests(&self) -> Result<Vec<Value>> {
        let reply = crate::http::HttpClient::loopback(self.port, crate::http::Token::new(""))
            .with_timeout(Duration::from_secs(10))
            .get("/_requests")?;
        Ok(serde_json::from_value(reply.json()?)?)
    }

    /// The first request by `role` whose last user text contains `needle` and
    /// that arrived at or after `after` — a wall-clock time taken before the
    /// prompt, so a stale request from an earlier step is not mistaken for this
    /// one's. `role` is `coordinator`, `agent`, or `lane 1`.
    pub fn find(&self, role: &str, needle: &str, after: f64) -> Option<Value> {
        self.requests().ok()?.into_iter().find(|request| {
            request.get("role").and_then(Value::as_str) == Some(role)
                && request
                    .get("last_user")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(needle))
                && request.get("time").and_then(Value::as_f64).unwrap_or(0.0) >= after
        })
    }
}

impl Drop for StubProvider {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A directory that removes itself, unless `EVO_SWARM_KEEP_TMP` is set (or
/// [`TempDir::keep`] was called) — a failed test's logs are worth keeping.
pub struct TempDir {
    path: PathBuf,
    keep: bool,
}

impl TempDir {
    pub fn new(prefix: &str) -> Result<TempDir> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{nanos:x}-{:x}",
            std::process::id(),
            unique()
        ));
        fs::create_dir_all(&path)?;
        Ok(TempDir { path, keep: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.keep || std::env::var_os("EVO_SWARM_KEEP_TMP").is_some() {
            eprintln!("swarm_client test dir kept: {}", self.path.display());
            return;
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// How the harness's swarm is started.
#[derive(Clone, Debug)]
pub struct HarnessConfig {
    /// Lanes to start. The reference harness uses two.
    pub workers: u16,
    /// The model `init.lisp` registers and sets.
    pub model: String,
    /// Extra Lisp appended to the temp `init.lisp`.
    pub init_extra: String,
    /// Run the coordinator in quarantine mode (`--no-userspace`).
    pub no_userspace: bool,
}

impl Default for HarnessConfig {
    fn default() -> HarnessConfig {
        HarnessConfig { workers: 2, model: STUB_MODEL.to_owned(), init_extra: String::new(), no_userspace: false }
    }
}

/// The environment a swarm needs, without the swarm: a temp `HOME` whose
/// `init.lisp` registers the stub provider, a temp project, and the installed
/// binaries. What a crate that starts a server itself (tab_engine) needs, so it
/// does not have to re-derive the hermetic setup.
///
/// `dir` is declared last so it is dropped last: the stub is stopped before the
/// directory it logs into disappears.
pub struct Fixture {
    pub bins: Bins,
    pub stub: StubProvider,
    pub home: PathBuf,
    pub project: PathBuf,
    /// The model `init.lisp` registers and sets.
    pub model: String,
    pub dir: TempDir,
}

impl Fixture {
    pub fn new(config: HarnessConfig) -> Result<Fixture> {
        let bins = Bins::installed();
        if !bins.available() {
            return Err(Error::Config(format!(
                "no binaries to test against: {} / {}",
                bins.swarm.display(),
                bins.agent.display()
            )));
        }
        let dir = TempDir::new("swarm-client-fixture")?;
        let home = dir.join("home");
        let project = dir.join("proj");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&project)?;
        let stub = StubProvider::start()?;
        let mut init = stub_init_lisp(stub.port(), &config.model);
        init.push_str(&config.init_extra);
        fs::write(home.join("init.lisp"), init)?;
        Ok(Fixture { bins, stub, home, project, model: config.model, dir })
    }

    /// The tab directory the server is told to keep its token and log in.
    pub fn tab_dir(&self) -> &Path {
        self.dir.path()
    }

    /// The environment a hermetic server needs: `EVO_HOME` at the temp home, the
    /// agent binary for the lanes, and no real provider key.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("EVO_HOME".to_owned(), self.home.to_string_lossy().into_owned()),
            ("EVO_BINARY".to_owned(), self.bins.agent.to_string_lossy().into_owned()),
            ("TERM".to_owned(), "xterm-256color".to_owned()),
        ]
    }

    /// The environment variables to drop, so no real provider key leaks in.
    pub fn env_remove(&self) -> Vec<String> {
        vec!["ANTHROPIC_API_KEY".to_owned()]
    }

    /// The config a caller hands to [`Server::start`]: the fixture's project as
    /// cwd, its directory as the tab directory, `--evo` pointed at the installed
    /// agent, the stub model, and the hermetic environment above.
    pub fn server_config(&self, workers: u16, no_userspace: bool) -> ServerConfig {
        let mut config = ServerConfig::swarm(&self.bins.swarm, &self.project, self.dir.path())
            .with_workers(workers)
            .with_evo(&self.bins.agent)
            .with_no_userspace(no_userspace);
        for (key, value) in self.env() {
            config = config.with_env(key, value);
        }
        for key in self.env_remove() {
            config = config.with_env_removed(key);
        }
        config.ready_timeout = Duration::from_secs(120);
        config
    }
}

/// A live swarm, its stub model and its temp `HOME`.
///
/// `dir` is declared last so it is dropped last: the server and the stub are
/// stopped before the directory they are logging into disappears.
pub struct Harness {
    pub home: PathBuf,
    pub project: PathBuf,
    pub bins: Bins,
    pub stub: StubProvider,
    pub server: Server,
    pub token_file: PathBuf,
    pub log_path: PathBuf,
    pub dir: TempDir,
}

impl Harness {
    /// Start a stub provider, write the temp `HOME`, and start the swarm.
    pub fn start(config: HarnessConfig) -> Result<Harness> {
        let fixture = Fixture::new(config.clone())?;
        let server_config = fixture.server_config(config.workers, config.no_userspace);
        let token_file = server_config.token_file.clone();
        let log_path = server_config.log_path.clone();
        let server = Server::start(&server_config)?;
        let Fixture { bins, stub, home, project, dir, .. } = fixture;
        Ok(Harness {
            token_file,
            log_path,
            home,
            project,
            bins,
            stub,
            server,
            dir,
        })
    }

    pub fn client(&self) -> &Client {
        self.server.client()
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }

    pub fn state(&self) -> Result<Payload<State>> {
        self.client().state()
    }

    pub fn lanes(&self) -> Result<Payload<Lanes>> {
        self.client().lanes()
    }

    /// The lane rows once every lane is idle, else an error naming what they were.
    pub fn wait_for_lanes_idle(&self, timeout: Duration) -> Result<Payload<Lanes>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(lanes) = self.lanes() {
                if !lanes.lanes.is_empty() && lanes.all_idle() {
                    return Ok(lanes);
                }
            }
            if Instant::now() >= deadline {
                let seen = self.lanes().map(|lanes| {
                    lanes
                        .lanes
                        .iter()
                        .map(|lane| format!("{}:{}", lane.n, lane.state))
                        .collect::<Vec<_>>()
                });
                return Err(Error::Timeout(format!(
                    "lanes never came up idle within {timeout:?}: {seen:?}"
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait for lane `n` to reach `want`.
    pub fn wait_lane_state(&self, n: u32, want: LaneState, timeout: Duration) -> Result<Lane> {
        let deadline = Instant::now() + timeout;
        loop {
            let seen = self
                .lanes()
                .ok()
                .and_then(|lanes| lanes.lane(n).cloned())
                .map(|lane| (lane.state(), lane));
            if let Some((state, lane)) = seen {
                if state == want {
                    return Ok(lane);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!("lane {n} never reached {want:?} in {timeout:?}")));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Wait until the swarm process is gone.
    pub fn wait_server_gone(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !crate::server::process_alive(self.server.pid()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stop the swarm the ladder way (§3) and report how it went.
    pub fn shutdown(&mut self) -> Result<Shutdown> {
        self.server.shutdown()
    }

    /// Every file under the temp `HOME` holding `needle` — the no-secrets check:
    /// no provider key reaches a journal, a log or a lane file.
    pub fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![self.home.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path != self.home.join("init.lisp") {
                    if let Ok(text) = fs::read_to_string(&path) {
                        if text.contains(needle) {
                            found.push(path);
                        }
                    }
                }
            }
        }
        found
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Server's own Drop finishes the ladder if this never ran.
        let _ = self.server.shutdown();
    }
}
