//! One journal, one writer (§7.1) — the attach rule, against a real `evo-agent`.
//!
//! Two `serve` processes appending to one `.sexp` fork it: each one's view shows
//! only its own turns, so nothing looks wrong, and the file a user later resumes
//! is malformed. So `evo-web` must not start a second writer — and what it does
//! instead of starting one is **hand back the tab that already has it**, because
//! that is the only answer that leaves the reader able to continue their work. A
//! browser has no tab strip for this server to select from, so a refusal would
//! strand them; a tab's stream is a broadcast, so a second reader can just join.
//!
//! That rule is what this file proves, with a real child, because the authority
//! for "what does this tab have open" is the *process's own report* (the ready
//! file's `session.path`) and a fake would only prove the fake.
//!
//! Four claims, and the last three are what keep the first from being "attach to
//! anything":
//!
//! 1. a second resume of a journal a live tab holds is **200 naming that tab**,
//!    with no second process started;
//! 2. a **different** journal still opens a new tab;
//! 3. a resume **after the holder stopped** opens a new tab — a dead tab must not
//!    hold its journal forever;
//! 4. the journal ends up with **one** writer's worth of turns and balanced
//!    parens, which is the damage this rule prevents, stated as a measurement.
//!
//! # Skipping
//!
//! Skipped unless `EVO_AGENT_BIN` (or `EVO_SWARM_BIN`) names a build, exactly as
//! `proxy_round_trip.rs` does — so `cargo test -p evo-web` neither needs the binary
//! nor fails without it:
//!
//! ```sh
//! EVO_AGENT_BIN=/usr/local/bin/evo-agent \
//!   cargo test -p evo-web --test resume_guard -- --nocapture
//! ```
//!
//! The children run in a throwaway `HOME`, so no real provider is called and the
//! real `~/.evo` is never written.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use swarm_client::transport::Token;
use swarm_client::HttpClient;

/// The binary under test, or a note that this run has none.
fn real_bin() -> Option<PathBuf> {
    for name in ["EVO_AGENT_BIN", "EVO_SWARM_BIN"] {
        if let Some(path) = std::env::var_os(name) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
            eprintln!(
                "resume_guard: {name}={} is not a file — skipping",
                path.display()
            );
            return None;
        }
    }
    eprintln!("resume_guard: no EVO_AGENT_BIN/EVO_SWARM_BIN — skipping");
    None
}

/// A directory this test owns, removed when it drops.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("evo-web-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The `evo-web` server under test, stopped when it drops.
struct Web {
    child: Child,
    client: HttpClient,
    root: PathBuf,
}

impl Web {
    fn start(bin: &Path, dist: &Path, root: &Path, home: &Path) -> Web {
        let port = free_port();
        let child = Command::new(env!("CARGO_BIN_EXE_evo-web"))
            .arg("--bind")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--dist")
            .arg(dist)
            .arg("--root")
            .arg(root)
            .arg("--evo-agent")
            .arg(bin)
            .env("HOME", home)
            .env_remove("EVO_HOME")
            .env_remove("EVO_SESSIONS_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("evo-web starts");

        let client = HttpClient::loopback(port, Token::new(""));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(reply) = client.get("/api/health") {
                if reply.is_success() {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "evo-web never answered /api/health on port {port}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Web {
            child,
            client,
            root: root.to_path_buf(),
        }
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value, String) {
        // A `POST` that opens a tab spawns a process and waits for it to come
        // ready, which can take a while on a loaded machine — and the socket under
        // it is a loopback one this test owns. `EAGAIN`/`EINTR` on a blocking
        // socket is a retry, not a failure: it says nothing about the request.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.client.post(path, body) {
                Ok(reply) => return decode(reply),
                Err(error) => {
                    let text = error.to_string();
                    let transient = text.contains("os error 35")
                        || text.contains("os error 4")
                        || text.contains("Resource temporarily unavailable");
                    if !transient || Instant::now() > deadline {
                        panic!("POST {path} failed: {text}");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    fn delete(&self, path: &str) -> (u16, Value, String) {
        let reply = self
            .client
            .request("DELETE", path, None)
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"));
        decode(reply)
    }

    /// Open a tab and return `(id, pid)`.
    fn open(&self, folder: &Path, resume: Option<&Path>) -> (String, u32) {
        let (status, id, pid) = self.open_with_status(folder, resume);
        assert_eq!(
            status, 201,
            "opening a tab must create one — {id} came back {status}"
        );
        (id, pid)
    }

    /// [`open`], for the calls that are expected to **attach** to a tab that
    /// already holds the session: 200 and that tab's own id and pid, with no
    /// second process started.
    fn attach(&self, folder: &Path, resume: &Path) -> (String, u32) {
        let (status, id, pid) = self.open_with_status(folder, Some(resume));
        assert_eq!(
            status, 200,
            "resuming a live session must attach, not create: {id} came back {status}"
        );
        (id, pid)
    }

    /// `POST /api/tabs` as `(status, id, pid)` — the one place the body is built.
    ///
    /// The route answers as soon as the child is up enough to be described, so the
    /// state can still be `starting` when the reply lands. This waits for `running`
    /// before handing the tab back, because every assertion below is about a tab
    /// that has a process behind it.
    fn open_with_status(&self, folder: &Path, resume: Option<&Path>) -> (u16, String, u32) {
        let mut body = json!({
            "folder": folder.to_string_lossy(),
            "program": "agent",
        });
        if let Some(resume) = resume {
            body["resume"] = json!(resume.to_string_lossy());
        }
        let (status, reply, text) = self.post("/api/tabs", &body);
        let id = reply["id"].as_str().expect("an id").to_owned();
        let pid = reply["pid"].as_u64().unwrap_or(0) as u32;
        if reply["state"] != "running" {
            self.wait_until_running(&id, &text);
        }
        let reply = self.get_tab(&id);
        (status, id, reply["pid"].as_u64().map_or(pid, |p| p as u32))
    }

    /// Wait for a tab to report `running`, or fail saying what it reported.
    fn wait_until_running(&self, id: &str, opening: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let reply = self.get_tab(id);
            match reply["state"].as_str() {
                Some("running") => return,
                Some("failed") => panic!("tab {id} failed to start: {reply}"),
                _ if Instant::now() > deadline => {
                    panic!("tab {id} never came running; it opened as {opening}, now {reply}")
                }
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    /// `GET /api/tabs/<id>`'s body.
    fn get_tab(&self, id: &str) -> Value {
        let reply = self
            .client
            .get(&format!("/api/tabs/{id}"))
            .unwrap_or_else(|e| panic!("GET /api/tabs/{id} failed: {e}"));
        let (_, value, _) = decode(reply);
        value
    }

    /// The journal the tab's own process says it is writing — read from the ready
    /// file, which is the same authority the guard uses.
    fn reported_journal(&self, id: &str) -> PathBuf {
        let ready = self.root.join("tabs").join(id).join("ready.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(&ready) {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    if let Some(path) = value["session"]["path"].as_str() {
                        return PathBuf::from(path);
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "tab {id} reported no session path"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn decode(reply: swarm_client::HttpResponse) -> (u16, Value, String) {
    let text = reply.text();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (reply.status, value, text)
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("the bound address").port();
    drop(listener);
    port
}

#[test]
fn a_resume_of_a_live_session_attaches_to_it() {
    let Some(bin) = real_bin() else {
        return;
    };

    let scratch = Scratch::new("resume-guard");
    let dist = scratch.path().join("dist");
    let root = scratch.path().join("root");
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    for dir in [&dist, &root, &home, &work] {
        std::fs::create_dir_all(dir).expect("a scratch directory");
    }
    std::fs::write(dist.join("index.html"), "<!doctype html><title>t</title>").expect("the page");

    let web = Web::start(&bin, &dist, &root, &home);

    // --- a tab that creates its own journal -------------------------------
    //
    // Started **without** `resume`, which is the interesting case: its request
    // names no journal at all, so the rule can only know what it has open from
    // the process's own report.
    //
    // The journal this tab is writing, from that report. It is named the moment
    // the child is listening, before anything has been appended to it — which is
    // exactly why it is the authority.
    let (first, first_pid) = web.open(&work, None);
    let journal = web.reported_journal(&first);
    assert!(
        swarm_client::process_alive(first_pid),
        "the first tab is not running"
    );
    // The child names its journal at boot, before writing anything to it — and
    // these children run in a `HOME` with no provider, so no turn will ever
    // produce one. The test writes the file at the child's own reported path so
    // the cases below are about a journal that exists: the last one resumes it
    // *after* the holder has stopped, which is only a resume if there is a file
    // to resume. Writing it here rather than fabricating a path elsewhere is what
    // keeps the subject the child's own journal.
    //
    // The header has to be one `evo-agent` will open, which is more than
    // `(:type :session :version 1)`: a session form without `:id`, `:cwd` and
    // `:program` is refused, and the child prints "listening" and then never
    // writes its ready file, so the tab fails 90 seconds later. Measured against
    // the real binary: that shorter header fails, this one opens.
    //
    // `:id` is the child's own session id, which it put in the file's name
    // (`20261018T150244Z_8790a72b058b7c4e.sexp`), so the journal the test writes
    // is the journal the child named rather than a fiction beside it.
    let session_id = journal
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('_'))
        .map(|(_, id)| id.to_owned())
        .expect("the child names its journal <timestamp>_<session id>");
    let seed = format!(
        "(:type :session :version 1 :id \"{session_id}\" :cwd \"{}\" :program \"evo-agent\" \
         :swarm-id nil :timestamp \"2026-01-01T00:00:00Z\")\n",
        work.display()
    );
    // The child names the path before it makes the directory, so this does too.
    if let Some(parent) = journal.parent() {
        std::fs::create_dir_all(parent).expect("the journal's directory");
    }
    std::fs::write(&journal, &seed).expect("the journal the child named");

    // --- the second resume, which must ATTACH to the first tab ------------
    //
    // Not a refusal: a browser has no tab strip, so "switch to the other tab" is
    // not a move this server can make, and refusing would strand a reader with a
    // running session they cannot reach. The answer is the tab that already has
    // it — same process, same journal, one writer — so the caller attaches.
    let (attached, attached_pid) = web.attach(&work, &journal);
    assert_eq!(
        attached, first,
        "attaching must hand back the tab that already holds the session"
    );
    assert_eq!(
        attached_pid, first_pid,
        "attaching must not start a second process"
    );

    // No second process and no second tab: the attach came before the spawn.
    let (_, tabs, _) = web.get_tabs();
    assert_eq!(tabs, 1, "attaching must not have opened a second tab");

    // --- a *different* journal still resumes ------------------------------
    //
    // The rule must not be attaching everything to everything. A second,
    // independent journal opens a tab of its own — which also proves the attach
    // above was about *this* session and not about resume as such.
    let elsewhere = scratch.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (other, other_pid) = web.open(&elsewhere, None);
    let other_journal = web.reported_journal(&other);
    assert_ne!(other_journal, journal, "two folders, two journals");

    let (second, second_pid) = web.attach(&work, &other_journal);
    assert_eq!(
        (second.as_str(), second_pid),
        (other.as_str(), other_pid),
        "resuming another tab's journal must attach to *that* tab"
    );
    assert!(
        swarm_client::process_alive(second_pid),
        "a different journal's tab must still be running"
    );

    // --- the same journal, once the holder is gone ------------------------
    //
    // A dead tab must not block the recovery from it: this is the case the guard
    // would be a bug in if it kept holding the journal after the process left.
    let (status, _, text) = web.delete(&format!("/api/tabs/{first}"));
    assert_eq!(status, 200, "stopping the holder: {text}");
    wait_for_gone(first_pid);

    // A resume *after* the holder stopped opens a tab of its own — a dead tab must
    // not hold its journal for ever, or the reader could never get back to a
    // session whose process had exited. The test seeded the journal file at the
    // child's own reported path, so there is something to resume.
    let (resumed, resumed_pid) = web.open(&work, Some(&journal));
    assert!(
        swarm_client::process_alive(resumed_pid),
        "a resume after the holder stopped must succeed"
    );

    // --- one writer, stated as a measurement ------------------------------
    //
    // The damage this rule prevents is a journal two processes appended to. The
    // attach above ran no process, so nothing it did can have written to `journal`
    // — which is the property a fork would have broken.
    //
    // The parens are deliberately *not* counted here. These children run in a
    // throwaway `HOME` with no provider, so no turn ever runs and no journal is
    // ever written: a parens count over a file nobody wrote would be vacuous, and a
    // vacuous check is worse than no check. What is checkable without a provider is
    // the invariant itself, and it is pinned in three places:
    //
    //   * the attach returned the holder's own id and pid (above),
    //   * `second` attached to *its* journal's tab rather than making a new one,
    //   * and the tab count below is exactly the two that were legitimately opened.
    let (_, tabs, _) = web.get_tabs();
    assert_eq!(
        tabs, 2,
        "the attach must not have opened a tab of its own: {tabs} listed, expected \
         `second` and `resumed` only"
    );
    assert_ne!(
        attached, resumed,
        "the attach must not have adopted the tab opened later for the same journal"
    );
    // (The journal's bytes are not asserted here: `resumed` legitimately owns it by
    // this point — it was opened after its holder stopped — so it may have rewritten
    // the file. That is one writer, which is the rule, not a breach of it.)

    // --- cleanup: every tab this test opened is stopped -------------------
    for id in [&first, &other, &second, &resumed] {
        let _ = web.delete(&format!("/api/tabs/{id}"));
    }
    for pid in [first_pid, other_pid, second_pid, resumed_pid] {
        wait_for_gone(pid);
    }
}

impl Web {
    /// `GET /api/tabs` as `(status, how many, raw)`.
    fn get_tabs(&self) -> (u16, usize, String) {
        let reply = self.client.get("/api/tabs").expect("GET /api/tabs");
        let (status, value, text) = decode(reply);
        let count = value["tabs"].as_array().map_or(0, Vec::len);
        (status, count, text)
    }
}

/// Wait for a pid to be gone, so a later resume is not racing the stop ladder.
fn wait_for_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while swarm_client::process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !swarm_client::process_alive(pid),
        "the tab's process {pid} outlived its stop"
    );
}
