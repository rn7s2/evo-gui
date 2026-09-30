//! t04 — a restart resumes the *exact* session: two servers in one folder, one
//! of them killed, and the one that comes back is the one that died.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t04_restart_resumes
//! ```
//!
//! This is CONTRACT §1's E1, the most serious of the old bugs: the supervisor used
//! to restart a child with a bare `--resume`, which means *the newest journal in
//! this folder* — so with two tabs (or a TUI) in one folder, a restarted tab came
//! back on whatever session was written last. Here both servers run in **one**
//! folder, both answer a turn, one is killed, and the restarted one must:
//!
//! * rewrite its ready file (new epoch, `restarts` ≥ 1) — §1;
//! * name the **same** journal path in it;
//! * still hold the turn it had answered;
//! * and have been started with `--resume <that path>`, which the process's own
//!   command line proves.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, send, snapshot, wait_for, Watcher};
use store::launch::Program;
use swarm_client::ReadyFile;

/// A tab's server is a swarm; the restart machinery is the same in both, and the
/// two-in-one-folder hazard is about folders, not lanes. Switch to
/// `Program::Swarm` (with workers) to run the same proof through a swarm.
const PROGRAM: Program = Program::Agent;

const FIRST: &str = "t04 first turn";
const OTHER: &str = "t04 the other tab's turn";

#[test]
fn t04_restart_resumes() {
    let started = Instant::now();
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t04");
    let (tab_a, tab_b) = (fixture.new_tab(), fixture.new_tab());
    let mut a = fixture.spawn(&fixture.spec_in(PROGRAM, 0, &tab_a));
    let mut b = fixture.spawn(&fixture.spec_in(PROGRAM, 0, &tab_b));
    assert_eq!(
        a.ready().program,
        "evo-agent",
        "both run in the same folder: {}",
        fixture.folder.display()
    );

    // --- each tab answers its own turn ------------------------------------------
    let a_path = a.ready().session.path.clone();
    let b_path = b.ready().session.path.clone();
    assert_ne!(a_path, b_path, "two servers in one folder: two journals");
    answer(&a, FIRST);
    answer(&b, OTHER);

    // --- kill A's child, and only its child -------------------------------------
    let child = a.client().get_json("/health").expect("health")["pid"]
        .as_u64()
        .expect("the coordinator's pid") as i32;
    let before = a.ready().clone();
    if let Some(supervisor) = before.supervisor_pid {
        assert_ne!(supervisor, before.pid, "the supervisor is another process");
    } else {
        // The contract's ready file names the supervisor when there is one; the
        // child can only name it if the supervisor tells it, so this is a note
        // rather than a failure — the restart below is what proves it exists.
        println!("{NOTE} the ready file names no supervisor: {before:?}");
    }
    unsafe { libc::kill(child, libc::SIGKILL) };
    println!("{NOTE} killed {} (epoch {})", child, before.epoch);

    // --- it comes back, and it comes back as itself -----------------------------
    let restarted: ReadyFile = wait_for(deadline, "the supervisor to restart it", || {
        let ready = fixture.read_ready_of(&tab_a)?;
        (ready.epoch != before.epoch).then_some(ready)
    });
    let argv = Fixture::command_line(restarted.pid);
    // CONTRACT §1: the restart names the exact journal the child died with. A
    // bare `--resume` resolves to the newest journal in the folder, which with
    // two tabs in one folder is the *other* tab's — the E1 bug this proof exists
    // for.
    assert!(
        proofs::same_path(
            std::path::Path::new(&restarted.session.path),
            std::path::Path::new(&a_path)
        ),
        "the restarted child serves the session it died with, not another: it \
         resumed {}, this tab's journal was {a_path}, and it was started as {argv}",
        restarted.session.path
    );
    assert!(
        argv.contains(&format!("--resume {a_path}")),
        "the restart names that exact journal: {argv}"
    );
    assert!(!argv.contains(&b_path), "and never the other tab's: {argv}");
    // §1: the ready file is rewritten after every restart, and says how many.
    assert!(
        restarted.restarts >= 1,
        "the ready file counts the restart: {restarted:?}"
    );
    println!("{NOTE} restarted after {:?}: {}", started.elapsed(), argv);

    // --- and the conversation it had is still there ------------------------------
    let client = Fixture::client_of(&restarted);
    let seeded = snapshot(&client, &["session"], 200);
    let mut watcher = Watcher::start(
        &client,
        &["session"],
        Some(swarm_client::Cursor::new(
            restarted.epoch.clone(),
            seeded["seq"].as_u64().unwrap(),
        )),
    );
    watcher.reseed(&seeded);
    let items = watcher.mirror.items("session").to_vec();
    assert!(
        items
            .iter()
            .any(|item| item["kind"] == "user" && item["text"].as_str() == Some(FIRST)),
        "the turn it had answered is still in the session: {:?}",
        watcher.mirror.kinds("session")
    );
    assert!(
        !items
            .iter()
            .any(|item| item["text"].as_str() == Some(OTHER)),
        "and the other tab's turn is not: {items:?}"
    );

    // --- the other tab was not touched -------------------------------------------
    let b_after = fixture
        .read_ready_of(&tab_b)
        .expect("B's ready file is still there");
    assert_eq!(
        b_after.epoch,
        b.ready().epoch,
        "B was not restarted, renamed or resumed"
    );

    let stopped_a = a.shutdown().expect("the ladder ran");
    let stopped_b = b.shutdown().expect("the ladder ran");
    println!(
        "{NOTE} shutdown: {stopped_a:?} / {stopped_b:?} (t04 in {:?})",
        started.elapsed()
    );
    let (pid_a, pid_b) = (restarted.pid, b.ready().pid);
    wait_for(deadline, "both children gone", || {
        (!swarm_client::process_alive(pid_a) && !swarm_client::process_alive(pid_b)).then_some(())
    });
}

/// Send one turn and wait for the answer to land, so the journal really holds it.
fn answer(server: &swarm_client::Server, text: &str) {
    let deadline = deadline_after(WAIT);
    let client = server.client().clone();
    let seeded = snapshot(&client, &["session"], 200);
    let mut watcher = Watcher::start(
        &client,
        &["session"],
        Some(swarm_client::Cursor::new(
            server.ready().epoch.clone(),
            seeded["seq"].as_u64().unwrap(),
        )),
    );
    watcher.reseed(&seeded);
    send(&client, text);
    watcher.wait_mirror(deadline, "the answer", |mirror| {
        mirror
            .items("session")
            .iter()
            .any(|item| item["kind"] == "assistant" && item["status"] == "final")
    });
}
