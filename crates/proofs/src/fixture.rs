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
        let mut spec = LaunchSpec::new(program, &self.folder);
        spec.ready_file = Some(LaunchSpec::ready_file_in(&self.root, &self.tab));
        spec.watch_stdin = true;
        spec.port = Some(0);
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

    /// Where a server's ready file is (§1).
    pub fn ready_path(&self) -> PathBuf {
        self.root.tab_ready(&self.tab)
    }

    /// The ready file as it is *now* — after a supervisor restart it has been
    /// rewritten, which is how a client learns the new epoch, port and token.
    pub fn read_ready(&self) -> Option<swarm_client::ReadyFile> {
        let text = fs::read_to_string(self.ready_path()).ok()?;
        serde_json::from_str(&text).ok()
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
        let tab_dir = self.root.tab_dir(&self.tab);
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
        self.stub.stop();
        if std::env::var_os("EVO_PROOFS_KEEP").is_some() {
            println!("{NOTE} kept {}", self.dir.display());
            return;
        }
        let _ = fs::remove_dir_all(&self.dir);
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
