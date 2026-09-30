//! Starting and stopping one tab's swarm (§1, §3, §7.2).
//!
//! Everything in here happens *before* the UI shows a running tab and *after* it
//! stops showing one: choose the tab's own directory, turn the empty tab's choices
//! into the flags a launch passes, and hand the spawn a
//! [`swarm_client::ServerConfig`] built from them.
//!
//! The tab directory is what ties the two sides together: `ServerConfig::swarm`
//! derives `ready.json` and `swarm.log` from it, and the argv
//! ([`store::launch::LaunchSpec`], the one owner of evo's launch flags) names the
//! same ready file.
//!
//! Nothing here writes a project file. A lane's model is `--lane-model` (§1, F3),
//! so the folder's own `swarm.lisp` is never touched. Stopping needs nothing from
//! this file either: an [`EngineHandle`] closes the child's stdin when it is
//! dropped, which is the server's own signal to stop, and never blocks the UI
//! thread.

use std::path::{Path, PathBuf};

use async_channel::Receiver;
use session::LaunchPlan;
use store::catalog::{CheckReport, ModelRef};
use store::cli::{self, CliError};
use store::launch::{LaunchSpec, Program};
use store::paths::Root;
use tab_engine::{EngineHandle, Update};

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
    let tab_dir = env.root.tab_dir(id);
    let mut spec = LaunchSpec::tab(Program::Swarm, launch.folder().clone(), &tab_dir);
    // A new swarm asks the child to pick a port and report it in the ready file
    // (§1): nothing has bound one yet, and the supervisor keeps the port it got.
    spec.port = Some(0);
    // The lanes run this app's own `evo-agent`; `evo-swarm` takes it as `--evo`.
    spec.agent_bin = Some(env.agent_bin.clone());
    spec.resume = launch.session().cloned();
    if let Some(plan) = launch.plan() {
        spec.model = plan.model.as_ref().map(model_ref);
        // The coordinator's own ladder rung: the empty tab resolves one and shows it, so
        // the launch passes it (§7.2) rather than leaving the level to evo.
        spec.thinking = plan.thinking.clone();
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
/// The tab directory is made first: the ready file, the log and the argv all name
/// paths inside it, so it has to exist before the spawn does.
pub fn start(env: &LaunchEnv, launch: &Launch) -> std::io::Result<Started> {
    let store_id = store::paths::TabId::new();
    let tab_dir = env.root.ensure_tab_dir(&store_id)?;
    let spec = launch_spec(env, launch, &store_id);

    // The spawn is `swarm_client`'s, and `tab_engine` is what drives it: the config
    // says which binary, which flags, where to run, where the ready file and the log
    // go, and what environment a hermetic run needs. Every flag comes from the spec.
    let mut config =
        swarm_client::ServerConfig::swarm(env.swarm_bin.clone(), launch.folder().clone(), &tab_dir)
            .with_argv(spec.argv());
    debug_assert_eq!(
        config.ready_file,
        spec.ready_file.clone().unwrap_or_default(),
        "the argv's --ready-file must be the file the spawn waits on"
    );
    for (key, value) in &env.env {
        config = config.with_env(key.clone(), value.clone());
    }
    for key in &env.env_remove {
        config = config.with_env_removed(key.clone());
    }

    let (engine, updates) = tab_engine::TabEngine::start(config);
    Ok(Started {
        engine,
        updates,
        tab_dir,
        store_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use session::LaunchPlan;

    fn plan(
        model: Option<(&str, &str)>,
        lanes: Option<(&str, &str)>,
        workers: Option<u16>,
    ) -> LaunchPlan {
        LaunchPlan {
            model: model.map(|(id, provider)| (id.to_string(), provider.to_string())),
            thinking: None,
            workers,
            lanes_model: lanes.map(|(id, provider)| (id.to_string(), provider.to_string())),
            lane_thinking: None,
        }
    }

    #[test]
    fn a_new_swarm_gets_the_flags_the_choosers_chose() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            swarm_bin: PathBuf::from("/opt/evo/evo-swarm"),
            agent_bin: PathBuf::from("/opt/evo/evo-agent"),
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
        assert!(spec.watch_stdin, "the spawn holds the pipe (§1)");
        assert_eq!(
            spec.argv(),
            vec![
                "serve",
                "--ready-file",
                "/tmp/evo-desktop/tabs/t1/ready.json",
                "--watch-stdin",
                "--port",
                "0",
                "--model",
                "claude-opus-5@anthropic",
                "--evo",
                "/opt/evo/evo-agent",
                "--workers",
                "4",
                "--lane-model",
                "ark-deepseek-v4.1-flash@aiden",
            ]
        );
    }

    /// §1: the spawn waits on a ready file, and the file it waits on is the one
    /// the argv names — `ServerConfig::swarm` derives both from the tab directory.
    #[test]
    fn the_ready_file_the_spawn_waits_on_is_the_one_the_argv_names() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t9").unwrap();
        let launch = Launch::New {
            folder: PathBuf::from("/Users/you/coding/foo"),
            plan: plan(None, None, None),
        };
        let spec = launch_spec(&env, &launch, &id);
        let tab_dir = env.root.ensure_tab_dir(&id).unwrap();
        let config = swarm_client::ServerConfig::swarm(
            env.swarm_bin.clone(),
            launch.folder().clone(),
            &tab_dir,
        )
        .with_argv(spec.argv());
        assert_eq!(
            config.ready_file,
            PathBuf::from("/tmp/evo-desktop/tabs/t9/ready.json")
        );
        assert_eq!(
            spec.ready_file.as_deref(),
            Some(config.ready_file.as_path())
        );
        assert_eq!(
            config.log_path,
            PathBuf::from("/tmp/evo-desktop/tabs/t9/swarm.log")
        );
        let at = config
            .argv
            .iter()
            .position(|flag| flag == "--ready-file")
            .expect("the argv names a ready file");
        assert_eq!(config.argv[at + 1], config.ready_file.display().to_string());
        assert!(config.argv.iter().any(|flag| flag == "--watch-stdin"));
        let _ = std::fs::remove_dir_all(env.root.path());
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
                thinking: Some("high".to_string()),
                lane_thinking: Some("high".to_string()),
                ..LaunchPlan::default()
            })
            .check_argv(),
            vec!["check", "--json"],
            "a thinking level is not a check's business"
        );
        // A check starts no server: it asks about the flags, and names no ready
        // file or pipe.
        let asked = check_spec(&LaunchPlan::default()).check_argv();
        assert!(!asked.iter().any(|flag| flag == "--ready-file"));
        assert!(!asked.iter().any(|flag| flag == "--watch-stdin"));
        assert!(LaunchPlan::default().is_default());
    }
}
