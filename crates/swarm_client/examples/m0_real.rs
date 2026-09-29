//! m0_real — the M0 proof, against the **installed** `evo-swarm` and the user's
//! real configuration (docs/PROMPT.md §12, §3): a fresh temp folder, the default
//! model (no `--model`), one worker, a prompt that makes the coordinator delegate
//! one tiny task to lane 1, the whole event stream counted, then `/transcript`,
//! `/state`, `/registry`, `/lanes`, `/lanes/1/transcript`, `POST /shutdown`, and a
//! check that every process the swarm started is gone.
//!
//! ```text
//! CARGO_TARGET_DIR=target/real \
//!   cargo run -p swarm_client --example m0_real -- /tmp/m0-real-results
//! ```
//!
//! Arguments: `[results_dir] [prompt]`. The results directory is created if
//! missing; the run's working folder is a fresh `project/` inside it, and every
//! reply the proof quotes is written there as JSON — `state.json`,
//! `registry.json`, `transcript.json`, `lanes.json`, `lane1-transcript.json`,
//! `usage.jsonl` — so `real_readout` can render the §7.3 line from the same data.
//!
//! A delegated run settles before its lane is done (§12: `settled` is the
//! coordinator idle again), so after `settled` the proof keeps reading the stream
//! and polling `/lanes` until lane 1 has been busy and gone idle again, or a cap
//! is reached — otherwise lane 1's transcript would be fetched mid-task.
//!
//! Environment: `EVO_SWARM_BIN`, `EVO_AGENT_BIN` (the installed binaries),
//! `EVO_M0_TIMEOUT_SECS` (how long to wait for `settled`, 900 by default),
//! `EVO_M0_LANE_TIMEOUT_SECS` (how long to wait for the lane afterwards, 300).
//! No token is ever printed, and a reply that echoes one is redacted.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use swarm_client::{
    default_agent_bin, default_swarm_bin, process_alive, redact, EventStream, Server, ServerConfig,
    StreamConfig, StreamMsg, StreamTarget,
};

/// One small delegation, so the proof covers lane 1 without paying for much.
const DEFAULT_PROMPT: &str =
    "Delegate to a lane: create hello.txt containing 'hi' in this folder, \
     then tell me when it's done. Keep it brief.";

/// Lane states that mean the lane still has the task.
const BUSY: [&str; 3] = ["working", "compacting", "starting"];

/// Everything the proof collects from the event stream.
#[derive(Default)]
struct Tally {
    kinds: BTreeMap<String, u64>,
    /// Every completed assistant message of the coordinator, in order.
    assistant: Vec<String>,
    /// The message being streamed.
    current: String,
    /// `report` events, verbatim.
    reports: Vec<String>,
    /// Each `message-end`'s `usage`, one JSON object per line.
    usage: Vec<String>,
    /// The first `settled` outcome.
    settled: Option<String>,
}

impl Tally {
    fn event(&mut self, kind: &str, data: &Value) {
        *self.kinds.entry(kind.to_string()).or_insert(0) += 1;
        match kind {
            "text-delta" => {
                let text = data.get("text").and_then(Value::as_str).unwrap_or("");
                self.current.push_str(text);
                print!("{text}");
            }
            "message-end" => {
                if !self.current.trim().is_empty() {
                    self.assistant.push(std::mem::take(&mut self.current));
                }
                if let Some(usage) = data.get("usage") {
                    self.usage.push(usage.to_string());
                }
                println!(
                    "\n— message-end stop_reason={} usage={}",
                    data.get("stop_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                    data.get("usage")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "none".into())
                );
            }
            "tool-call-start" => println!(
                "\n◆ tool {} {}",
                data.get("name").and_then(Value::as_str).unwrap_or("?"),
                data.get("arguments")
                    .map(Value::to_string)
                    .unwrap_or_default()
            ),
            "report" => {
                let line = format!("report: {}", redact(&data.to_string()));
                println!("\n{line}");
                self.reports.push(line);
            }
            "output" => println!(
                "\n{}",
                data.get("text").and_then(Value::as_str).unwrap_or("")
            ),
            "settled" => {
                let outcome = data.get("outcome").and_then(Value::as_str).unwrap_or("?");
                println!("\nsettled:            {} at {}", outcome, utc_now());
                self.settled = self.settled.take().or_else(|| Some(outcome.to_string()));
            }
            _ => {}
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("m0_real: {}", redact(&error.to_string()));
        if let Some(tail) = error.log_tail() {
            eprintln!("--- the server's log ---\n{}", redact(tail));
        }
        std::process::exit(1);
    }
}

fn run() -> swarm_client::Result<()> {
    let mut args = std::env::args().skip(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let results = PathBuf::from(args.next().unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!("evo-desktop-m0-real-{nanos}"))
            .display()
            .to_string()
    }));
    let folder = results.join("project");
    fs::create_dir_all(&folder)?;
    let tab_dir = results.join("tab");
    fs::create_dir_all(&tab_dir)?;
    let prompt = args.next().unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
    let timeout = secs_env("EVO_M0_TIMEOUT_SECS", 900);
    let lane_timeout = secs_env("EVO_M0_LANE_TIMEOUT_SECS", 300);
    let folder = folder.canonicalize()?;

    println!("=== m0_real ===");
    println!("started_utc:        {}", utc_now());
    println!("bin:                {}", default_swarm_bin().display());
    println!("evo (lanes):        {}", default_agent_bin().display());
    println!("folder:             {}", folder.display());
    println!("results:            {}", results.display());
    println!("workers:            1");
    println!("model:              (default: none passed)");
    println!("prompt:             {prompt}");

    let config = ServerConfig::swarm(default_swarm_bin(), &folder, &tab_dir)
        .with_evo(default_agent_bin())
        .with_workers(1);
    let mut server = Server::start(&config)?;
    let health = server.health().clone();
    println!(
        "ready_utc:          {}\nready:              {} {} pid {} port {} features {:?}",
        utc_now(),
        health.name.as_deref().unwrap_or("?"),
        health.version.as_deref().unwrap_or("?"),
        health.pid,
        server.port(),
        health.features
    );
    let client = server.client().clone();
    let seeded = client.state()?;
    println!(
        "seeded_state_utc:   {}\nseeded_state:       status={} model={:?} provider={:?} thinking={:?} context={}/{:?}",
        utc_now(),
        seeded.typed.status,
        seeded.typed.model,
        seeded.typed.provider,
        seeded.typed.thinking,
        seeded.typed.context_tokens,
        seeded.typed.context_window
    );

    let stream = EventStream::start(
        StreamTarget::coordinator(&client, Some(0)),
        StreamConfig::default(),
    );
    let deadline = Instant::now() + timeout;
    let reply = client.prompt(&prompt)?;
    println!(
        "prompt_utc:         {}\nprompt_reply:       ok={} queued={:?}",
        utc_now(),
        reply.ok,
        reply.data
    );

    let mut tally = Tally::default();
    while tally.settled.is_none() && Instant::now() < deadline {
        match next(&stream, deadline) {
            Some(StreamMsg::Event { kind, data, .. }) => tally.event(&kind, &data),
            Some(_) => {}
            None => break,
        }
    }

    // The coordinator is idle again, but lane 1 may still be running: keep reading
    // (its events arrive on the coordinator's stream too) and poll /lanes until it
    // has been busy and stops being busy, so lane 1's transcript is final.
    let lane_deadline = Instant::now() + lane_timeout;
    let mut lane_seen_busy = false;
    let mut lane_states: Vec<String> = Vec::new();
    while Instant::now() < lane_deadline {
        while let Some(message) = stream.try_recv() {
            if let StreamMsg::Event { kind, data, .. } = message {
                tally.event(&kind, &data);
            }
        }
        let state = client
            .lanes()
            .ok()
            .and_then(|lanes| lane_state(&lanes.raw, 1))
            .unwrap_or_else(|| "?".to_string());
        if lane_states.last() != Some(&state) {
            println!("lane1_state:        {state} at {}", utc_now());
            if BUSY.contains(&state.as_str()) {
                lane_seen_busy = true;
            } else if lane_seen_busy {
                lane_states.push(state);
                break;
            }
            lane_states.push(state);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    println!(
        "lane1_done_utc:     {} (busy seen: {lane_seen_busy}, states: {})",
        utc_now(),
        lane_states.join(" → ")
    );
    drop(stream);

    // What the UI reads after a run: the reply, the state, the catalog, the lanes
    // and lane 1's own transcript. One failing route must not hide the others.
    let mut failures: Vec<String> = Vec::new();
    for (path, file) in [
        ("/transcript", "transcript.json"),
        ("/state", "state.json"),
        ("/registry", "registry.json"),
        ("/lanes", "lanes.json"),
        ("/lanes/1/transcript", "lane1-transcript.json"),
    ] {
        match client.get_raw(path) {
            Ok(body) => {
                fs::write(results.join(file), serde_json::to_string_pretty(&body)?)?;
                println!(
                    "route {path:<20} 200 ({} bytes) → {file}",
                    body.to_string().len()
                );
            }
            Err(error) => {
                // Printed as the client's own error: swarm_client redacts a
                // refusal where it is built, so this is already safe to show.
                let text = error.to_string();
                println!("route {path:<20} FAILED: {text}");
                failures.push(format!("{path}: {text}"));
            }
        }
    }
    fs::write(results.join("usage.jsonl"), tally.usage.join("\n") + "\n")?;

    let read = |file: &str| -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(results.join(file)).ok()?).ok()
    };
    if let Some(state) = read("state.json") {
        println!(
            "state:              status={} model={:?} provider={:?} thinking={:?} context={}/{:?} turn={} todos={}",
            state.get("status").and_then(Value::as_str).unwrap_or("?"),
            state.get("model").and_then(Value::as_str),
            state.get("provider").and_then(Value::as_str),
            state.get("thinking").and_then(Value::as_str),
            state.get("context_tokens").and_then(Value::as_u64).unwrap_or(0),
            state.get("context_window").and_then(Value::as_u64),
            state.get("turn").and_then(Value::as_u64).unwrap_or(0),
            state.get("todos").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
        );
    }
    for (file, key, what) in [
        ("transcript.json", "messages", "coordinator transcript"),
        ("lane1-transcript.json", "messages", "lane 1 transcript"),
        ("registry.json", "models", "registry models"),
        ("lanes.json", "lanes", "lanes"),
    ] {
        if let Some(body) = read(file) {
            println!(
                "{:<19} {} {} item(s)",
                file,
                body.get(key)
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                what
            );
        }
    }
    println!("event_kinds ({} total):", tally.kinds.values().sum::<u64>());
    for (kind, count) in &tally.kinds {
        println!("  {kind} {count}");
    }
    println!(
        "settled_outcome:    {}",
        tally.settled.as_deref().unwrap_or("(none)")
    );
    println!("assistant_messages: {}", tally.assistant.len());
    if let Some(last) = tally.assistant.last() {
        println!("--- final coordinator text ---\n{last}\n--- end ---");
    }
    if !tally.reports.is_empty() {
        println!("lane_reports:       {}", tally.reports.len());
        for report in &tally.reports {
            println!("  {report}");
        }
    }
    if let Some(text) = read("lane1-transcript.json").and_then(|body| last_assistant_text(&body)) {
        println!("--- lane 1's final text ---\n{text}\n--- end ---");
    }

    // Every process the swarm started, so a shutdown can be checked for leaks.
    let before = descendants(server.pid());
    println!(
        "processes_before:   coordinator {} descendants {:?}",
        server.pid(),
        before
    );
    let report = server.shutdown()?;
    println!(
        "shutdown_utc:       {}\nshutdown:           {:?} exit {:?} waited {:?}",
        utc_now(),
        report.outcome,
        report.exit_code,
        report.waited
    );
    std::thread::sleep(Duration::from_millis(500));
    let alive: Vec<u32> = std::iter::once(server.pid())
        .chain(before.iter().copied())
        .filter(|pid| process_alive(*pid))
        .collect();
    println!("processes_after:    still alive {:?}", alive);
    println!("hello_txt:          {}", folder.join("hello.txt").exists());
    if let Ok(text) = fs::read_to_string(folder.join("hello.txt")) {
        println!("hello_txt_content:  {:?}", text);
    }
    println!("failures:           {}", failures.len());
    println!("log:                {}", config.log_path.display());
    println!("finished_utc:       {}", utc_now());
    if !failures.is_empty() {
        std::process::exit(2);
    }
    Ok(())
}

/// One message, polling so the deadline holds.
fn next(stream: &EventStream, deadline: Instant) -> Option<StreamMsg> {
    loop {
        if let Some(message) = stream.try_recv() {
            return Some(message);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Lane `n`'s state in a `/lanes` body.
fn lane_state(body: &Value, n: u64) -> Option<String> {
    body.get("lanes")?
        .as_array()?
        .iter()
        .find(|lane| lane.get("n").and_then(Value::as_u64) == Some(n))?
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The text of a `/transcript` body's last assistant message.
fn last_assistant_text(body: &Value) -> Option<String> {
    let message = body
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))?;
    let text = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.trim().is_empty()).then_some(text)
}

/// Every process with `root` as an ancestor, from `ps`.
fn descendants(root: u32) -> Vec<u32> {
    let output = match Command::new("ps").args(["-axo", "pid=,ppid="]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    let table: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect();
    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        for (pid, ppid) in &table {
            if *ppid == parent && !found.contains(pid) {
                found.push(*pid);
                queue.push(*pid);
            }
        }
    }
    found
}

fn secs_env(name: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default),
    )
}

/// A UTC instant, from `date -u` — the proof's times are never naive local ones.
fn utc_now() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
