//! The offline surfaces of the two binaries (§9): `evo-agent sessions --json`,
//! `evo-swarm catalog --json`, `evo-swarm check --json`.
//!
//! Each of them boots in-process, prints **one JSON document** to stdout and
//! exits 0 or 1 — there is no listener, no port and no token. That is what lets
//! the app fill its choosers and its history list before anything is running,
//! and validate a launch before spawning one.
//!
//! The binary is never guessed: [`agent_bin`] and [`swarm_bin`] read
//! `EVO_AGENT_BIN` / `EVO_SWARM_BIN` first (the environment the test harness and
//! the stub home use) and fall back to the installed path. A caller that already
//! knows the path — `app.json`'s Settings value — hands it in, and the
//! environment is not consulted at all.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Where the agent binary is, when the environment does not say otherwise.
pub const INSTALLED_AGENT: &str = "/usr/local/bin/evo-agent";
/// Where the swarm binary is, when the environment does not say otherwise.
pub const INSTALLED_SWARM: &str = "/usr/local/bin/evo-swarm";

/// The environment variable that overrides the agent binary.
pub const AGENT_BIN_ENV: &str = "EVO_AGENT_BIN";
/// The environment variable that overrides the swarm binary.
pub const SWARM_BIN_ENV: &str = "EVO_SWARM_BIN";

/// The `evo-agent` this process should run: `$EVO_AGENT_BIN`, else the
/// installed path.
pub fn agent_bin() -> PathBuf {
    bin_from_env(AGENT_BIN_ENV, INSTALLED_AGENT)
}

/// The `evo-swarm` this process should run: `$EVO_SWARM_BIN`, else the installed
/// path.
pub fn swarm_bin() -> PathBuf {
    bin_from_env(SWARM_BIN_ENV, INSTALLED_SWARM)
}

fn bin_from_env(env: &str, installed: &str) -> PathBuf {
    match std::env::var_os(env) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(installed),
    }
}

/// The most of a child's own error output we keep: enough to name the mistake,
/// not enough to fill a log with a stack of them.
const STDERR_CAP: usize = 4096;

/// Why an offline run did not produce a document.
#[derive(Debug)]
pub enum CliError {
    /// The binary could not be started at all — not there, or not runnable.
    NotFound { bin: PathBuf, source: io::Error },
    /// It ran and exited non-zero.
    Failed {
        bin: PathBuf,
        status: Option<i32>,
        stderr: String,
    },
    /// It exited 0 and what it printed was not the JSON document §9 promises.
    Malformed { bin: PathBuf, message: String },
}

impl CliError {
    /// One line for a log: never the whole stderr, which may quote a
    /// configuration value.
    pub fn summary(&self) -> String {
        match self {
            CliError::NotFound { bin, .. } => format!("{} could not be run", bin.display()),
            CliError::Failed { bin, status, .. } => match status {
                Some(code) => format!("{} exited {code}", bin.display()),
                None => format!("{} was killed", bin.display()),
            },
            CliError::Malformed { bin, message } => {
                format!("{} printed no JSON: {message}", bin.display())
            }
        }
    }

    /// The child's own words, for a hover or a log line — `None` when it said
    /// nothing.
    pub fn detail(&self) -> Option<String> {
        match self {
            CliError::NotFound { source, .. } => Some(source.to_string()),
            CliError::Failed { stderr, .. } if !stderr.is_empty() => Some(stderr.clone()),
            _ => None,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for CliError {}

/// Run `bin args…`, and read stdout as one JSON document.
pub fn run_json(bin: &Path, args: &[String]) -> Result<Value, CliError> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .map_err(|source| CliError::NotFound {
            bin: bin.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(CliError::Failed {
            bin: bin.to_path_buf(),
            status: output.status.code(),
            stderr: tail(&output.stderr),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|error| CliError::Malformed {
        bin: bin.to_path_buf(),
        message: error.to_string(),
    })
}

/// Run `bin args…` for a command that prints its document whether or not it is
/// happy: `evo-swarm check --json` exits **1** when it found problems (§2) and still
/// prints them, so a caller that wants the problems has to read stdout on a failure
/// too — otherwise the one answer the empty tab exists to show reads as a command
/// that could not run.
///
/// Anything else — a document that does not parse, and no exit code 1 to explain
/// it — is still the failure it looks like.
pub fn run_json_reporting(bin: &Path, args: &[String]) -> Result<Value, CliError> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .map_err(|source| CliError::NotFound {
            bin: bin.to_path_buf(),
            source,
        })?;
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => Ok(value),
        Err(_) if !output.status.success() => Err(CliError::Failed {
            bin: bin.to_path_buf(),
            status: output.status.code(),
            stderr: tail(&output.stderr),
        }),
        Err(error) => Err(CliError::Malformed {
            bin: bin.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// The last [`STDERR_CAP`] bytes of a child's stderr, on a character boundary,
/// with the trailing newline trimmed.
fn tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim_end();
    if trimmed.len() <= STDERR_CAP {
        return trimmed.to_owned();
    }
    let start = trimmed.len() - STDERR_CAP;
    let start = (start..trimmed.len())
        .find(|i| trimmed.is_char_boundary(*i))
        .unwrap_or(trimmed.len());
    trimmed[start..].to_owned()
}

/// The argv for one command, as `Vec<String>` so a test can compare it without
/// building a process.
pub fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_script(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("store-cli-{}-{name}.sh", std::process::id()));
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_document_comes_back_as_json() {
        let bin = temp_script("ok", "#!/bin/sh\nprintf '{\"ok\":true}'\n");
        let value = run_json(&bin, &[]).unwrap();
        assert_eq!(value["ok"], Value::Bool(true));
        std::fs::remove_file(&bin).unwrap();
    }

    #[test]
    fn exit_one_is_a_failure_that_keeps_the_childs_words() {
        let bin = temp_script("fail", "#!/bin/sh\necho 'no such model' >&2\nexit 1\n");
        let error = run_json(&bin, &[]).unwrap_err();
        assert!(matches!(
            error,
            CliError::Failed {
                status: Some(1),
                ..
            }
        ));
        assert_eq!(error.detail().as_deref(), Some("no such model"));
        assert!(error.summary().contains("exited 1"));
        std::fs::remove_file(&bin).unwrap();
    }

    #[test]
    fn success_with_junk_is_malformed_not_a_panic() {
        let bin = temp_script("junk", "#!/bin/sh\necho 'not json'\n");
        let error = run_json(&bin, &[]).unwrap_err();
        assert!(matches!(error, CliError::Malformed { .. }));
        std::fs::remove_file(&bin).unwrap();
    }

    #[test]
    fn a_binary_that_is_not_there_is_not_found() {
        let error = run_json(Path::new("/nonexistent/evo-swarm"), &[]).unwrap_err();
        assert!(matches!(error, CliError::NotFound { .. }));
        assert!(error.summary().contains("could not be run"));
    }

    #[test]
    fn the_args_reach_the_child_in_order() {
        let bin = temp_script(
            "args",
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
        );
        let output = Command::new(&bin)
            .args(["a", "b c", "--x"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "a\nb c\n--x\n");
        assert_eq!(args(&["a", "b"]), vec!["a".to_string(), "b".to_string()]);
        std::fs::remove_file(&bin).unwrap();
    }

    #[test]
    fn a_huge_stderr_is_capped_at_its_tail() {
        let long = "e".repeat(STDERR_CAP * 2);
        let tailed = tail(long.as_bytes());
        assert_eq!(tailed.len(), STDERR_CAP);
    }
}
