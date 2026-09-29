//! Learning the model catalog without a running tab (§9.4): start a throwaway
//! `evo-agent serve`, read `/registry`, shut it down.

use std::path::PathBuf;

use crate::api::{Payload, Registry};
use crate::error::Result;
use crate::server::{Server, ServerConfig};

/// Start a throwaway `evo-agent serve` in `probe_dir`, read `GET /registry`, and
/// shut it down again.
///
/// `no_userspace` runs the probe in quarantine mode (`--no-userspace`), which is
/// what tells the **lanes** chooser which models a lane can register: a lane
/// starts with no userspace, so only the APIs the kernel itself defines — what
/// the probe's `/registry.apis` lists, and [`Registry::lane_ready`] checks — are
/// reachable there (§9.4).
///
/// The probe's `token` and `probe.log` are written inside `probe_dir`, so a
/// failed probe leaves its log behind.
///
/// [`Registry::lane_ready`]: crate::api::Registry::lane_ready
pub fn learn_registry(
    evo_agent_bin: impl Into<PathBuf>,
    probe_dir: impl Into<PathBuf>,
    no_userspace: bool,
) -> Result<Payload<Registry>> {
    let dir = probe_dir.into();
    std::fs::create_dir_all(&dir)?;
    learn_registry_with(
        ServerConfig::agent(evo_agent_bin, dir.clone(), &dir).with_no_userspace(no_userspace),
    )
}

/// The probe with a configuration of your own — the same thing, for a caller
/// that needs its own environment (a test with a temp `HOME`).
pub fn learn_registry_with(config: ServerConfig) -> Result<Payload<Registry>> {
    let mut server = Server::start(&config)?;
    let registry = server.client().registry();
    // However it went, do not leave the probe running.
    let _ = server.shutdown();
    registry
}
