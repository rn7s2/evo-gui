//! The terminal's environment, for an app the Dock started.
//!
//! A macOS app opened from Finder, the Dock or `open` inherits launchd's
//! environment, not the one a terminal gives: nothing `~/.zshenv`, `~/.zprofile`
//! or `~/.zshrc` exports is there. Every server a tab starts inherits this
//! process's environment, so an `evo-swarm` started by the app ran without the
//! user's proxy, API keys or `PATH` — and behaved differently from the same
//! command typed into a terminal (a provider reached without its proxy can
//! answer `HTTP 403`).
//!
//! So on the way in, before any other thread exists, the app asks the user's
//! shell for the environment a terminal session would have — a login,
//! interactive shell, as Terminal.app starts one (`$SHELL -l -i -c 'env -0'`) —
//! and makes it this process's environment, wholesale: variables the shell has
//! are set to its values, variables it does not have are removed. Every child
//! the app starts then sees what it would have seen from a terminal.
//!
//! Two of the shell's variables describe the probe rather than the session and
//! are not taken: `_` (the probe's last command) and `OLDPWD`. `PWD` is the
//! child's own folder, set where a server is spawned. Nothing that names a
//! terminal (`TERM`, `TERM_PROGRAM`, …) is invented: there is no terminal.
//!
//! Started from a terminal, the app already has that environment (plus whatever
//! that terminal changed on purpose) and the shell is not asked. A shell that
//! fails, prints nothing usable or takes longer than [`TIMEOUT`] changes
//! nothing: the app keeps what it was given.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long the shell gets. A typical one answers in well under a second; the
/// window waits on this, so a slow one is given up on rather than waited out.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Printed before `env -0`, so whatever an interactive rc file writes to stdout
/// (a banner, a fortune) is not read as a variable.
const MARKER: &str = "__EVO_DESKTOP_LOGIN_ENV_7f3a__";

/// The probe's own bookkeeping, not the session's.
const PROBE_ONLY: &[&str] = &["_", "OLDPWD"];

/// What [`adopt`] did, for the app log.
#[derive(Debug)]
pub enum Outcome {
    /// Started from a terminal: the environment is a terminal's already.
    FromTerminal,
    /// The shell answered and its environment is now this process's. Names
    /// only — values may be secrets.
    Adopted {
        shell: PathBuf,
        set: Vec<String>,
        removed: Vec<String>,
        took: Duration,
    },
    /// The shell could not be asked, or its answer was unusable.
    Failed { shell: PathBuf, reason: String },
}

impl Outcome {
    pub fn failed(&self) -> bool {
        matches!(self, Outcome::Failed { .. })
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::FromTerminal => write!(f, "login env: started from a terminal, kept as is"),
            Outcome::Adopted {
                shell,
                set,
                removed,
                took,
            } => write!(
                f,
                "login env: {} answered in {} ms; set {} [{}], removed {} [{}]",
                shell.display(),
                took.as_millis(),
                set.len(),
                set.join(" "),
                removed.len(),
                removed.join(" ")
            ),
            Outcome::Failed { shell, reason } => write!(
                f,
                "login env: {} gave nothing usable ({reason}); children inherit launchd's environment",
                shell.display()
            ),
        }
    }
}

/// One edit to the process environment.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Set(OsString, OsString),
    Remove(OsString),
}

/// Ask the shell for a terminal session's environment and make it this
/// process's.
///
/// Must run before any other thread exists: it calls `setenv`/`unsetenv`.
pub fn adopt() -> Outcome {
    if from_terminal() {
        return Outcome::FromTerminal;
    }
    let shell = login_shell();
    let started = Instant::now();
    let login = match read_login_env(&shell, &[], TIMEOUT) {
        Ok(vars) => vars,
        Err(reason) => return Outcome::Failed { shell, reason },
    };
    let current: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let (mut set, mut removed) = (Vec::new(), Vec::new());
    for change in plan(&current, login) {
        match change {
            Change::Set(key, value) => {
                set.push(key.to_string_lossy().into_owned());
                std::env::set_var(key, value);
            }
            Change::Remove(key) => {
                removed.push(key.to_string_lossy().into_owned());
                std::env::remove_var(key);
            }
        }
    }
    Outcome::Adopted {
        shell,
        set,
        removed,
        took: started.elapsed(),
    }
}

/// A process launchd started has no terminal on stdin and no `TERM`.
fn from_terminal() -> bool {
    let tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    tty || std::env::var_os("TERM").is_some()
}

/// `$SHELL`, else the account's shell, else zsh (macOS's default).
fn login_shell() -> PathBuf {
    if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return PathBuf::from(shell);
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let bytes = std::ffi::CStr::from_ptr((*pw).pw_shell).to_bytes();
            if !bytes.is_empty() {
                return PathBuf::from(OsStr::from_bytes(bytes));
            }
        }
    }
    PathBuf::from("/bin/zsh")
}

/// Run `shell -l -i -c 'printf MARKER; env -0'` and parse what follows the
/// marker. `env` is added to the shell's environment (tests point `HOME` at a
/// scratch folder with it).
fn read_login_env(
    shell: &Path,
    env: &[(&str, &OsStr)],
    timeout: Duration,
) -> Result<Vec<(OsString, OsString)>, String> {
    let script = format!("printf '%s' '{MARKER}'; /usr/bin/env -0");
    let mut command = Command::new(shell);
    command
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own group, so a timeout takes whatever the rc files started with it.
        .process_group(0);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|e| format!("cannot run it: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let out = match rx.recv_timeout(timeout) {
        Ok(out) => out,
        Err(_) => {
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
            return Err(format!("no answer within {} s", timeout.as_secs()));
        }
    };
    let _ = child.wait();
    parse(&out).ok_or_else(|| "no environment in its output".to_string())
}

/// The `KEY=VALUE\0…` block after the last [`MARKER`]; `None` when there is none.
pub fn parse(out: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let marker = MARKER.as_bytes();
    let at = out.windows(marker.len()).rposition(|w| w == marker)?;
    let block = &out[at + marker.len()..];
    let vars: Vec<_> = block
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|&b| b == b'=')?;
            if eq == 0 {
                return None;
            }
            Some((
                OsString::from_vec(entry[..eq].to_vec()),
                OsString::from_vec(entry[eq + 1..].to_vec()),
            ))
        })
        .collect();
    // A shell always has HOME; an answer without it is not an environment.
    if vars.iter().any(|(k, _)| k == "HOME") {
        Some(vars)
    } else {
        None
    }
}

/// The edits that turn `current` into the shell's environment: every variable
/// the shell has, at its value; every other one gone. The probe's own
/// bookkeeping ([`PROBE_ONLY`]) is neither set nor removed.
pub fn plan(current: &[(OsString, OsString)], login: Vec<(OsString, OsString)>) -> Vec<Change> {
    let probe_only = |key: &OsStr| key.to_str().is_some_and(|k| PROBE_ONLY.contains(&k));
    let mut changes = Vec::new();
    let mut kept = HashSet::new();
    for (key, value) in login {
        if probe_only(&key) {
            continue;
        }
        kept.insert(key.clone());
        let same = current.iter().any(|(k, v)| *k == key && *v == value);
        if !same {
            changes.push(Change::Set(key, value));
        }
    }
    for (key, _) in current {
        if !kept.contains(key) && !probe_only(key) {
            changes.push(Change::Remove(key.clone()));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (os(k), os(v))).collect()
    }

    #[test]
    fn parse_skips_what_the_rc_files_printed() {
        let mut out = b"Welcome!\nfortune: today is a good day\n".to_vec();
        out.extend_from_slice(MARKER.as_bytes());
        out.extend_from_slice(b"HOME=/home/user\0A=b=c\0MULTI=line1\nline2\0");
        assert_eq!(
            parse(&out).unwrap(),
            vars(&[
                ("HOME", "/home/user"),
                ("A", "b=c"),
                ("MULTI", "line1\nline2")
            ])
        );
    }

    #[test]
    fn parse_without_an_environment_is_nothing() {
        assert!(parse(b"HOME=/x\0").is_none(), "no marker");
        assert!(parse(MARKER.as_bytes()).is_none(), "nothing after it");
        let mut no_home = MARKER.as_bytes().to_vec();
        no_home.extend_from_slice(b"A=1\0");
        assert!(parse(&no_home).is_none(), "no HOME");
    }

    #[test]
    fn the_process_becomes_the_shells_environment() {
        let current = vars(&[
            ("HOME", "/home/user"),
            ("PATH", "/usr/bin:/bin"),
            ("SET_BY_LAUNCHD", "x"),
            ("CHANGED_BY_RC", "old"),
        ]);
        let login = vars(&[
            ("HOME", "/home/user"),
            ("PATH", "/opt/example/bin:/usr/bin:/bin"),
            ("CHANGED_BY_RC", "new"),
            ("https_proxy", "http://proxy.example.com:8080"),
        ]);
        assert_eq!(
            plan(&current, login),
            vec![
                Change::Set(os("PATH"), os("/opt/example/bin:/usr/bin:/bin")),
                Change::Set(os("CHANGED_BY_RC"), os("new")),
                Change::Set(os("https_proxy"), os("http://proxy.example.com:8080")),
                Change::Remove(os("SET_BY_LAUNCHD")),
            ]
        );
    }

    #[test]
    fn the_probes_bookkeeping_is_left_alone() {
        let current = vars(&[("HOME", "/h"), ("_", "/app/binary")]);
        let login = vars(&[("HOME", "/h"), ("_", "/usr/bin/env"), ("OLDPWD", "/")]);
        assert!(plan(&current, login).is_empty());
    }

    /// The real thing: a login shell, asked through the same code path, reports
    /// what its rc files export — the variable a terminal would have.
    #[test]
    fn a_login_shell_reports_its_rc_files() {
        let home = std::env::temp_dir().join(format!("evo-login-env-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".zshenv"),
            "export EVO_LOGIN_ENV_PROBE=from-zshenv\n",
        )
        .unwrap();
        std::fs::write(home.join(".zshrc"), "echo hello from zshrc\n").unwrap();
        let env = [("HOME", home.as_os_str()), ("ZDOTDIR", home.as_os_str())];
        let got = read_login_env(Path::new("/bin/zsh"), &env, TIMEOUT).unwrap();
        let _ = std::fs::remove_dir_all(&home);
        assert!(
            got.contains(&(os("EVO_LOGIN_ENV_PROBE"), os("from-zshenv"))),
            "{got:?}"
        );
    }

    #[test]
    fn a_shell_that_hangs_is_given_up_on() {
        let dir = std::env::temp_dir().join(format!("evo-login-env-slow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("slowsh");
        std::fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let err = read_login_env(&fake, &[], Duration::from_millis(300)).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("no answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
