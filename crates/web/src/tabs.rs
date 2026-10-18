//! Tabs: one tab is one `serve` process in a folder on this machine.
//!
//! A tab is the desktop app's tab, moved to the serving machine. The process it
//! starts, the flags it is started with and the shutdown ladder that stops it are
//! **not** re-derived here: the argv comes from [`store::launch`], the spawn and
//! the ready file from [`swarm_client::Server`], and the directory the ready file
//! and the log live in from [`store::paths::Root`] — the same three the desktop
//! app uses, so a tab this server opens is a tab that app could adopt.
//!
//! Everything here is blocking (a spawn, a ready-file wait, an HTTP client on a
//! plain socket), so every entry point is `async` and does its waiting on
//! `spawn_blocking`: the axum runtime is never held by a boot.
//!
//! What a tab hands the rest of this server is a [`Client`] — and that is the
//! whole point of the arrangement. The bearer token lives in the tab's own 0600
//! ready file and in this process's memory; the browser never sees it, and never
//! sees a port either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use store::launch::{LaunchSpec, Program};
use store::paths::{Root, TabId};
use swarm_client::{Client, Server, ServerConfig, Shutdown};

/// Which program a tab runs. The wire spelling is the lower-case name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramChoice {
    /// `evo-swarm serve`: a coordinator and its pool of lanes.
    #[default]
    Swarm,
    /// `evo-agent serve`: one agent, no lanes.
    Agent,
}

impl ProgramChoice {
    pub fn program(self) -> Program {
        match self {
            ProgramChoice::Swarm => Program::Swarm,
            ProgramChoice::Agent => Program::Agent,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProgramChoice::Swarm => "swarm",
            ProgramChoice::Agent => "agent",
        }
    }
}

/// What a caller asks for when it opens a tab.
///
/// Unknown fields are ignored rather than refused: the frontend and this server
/// are built by different hands, and a field one of them grows must not turn into
/// a 400 for the other.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct OpenRequest {
    /// The folder the tab runs in — the child's working directory. Absolute:
    /// a relative one would silently mean "wherever this server was started".
    pub folder: PathBuf,
    pub program: ProgramChoice,
    /// `--model ID@PROVIDER`, the coordinator's (or the lone agent's).
    pub model: Option<String>,
    /// `--thinking LEVEL`.
    pub thinking: Option<String>,
    /// `--lane-model ID@PROVIDER`, the model every lane registers.
    pub lane_model: Option<String>,
    /// `--lane-thinking LEVEL`.
    pub lane_thinking: Option<String>,
    /// `--workers N`.
    pub workers: Option<u16>,
    /// `--resume PATH`: the exact journal path to resume.
    pub resume: Option<PathBuf>,
}

impl OpenRequest {
    /// Everything about the request that is this server's to check — the rest is
    /// evo's to refuse, and its refusal arrives as the boot failure.
    ///
    /// This is a system boundary, so it is where the checking belongs: an argv
    /// value this server hands a child is never built by string interpolation
    /// (see [`store::launch`]), but a value that cannot be a model name or a
    /// folder is still worth refusing here, in words the browser can show.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.folder.as_os_str().is_empty() {
            return Err(ApiError::bad_request("folder is required"));
        }
        if !self.folder.is_absolute() {
            return Err(ApiError::bad_request(format!(
                "folder {} must be an absolute path",
                self.folder.display()
            )));
        }
        if !self.folder.is_dir() {
            return Err(ApiError::bad_request(format!(
                "folder {} is not a directory on the serving machine",
                self.folder.display()
            )));
        }
        if let Some(path) = &self.resume {
            if !path.is_file() {
                return Err(ApiError::bad_request(format!(
                    "resume {} is not a file on the serving machine",
                    path.display()
                )));
            }
        }
        if self.workers == Some(0) {
            return Err(ApiError::bad_request("workers must be at least 1"));
        }
        for (name, value) in [
            ("model", &self.model),
            ("thinking", &self.thinking),
            ("lane_model", &self.lane_model),
            ("lane_thinking", &self.lane_thinking),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(ApiError::bad_request(format!("{name} is empty")));
                }
                // A control character has no business in a flag value: it cannot
                // reach a shell (there is none), but it can reach a log line.
                if value.chars().any(char::is_control) {
                    return Err(ApiError::bad_request(format!(
                        "{name} contains a control character"
                    )));
                }
            }
        }
        if self.program == ProgramChoice::Agent {
            for (name, given) in [
                ("lane_model", self.lane_model.is_some()),
                ("lane_thinking", self.lane_thinking.is_some()),
                ("workers", self.workers.is_some()),
            ] {
                if given {
                    return Err(ApiError::bad_request(format!(
                        "{name} is a swarm flag: an agent tab has no lanes"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The launch this request becomes. `tab_dir` is the tab's own directory —
    /// the ready file and the log are named after it, which is what makes the
    /// spawn attachable.
    pub fn spec(&self, tab_dir: &Path, agent_bin: &Path) -> LaunchSpec {
        let program = self.program.program();
        let mut spec = LaunchSpec::tab(program, self.folder.clone(), tab_dir);
        // `--port 0`: nothing has bound a port yet, and the child reports the one
        // it got in the ready file.
        spec.port = Some(0);
        spec.resume = self.resume.clone();
        spec.model = self.model.as_deref().map(model_ref);
        spec.thinking = self.thinking.clone();
        if program == Program::Swarm {
            spec.agent_bin = Some(agent_bin.to_path_buf());
            spec.lane_model = self.lane_model.as_deref().map(model_ref);
            spec.lane_thinking = self.lane_thinking.clone();
            spec.workers = self.workers;
        }
        spec
    }
}

/// `ID@PROVIDER`, or a bare id when there is no provider — the spelling
/// `store::catalog` owns.
fn model_ref(spec: &str) -> store::catalog::ModelRef {
    match spec.split_once('@') {
        Some((id, provider)) if !provider.is_empty() => {
            store::catalog::ModelRef::new(id, Some(provider))
        }
        _ => store::catalog::ModelRef::new(spec, None::<String>),
    }
}

/// What a tab is doing right now.
enum State {
    /// The process is being started and the ready file is still being waited for.
    Starting,
    /// It is listening: this is the handle the proxy talks through.
    Running(Arc<Live>),
    /// It never came up. The reason and the log tail are what the frontend shows.
    Failed(BootError),
    /// It was stopped by a caller, or it left on its own.
    Stopped,
}

/// A live `serve`: its client, and the process behind it.
struct Live {
    client: Client,
    /// Shared so a stop can be moved onto another thread; the `Server` itself is
    /// not `Sync` (it owns a `Child`), so the lock is what makes it usable from
    /// the async side at all.
    server: Arc<Mutex<Server>>,
    pid: u32,
    port: u16,
    epoch: String,
    url: String,
}

/// Why a tab never came up, in the words the API hands the browser.
#[derive(Clone, Debug, Serialize)]
pub struct BootError {
    pub reason: String,
    /// The last lines of the child's log — redacted by `swarm_client`, because a
    /// server log carries whatever the user's configuration put in front of it.
    pub log_tail: String,
    pub log_path: PathBuf,
}

impl BootError {
    /// A boot failure as `swarm_client` reports it, plus the one case it has no
    /// shape for: a process that never started at all.
    fn from_error(error: &swarm_client::Error, log_path: &Path) -> BootError {
        match error {
            swarm_client::Error::Boot(failure) => BootError {
                reason: failure.reason.clone(),
                log_tail: failure.log_tail.clone(),
                log_path: failure
                    .log_path
                    .clone()
                    .unwrap_or_else(|| log_path.to_path_buf()),
            },
            other => BootError {
                reason: other.to_string(),
                log_tail: swarm_client::log_tail(log_path, 40),
                log_path: log_path.to_path_buf(),
            },
        }
    }
}

/// One tab.
pub struct Tab {
    pub id: String,
    pub folder: PathBuf,
    pub program: ProgramChoice,
    pub request: OpenRequest,
    pub created: u64,
    state: Mutex<State>,
}

impl Tab {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// The client of a running tab, or why there is none.
    pub fn client(&self) -> Result<Client, ApiError> {
        match &*self.state() {
            State::Running(live) => Ok(live.client.clone()),
            State::Starting => Err(ApiError::conflict(format!(
                "tab {} is still starting",
                self.id
            ))),
            State::Failed(error) => Err(ApiError::unavailable(error.clone())),
            State::Stopped => Err(ApiError::gone(format!("tab {} is not running", self.id))),
        }
    }

    /// This tab as the API hands it out. Never the token: what a ready file
    /// holds is not this process's to publish (§2 rule 4).
    pub fn view(&self) -> Value {
        let state = self.state();
        let mut view = json!({
            "id": self.id,
            "folder": self.folder,
            "program": self.program.as_str(),
            "model": self.request.model,
            "thinking": self.request.thinking,
            "lane_model": self.request.lane_model,
            "lane_thinking": self.request.lane_thinking,
            "workers": self.request.workers,
            "resume": self.request.resume,
            "created": self.created,
        });
        let map = view.as_object_mut().expect("an object was just built");
        match &*state {
            State::Starting => {
                map.insert("state".into(), json!("starting"));
            }
            State::Running(live) => {
                map.insert("state".into(), json!("running"));
                map.insert("pid".into(), json!(live.pid));
                map.insert("port".into(), json!(live.port));
                map.insert("epoch".into(), json!(live.epoch));
                map.insert("url".into(), json!(live.url));
            }
            State::Failed(error) => {
                map.insert("state".into(), json!("failed"));
                map.insert("error".into(), json!(error));
            }
            State::Stopped => {
                map.insert("state".into(), json!("stopped"));
            }
        }
        view
    }
}

/// Every tab this server has opened, and the machine's own binaries and root.
pub struct Tabs {
    root: Root,
    agent_bin: PathBuf,
    swarm_bin: PathBuf,
    prompt_note: Option<PathBuf>,
    tabs: Mutex<BTreeMap<String, Arc<Tab>>>,
}

impl Tabs {
    pub fn new(
        root: Root,
        agent_bin: PathBuf,
        swarm_bin: PathBuf,
        prompt_note: Option<PathBuf>,
    ) -> Tabs {
        Tabs {
            root,
            agent_bin,
            swarm_bin,
            prompt_note,
            tabs: Mutex::new(BTreeMap::new()),
        }
    }

    fn map(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Tab>>> {
        lock(&self.tabs)
    }

    /// The tab with this id — `None` for an id this server never minted, which is
    /// what makes a path segment safe to use: [`TabId::parse`] refuses anything
    /// that is not `[A-Za-z0-9_-]{1,64}` before it is ever looked up.
    pub fn get(&self, raw: &str) -> Option<Arc<Tab>> {
        let id = TabId::parse(raw)?;
        self.map().get(id.as_str()).cloned()
    }

    /// Every tab, oldest first.
    pub fn list(&self) -> Vec<Arc<Tab>> {
        let mut tabs: Vec<Arc<Tab>> = self.map().values().cloned().collect();
        tabs.sort_by_key(|tab| (tab.created, tab.id.clone()));
        tabs
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    /// Open a tab: make its directory, spawn the process and wait for its ready
    /// file.
    ///
    /// The wait is `swarm_client`'s — the ready file, and nothing else, is
    /// readiness — and it runs off the runtime, so a boot that takes its whole 90
    /// seconds holds nothing else up. A boot that fails leaves the tab in the
    /// registry, `failed`, with the child's own words; the caller gets a 502 that
    /// carries the same thing.
    pub async fn open(&self, request: OpenRequest) -> Result<Arc<Tab>, ApiError> {
        request.validate()?;
        let id = self.mint_id()?;
        let tab_dir = self
            .root
            .ensure_tab_dir(&id)
            .map_err(|e| ApiError::internal(format!("cannot create the tab directory: {e}")))?;

        let mut spec = request.spec(&tab_dir, &self.agent_bin);
        spec.prompt_note = self.prompt_note.clone();

        let tab = Arc::new(Tab {
            id: id.as_str().to_owned(),
            folder: request.folder.clone(),
            program: request.program,
            request: request.clone(),
            created: now_epoch(),
            state: Mutex::new(State::Starting),
        });
        self.map().insert(tab.id.clone(), tab.clone());

        let bin = match request.program {
            ProgramChoice::Swarm => self.swarm_bin.clone(),
            ProgramChoice::Agent => self.agent_bin.clone(),
        };
        let log_path = self.root.tab_log(&id);
        let config =
            ServerConfig::swarm(bin, request.folder.clone(), &tab_dir).with_argv(spec.argv());
        // The argv's `--ready-file` and the file the spawn waits on are one path
        // (§1): a mismatch is a boot that never comes ready, and there is no
        // reason to find that out 90 seconds from now.
        if config.ready_file != spec.ready_file.clone().unwrap_or_default() {
            return Err(ApiError::internal(
                "the launch's ready file and the spawn's are not the same path",
            ));
        }

        let booted = tokio::task::spawn_blocking(move || Server::start(&config))
            .await
            .map_err(|e| ApiError::internal(format!("the spawn task failed: {e}")))?;

        match booted {
            Ok(server) => {
                let live = Arc::new(Live {
                    pid: server.pid(),
                    port: server.port(),
                    epoch: server.epoch().to_owned(),
                    url: server.ready().url.clone(),
                    client: server.client().clone(),
                    server: Arc::new(Mutex::new(server)),
                });
                *tab.state() = State::Running(live);
                Ok(tab)
            }
            Err(error) => {
                let boot = BootError::from_error(&error, &log_path);
                *tab.state() = State::Failed(boot.clone());
                Err(ApiError::bad_gateway(
                    format!("tab {} did not come up: {}", tab.id, boot.reason),
                    json!({ "tab": tab.id, "error": boot.reason, "log_tail": boot.log_tail, "log_path": boot.log_path }),
                ))
            }
        }
    }

    /// Stop a tab with the ladder `swarm_client` owns — stdin EOF, the shutdown
    /// op, `SIGTERM`, `SIGKILL` — and forget it.
    ///
    /// Its directory is left alone: the ready file is written by the child and is
    /// gone with it, and the log is what a person reads when a session did
    /// something surprising. Nothing here holds a secret once the process is
    /// stopped.
    ///
    /// A tab that never came up has no process to stop and is simply forgotten;
    /// that is what makes a failed tab removable from the frontend.
    pub async fn stop(&self, raw_id: &str) -> Result<Value, ApiError> {
        let tab = self
            .get(raw_id)
            .ok_or_else(|| ApiError::not_found(format!("no tab {raw_id}")))?;
        let live = match &*tab.state() {
            State::Running(live) => Some(live.clone()),
            _ => None,
        };
        let outcome = match live {
            Some(live) => {
                let server = live.server.clone();
                let shutdown = tokio::task::spawn_blocking(move || {
                    let mut server = lock(&server);
                    server.shutdown()
                })
                .await
                .map_err(|e| ApiError::internal(format!("the stop task failed: {e}")))?;
                shutdown
                    .map(
                        |Shutdown {
                             outcome, exit_code, ..
                         }| {
                            json!({
                                "outcome": format!("{outcome:?}").to_lowercase(),
                                "exit_code": exit_code,
                            })
                        },
                    )
                    .map_err(|e| ApiError::internal(format!("cannot stop the tab: {e}")))?
            }
            None => json!({ "outcome": "nothing_to_stop", "exit_code": Value::Null }),
        };
        *tab.state() = State::Stopped;
        self.map().remove(tab.id.as_str());
        Ok(json!({ "id": tab.id, "stopped": outcome }))
    }

    /// Stop every tab — the shutdown path. Blocking: it runs the ladder for each.
    pub fn stop_all(&self) -> usize {
        let tabs: Vec<Arc<Tab>> = self.map().values().cloned().collect();
        let mut stopped = 0;
        for tab in tabs {
            let live = match &*tab.state() {
                State::Running(live) => Some(live.clone()),
                _ => None,
            };
            if let Some(live) = live {
                let mut server = lock(&live.server);
                if server.shutdown().is_ok() {
                    stopped += 1;
                }
            }
            *tab.state() = State::Stopped;
        }
        stopped
    }

    /// A fresh id, and one this server has not handed out before.
    fn mint_id(&self) -> Result<TabId, ApiError> {
        let tabs = self.map();
        for _ in 0..64 {
            let id = TabId::new();
            if !tabs.contains_key(id.as_str()) {
                return Ok(id);
            }
        }
        Err(ApiError::internal("cannot mint an unused tab id"))
    }
}

/// A mutex a poisoned lock cannot make useless: a panic elsewhere must not take
/// every tab with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// An error as the API hands it to the browser: a status, a message, and the
/// detail a frontend needs to say what happened.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub detail: Option<Value>,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            message: message.into(),
            detail: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(400, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> ApiError {
        ApiError::new(401, message)
    }

    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(404, message)
    }

    pub fn conflict(message: impl Into<String>) -> ApiError {
        ApiError::new(409, message)
    }

    pub fn gone(message: impl Into<String>) -> ApiError {
        ApiError::new(410, message)
    }

    pub fn bad_gateway(message: impl Into<String>, detail: Value) -> ApiError {
        ApiError {
            status: 502,
            message: message.into(),
            detail: Some(detail),
        }
    }

    pub fn unavailable(error: BootError) -> ApiError {
        ApiError {
            status: 502,
            message: format!("the tab did not come up: {}", error.reason),
            detail: Some(json!({
                "error": error.reason,
                "log_tail": error.log_tail,
                "log_path": error.log_path,
            })),
        }
    }

    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(500, message)
    }

    /// The body: `{"error": …, "detail": …}`, which is what the frontend shows.
    pub fn body(&self) -> Value {
        let mut body = json!({ "error": self.message });
        if let Some(detail) = &self.detail {
            body["detail"] = detail.clone();
        }
        body
    }
}

/// An error as an HTTP reply: the status, and the JSON body above.
impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = axum::http::StatusCode::from_u16(self.status)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(self.body())).into_response()
    }
}

impl From<swarm_client::Error> for ApiError {
    /// A refusal from the serve behind a tab, handed on with its status: a 401
    /// stays a 401 (the frontend can tell "this tab is not ours" from "the
    /// session refused the op"), and anything the transport could not do is a
    /// 502 with the reason.
    fn from(error: swarm_client::Error) -> ApiError {
        match error {
            swarm_client::Error::Status(status) => {
                use swarm_client::StatusError;
                let code = status.status();
                let message = status.message().to_owned();
                let detail = match &status {
                    StatusError::BadRequest(r)
                    | StatusError::Unauthorized(r)
                    | StatusError::ShuttingDown(r)
                    | StatusError::Other(r) => r.raw.clone(),
                };
                ApiError {
                    status: code,
                    message,
                    detail: Some(detail),
                }
            }
            other => ApiError::bad_gateway(
                format!("the tab's server did not answer: {other}"),
                Value::Null,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(folder: &str) -> OpenRequest {
        OpenRequest {
            folder: PathBuf::from(folder),
            ..OpenRequest::default()
        }
    }

    #[test]
    fn a_folder_has_to_be_a_real_absolute_directory() {
        assert!(request("relative/folder").validate().is_err());
        assert!(request("/no/such/folder/at/all").validate().is_err());
        let here = std::env::current_dir().unwrap();
        assert!(request(&here.display().to_string()).validate().is_ok());
    }

    #[test]
    fn an_agent_tab_refuses_the_swarms_flags() {
        let here = std::env::current_dir().unwrap();
        let mut open = request(&here.display().to_string());
        open.program = ProgramChoice::Agent;
        open.workers = Some(4);
        let error = open.validate().unwrap_err();
        assert_eq!(error.status, 400);
        assert!(error.message.contains("swarm flag"), "{}", error.message);
        open.workers = None;
        assert!(open.validate().is_ok());
        // A swarm takes them.
        open.program = ProgramChoice::Swarm;
        open.workers = Some(4);
        assert!(open.validate().is_ok());
    }

    #[test]
    fn a_model_spec_becomes_the_flag_pair() {
        assert_eq!(model_ref("claude@anthropic").spec(), "claude@anthropic");
        assert_eq!(model_ref("bare").spec(), "bare");
        assert_eq!(model_ref("m@").spec(), "m@");
    }

    /// The spec a tab builds names its own ready file, the tab's directory, and
    /// `--port 0`; a swarm gets the lane flags and the agent binary, an agent does
    /// not.
    #[test]
    fn a_tabs_spec_is_the_apps_spec() {
        let tab_dir = Path::new("/tmp/root/tabs/t1");
        let mut open = request("/tmp");
        open.model = Some("m@acme".to_owned());
        open.workers = Some(3);
        open.lane_model = Some("l@acme".to_owned());
        let spec = open.spec(tab_dir, Path::new("/opt/evo-agent"));
        let argv = spec.argv();
        assert_eq!(spec.ready_file, Some(tab_dir.join("ready.json")));
        assert_eq!(argv[0], "serve");
        assert!(argv.windows(2).any(|w| w == ["--port", "0"]), "{argv:?}");
        assert!(
            argv.windows(2).any(|w| w == ["--model", "m@acme"]),
            "{argv:?}"
        );
        assert!(argv.windows(2).any(|w| w == ["--workers", "3"]), "{argv:?}");
        assert!(
            argv.windows(2).any(|w| w == ["--evo", "/opt/evo-agent"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w == ["--lane-model", "l@acme"]),
            "{argv:?}"
        );

        let mut agent = open.clone();
        agent.program = ProgramChoice::Agent;
        agent.workers = None;
        agent.lane_model = None;
        let argv = agent.spec(tab_dir, Path::new("/opt/evo-agent")).argv();
        assert!(!argv.iter().any(|a| a.starts_with("--lane")), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--evo"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--workers"), "{argv:?}");
    }

    /// The id in a URL is the store's own: nothing that is not
    /// `[A-Za-z0-9_-]{1,64}` is ever looked up.
    #[test]
    fn a_path_segment_that_is_not_an_id_is_not_a_tab() {
        assert!(TabId::parse("../../etc/passwd").is_none());
        assert!(TabId::parse("a/b").is_none());
        assert!(TabId::parse("").is_none());
        assert!(TabId::parse("has space").is_none());
        assert!(TabId::parse("ok-id_1").is_some());
    }

    #[test]
    fn an_error_body_carries_the_detail_a_frontend_shows() {
        let boot = BootError {
            reason: "cannot run /usr/local/bin/evo-swarm".to_owned(),
            log_tail: "no such file".to_owned(),
            log_path: PathBuf::from("/tmp/root/tabs/t1/swarm.log"),
        };
        let error = ApiError::unavailable(boot);
        assert_eq!(error.status, 502);
        let body = error.body();
        assert!(body["error"].as_str().unwrap().contains("did not come up"));
        assert_eq!(body["detail"]["log_tail"], "no such file");
    }
}
