//! m0_real — the M0 proof, against the **installed** `evo-swarm` and the user's
//! real configuration (docs/PROMPT.md §12, §3): a fresh temp folder, the default
//! model (no `--model`), one worker, a prompt that makes the coordinator delegate
//! one tiny task, the whole event stream counted, then `/transcript`, `/state`,
//! `/registry`, `/lanes`, `/lanes/1/transcript`, `POST /shutdown`, and a check
//! that every process the swarm started is gone.
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
//! Environment: `EVO_SWARM_BIN`, `EVO_AGENT_BIN` (the installed binaries),
//! `EVO_M0_TIMEOUT_SECS` (how long to wait for `settled`, 900 by default). The
//! token is never printed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use swarm_client::{
    EventStream, Server, ServerConfig, StreamConfig, StreamMsg, StreamTarget, default_agent_bin,
    default_swarm_bin, process_alive,
};

/// One small delegation, so the proof covers lane 1 without paying for much.
const DEFAULT_PROMPT: &str = "Delegate to a lane: create hello.txt containing 'hi' in this folder, \
     then tell me when it's done. Keep it brief.";

fn main() {
    if let Err(error) = run() {
        eprintln!("m0_real: {error}");
        if let Some(tail) = error.log_tail() {
            eprintln!("--- the server's log ---\n{tail}");
        }
        std::process::exit(1);
    }
}

fn run() -> swarm_client::Result<()> {
    let mut args = std::env::args().skip(1);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let results = PathBuf::from(
        args.next()
            .unwrap_or_else(|| std::env::temp_dir().join(format!("evo-desktop-m0-real-{nanos}")).display().to_string()),
    );
    let folder = results.join("project");
    fs::create_dir_all(&folder)?;
    let tab_dir = results.join("tab");
    fs::create_dir_all(&tab_dir)?;
    let prompt = args.next().unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
    let timeout = std::env::var("EVO_M0_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(900));
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
        "seeded_state:       status={} model={:?} provider={:?}",
        seeded.typed.status, seeded.typed.model, seeded.typed.provider
    );

    let stream = EventStream::start(StreamTarget::coordinator(&client, Some(0)), StreamConfig::default());
    let deadline = Instant::now() + timeout;
    let reply = client.prompt(&prompt)?;
    println!("prompt_utc:         {}\nprompt_reply:       ok={} queued={:?}", utc_now(), reply.ok, reply.data);

    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut assistant: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut reports: Vec<String> = Vec::new();
    let mut usage_lines: Vec<String> = Vec::new();
    let mut settled: Option<String> = None;
    while settled.is_none() {
        if Instant::now() >= deadline {
            println!("gave up:            no settled event within {}s", timeout.as_secs());
            break;
        }
        match next(&stream, deadline) {
            Some(StreamMsg::Event { kind, data, .. }) => {
                *kinds.entry(kind.clone()).or_insert(0) += 1;
                match kind.as_str() {
                    "text-delta" => {
                        let text = data.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        current.push_str(text);
                        print!("{text}");
                    }
                    "message-end" => {
                        if !current.trim().is_empty() {
                            assistant.push(std::mem::take(&mut current));
                        }
                        if let Some(usage) = data.get("usage") {
                            usage_lines.push(usage.to_string());
                        }
                        println!(
                            "\n— message-end stop_reason={} usage={}",
                            data.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("?"),
                            data.get("usage").map(|u| u.to_string()).unwrap_or_else(|| "none".into())
                        );
                    }
                    "tool-call-start" => println!(
                        "\n◆ tool {} {}",
                        data.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                        data.get("arguments").map(|a| a.to_string()).unwrap_or_default()
                    ),
                    "report" => {
                        let line = format!("report: {}", data);
                        println!("\n{}", line);
                        reports.push(line);
                    }
                    "output" => println!(
                        "\n{}",
                        data.get("text").and_then(|t| t.as_str()).unwrap_or("")
                    ),
                    "settled" => {
                        let outcome = data.get("outcome").and_then(|o| o.as_str()).unwrap_or("?");
                        println!("\nsettled:            {} at {}", outcome, utc_now());
                        settled = Some(outcome.to_string());
                    }
                    _ => {}
                }
            }
            Some(_) => {}
            None => break,
        }
    }
    drop(stream);

    // What the UI reads after a run: the whole reply, the state, the catalog, the
    // lanes and lane 1's own transcript.
    let transcript = client.transcript(None)?;
    write_json(&results.join("transcript.json"), &transcript.raw)?;
    let state = client.state()?;
    write_json(&results.join("state.json"), &state.raw)?;
    let registry = client.registry()?;
    write_json(&results.join("registry.json"), &registry.raw)?;
    let lanes = client.lanes()?;
    write_json(&results.join("lanes.json"), &lanes.raw)?;
    let lane1 = client.lane_transcript(1, None)?;
    write_json(&results.join("lane1-transcript.json"), &lane1.raw)?;
    fs::write(results.join("usage.jsonl"), usage_lines.join("\n") + "\n")?;

    println!("fetched_utc:        {}", utc_now());
    println!(
        "state:              status={} model={:?} provider={:?} context={}/{:?} turn={} todos={}",
        state.typed.status,
        state.typed.model,
        state.typed.provider,
        state.typed.context_tokens,
        state.typed.context_window,
        state.typed.turn,
        state.typed.todos.len()
    );
    println!(
        "transcript:         {} message(s)",
        transcript.typed.messages.len()
    );
    println!(
        "registry:           {} model(s), {} provider(s), {} kernel api(s)",
        registry.typed.models.len(),
        registry.typed.providers.len(),
        registry.typed.apis.len()
    );
    println!("lanes:              {}", one_line(&lanes.raw));
    println!(
        "lane1_transcript:   {} message(s)",
        lane1.typed.messages.len()
    );
    println!("event_kinds:");
    for (kind, count) in &kinds {
        println!("  {kind} {count}");
    }
    println!("settled_outcome:    {}", settled.as_deref().unwrap_or("(none)"));
    println!("assistant_messages: {}", assistant.len());
    if let Some(last) = assistant.last() {
        println!("--- final assistant text ---\n{last}\n--- end ---");
    }
    if !reports.is_empty() {
        println!("lane_reports:       {}", reports.len());
    }

    // Every process the swarm started, so a shutdown can be checked for leaks.
    let before = descendants(server.pid());
    println!("processes_before:   coordinator {} descendants {:?}", server.pid(), before);
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
    println!("log:                {}", config.log_path.display());
    println!("finished_utc:       {}", utc_now());
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

/// Every process with `root` as an ancestor, deepest last, from `ps`.
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

fn write_json(path: &Path, value: &serde_json::Value) -> swarm_client::Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

/// A short one-line summary of a `/lanes` reply, without dumping the whole body.
fn one_line(body: &serde_json::Value) -> String {
    let Some(lanes) = body.get("lanes").and_then(|l| l.as_array()) else {
        return "(no lanes array)".to_string();
    };
    lanes
        .iter()
        .map(|lane| {
            format!(
                "{}:{}",
                lane.get("n").and_then(|n| n.as_u64()).map(|n| n.to_string()).unwrap_or("?".into()),
                lane.get("state").and_then(|s| s.as_str()).unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
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
