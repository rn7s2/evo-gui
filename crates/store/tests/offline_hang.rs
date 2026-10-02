//! A read that never ends, and the two things that end it (§9.4, §9.5, §9.8).
//!
//! In a file of its own, and in one test: [`store::cli::stop_live_children`]
//! stops *every* offline read still running — which is exactly what the quit path
//! needs from it — so a test that calls it has to be the only one in its process.
//! Each integration test binary is a process of its own; the tests inside one run
//! in parallel, which is why the two halves of this are one test.
//!
//! What is tested is the shape the bug had: a child that never exits and ignores
//! `SIGTERM` (an `evo-swarm check --json` was found still going, orphaned, twenty
//! hours after the app that spawned it quit), and a helper of its own beside it —
//! which is why the claim the app holds is a *process group*, and why the assertion
//! at the end is about the group and not only about the process.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use store::cli::{self, CliError};

/// The bound the first half gives its read.
///
/// Two seconds rather than a handful of milliseconds: a machine running the whole
/// suite in parallel has to have *run* the script's first line — the pid it names
/// itself by, which is how the assertions below find its group — before this bound
/// ends it, and a bound that is too short tests the scheduler instead of the
/// timeout. Two seconds is still nothing to wait for.
const SHORT: Duration = Duration::from_secs(2);

/// How long the group is given to be really gone: the leader is reaped by the
/// read's own thread, and its helper by whoever inherits it, so "gone" is a
/// moment after the kill rather than the instant of it.
const GONE_WITHIN: Duration = Duration::from_secs(5);

/// A script that never ends, ignores `SIGTERM`, and holds a helper of its own —
/// and that writes the pid it runs as, so a test can ask afterwards whether
/// anything of it is left.
///
/// It loops rather than `wait`s for that helper on purpose: `SIGTERM` kills the
/// helper (a `sleep` does not trap), and a shell waiting on it would then exit 0
/// with nothing on stdout — the read would come back as a *document* problem
/// instead of the killed read this is about.
struct Hung {
    bin: PathBuf,
    pid_file: PathBuf,
    dir: PathBuf,
}

impl Hung {
    fn new(name: &str) -> Hung {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("store-hang-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let bin = dir.join("evo-hang.sh");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
trap '' TERM
echo $$ > "{pid}"
sleep 3600 &
while :; do sleep 1; done
"#,
                pid = pid_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        Hung { bin, pid_file, dir }
    }

    /// Watch for the read to have started — it writes the pid it runs as — from
    /// beside it, because the read itself is one blocking wait. `None` when it
    /// never got that far, which is a failure the caller names.
    fn watch_started(&self) -> JoinHandle<Option<libc::pid_t>> {
        let pid_file = self.pid_file.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + GONE_WITHIN;
            while Instant::now() < deadline {
                if let Ok(text) = fs::read_to_string(&pid_file) {
                    if let Ok(pid) = text.trim().parse() {
                        return Some(pid);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            None
        })
    }
}

impl Drop for Hung {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Whether the process group `pid` leads still has a member: `killpg(pid, 0)`
/// says so by existing, and `ESRCH` is the answer the assertions want.
fn group_there(pid: libc::pid_t) -> bool {
    if unsafe { libc::killpg(pid, 0) } == 0 {
        return true;
    }
    // `EPERM` is a yes: the group is there and it is not ours to signal. It cannot
    // happen to a child of ours; it is spelled out so the answer here is about
    // existence and not about permission.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Wait for the group to be gone, and say whether it went.
fn group_went(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + GONE_WITHIN;
    while group_there(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    !group_there(pid)
}

#[test]
fn a_bound_runs_out_and_the_quit_stops_what_is_running() {
    // The bound: a read that ignores SIGTERM is stopped anyway — within its own
    // bound, plus the grace it is given to go — and the group goes with it.
    let hung = Hung::new("bound");
    let started = hung.watch_started();
    let at = Instant::now();
    let error = cli::run_json_within(&hung.bin, &[], SHORT).unwrap_err();
    let waited = at.elapsed();
    let pid = started
        .join()
        .expect("the watcher did not panic")
        .expect("the hung read started and named itself");
    let CliError::TimedOut { after, .. } = &error else {
        panic!("a read that never ends is a timeout, got {error:?}");
    };
    assert_eq!(*after, SHORT, "the error states the bound it ran out of");
    assert!(
        error.summary().contains("did not answer within"),
        "the line names the read: {error}"
    );
    assert_eq!(error.detail(), None, "there is no answer to quote");
    assert!(
        waited >= SHORT && waited < SHORT + GONE_WITHIN,
        "the bound is what ended it, not luck: {waited:?}"
    );
    assert!(group_went(pid), "the hung read's group outlived the bound");
    assert_eq!(
        cli::stop_live_children(),
        0,
        "a read that ended is not one the quit has to stop"
    );

    // The quit: a read still running when the app leaves is stopped by
    // `stop_live_children` — the call the quit path makes — and says how many it
    // stopped. Its bound is far off, so nothing but the quit can have ended it.
    let hung = Hung::new("quit");
    let started = hung.watch_started();
    let read = std::thread::spawn({
        let bin = hung.bin.clone();
        move || cli::run_json_within(&bin, &[], Duration::from_secs(30))
    });
    let pid = started
        .join()
        .expect("the watcher did not panic")
        .expect("the hung read started and named itself");
    assert!(group_there(pid), "the read is not running yet (pid {pid})");
    assert_eq!(cli::stop_live_children(), 1, "the read the app started");
    let error = read.join().unwrap().unwrap_err();
    assert!(
        matches!(error, CliError::Failed { status: None, .. }),
        "a read the quit stopped is one that was killed, not one that failed: {error:?}"
    );
    assert!(group_went(pid), "the group outlived the quit (pid {pid})");
    assert_eq!(cli::stop_live_children(), 0, "nothing is left to stop");
}
