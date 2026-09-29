//! Learning the model catalog for an empty tab (§9.4).
//!
//! A tab's model chooser needs to know two things before any swarm exists: the
//! models and providers the user's `init.lisp` registers, and — for the *lanes*
//! chooser — which models a lane could register, since a lane may run in
//! quarantine (`--no-userspace`) where only the kernel's own wire APIs are
//! reachable.
//!
//! So this starts two throwaway `evo-agent serve` probes, each in its own
//! directory under `probe_dir`, reads `GET /registry` from each, and stops both
//! the ladder's way. Two servers, no more; the app has no swarm running yet, and
//! the probes are the only processes involved. Nothing here writes outside
//! `probe_dir` — persistence belongs to the store crate.

use std::path::{Path, PathBuf};
use std::thread;

use async_channel::Receiver;
use serde_json::Value;

use swarm_client::{Payload, Registry, Server, ServerConfig};

/// What a catalog probe learned.
#[derive(Clone, Debug)]
pub enum CatalogUpdate {
    /// Both probes finished.
    Done {
        /// The whole `GET /registry` from the userspace probe.
        registry: Value,
        /// The wire APIs the kernel itself defines, from the `--no-userspace`
        /// probe: what [`Registry::lane_ready`] is checked against. `None` when
        /// that probe did not answer.
        kernel_apis: Option<Vec<String>>,
    },
    /// The catalog could not be learned: the tab stays on the model it has.
    Failed {
        message: String,
        /// The last lines of the failing probe's log.
        log_tail: String,
    },
}

/// Probe the model catalog, on a thread of its own.
///
/// The receiver yields exactly one [`CatalogUpdate`] and then closes. The probe
/// inherits this process's environment, so it reads the same `HOME`, `init.lisp`
/// and extensions the app does; `probe_dir` is where the probes keep their
/// tokens and logs.
pub fn learn(
    agent_bin: impl Into<PathBuf>,
    probe_dir: impl Into<PathBuf>,
) -> Receiver<CatalogUpdate> {
    learn_with(agent_bin, probe_dir, Vec::new())
}

/// [`learn`] with environment variables of your own — a test with a temp
/// `EVO_HOME`, or an app that overrides `EVO_BINARY`.
pub fn learn_with(
    agent_bin: impl Into<PathBuf>,
    probe_dir: impl Into<PathBuf>,
    env: Vec<(String, String)>,
) -> Receiver<CatalogUpdate> {
    let (updates, receiver) = async_channel::bounded(1);
    let fallback = updates.clone();
    let bin = agent_bin.into();
    let dir = probe_dir.into();
    let spawned = thread::Builder::new()
        .name("tab-engine catalog".to_owned())
        .spawn(move || {
            let update = probe(bin, dir, env);
            let _ = updates.send_blocking(update);
        });
    if let Err(error) = spawned {
        // A thread that never started still owes the caller an answer.
        let _ = fallback.try_send(CatalogUpdate::Failed {
            message: format!("could not start the catalog probe thread: {error}"),
            log_tail: String::new(),
        });
    }
    receiver
}

/// Run both probes at once — two servers, which is the most this ever wants —
/// and fold them into one update.
fn probe(bin: PathBuf, dir: PathBuf, env: Vec<(String, String)>) -> CatalogUpdate {
    let userspace_dir = dir.join("userspace");
    let kernel_dir = dir.join("kernel");
    for sub in [&userspace_dir, &kernel_dir] {
        if let Err(error) = std::fs::create_dir_all(sub) {
            return CatalogUpdate::Failed {
                message: format!("cannot use {}: {error}", sub.display()),
                log_tail: String::new(),
            };
        }
    }

    let full = {
        let bin = bin.clone();
        let dir = userspace_dir.clone();
        let env = env.clone();
        thread::spawn(move || run_probe(&bin, &dir, false, env))
    };
    let kernel = {
        let dir = kernel_dir.clone();
        thread::spawn(move || run_probe(&bin, &dir, true, env))
    };

    let full = join(full);
    let kernel = join(kernel);
    match full {
        Ok(registry) => CatalogUpdate::Done {
            registry: registry.raw,
            kernel_apis: kernel.ok().map(|registry| registry.typed.apis),
        },
        Err(error) => error,
    }
}

fn join(
    handle: thread::JoinHandle<Result<Payload<Registry>, CatalogUpdate>>,
) -> Result<Payload<Registry>, CatalogUpdate> {
    handle.join().unwrap_or_else(|_| {
        Err(CatalogUpdate::Failed {
            message: "the catalog probe panicked".to_owned(),
            log_tail: String::new(),
        })
    })
}

/// One probe: start `evo-agent serve` in `dir`, read `/registry`, shut it down.
fn run_probe(
    bin: &Path,
    dir: &Path,
    no_userspace: bool,
    env: Vec<(String, String)>,
) -> Result<Payload<Registry>, CatalogUpdate> {
    let mut config = ServerConfig::agent(bin, dir, dir).with_no_userspace(no_userspace);
    for (key, value) in env {
        config.extra_env.push((key, value));
    }
    let mut server = Server::start(&config).map_err(|error| CatalogUpdate::Failed {
        message: error.to_string(),
        log_tail: error.log_tail().unwrap_or_default().to_owned(),
    })?;
    let registry = server
        .client()
        .registry()
        .map_err(|error| CatalogUpdate::Failed {
            message: error.to_string(),
            log_tail: server.log_tail(40),
        });
    // However the read went, the probe does not stay running.
    let _ = server.shutdown();
    registry
}
