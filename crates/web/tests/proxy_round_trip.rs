//! The proxy round trip, against a real `evo-agent`: `evo-web` on one port, a
//! tab opened through its own API, and the tab's protocol reached through
//! `/api/tabs/<id>/…` — the path a browser takes, and the one part of the
//! frontend that `scripts/web_check.sh` cannot reach (it proves the UI boots, but
//! opens no session).
//!
//! # What this proves, and what it does not
//!
//! Proven: `POST /api/tabs` opens a tab and returns its id; `GET …/snapshot`
//! returns a real snapshot whose topics carry the running session's state;
//! `POST …/ops` returns `ok:true` with a result the tab produced; `DELETE` stops
//! the child (its pid is gone) and the tab answers 404 afterwards; and the tab's
//! 64-character bearer token appears in none of those replies.
//!
//! Not proven here: `/stream` (SSE through the proxy), `/items` paging,
//! `/media`, `/catalog`, resume, and anything about the frontend's own
//! JavaScript. Each is a separate claim; none is made by this test.
//!
//! # Skipping
//!
//! Skipped unless `EVO_AGENT_BIN` (or `EVO_SWARM_BIN`) names a build, the same
//! way `crates/tab_engine/tests/real_tab.rs` and
//! `crates/swarm_client/tests/real_server.rs` skip — so an ordinary
//! `cargo test -p evo-web` neither needs the binary nor fails without it:
//!
//! ```sh
//! EVO_AGENT_BIN=/usr/local/bin/evo-agent \
//!   cargo test -p evo-web --test proxy_round_trip -- --nocapture
//! ```
//!
//! The child runs in a throwaway `HOME`, so a real provider is never called and
//! the real `~/.evo` is never written. Unlike the other real-binary tests this
//! one does not demand `EVO_TEST_HOME`: the proxy round trip needs no model to
//! answer (it opens a tab, reads its snapshot and sets a goal), so a temp home of
//! this test's own is enough — and it is still not the real one.

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
            // A path that does not exist is a misconfigured run, not a missing
            // one: `EVO_AGENT_BIN=/nonexistent` must skip, not fail.
            if path.is_file() {
                return Some(path);
            }
            eprintln!(
                "proxy_round_trip: {name}={} is not a file — skipping",
                path.display()
            );
            return None;
        }
    }
    eprintln!("proxy_round_trip: no EVO_AGENT_BIN/EVO_SWARM_BIN — skipping");
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
}

impl Web {
    /// Start `evo-web` serving `dist`, rooted in `root`, running `bin`.
    ///
    /// The port is asked of the OS first and then handed to `evo-web`, so the
    /// client knows where to knock without parsing the server's own ready line.
    /// The gap between releasing it and the server binding it is real but
    /// harmless: a collision makes the server fail to start, which fails the
    /// test loudly rather than passing it quietly.
    fn start(bin: &Path, dist: &Path, root: &Path, home: &Path) -> Web {
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_evo-web"))
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
            // The child inherits this, so the tab's own evo-agent runs in a
            // throwaway home rather than the real one.
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
            if let Some(status) = child.try_wait().expect("the child is checkable") {
                let mut out = String::new();
                use std::io::Read;
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut out);
                }
                panic!("evo-web exited ({status}) before it answered /api/health:\n{out}");
            }
            assert!(
                Instant::now() < deadline,
                "evo-web never answered /api/health on port {port}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        Web { child, client }
    }

    fn get(&self, path: &str) -> (u16, Value, String) {
        let reply = self
            .client
            .get(path)
            .unwrap_or_else(|e| panic!("GET {path} failed: {e}"));
        decode(reply)
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value, String) {
        let reply = self
            .client
            .post(path, body)
            .unwrap_or_else(|e| panic!("POST {path} failed: {e}"));
        decode(reply)
    }

    fn delete(&self, path: &str) -> (u16, Value, String) {
        let reply = self
            .client
            .request("DELETE", path, None)
            .unwrap_or_else(|e| panic!("DELETE {path} failed: {e}"));
        decode(reply)
    }
}

/// Status, parsed body (or `Null` for a body that is not JSON), raw text.
///
/// The raw text is kept because the token check is a substring search over what
/// actually went over the wire — parsing first could hide a leak in a field the
/// parser drops.
fn decode(reply: swarm_client::HttpResponse) -> (u16, Value, String) {
    let text = reply.text();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (reply.status, value, text)
}

impl Drop for Web {
    fn drop(&mut self) {
        // The child is `evo-web`; killing it does not stop the tabs it started,
        // so the test's own `DELETE` is what stops those. This is the belt to
        // that pair of braces, for the case where the test failed early.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("the bound address").port();
    drop(listener);
    port
}

/// The token the tab's serve process wrote in its ready file — the secret that
/// must never reach a caller of this server.
fn tab_token(root: &Path, id: &str) -> Option<String> {
    let ready = root.join("tabs").join(id).join("ready.json");
    let text = std::fs::read_to_string(ready).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("token")?.as_str().map(str::to_owned)
}

/// Whether the tab's process is still there — `swarm_client`'s own probe, the
/// one the desktop app uses, so this test and the app agree on what "running"
/// means.
fn alive(pid: u32) -> bool {
    swarm_client::process_alive(pid)
}

#[test]
fn the_proxy_round_trip_reaches_a_real_tab() {
    let Some(bin) = real_bin() else {
        return;
    };

    let scratch = Scratch::new("proxy");
    let dist = scratch.path().join("dist");
    let root = scratch.path().join("root");
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    for dir in [&dist, &root, &home, &work] {
        std::fs::create_dir_all(dir).expect("a scratch directory");
    }
    // A page is all `evo-web` wants in its dist; this test is not about the UI.
    std::fs::write(dist.join("index.html"), "<!doctype html><title>t</title>").expect("the page");

    let web = Web::start(&bin, &dist, &root, &home);

    // --- open a tab -------------------------------------------------------
    let (status, opened, text) = web.post(
        "/api/tabs",
        &json!({
            "folder": work.to_string_lossy(),
            "program": "agent",
        }),
    );
    assert_eq!(status, 201, "opening a tab: {text}");
    let id = opened["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in the reply: {text}"))
        .to_owned();
    assert_eq!(
        opened["state"], "running",
        "the tab did not come up running: {text}"
    );
    let pid = opened["pid"].as_u64().expect("a pid") as u32;
    assert!(alive(pid), "the tab's process {pid} is not running");

    // The tab's own secret, read from where its serve process wrote it. Every
    // reply below is checked against this.
    let token = tab_token(&root, &id).expect("the tab wrote a ready file with a token");
    assert_eq!(token.len(), 64, "a serve token is 64 characters");
    assert!(
        !text.contains(&token),
        "the token leaked in the POST /api/tabs reply"
    );

    // --- the snapshot, through the proxy ----------------------------------
    let (status, snapshot, text) = web.get(&format!("/api/tabs/{id}/snapshot"));
    assert_eq!(status, 200, "reading the snapshot: {text}");
    assert!(
        !text.contains(&token),
        "the token leaked in the snapshot reply"
    );
    // A 200 carrying an error is a failure: assert on the body, not the status.
    assert!(
        snapshot.get("error").is_none(),
        "the snapshot reply carried an error: {text}"
    );
    let epoch = snapshot["epoch"]
        .as_str()
        .unwrap_or_else(|| panic!("no epoch in the snapshot: {text}"));
    assert_eq!(epoch.len(), 8, "an epoch is eight hex characters: {epoch}");
    assert!(
        snapshot["seq"].as_u64().is_some(),
        "no seq in the snapshot: {text}"
    );
    // The topics: the session's, which is what a tab always reads. An agent has
    // no lanes, so `swarm` and `lane:*` carry nothing — but `session` is the one
    // that must be there, and it must carry the running model's state, which is
    // the proof that this is a live session and not an empty shell.
    let topics = snapshot["topics"]
        .as_object()
        .unwrap_or_else(|| panic!("no topics in the snapshot: {text}"));
    assert!(
        topics.contains_key("session"),
        "the snapshot carried no session topic: {text}"
    );
    let session = &topics["session"];
    assert!(
        session["state"]["model"].is_object(),
        "the session topic carried no model state: {text}"
    );
    assert!(
        session["items"].is_array(),
        "the session topic carried no items: {text}"
    );

    // --- an op, through the proxy -----------------------------------------
    let (status, reply, text) = web.post(
        &format!("/api/tabs/{id}/ops"),
        &json!({ "op": "goal.set", "args": { "objective": "proxy round trip" } }),
    );
    assert_eq!(status, 200, "posting an op: {text}");
    assert!(!text.contains(&token), "the token leaked in the op reply");
    assert_eq!(reply["ok"], true, "the op was refused: {text}");
    assert!(
        reply["rid"].as_str().is_some_and(|r| !r.is_empty()),
        "the reply carried no rid: {text}"
    );
    // The effect, not just the acknowledgement: the goal the tab now holds.
    assert_eq!(
        reply["result"]["goal"]["objective"], "proxy round trip",
        "the op was acknowledged but its result does not carry the goal: {text}"
    );

    // A refused op is a 200-or-4xx with `ok:false` — the point here is that the
    // proxy does not turn a refusal into a success. An op name the server does
    // not know is the cheapest refusal to provoke.
    let (status, refused, text) = web.post(
        &format!("/api/tabs/{id}/ops"),
        &json!({ "op": "not.a.real.op", "args": {} }),
    );
    assert!(
        (200..300).contains(&status) || (400..500).contains(&status),
        "an unknown op returned {status}: {text}"
    );
    assert_ne!(
        refused["ok"], true,
        "an unknown op was reported as ok:true: {text}"
    );

    // --- the tab's own view, and the list --------------------------------
    let (status, view, text) = web.get(&format!("/api/tabs/{id}"));
    assert_eq!(status, 200, "reading the tab: {text}");
    assert_eq!(view["id"], id.as_str());
    assert_eq!(view["state"], "running");
    assert!(
        view.get("token").is_none(),
        "the tab's view carries a token field: {text}"
    );
    assert!(!text.contains(&token), "the token leaked in the tab view");

    let (status, list, text) = web.get("/api/tabs");
    assert_eq!(status, 200, "listing tabs: {text}");
    // The list is an object with a `tabs` array, not a bare array — the shape the
    // frontend's own client reads.
    assert!(
        list["tabs"]
            .as_array()
            .is_some_and(|tabs| tabs.iter().any(|tab| tab["id"] == id.as_str())),
        "the opened tab is not in the list: {text}"
    );

    // --- stop it ----------------------------------------------------------
    let (status, stopped, text) = web.delete(&format!("/api/tabs/{id}"));
    assert_eq!(status, 200, "stopping the tab: {text}");
    assert_eq!(stopped["id"], id.as_str());
    assert!(!text.contains(&token), "the token leaked in the stop reply");

    // The child is really gone, and the tab is really forgotten: a stopped tab
    // that still answers would be the failure this asserts against.
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "the tab's process {pid} outlived its DELETE");

    let (status, _gone, text) = web.get(&format!("/api/tabs/{id}"));
    assert_eq!(status, 404, "a stopped tab still answers: {status} {text}");
    assert!(!text.contains(&token), "the token leaked in the 404 body");

    let (status, after, text) = web.get(&format!("/api/tabs/{id}/snapshot"));
    assert_eq!(
        status, 404,
        "the proxy still answers for a stopped tab: {status} {text}"
    );
    assert!(
        after.get("error").is_some(),
        "a stopped tab's snapshot is not an error: {text}"
    );
    assert!(!text.contains(&token), "the token leaked in the 404 body");
}

/// The proxy against a binary that is not there: the server must refuse the tab
/// rather than claim one, and the refusal must not carry a token.
///
/// This is the one path that runs without a real binary, so it always runs.
#[test]
fn a_tab_whose_binary_is_missing_is_refused_not_faked() {
    let scratch = Scratch::new("proxy-nobin");
    let dist = scratch.path().join("dist");
    let root = scratch.path().join("root");
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    for dir in [&dist, &root, &home, &work] {
        std::fs::create_dir_all(dir).expect("a scratch directory");
    }
    std::fs::write(dist.join("index.html"), "<!doctype html><title>t</title>").expect("the page");

    let missing = scratch.path().join("not-an-evo-agent");
    let web = Web::start(&missing, &dist, &root, &home);

    let (status, body, text) = web.post(
        "/api/tabs",
        &json!({
            "folder": work.to_string_lossy(),
            "program": "agent",
        }),
    );
    // A tab that cannot start is a failure the caller sees, never a 201 with a
    // tab that is quietly not running.
    assert!(
        !(200..300).contains(&status),
        "a missing binary produced a success ({status}): {text}"
    );
    assert!(
        body.get("error").is_some(),
        "the refusal carried no error: {text}"
    );
    assert!(
        text.contains("not-an-evo-agent"),
        "the refusal does not name the binary that is missing: {text}"
    );
}
