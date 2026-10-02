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
//!
//! One read here is not a document at all: `<bin> --help`, whose usage text is how
//! a window learns whether the binary it is about to run takes a flag the window
//! wants to pass it ([`supports_prompt_note`]).
//!
//! # Every read is bounded, and nothing is left behind (§9.4, §9.5, §9.8)
//!
//! A run that would never exit has happened: an `evo-swarm check --json` the app
//! spawned on a user's machine was still going twenty hours later, orphaned when
//! the app that started it quit (`ppid 1`), and it ignored the `SIGTERM` sent to
//! it by hand — the command has no timeout of its own and reads nothing. So:
//!
//! * each run is given a bound ([`PROBE_TIMEOUT`], [`SESSIONS_TIMEOUT`],
//!   [`VERSION_TIMEOUT`]) and is spawned as the leader of **its own process
//!   group**, so the claim the app makes on it — and on anything *it* spawned —
//!   can be given up as a group;
//! * the bound running out is [`CliError::TimedOut`]: the group is sent `SIGTERM`,
//!   given a short grace, and then killed, and the caller gets one line saying
//!   which binary never answered;
//! * [`stop_live_children`] is the same ladder over every read still running, and
//!   the quit path calls it — the app's own exit is what stops what it started.
//!
//! What this does **not** cover, on macOS or anywhere else without a parent-death
//! signal: an app killed outright (`SIGKILL`, or a crash) runs no code, so nothing
//! *in* it can stop anything. The guarantee is two-sided rather than absolute — the
//! quit path stops what is still running when the app leaves (§9.8), and the bound
//! stops what never answers while the app is there — which is the most macOS
//! allows (`PDEATHSIG` is Linux-only and has no equivalent here). A force-quit is
//! the one case neither side reaches: the read then ends by its own exit, or does
//! not, which is how the twenty-hour one was found.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Where the agent binary is, when the environment does not say otherwise.
pub const INSTALLED_AGENT: &str = "/usr/local/bin/evo-agent";
/// Where the swarm binary is, when the environment does not say otherwise.
pub const INSTALLED_SWARM: &str = "/usr/local/bin/evo-swarm";

/// The environment variable that overrides the agent binary.
pub const AGENT_BIN_ENV: &str = "EVO_AGENT_BIN";
/// The environment variable that overrides the swarm binary.
pub const SWARM_BIN_ENV: &str = "EVO_SWARM_BIN";

/// The flag a client's own note is passed with, by both binaries
/// (`serve --prompt-note <path>`), and the string the probe below looks for.
pub const PROMPT_NOTE_FLAG: &str = "--prompt-note";

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

/// How long a `catalog --json` or `check --json` may take before the app gives up
/// on it (§9.4).
///
/// Generous rather than tight, on purpose: these boot evo in-process and evaluate
/// a project's lane forms, so a healthy one takes a second or two on a busy
/// machine — and a launch validated against a timeout that was too eager would be
/// worse than one that waited. Thirty seconds is a bound against *never*, not a
/// budget.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long `sessions --json` may take.
///
/// Shorter than the probes because it is the lighter read (§9.5) — it walks the session
/// index rather than the registries — and because the empty tab shows something
/// either way while it is missing (§9.5).
pub const SESSIONS_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `<bin> --version` may take before the app gives up on it (§13).
///
/// The shortest bound here, because the least is asked of it: both binaries introduce
/// themselves before they do anything else, and five seconds is already far longer
/// than that takes. It is a bound against a binary that does not answer *at all* —
/// the failure the longer bounds are for too — and the same ladder ends it.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `<bin> --help` may take before the app gives up on it.
///
/// Cheaper than [`PROBE_TIMEOUT`] on purpose: usage text is printed by the
/// argument parser, before the binary boots the session the other reads need, so
/// a healthy one answers in the time it takes to start the image — and this is a
/// question asked while a window is being built. The bound is there all the same:
/// a binary that hangs on `--help` must not hang a window.
pub const USAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a child is given to go after `SIGTERM`, before its group is killed.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// How often a bounded wait looks whether the child is done.
const WAIT_POLL: Duration = Duration::from_millis(20);

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
    /// It was still running when its bound ran out, and was stopped (§9.4, §9.5).
    ///
    /// The one failure that is about *time* rather than about what the binary
    /// said: there is no exit status and no stderr to show, because there was no
    /// answer — only a process that had to be given up on.
    TimedOut { bin: PathBuf, after: Duration },
}

impl CliError {
    /// One line for a log or a page: never the whole stderr, which may quote a
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
            CliError::TimedOut { bin, after } => {
                format!(
                    "{} did not answer within {}s",
                    bin.display(),
                    seconds(after)
                )
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

/// A bound as the whole seconds a person reads, never "0s".
fn seconds(after: &Duration) -> u64 {
    after.as_secs().max(1)
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for CliError {}

/// Run `bin args…`, and read stdout as one JSON document, within
/// [`PROBE_TIMEOUT`].
pub fn run_json(bin: &Path, args: &[String]) -> Result<Value, CliError> {
    run_json_within(bin, args, PROBE_TIMEOUT)
}

/// Run `bin args…`, and read stdout as one JSON document, giving it `limit`
/// before it is stopped.
pub fn run_json_within(bin: &Path, args: &[String], limit: Duration) -> Result<Value, CliError> {
    let run = run_bounded(bin, args, limit)?;
    if !run.status.success() {
        return Err(CliError::Failed {
            bin: bin.to_path_buf(),
            status: run.status.code(),
            stderr: tail(&run.stderr),
        });
    }
    serde_json::from_slice(&run.stdout).map_err(|error| CliError::Malformed {
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
    run_json_reporting_within(bin, args, PROBE_TIMEOUT)
}

/// [`run_json_reporting`], giving the child `limit` before it is stopped.
pub fn run_json_reporting_within(
    bin: &Path,
    args: &[String],
    limit: Duration,
) -> Result<Value, CliError> {
    let run = run_bounded(bin, args, limit)?;
    match serde_json::from_slice(&run.stdout) {
        Ok(value) => Ok(value),
        Err(_) if !run.status.success() => Err(CliError::Failed {
            bin: bin.to_path_buf(),
            status: run.status.code(),
            stderr: tail(&run.stderr),
        }),
        Err(error) => Err(CliError::Malformed {
            bin: bin.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// Whether `bin` takes [`PROMPT_NOTE_FLAG`] — asked of the binary, once.
///
/// The binary a window runs is not necessarily the one the app was built against:
/// `app.json` can name a build from before the flag existed, and to a binary that
/// does not know it the flag is a refusal to start (exit 64 — a launch that never
/// comes ready). So the usage text is what is read, not a version number: a flag
/// that exists is a line in it, `--help` prints it before anything boots, and the
/// read is the same bounded one every other offline ask uses — its own process
/// group, stopped on the way out ([`stop_live_children`]).
///
/// The answer is cached by path, size and modification time: the app asks once per
/// binary per run, and a binary that is replaced is a different question. Every way
/// the ask can fail — not there, not runnable, nothing printed, no answer inside
/// [`USAGE_TIMEOUT`] — is `false`. A session started without the note reads its
/// formulas as source, which is a great deal better than a session that will not
/// start; a client that does not need the answer asks nothing.
pub fn supports_prompt_note(bin: &Path) -> bool {
    let stamp = Stamp::of(bin);
    // The lock is let go around the ask: a probe is a process, and another thread
    // asking about another binary must not wait on this one. Two threads asking
    // about the same one probe twice and agree.
    let known = asked()
        .iter()
        .find(|(asked, _)| *asked == stamp)
        .map(|(_, known)| *known);
    if let Some(known) = known {
        return known;
    }
    let answer = usage_names_the_flag(bin, USAGE_TIMEOUT);
    let mut asked = asked();
    match asked.iter_mut().find(|(asked, _)| *asked == stamp) {
        Some(entry) => entry.1 = answer,
        None => asked.push((stamp, answer)),
    }
    answer
}

/// `<bin> --help` within `limit`, and whether the flag is in what it printed.
///
/// Either stream, and whatever the exit code: usage belongs to the argument parser,
/// and where a binary writes it and with what status is not this question. What is
/// asked is only whether the flag is written down.
fn usage_names_the_flag(bin: &Path, limit: Duration) -> bool {
    let Ok(run) = run_bounded(bin, &args(&["--help"]), limit) else {
        return false;
    };
    let printed = |stream: &[u8]| String::from_utf8_lossy(stream).into_owned();
    format!("{}{}", printed(&run.stdout), printed(&run.stderr)).contains(PROMPT_NOTE_FLAG)
}

/// One binary, as the file it was when it was asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    len: u64,
    /// Modified, in nanoseconds since the epoch; `0` when the file cannot be read,
    /// which is its own answer and is cached like any other.
    modified: i64,
}

impl Stamp {
    fn of(bin: &Path) -> Stamp {
        let meta = std::fs::metadata(bin).ok();
        Stamp {
            path: bin.to_path_buf(),
            len: meta.as_ref().map_or(0, std::fs::Metadata::len),
            modified: meta
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| {
                    i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
                }),
        }
    }
}

/// What each binary has already answered. A probe thread that panicked while
/// holding it must not turn every later ask into a panic: a stamp whose answer is
/// lost is one more `--help`, and nothing is signalled here.
static ASKED: Mutex<Vec<(Stamp, bool)>> = Mutex::new(Vec::new());

fn asked() -> MutexGuard<'static, Vec<(Stamp, bool)>> {
    ASKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `<bin> --version` within `limit` (§13): what a binary says about itself, asked
/// the way both Settings rows and the About dialog ask.
///
/// The one bounded read with no document in it: the caller gets the exit status and
/// both streams, in bytes, and decides what they mean — "it introduced itself",
/// "not an evo binary", "nothing to read". A binary that never answers is
/// [`CliError::TimedOut`], like everywhere else in this module, and its group is
/// stopped the same way. [`VERSION_TIMEOUT`] is the bound the app gives it; a caller
/// that wants another one says so.
pub fn version_within(bin: &Path, limit: Duration) -> Result<Output, CliError> {
    run_bounded(bin, &args(&["--version"]), limit)
}

/// What a bounded run said: its exit, and its two streams, exactly as they came.
pub struct Output {
    /// How it ended — a signal is not a status, so a killed read has no code.
    pub status: ExitStatus,
    /// Everything it printed.
    pub stdout: Vec<u8>,
    /// Everything it complained about, uncapped: [`tail`] is what a *message* does
    /// with this, and a caller that wants the first line wants all of it.
    pub stderr: Vec<u8>,
}

/// Run `bin args…` in its own process group, and give it `limit` to exit.
///
/// The child's two pipes are drained by threads of their own: `check --json`
/// prints a document with every model the session has, and a child that fills a
/// pipe nobody is reading blocks writing it — the same reason `Command::output`
/// does it this way. Its stdin is `/dev/null`, so a command that reads the
/// terminal gets an end-of-file instead of a wait.
///
/// Out of time, the *group* goes (see the module docs): `SIGTERM`, [`TERM_GRACE`],
/// then `SIGKILL`, which is what a child that ignores `SIGTERM` has earned. The
/// child is then waited for, so nothing of it is left as a zombie, and the
/// readers are let go rather than joined: the group is gone, and joining a reader
/// whose pipe a surviving grandchild held would be a second way to hang.
fn run_bounded(bin: &Path, args: &[String], limit: Duration) -> Result<Output, CliError> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_process_group(&mut command);
    let not_found = |source: io::Error| CliError::NotFound {
        bin: bin.to_path_buf(),
        source,
    };
    let mut child = command.spawn().map_err(not_found)?;
    let pid = child.id();
    let live = Live::new(pid);
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let out_reader = std::thread::spawn(move || read_all(stdout));
    let err_reader = std::thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(WAIT_POLL),
            Ok(None) => {
                abandon(&mut child, pid);
                return Err(CliError::TimedOut {
                    bin: bin.to_path_buf(),
                    after: limit,
                });
            }
            Err(source) => {
                // A wait that failed is a read this thread can no longer see: it is
                // given up on rather than left running with nobody holding it.
                abandon(&mut child, pid);
                return Err(not_found(source));
            }
        }
    };
    // The child is gone, so both pipes are closed and the readers are at their
    // end — except for a helper that inherited one and outlived it.
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    drop(live);
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Give up on a read that did not answer: its group goes ([`stop_group`]), then the
/// child itself — which is what ends it where there are no groups to signal — and
/// then it is waited for, so nothing of it is left as a zombie the app would have
/// to outlive anyway.
fn abandon(child: &mut Child, pid: u32) {
    stop_group(pid);
    let _ = child.kill();
    let _ = child.wait();
}

/// Read a pipe to its end, wherever it is. A read that fails is the end of it:
/// what arrived is what the caller gets to see.
fn read_all(mut pipe: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    bytes
}

/// The offline reads running right now, by process id — which is their group id
/// too, since each is spawned as its own group's leader (see
/// [`own_process_group`]).
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// The live list. A probe thread that panicked while holding it must not turn
/// every later read into a panic: the list is only pids, and a stale one is one
/// signal to a process that is not there.
fn running() -> MutexGuard<'static, Vec<u32>> {
    LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One read as long as it is running: leaving the list is what its drop is for, so
/// every way out of [`run_bounded`] deregisters it.
struct Live(u32);

impl Live {
    fn new(pid: u32) -> Live {
        running().push(pid);
        Live(pid)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        running().retain(|live| *live != self.0);
    }
}

/// Stop every offline read still running, and answer how many there were.
///
/// The quit path's call (§9.8): the bound in [`run_bounded`] would end any of
/// these in a few seconds, but a quit is not a place to leave a process behind,
/// and a read already hung is exactly the case this exists for. Bounded by the
/// same ladder as a timeout — `SIGTERM`, one shared [`TERM_GRACE`], `SIGKILL` —
/// so it costs the quit half a second at the very most, and only while something
/// is actually still running.
///
/// The list is held across the ladder: a child cannot leave it (and its pid be
/// handed to somebody else) while its group is being signalled here.
pub fn stop_live_children() -> usize {
    let live = running();
    if live.is_empty() {
        return 0;
    }
    let count = live.len();
    for pid in live.iter() {
        signal_group(*pid, Signal::Term);
    }
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline && live.iter().any(|pid| group_alive(*pid)) {
        std::thread::sleep(WAIT_POLL);
    }
    // Not waited for: the signal is what the group is owed, and the app is
    // leaving. Whoever is still there has been killed for it.
    for pid in live.iter() {
        signal_group(*pid, Signal::Kill);
    }
    count
}

/// The two signals the ladder uses.
#[derive(Clone, Copy)]
enum Signal {
    /// Ask the group to go — what a program that reads its signals honours.
    Term,
    /// Take it down: for a program that does not, which is how a hung read was
    /// found in the first place.
    Kill,
}

/// Put the child at the head of a process group of its own.
///
/// Two things follow. A signal to the group reaches everything the read spawned
/// as well as the read itself — `evo-swarm check` evaluates a project's lane
/// forms, and a lane-shaped helper left behind is the same leak one level down.
/// And nothing signalled here can reach the app: the group is the read's, not
/// the window's.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Where there are no process groups the child is spawned the plain way, and the
/// ladder below still ends it: see [`signal_group`].
#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

/// Whether this group still has a member.
#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    if unsafe { libc::killpg(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    // `EPERM` is a no: the group is there and it is not ours to signal, which is
    // not a thing that can happen to a child of ours — unless it changed hands,
    // and then the signal was never ours to send either.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Signal a whole process group; a group that is already gone is not an error.
#[cfg(unix)]
fn signal_group(pid: u32, signal: Signal) {
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    unsafe { libc::killpg(pid as libc::pid_t, signal) };
}

/// Take a process group down: `SIGTERM`, one grace, then `SIGKILL`.
fn stop_group(pid: u32) {
    signal_group(pid, Signal::Term);
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline && group_alive(pid) {
        std::thread::sleep(WAIT_POLL);
    }
    signal_group(pid, Signal::Kill);
}

/// Without process groups there is no group to signal, and no way from here to
/// signal a process at all — the `Child` that could is what the timeout path
/// already gives up by the time this runs. The read is then ended by its own
/// exit, as it was before any of this: the app is Unix, and this is only so the
/// crate still builds where it is not.
#[cfg(not(unix))]
fn group_alive(_pid: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn signal_group(_pid: u32, _signal: Signal) {}

#[cfg(not(unix))]
fn stop_group(_pid: u32) {}

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

    #[test]
    fn a_read_that_never_answered_says_so_and_quotes_nothing() {
        let error = CliError::TimedOut {
            bin: PathBuf::from("/usr/local/bin/evo-swarm"),
            after: PROBE_TIMEOUT,
        };
        assert_eq!(
            error.summary(),
            "/usr/local/bin/evo-swarm did not answer within 30s"
        );
        assert_eq!(error.detail(), None, "there is no answer to quote");
        assert!(error.to_string().contains("did not answer"));
    }

    #[test]
    fn a_bound_below_a_second_is_still_read_as_a_second() {
        // The line a person reads is in whole seconds: `0s` would be a bound
        // nobody can act on, and it is the only thing a sub-second bound — a
        // test's — could say.
        assert_eq!(seconds(&Duration::from_millis(300)), 1);
        assert_eq!(seconds(&SESSIONS_TIMEOUT), 10);
    }

    /// The flag is taken where the binary's own usage writes it down — and a build
    /// from before the flag existed is a "no", never a guess in its favour.
    #[test]
    fn the_usage_text_says_whether_the_flag_is_taken() {
        let with = temp_script(
            "note-flag",
            "#!/bin/sh\necho 'usage: evo-agent serve [--ready-file PATH] [--prompt-note PATH]'\n",
        );
        assert!(supports_prompt_note(&with), "{with:?} names the flag");
        // The same question of a binary that does not know it: this is the old
        // build, and the launch it is about goes out without the note.
        let without = temp_script(
            "note-flag-old",
            "#!/bin/sh\necho 'usage: evo-agent serve [--ready-file PATH]'\n",
        );
        assert!(!supports_prompt_note(&without));
        std::fs::remove_file(&with).unwrap();
        std::fs::remove_file(&without).unwrap();
    }

    /// Usage on stderr and a non-zero exit are still an answer: what is asked is
    /// whether the flag is written down, not where or with what status.
    #[test]
    fn usage_written_to_stderr_is_read_too() {
        let bin = temp_script(
            "note-flag-stderr",
            "#!/bin/sh\necho 'usage: evo-swarm serve [--prompt-note PATH]' >&2\nexit 1\n",
        );
        assert!(supports_prompt_note(&bin));
        std::fs::remove_file(&bin).unwrap();
    }

    /// A binary that cannot be asked is a binary that gets no note: never an
    /// error, and never a hang — the read is bounded like every other one.
    #[test]
    fn a_binary_that_cannot_be_asked_takes_no_note() {
        assert!(!supports_prompt_note(Path::new(
            "/nonexistent/evo-agent-evo-gui-test"
        )));
        let stubborn = temp_script("note-flag-hang", "#!/bin/sh\nsleep 60\n");
        let started = Instant::now();
        assert!(
            !usage_names_the_flag(&stubborn, Duration::from_millis(200)),
            "nothing printed, no flag"
        );
        assert!(
            started.elapsed() < USAGE_TIMEOUT,
            "the read ends at its bound, not at the binary's"
        );
        std::fs::remove_file(&stubborn).unwrap();
    }

    /// A rebuild that learned the flag is a different binary, and is asked again:
    /// the answer is cached for the file it was given, not for the path.
    #[test]
    fn a_binary_that_is_replaced_is_asked_again() {
        let bin = temp_script(
            "note-flag-rebuilt",
            "#!/bin/sh\necho 'usage: evo-agent serve'\n",
        );
        assert!(!supports_prompt_note(&bin));
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'usage: evo-agent serve [--prompt-note PATH]'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(supports_prompt_note(&bin), "the new file is its own answer");
        std::fs::remove_file(&bin).unwrap();
    }
}
