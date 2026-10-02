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

use crate::prompt_note::PromptNote;

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
    /// What this client tells the sessions it starts about itself (§1): the path of
    /// the note file, per program, or nothing for a binary that does not take the
    /// flag. The file itself is written by the app, which is also where the flag is
    /// asked for (`store::cli`) — a launch only passes what it is handed.
    pub prompt_note: PromptNote,
}

impl Default for LaunchEnv {
    fn default() -> LaunchEnv {
        LaunchEnv {
            swarm_bin: store::cli::swarm_bin(),
            agent_bin: store::cli::agent_bin(),
            root: Root::default(),
            env: Vec::new(),
            env_remove: Vec::new(),
            prompt_note: PromptNote::default(),
        }
    }
}

/// What starting a tab means: a new session in a folder, or a recorded session
/// resumed in its own folder (§7.2, §2).
///
/// Every launch is one of the two programs, and which one is decided before it is
/// built: by the workers card's switch for a new session (§7.2), and by what the
/// session on disk was for a resumed one (§9.5) — an `evo-agent` journal is a
/// single agent's, and `evo-swarm` cannot run it.
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// The user chose a folder and (optionally) models and a worker count.
    New { folder: PathBuf, plan: LaunchPlan },
    /// The user picked a past session: that journal, in the folder it ran in, and the
    /// program that wrote it.
    Resume {
        folder: PathBuf,
        session: PathBuf,
        swarm: bool,
    },
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

    /// Whether this launch is a swarm or one `evo-agent` (§7.2, §9.5).
    pub fn swarm(&self) -> bool {
        match self {
            Launch::New { plan, .. } => plan.swarm,
            Launch::Resume { swarm, .. } => *swarm,
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
    let swarm = launch.swarm();
    let program = if swarm {
        Program::Swarm
    } else {
        Program::Agent
    };
    let mut spec = LaunchSpec::tab(program, launch.folder().clone(), &tab_dir);
    // A new session asks the child to pick a port and report it in the ready file
    // (§1): nothing has bound one yet, and the supervisor keeps the port it got.
    spec.port = Some(0);
    spec.resume = launch.session().cloned();
    // What this client is, in the session's own words (§1): the same note for a new
    // session and a resumed one — a resumed journal is read by the same renderer —
    // and one per program, because whether it can be passed at all is a question
    // about the binaries.
    spec.prompt_note = env.prompt_note.for_program(program);
    if swarm {
        // The lanes run this app's own `evo-agent`; `evo-swarm` takes it as `--evo`.
        // `LaunchSpec::argv` passes no lane flag of any kind for one agent, so the
        // spec's own program is the only thing that has to be right here.
        spec.agent_bin = Some(env.agent_bin.clone());
    }
    if let Some(plan) = launch.plan() {
        // The coordinator's own flags, which both programs take.
        spec.model = plan.model.as_ref().map(model_ref);
        // The coordinator's own ladder rung: the empty tab resolves one and shows it, so
        // the launch passes it (§7.2) rather than leaving the level to evo.
        spec.thinking = plan.thinking.clone();
        if swarm {
            spec.lane_model = plan.lanes_model.as_ref().map(model_ref);
            spec.lane_thinking = plan.lane_thinking.clone();
            spec.workers = plan.workers;
        }
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

/// The argv a single agent's own catalog is read with (§5.6): `evo-agent catalog
/// --json`.
///
/// One binary, one subcommand, no flags. `evo-agent` has no `check` — the flags
/// `evo-swarm check` is asked about are `--workers` and `--lane-model`, which a
/// launch with no lanes neither passes nor asks after — so this is everything a
/// single-agent launch can be told about itself before it starts.
pub fn agent_catalog_argv() -> Vec<String> {
    vec!["catalog".to_owned(), "--json".to_owned()]
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
    let mut spec = launch_spec(env, launch, &store_id);

    // What the spec names has to be there when the child reads it: evo reads the
    // path at parse time, and a path it cannot read is a session that refuses to
    // start (exit 64). So the note is written here, on every launch and every
    // resume — a no-op when the app already wrote it, which is the usual case — and
    // the flag is taken back out of the argv if it could not be written: a session
    // that reads a formula as its source still runs, one that never comes ready
    // does not. The root is the one the tab's own directory was just made under.
    if spec.prompt_note.is_some() && crate::prompt_note::write(&env.root).is_err() {
        spec.prompt_note = None;
    }

    // The spawn is `swarm_client`'s, and `tab_engine` is what drives it: the config
    // says which binary, which flags, where to run, where the ready file and the log
    // go, and what environment a hermetic run needs. Every flag comes from the spec —
    // and so does which binary: `evo-swarm` for a swarm, `evo-agent` for one agent,
    // over the same app root, the same tab directory, the same ready file and log,
    // and the same protocol behind them (§1, §7.2).
    let bin = if launch.swarm() {
        env.swarm_bin.clone()
    } else {
        env.agent_bin.clone()
    };
    let mut config = swarm_client::ServerConfig::swarm(bin, launch.folder().clone(), &tab_dir)
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
            swarm: true,
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
                Some(("deepseek-v4.1-flash", "acme")),
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
                "deepseek-v4.1-flash@acme",
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

    /// §1, §7.2: what this client is goes on the command line of **both** programs,
    /// for a new session and for a resumed one — the note is about the renderer,
    /// which does not change because a journal was written earlier.
    #[test]
    fn the_clients_note_rides_on_every_launch_and_resume() {
        let note = PathBuf::from("/tmp/evo-desktop/prompt-notes/gui-math.md");
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            prompt_note: PromptNote {
                agent: Some(note.clone()),
                swarm: Some(note.clone()),
            },
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t5").unwrap();
        let launches = [
            Launch::New {
                folder: PathBuf::from("/Users/you/coding/foo"),
                plan: plan(None, None, None),
            },
            Launch::New {
                folder: PathBuf::from("/Users/you/coding/foo"),
                plan: LaunchPlan {
                    swarm: false,
                    ..plan(None, None, None)
                },
            },
            Launch::Resume {
                folder: PathBuf::from("/Users/you/coding/foo"),
                session: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                swarm: true,
            },
            Launch::Resume {
                folder: PathBuf::from("/Users/you/coding/foo"),
                session: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                swarm: false,
            },
        ];
        for launch in &launches {
            let argv = launch_spec(&env, launch, &id).argv();
            let at = argv
                .iter()
                .position(|flag| flag == "--prompt-note")
                .unwrap_or_else(|| panic!("{launch:?} is told about this client: {argv:?}"));
            assert_eq!(argv[at + 1], note.display().to_string(), "{launch:?}");
        }

        // An environment whose binaries take no note passes none — the same four
        // launches, each without the flag and without an empty value.
        let without = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            ..LaunchEnv::default()
        };
        for launch in &launches {
            let argv = launch_spec(&without, launch, &id).argv();
            assert!(
                !argv.iter().any(|flag| flag == "--prompt-note"),
                "{launch:?}"
            );
            assert!(!argv.iter().any(String::is_empty), "{launch:?}");
        }
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
            swarm: true,
        };
        let spec = launch_spec(&env, &launch, &id);
        let argv = spec.argv();
        let at = argv.iter().position(|flag| flag == "--resume").unwrap();
        assert_eq!(argv[at + 1], "/Users/you/.evo/sessions/a/1.sexp");
        assert!(!argv.iter().any(|flag| flag == "--model"));
        assert_eq!(launch.plan(), None);
    }

    /// §7.2: one agent's launch. The same `serve --ready-file … --watch-stdin --port 0`
    /// as a swarm's, with the coordinator's own `--model` and `--thinking` and
    /// **nothing** that belongs to lanes: no `--evo`, no `--workers`, no `--lane-model`,
    /// no `--lane-thinking`.
    #[test]
    fn an_agent_launch_is_an_agent_and_no_lane_flags() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            swarm_bin: PathBuf::from("/opt/evo/evo-swarm"),
            agent_bin: PathBuf::from("/opt/evo/evo-agent"),
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t3").unwrap();
        let launch = Launch::New {
            folder: PathBuf::from("/Users/you/coding/foo"),
            plan: LaunchPlan {
                swarm: false,
                thinking: Some("high".to_string()),
                lane_thinking: Some("low".to_string()),
                ..plan(
                    Some(("claude-opus-5", "anthropic")),
                    Some(("deepseek-v4.1-flash", "acme")),
                    Some(4),
                )
            },
        };
        let spec = launch_spec(&env, &launch, &id);
        assert_eq!(spec.program, Some(Program::Agent), "one agent, not a swarm");
        assert_eq!(
            spec.folder(),
            Path::new("/Users/you/coding/foo"),
            "the folder is the child's cwd"
        );
        assert_eq!(
            spec.ready_file,
            Some(PathBuf::from("/tmp/evo-desktop/tabs/t3/ready.json")),
            "the same handshake a swarm's launch waits on"
        );
        assert_eq!(
            spec.agent_bin, None,
            "there are no lanes to point elsewhere"
        );
        assert_eq!(
            spec.argv(),
            vec![
                "serve",
                "--ready-file",
                "/tmp/evo-desktop/tabs/t3/ready.json",
                "--watch-stdin",
                "--port",
                "0",
                "--model",
                "claude-opus-5@anthropic",
                "--thinking",
                "high",
            ]
        );
    }

    /// §9.5: a resumed session runs in the program that wrote it — an `evo-agent`
    /// journal is a single agent's, and its `--resume` goes to `evo-agent`.
    #[test]
    fn a_resumed_session_runs_in_the_program_that_wrote_it() {
        let env = LaunchEnv {
            root: Root::at("/tmp/evo-desktop"),
            ..LaunchEnv::default()
        };
        let id = store::paths::TabId::parse("t4").unwrap();
        for (swarm, program) in [(false, Program::Agent), (true, Program::Swarm)] {
            let launch = Launch::Resume {
                folder: PathBuf::from("/Users/you/coding/foo"),
                session: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                swarm,
            };
            let spec = launch_spec(&env, &launch, &id);
            assert_eq!(spec.program, Some(program), "the program that wrote it");
            let mut argv: Vec<String> = vec![
                "serve",
                "--ready-file",
                "/tmp/evo-desktop/tabs/t4/ready.json",
                "--watch-stdin",
                "--port",
                "0",
                "--resume",
                "/Users/you/.evo/sessions/a/1.sexp",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            if swarm {
                // A swarm names the binary its lanes run; one agent has no lanes.
                argv.extend(["--evo".to_string(), env.agent_bin.display().to_string()]);
            }
            assert_eq!(
                spec.argv(),
                argv,
                "a resume passes no model or level of its own: the journal has them"
            );
        }
    }

    /// §5.6: one agent's own catalog is one subcommand — there is no `check`, and
    /// nothing to pass it.
    #[test]
    fn a_single_agents_catalog_is_one_subcommand_with_no_flags() {
        assert_eq!(agent_catalog_argv(), vec!["catalog", "--json"]);
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
