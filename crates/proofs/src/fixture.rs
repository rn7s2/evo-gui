//! The hermetic environment every proof runs in: a temp `HOME` whose evo home
//! registers the scripted stub model, the binaries the environment names, and
//! one tab directory to run them in.
//!
//! Nothing of the machine is touched: `HOME`, `EVO_HOME` and the project folder
//! are all inside one temp directory, which is removed when the proof passes
//! (`EVO_PROOFS_KEEP=1` keeps it for a failure to read).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use store::launch::{LaunchSpec, Program};
use store::paths::{Root, TabId};
use swarm_client::{Server, ServerConfig};

/// How long a proof's own progress is given before it gives up.
pub const WAIT: Duration = Duration::from_secs(240);

/// What the proofs' timing lines are prefixed with.
pub const NOTE: &str = "[proof]";

/// The model the stub home registers, and the (unchecked) key it is registered
/// with: a provider needs one.
const STUB_MODEL: &str = "stub-a";
const STUB_SECRET: &str = "evo-proofs-stub-secret";

/// How long the stub model gets to print the port it bound.
const STUB_START: Duration = Duration::from_secs(10);

/// What a child must not inherit from the process running the proofs: the outer
/// evo (this may be running inside a lane), a stale test token, and the real
/// provider keys.
const SCRUB: &[&str] = &[
    "EVO_SESSIONS_DIR",
    "EVO_SERVE_TOKEN",
    "EVO_SERVE_WATCH_PID",
    "EVO_SUPERVISED_CHILD",
    "EVO_NO_SUPERVISOR",
    "EVO_HEARTBEAT_FILE",
    "EVO_PID",
    "EVO_IDE_CONTEXT",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
];

/// The two binaries a launch may need, resolved the way the app resolves them:
/// `EVO_SWARM_BIN` / `EVO_AGENT_BIN`, else the installed path (CONTRACT §1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bins {
    pub swarm: PathBuf,
    pub agent: PathBuf,
}

impl Bins {
    pub fn resolve() -> Bins {
        Bins {
            swarm: store::cli::swarm_bin(),
            agent: store::cli::agent_bin(),
        }
    }

    pub fn available(&self) -> bool {
        self.swarm.is_file() && self.agent.is_file()
    }

    /// The binaries, or a panic that says which one to point `EVO_*_BIN` at. A
    /// proof that cannot run is not a proof that passed.
    pub fn expect(&self) -> &Bins {
        assert!(
            self.available(),
            "no binaries to test against: {} / {} — set EVO_SWARM_BIN and EVO_AGENT_BIN \
             to the integration build",
            self.swarm.display(),
            self.agent.display()
        );
        self
    }
}

/// One proof's world.
pub struct Fixture {
    /// The temp directory everything lives under.
    pub dir: PathBuf,
    /// `$HOME` for the children: the stub home.
    pub home: PathBuf,
    /// The app's own root, under the temp directory.
    pub root: Root,
    /// The folder a swarm runs in.
    pub folder: PathBuf,
    /// This proof's tab directory (`ready.json`, `swarm.log`).
    pub tab: TabId,
    pub bins: Bins,
    /// The scripted model, killed when the fixture goes.
    stub: Stub,
}

impl Fixture {
    /// Make one, named for the temp directory it gets.
    pub fn new(name: &str) -> Fixture {
        let bins = Bins::resolve().expect().clone();
        let dir = std::env::temp_dir().join(format!("evo-proofs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let folder = dir.join("project");
        fs::create_dir_all(home.join(".evo")).expect("a stub home");
        fs::create_dir_all(&folder).expect("a project folder");
        let root = Root::at(dir.join("desktop"));
        let tab = TabId::new();
        root.ensure_tab_dir(&tab)
            .expect("this proof's tab directory");

        let stub = Stub::start(&home);
        let fixture = Fixture {
            dir,
            home,
            root,
            folder,
            tab,
            bins,
            stub,
        };
        fixture.write_init_lisp();
        println!(
            "{NOTE} fixture {}: home {}, stub {}",
            name,
            fixture.home.display(),
            fixture.stub_url()
        );
        fixture
    }

    /// The stub model's address, as the home's `init.lisp` registers it.
    pub fn stub_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.stub.port)
    }

    /// A launch in this fixture: the project folder, this proof's ready file and
    /// log, a pipe we hold, and a port the child picks once and reports.
    pub fn spec(&self, program: Program, workers: u16) -> LaunchSpec {
        let mut spec = LaunchSpec::tab(program, &self.folder, &self.root.tab_dir(&self.tab));
        spec.port = Some(0);
        // The lanes run this fixture's own `evo-agent`, whatever `PATH` says.
        spec.agent_bin = (program == Program::Swarm).then(|| self.bins.agent.clone());
        if program == Program::Swarm {
            spec.workers = Some(workers);
        }
        spec
    }

    /// [`Fixture::spec`] in another tab directory: two servers in one folder,
    /// which is the case exact-session restarts exist for (E1).
    pub fn spec_in(&self, program: Program, workers: u16, tab: &TabId) -> LaunchSpec {
        let mut spec = self.spec(program, workers);
        spec.ready_file = Some(LaunchSpec::ready_file_in(&self.root, tab));
        spec
    }

    /// A launch that resumes one exact session (§1: never a bare `--resume`).
    pub fn resume_spec(&self, program: Program, session: &std::path::Path) -> LaunchSpec {
        let mut spec = self.spec(program, 0);
        spec.resume = Some(session.to_path_buf());
        spec
    }

    /// Another tab of the same window: the same folder, its own directory, its
    /// own ready file and its own session.
    pub fn new_tab(&self) -> TabId {
        let id = TabId::new();
        self.root.ensure_tab_dir(&id).expect("a tab directory");
        id
    }

    /// Where a server's ready file is (§1).
    pub fn ready_path(&self) -> PathBuf {
        self.ready_path_of(&self.tab)
    }

    pub fn ready_path_of(&self, tab: &TabId) -> PathBuf {
        self.root.tab_ready(tab)
    }

    /// The ready file as it is *now* — after a supervisor restart it has been
    /// rewritten, which is how a client learns the new epoch, port and token.
    pub fn read_ready(&self) -> Option<swarm_client::ReadyFile> {
        self.read_ready_of(&self.tab)
    }

    pub fn read_ready_of(&self, tab: &TabId) -> Option<swarm_client::ReadyFile> {
        let text = fs::read_to_string(self.ready_path_of(tab)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// A server's log, the evidence a boot failure shows.
    pub fn log_of(&self, tab: &TabId) -> String {
        fs::read_to_string(self.root.tab_log(tab)).unwrap_or_default()
    }

    /// The command line of a live process: what a restarted child was actually
    /// told, which is how "the exact session" is checked from outside.
    pub fn command_line(pid: u32) -> String {
        let out = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A client for a ready file, of the shape a tab builds.
    pub fn client_of(ready: &swarm_client::ReadyFile) -> swarm_client::Client {
        swarm_client::Client::loopback(ready.port, ready.token.clone()).expect("a loopback client")
    }

    /// Start it. The argv is `store::launch`'s — the one place that knows evo's
    /// flags — and the environment is this fixture's.
    pub fn spawn(&self, spec: &LaunchSpec) -> Server {
        let bin = match spec.program {
            Some(Program::Swarm) => self.bins.swarm.clone(),
            _ => self.bins.agent.clone(),
        };
        // The tab directory is the spec's own: the ready file the argv names and
        // the log beside it are that tab's, whichever tab of the fixture it is.
        let tab_dir = spec
            .ready_file
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.tab_dir(&self.tab));
        let mut cfg = ServerConfig::swarm(bin, &self.folder, &tab_dir).with_argv(spec.argv());
        for (key, value) in self.env() {
            cfg = cfg.with_env(key, value);
        }
        for key in self.env_remove() {
            cfg = cfg.with_env_removed(key);
        }
        Server::start(&cfg).expect("the server came up")
    }

    /// The environment a child runs in, as `scripts/stub_home.sh` sets it: the
    /// stub home (with evo's trailing separator), the stub's address, and the
    /// agent its lanes run.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".to_owned(), self.home.display().to_string()),
            (
                "EVO_HOME".to_owned(),
                format!("{}/", self.home.join(".evo").display()),
            ),
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            (
                "EVO_BINARY".to_owned(),
                self.bins.agent.display().to_string(),
            ),
            ("STUB_URL".to_owned(), self.stub_url()),
        ]
    }

    pub fn env_remove(&self) -> Vec<String> {
        SCRUB.iter().map(|key| (*key).to_owned()).collect()
    }

    /// Point *this* process at the stub home, for the proofs that run one of the
    /// offline CLIs (`store::cli` spawns with this process's environment). One
    /// proof per test binary, so this is the only thing setting them.
    pub fn enter(&self) {
        std::env::set_var("HOME", &self.home);
        std::env::set_var("EVO_HOME", format!("{}/", self.home.join(".evo").display()));
        std::env::set_var("EVO_AGENT_BIN", &self.bins.agent);
        std::env::set_var("EVO_SWARM_BIN", &self.bins.swarm);
        std::env::set_var("STUB_URL", self.stub_url());
    }

    /// The three lines `scripts/stub_home.sh` writes: the provider, the model,
    /// and the default.
    fn write_init_lisp(&self) {
        let lisp = format!(
            "(evo:register-provider :stub :base-url \"{}\" :api-key \"{STUB_SECRET}\")\n\
             (evo:register-model \"{STUB_MODEL}\" :provider :stub :context-window 200000 \
             :max-output 8000 :effort t)\n\
             (evo:set-setting :model \"{STUB_MODEL}\")\n",
            self.stub_url()
        );
        fs::write(self.home.join(".evo").join("init.lisp"), lisp)
            .expect("the stub home's init.lisp");
    }

    /// Every process of this fixture's, and only this fixture's.
    ///
    /// Found by the one thing they all have in common: their command line names
    /// this fixture's own directory — the tab directory the app passes a server,
    /// or a lane's. A pid is not enough. The ready file publishes the *session's*
    /// pid and leaves `supervisor_pid` null, so the process the app actually
    /// spawned (and the one that would restart the session) is named nowhere on
    /// disk; a supervisor that outlives its session is exactly the leak this
    /// exists to prevent. The directory is unique to this fixture — a temp path
    /// with this process's pid in it — so nothing else can match.
    fn processes(&self) -> Vec<u32> {
        let mut needles = vec![self.dir.display().to_string()];
        if let Ok(canonical) = fs::canonicalize(&self.dir) {
            needles.push(canonical.display().to_string());
        }
        let Ok(out) = Command::new("ps").args(["-eo", "pid=,command="]).output() else {
            return Vec::new();
        };
        let ours = std::process::id();
        let text = String::from_utf8_lossy(&out.stdout);
        let mut pids = Vec::new();
        for line in text.lines() {
            let line = line.trim_start();
            let Some((pid, command)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid.trim().parse::<u32>() else {
                continue;
            };
            if pid == ours || !needles.iter().any(|needle| command.contains(needle)) {
                continue;
            }
            pids.push(pid);
        }
        pids
    }

    /// Stop every process still alive in this fixture, and give them `wait` to go.
    ///
    /// All of them are signalled in the same round: telling a session to stop has
    /// no point while the supervisor above it is being told nothing, since its
    /// whole job is to start it again.
    fn reap(&self, signal: libc::c_int, wait: Duration) {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let pids = self.processes();
            if pids.is_empty() {
                return;
            }
            for pid in &pids {
                unsafe { libc::kill(*pid as libc::pid_t, signal) };
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// How many stub requests have been made, and what the last few asked for —
    /// what a proof reads when it wants to know what evo *sent*.
    pub fn stub_requests(&self) -> serde_json::Value {
        let out = Command::new("curl")
            .args(["-s", &format!("{}/_requests", self.stub_url())])
            .output()
            .expect("curl");
        serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A fixture's servers are the fixture's too. A proof's own `Server` stops
        // itself — it holds the child's pipe, and EOF is the whole signal — but a
        // server the *app* started (a capture, a UI test) belongs to nobody's
        // `Drop`, and closes its pipe only when the process running the app ends,
        // after which nothing is left to escalate. So whatever is still alive in
        // this fixture's own tab directories, and is still ours by its own command
        // line, is stopped here: politely, then not.
        self.reap(libc::SIGTERM, Duration::from_secs(3));
        self.reap(libc::SIGKILL, Duration::ZERO);
        self.stub.stop();
        if std::env::var_os("EVO_PROOFS_KEEP").is_some() {
            println!("{NOTE} kept {}", self.dir.display());
            return;
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Whether two spellings name the same place: a server canonicalizes the paths
/// it reports (`/var` is `/private/var` here), and a proof wants the two to be
/// compared as locations.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The scripted model: `evo-agent/tests/stub-messages.py`, on a port it picks.
struct Stub {
    child: Child,
    port: u16,
}

impl Stub {
    /// `EVO_STUB_MESSAGES`, else beside this checkout, else the workspace's own
    /// evo-agent checkout.
    fn script() -> PathBuf {
        if let Some(path) = std::env::var_os("EVO_STUB_MESSAGES") {
            return PathBuf::from(path);
        }
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut candidates = Vec::new();
        if let Some(agent) = std::env::var_os("EVO_AGENT_REPO") {
            candidates.push(PathBuf::from(agent).join("tests/stub-messages.py"));
        }
        candidates.push(repo.join("../evo-agent/tests/stub-messages.py"));
        candidates.push(PathBuf::from(
            "/Users/bytedance/coding/evo/evo-agent/tests/stub-messages.py",
        ));
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .unwrap_or_else(|| {
                panic!(
                    "no stub-messages.py: set EVO_STUB_MESSAGES (tried the sibling \
                     ../evo-agent of this checkout, and this workspace's evo-agent)"
                )
            })
    }

    /// Start it in `home`, and wait for the port it printed. Port 0 means the
    /// stub picks one, so nothing here races for a number.
    fn start(home: &Path) -> Stub {
        let log_path = home.join("stub.log");
        let log = fs::File::create(&log_path).expect("the stub's log");
        let child = Command::new("python3")
            .arg(Stub::script())
            .arg("0")
            .stdout(Stdio::from(log.try_clone().expect("the log twice")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("python3 starts the stub model");
        let port = Stub::wait_for_port(home, &log_path)
            .unwrap_or_else(|| panic!("the stub did not print a port: see {}", log_path.display()));
        Stub { child, port }
    }

    fn wait_for_port(home: &Path, log_path: &Path) -> Option<u16> {
        let started = std::time::Instant::now();
        loop {
            let text = fs::read_to_string(log_path).unwrap_or_default();
            if let Some(port) = text.lines().find_map(|line| {
                line.strip_prefix("stub listening ")
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|word| word.parse::<u16>().ok())
            }) {
                return Some(port);
            }
            if started.elapsed() > STUB_START {
                println!("{NOTE} the stub's log in {}:\n{text}", home.display());
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether PID is a live process. A zombie is not: it is a process already
    /// stopped whose parent — this test — has not reaped it.
    fn alive(pid: u32) -> bool {
        let out = Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        !state.is_empty() && !state.starts_with('Z')
    }

    /// The guarantee the harness makes: a server this fixture started is stopped
    /// when the fixture goes, even one nobody dropped — which is what a server the
    /// *app* started looks like from here. Without it, a capture or a UI test
    /// leaves a serve process behind on the machine it ran on.
    ///
    /// The `Server` value is forgotten, so its end of the child's stdin stays open
    /// in this process: nothing can stop the child but a signal.
    #[test]
    fn a_fixture_stops_a_server_nobody_dropped() {
        let fixture = Fixture::new("reap");
        let server = fixture.spawn(&fixture.spec(Program::Agent, 0));
        let pid = server.ready().pid;
        std::mem::forget(server);
        drop(fixture);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!alive(pid), "server {pid} outlived its fixture");
    }
}
