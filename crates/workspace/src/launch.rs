//! Starting and stopping one tab's swarm (§3, §6, §9.6).
//!
//! Everything in here happens *before* the UI shows a running tab and *after* it
//! stops showing one: choose the tab's directory, write the project's managed
//! `swarm.lisp` block, hand the engine a [`TabSpec`], and — on the way out — run
//! §3's shutdown ladder somewhere that is not the UI thread.

use std::path::PathBuf;
use std::time::Duration;

use async_channel::Receiver;
use session::LaunchPlan;
use store::paths::Root;
use store::swarm_config::{self, LanesModel};
use tab_engine::{shutdown_all, EngineHandle, ShutdownReport, TabSpec, Update};

use crate::bridge::{Bridge, Revision};

/// How long §3's ladder is given before a tab is left to finish stopping on its
/// own: `POST /shutdown` (≤10 s), then `SIGTERM` (5 s), then `SIGKILL`.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(25);

/// What every tab this window opens needs in order to start a swarm.
///
/// Production reads the installed binaries and `~/.evo/desktop`; a test points
/// them at a harness's binaries, its temp project and a temp app root, and adds
/// the environment that makes a swarm hermetic.
#[derive(Clone, Debug)]
pub struct SwarmConfig {
    /// `evo-swarm`, the binary a tab spawns (§3).
    pub swarm_bin: PathBuf,
    /// `evo-agent`, the binary the lanes run (`--evo`).
    pub agent_bin: PathBuf,
    /// `~/.evo/desktop` — where a tab's own directory lives (§6).
    pub root: Root,
    /// Extra environment every swarm this window starts gets.
    pub env: Vec<(String, String)>,
    /// Variables to drop from the child's environment.
    pub env_remove: Vec<String>,
}

impl Default for SwarmConfig {
    fn default() -> SwarmConfig {
        SwarmConfig {
            swarm_bin: swarm_client::default_swarm_bin(),
            agent_bin: swarm_client::default_agent_bin(),
            root: Root::default(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }
}

/// What starting a tab means: a new swarm in a folder, or a recorded session
/// resumed in its own folder (§7.2, §9.5).
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    /// The user chose a folder and (optionally) models and a worker count.
    New { folder: PathBuf, plan: LaunchPlan },
    /// The user picked a past swarm: that session, in the folder it ran in. The
    /// lane count comes from its journal, so no plan is passed (§14.2).
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

/// A tab's swarm, started and not yet ready: the engine that commands it, the
/// channel its updates arrive on, and where it keeps its token and its log.
pub struct Started {
    pub engine: EngineHandle,
    pub updates: Receiver<Update>,
    /// `tabs/<id>/` — the swarm's `token` and `swarm.log` (§6).
    pub tab_dir: PathBuf,
    /// The id that directory is named for: what a stored tab set has to keep to
    /// find the directory again (§6).
    pub store_id: store::paths::TabId,
}

/// Start one tab's swarm (§3).
///
/// The tab directory and the project's `swarm.lisp` block are written **before**
/// the server starts, so the lanes a new swarm initializes already read the model
/// that was chosen for them (§9.6). A folder whose `swarm.lisp` cannot be written
/// fails the launch instead: a swarm silently running the wrong lanes model is
/// worse than a tab that does not start.
pub fn start(config: &SwarmConfig, launch: &Launch) -> std::io::Result<Started> {
    let store_id = store::paths::TabId::new();
    let tab_dir = config.root.ensure_tab_dir(&store_id)?;
    let folder = launch.folder().clone();

    if let Some(plan) = launch.plan() {
        // The file belongs to the folder: Default removes our block, a choice
        // writes it at the top, and the user's own lines are never touched.
        let lanes = plan
            .lanes_model
            .as_ref()
            .map(|(model, provider)| LanesModel::new(model.clone(), provider.clone()));
        swarm_config::set_lanes_model(&folder, lanes.as_ref())?;
    }

    let mut spec = TabSpec::new(config.swarm_bin.clone(), folder, tab_dir.clone());
    spec.agent_bin = Some(config.agent_bin.clone());
    spec.workers = launch.plan().and_then(|plan| plan.workers);
    // The model id alone: the provider comes from how the model was registered.
    spec.model = launch
        .plan()
        .and_then(|plan| plan.model.as_ref().map(|(id, _)| id.clone()));
    spec.resume = launch.session().cloned();
    spec.env = config.env.clone();
    spec.env_remove = config.env_remove.clone();
    // §3: the swarm and its lanes shut down if this app dies.
    spec.env.push((
        "EVO_SERVE_WATCH_PID".to_owned(),
        std::process::id().to_string(),
    ));

    let (engine, updates) = tab_engine::TabEngine::start(spec);
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
