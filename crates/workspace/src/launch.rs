//! Starting and stopping one tab's swarm (§1, §3, §7.2).
//!
//! Everything in here happens *before* the UI shows a running tab and *after* it
//! stops showing one: choose the tab's own directory, turn the empty tab's choices
//! into the flags a launch passes, hand the spawn a [`LaunchSpec`], and — on the
//! way out — run §3's shutdown ladder somewhere that is not the UI thread.
//!
//! Nothing here writes a project file. A lane's model is `--lane-model` (§1, F3),
//! so the folder's own `swarm.lisp` is never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_channel::Receiver;
use session::LaunchPlan;
use store::catalog::{CheckReport, ModelRef};
use store::cli::{self, CliError};
use store::launch::{LaunchSpec, Program};
use store::paths::Root;
use tab_engine::{shutdown_all, EngineHandle, ShutdownReport, Update};

use crate::bridge::{Bridge, Revision};

/// How long §3's ladder is given before a tab is left to finish stopping on its
/// own: `server.shutdown` (≤10 s), then `SIGTERM` (5 s), then `SIGKILL`.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(25);

/// What every tab this window opens needs in order to start a swarm: the two
/// binaries, the app's own data root, and the environment a hermetic run needs.
///
/// The app hands the window one of these; the window hands it to each tab, and a
/// tab turns its choosers into flags through [`launch_spec`]. Nothing here is a
/// chooser's business.
#[derive(Clone, Debug)]
pub struct LaunchEnv {
    /// `evo-swarm`, the binary a tab spawns (§1) and a check runs (§9).
    pub swarm_bin: PathBuf,
    /// `evo-agent`, the binary the lanes run.
    pub agent_bin: PathBuf,
    /// `~/.evo/desktop` — where a tab's own directory lives (§6).
    pub root: Root,
    /// Extra environment every swarm this window starts gets.
    pub env: Vec<(String, String)>,
    /// Variables to drop from the child's environment.
    pub env_remove: Vec<String>,
}

impl Default for LaunchEnv {
    fn default() -> LaunchEnv {
        LaunchEnv {
            swarm_bin: store::cli::swarm_bin(),
            agent_bin: store::cli::agent_bin(),
            root: Root::default(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
}

/// What starting a tab means: a new swarm in a folder, or a recorded session
/// resumed in its own folder (§7.2, §2).
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// The user chose a folder and (optionally) models and a worker count.
    New { folder: PathBuf, plan: LaunchPlan },
    /// The user picked a past swarm: that session, in the folder it ran in.
    Resume { folder: PathBuf, session: PathBuf },
}

impl Launch {
    pub fn folder(&self) -> &PathBuf {
        match self {
            Launch::New { folder, .. } | Launch::Resume { folder, .. } => folder,
        }
    }

    /// The session this launch resumes, if it resumes one.
    pub fn session(&self) -> Option<&PathBuf> {
        match self {
            Launch::New { .. } => None,
            Launch::Resume { session, .. } => Some(session),
        }
    }

    /// The empty tab's plan; a resume has none.
    pub fn plan(&self) -> Option<&LaunchPlan> {
        match self {
            Launch::New { plan, .. } => Some(plan),
            Launch::Resume { .. } => None,
        }
    }
}

/// The flags one tab's swarm is launched with (§1) — a pure function of the tab's
/// own directory, what the person chose, and what the app runs.
///
/// The ready file path is the tab's, so the spawn waits on the file the child
/// writes; `--watch-stdin` is left to the spawn, which is the process that holds
/// the pipe.
pub fn launch_spec(env: &LaunchEnv, launch: &Launch, id: &store::paths::TabId) -> LaunchSpec {
    let mut spec = LaunchSpec::new(Program::Swarm, launch.folder().clone());
    spec.ready_file = Some(LaunchSpec::ready_file_in(&env.root, id));
    // A new swarm asks the child to pick a port and report it in the ready file
    // (§1): nothing has bound one yet, and the supervisor keeps the port it got.
    spec.port = Some(0);
    spec.resume = launch.session().cloned();
    if let Some(plan) = launch.plan() {
        spec.model = plan.model.as_ref().map(model_ref);
        spec.lane_model = plan.lanes_model.as_ref().map(model_ref);
        spec.lane_thinking = plan.lane_thinking.clone();
        spec.workers = plan.workers;
    }
    spec
}

/// The `ID@PROVIDER` pair a [`LaunchPlan`] carries, as the flag takes it (§1).
fn model_ref((id, provider): &(String, String)) -> ModelRef {
    ModelRef::new(id.clone(), Some(provider.clone()))
}

/// The flags `evo-swarm check --json` is asked about (§9). Only what the check
/// reads: the models and the worker count. Nothing here needs a folder — a check
/// is about the launch, not about the machine it would run on.
pub fn check_spec(plan: &LaunchPlan) -> LaunchSpec {
    let mut spec = LaunchSpec::new(Program::Swarm, PathBuf::new());
    spec.model = plan.model.as_ref().map(model_ref);
    spec.lane_model = plan.lanes_model.as_ref().map(model_ref);
    spec.workers = plan.workers;
    spec
}

/// Ask `evo-swarm check --json` about one launch (§9): are the models resolvable,
/// can a lane reach its API, is the key there.
///
/// Blocking — it is a process — so the caller runs it off the thread that draws
/// (the empty tab's own check does).
pub fn check(bin: &Path, plan: &LaunchPlan) -> Result<CheckReport, CliError> {
    let body = cli::run_json(bin, &check_spec(plan).check_argv())?;
    Ok(CheckReport::from_json(&body))
}

/// A tab's swarm, started and not yet ready: the engine that commands it, the
/// channel its updates arrive on, and where it keeps its log.
pub struct Started {
    pub engine: EngineHandle,
    pub updates: Receiver<Update>,
    /// `tabs/<id>/` — the swarm's log, and where the ready file is written (§6).
    pub tab_dir: PathBuf,
    /// The id that directory is named for: what a stored tab set has to keep to
    /// find the directory again (§6).
    pub store_id: store::paths::TabId,
}

/// Start one tab's swarm (§1).
///
/// The tab directory is made first, so the ready file has somewhere to land. The
/// spawn itself is `tab_engine`'s: it owns the child, its stdin pipe, its log and
/// the wait for the ready file, and it is handed **the flags**, not the choices —
/// [`LaunchSpec`] is the one owner of evo's launch argv.
pub fn start(env: &LaunchEnv, launch: &Launch) -> std::io::Result<Started> {
    let store_id = store::paths::TabId::new();
    let tab_dir = env.root.ensure_tab_dir(&store_id)?;
    let spec = launch_spec(env, launch, &store_id);
    let (engine, updates) = tab_engine::TabEngine::start(
        spec,
        env.swarm_bin.clone(),
        env.agent_bin.clone(),
        env.env.clone(),
        env.env_remove.clone(),
    );
    Ok(Started {
        engine,
        updates,
        tab_dir,
        store_id,
    })
}

/// Run §3's ladder for these tabs somewhere that is not the UI thread (§9.8), and
/// hand back the report once they are done.
///
/// Dropping the returned bridge does not abort the shutdown: the ladders run on
/// their own thread either way, in parallel, which is why closing a tab can
/// remove it from the window at once.
pub fn stop_in_background(handles: Vec<EngineHandle>) -> Bridge<ShutdownReport> {
    let (bridge, _worker) = Bridge::spawn(Revision::new(0), move |updates| {
        let report = shutdown_all(handles, SHUTDOWN_DEADLINE);
        let _ = updates.send(report);
    });
    bridge
}

#[cfg(test)]
mod tests {
    use super::*;
    use session::{LaunchPlan, DEFAULT_KEY};

    fn plan(
        model: Option<(&str, &str)>,
        lanes: Option<(&str, &str)>,
        workers: Option<u16>,
    ) -> LaunchPlan {
        LaunchPlan {
            model: model.map(|(id, provider)| (id.to_string(), provider.to_string())),
            lanes_model: lanes.map(|(id, provider)| (id.to_string(), provider.to_string())),
            lane_thinking: None,
            workers,
        }
    }

    #[test]
    fn a_new_swarm_gets_the_flags_the_choosers_chose() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t1").unwrap();
        let launch = Launch::New {
            folder: PathBuf::from("/Users/you/coding/foo"),
            plan: plan(
                Some(("claude-opus-5", "anthropic")),
                Some(("ark-deepseek-v4.1-flash", "aiden")),
                Some(4),
            ),
        };
        let spec = launch_spec(&env, &launch, &id);
        assert_eq!(spec.folder(), Path::new("/Users/you/coding/foo"));
        assert_eq!(
            spec.ready_file,
            Some(PathBuf::from("/tmp/evo-desktop/tabs/t1/ready.json"))
        );
        assert_eq!(spec.port, Some(0), "the child picks the port once (§1)");
        assert_eq!(spec.resume, None);
        assert_eq!(
            spec.argv(),
            vec![
                "serve",
                "--ready-file",
                "/tmp/evo-desktop/tabs/t1/ready.json",
                "--port",
                "0",
                "--model",
                "claude-opus-5@anthropic",
                "--workers",
                "4",
                "--lane-model",
                "ark-deepseek-v4.1-flash@aiden",
            ]
        );
    }

    #[test]
    fn a_resume_carries_the_exact_journal_path_and_no_plan() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t2").unwrap();
        let launch = Launch::Resume {
            folder: PathBuf::from("/Users/you/coding/foo"),
            session: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
        };
        let spec = launch_spec(&env, &launch, &id);
        let argv = spec.argv();
        let at = argv.iter().position(|flag| flag == "--resume").unwrap();
        assert_eq!(argv[at + 1], "/Users/you/.evo/sessions/a/1.sexp");
        assert!(!argv.iter().any(|flag| flag == "--model"));
        assert_eq!(launch.plan(), None);
    }

    #[test]
    fn the_check_is_asked_about_the_launchs_own_models() {
        let spec = check_spec(&plan(Some(("m", "p")), Some(("l", "p")), Some(2)));
        assert_eq!(
            spec.check_argv(),
            vec![
                "check",
                "--json",
                "--workers",
                "2",
                "--model",
                "m@p",
                "--lane-model",
                "l@p"
            ]
        );
        // Default everywhere is still a launch worth checking.
        assert_eq!(
            check_spec(&LaunchPlan {
                lane_thinking: Some("high".to_string()),
                ..LaunchPlan::default()
            })
            .check_argv(),
            vec!["check", "--json"],
            "a thinking level is not a check's business"
        );
        assert!(LaunchPlan::default().is_default());
        assert_eq!(DEFAULT_KEY, "default");
    }
}
