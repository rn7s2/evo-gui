//! Two *real* processes, one lock (§2 rule 1).
//!
//! The in-process tests in `src/single.rs` cover the protocol; this one proves
//! the part that only two processes can show: a second launch knocks and exits
//! instead of starting a second window, and when the first process dies the lock
//! dies with it — no stale lock blocks the next launch.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

/// The helper binary, built by `cargo test` for this package.
const PROBE: &str = env!("CARGO_BIN_EXE_store-lock-probe");

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(name: &str) -> TempRoot {
        let dir = std::env::temp_dir().join(format!("store-si-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempRoot(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn spawn_holder(root: &Path, seconds: u64) -> (Child, ChildStdout) {
    let mut child = Command::new(PROBE)
        .arg(root)
        .arg("hold")
        .arg(seconds.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn store-lock-probe");
    let stdout = child.stdout.take().expect("stdout");
    (child, stdout)
}

/// Read one line from the child. Blocks until it arrives — no polling.
fn first_line(stdout: &mut ChildStdout) -> String {
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).expect("read child stdout");
    line.trim().to_string()
}

fn poke(root: &Path) -> String {
    let out = Command::new(PROBE).arg(root).arg("poke").output().expect("run poke");
    assert!(out.status.success(), "poke failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_second_process_activates_the_first_and_the_lock_dies_with_it() {
    let root = TempRoot::new("two");
    let (mut holder, mut stdout) = spawn_holder(root.path(), 5);
    assert!(first_line(&mut stdout).starts_with("primary"), "the first process takes the lock");
    assert!(root.path().join("lock").exists());
    assert!(root.path().join("activate.sock").exists());
    assert_eq!(
        fs::read_to_string(root.path().join("lock")).unwrap().trim(),
        holder.id().to_string(),
        "the pid inside the lock file is the holder's"
    );

    // A second process does not take the lock: it knocks, is answered, exits 0.
    assert_eq!(poke(root.path()), "secondary activated=true");

    // While the holder is alive, every further knock behaves the same way.
    assert_eq!(poke(root.path()), "secondary activated=true");

    holder.wait().expect("the holder exits on its own");
    // The lock died with the process: the next launch takes it without any
    // cleanup step, even though the lock file is still on disk.
    assert!(root.path().join("lock").exists(), "the lock file remains; the lock does not");
    let line = poke(root.path());
    assert!(line.starts_with("primary"), "the freed lock must be taken again: {line}");
}

#[test]
fn a_killed_process_leaves_no_stale_lock() {
    let root = TempRoot::new("kill");
    let (mut holder, mut stdout) = spawn_holder(root.path(), 30);
    assert!(first_line(&mut stdout).starts_with("primary"));
    assert_eq!(poke(root.path()), "secondary activated=true");

    // SIGKILL: no destructor runs, no cleanup happens — and the lock still goes.
    holder.kill().expect("kill the holder");
    holder.wait().expect("reap");

    // The socket file was left behind by the kill; a fresh launch must take the
    // lock anyway and replace it.
    let out = Command::new(PROBE)
        .arg(root.path())
        .arg("hold")
        .arg("0")
        .output()
        .expect("run store-lock-probe");
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(line.starts_with("primary"), "a killed holder must not block the next launch: {line}");
}
