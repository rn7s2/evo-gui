//! m0_cli — the M0 spike, in the terminal (docs/PROMPT.md §12):
//! start a swarm in a folder, stream its events, send one prompt, print the
//! deltas as they arrive, and shut it down again.
//!
//! ```text
//! CARGO_TARGET_DIR=target/swarm_client \
//!   cargo run -p swarm_client --example m0_cli -- ~/coding/some-project "say hello"
//! ```
//!
//! Environment: `EVO_SWARM_BIN`, `EVO_AGENT_BIN` (default `/usr/local/bin/…`),
//! `EVO_WORKERS` (evo's own default when unset). The tab directory — token and
//! `swarm.log` — is printed and left behind, so a failure can be read.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use swarm_client::{
    default_agent_bin, default_swarm_bin, EventStream, Server, ServerConfig, StreamConfig,
    StreamMsg, StreamTarget,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("m0_cli: {error}");
        if let Some(tail) = error.log_tail() {
            eprintln!("--- the server's log ---\n{tail}");
        }
        std::process::exit(1);
    }
}

fn run() -> swarm_client::Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = args.next().unwrap_or_else(|| ".".to_owned());
    let prompt = args.next();
    let folder = PathBuf::from(folder).canonicalize()?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("evo-desktop-m0-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let mut config =
        ServerConfig::swarm(default_swarm_bin(), &folder, &dir).with_evo(default_agent_bin());
    if let Some(workers) = std::env::var("EVO_WORKERS")
        .ok()
        .and_then(|w| w.parse().ok())
    {
        config.workers = Some(workers);
    }
    println!(
        "starting {} in {}\n  token: {}\n  log:   {}",
        config.bin.display(),
        folder.display(),
        config.token_file.display(),
        config.log_path.display()
    );

    let mut server = Server::start(&config)?;
    let health = server.health().clone();
    println!(
        "ready: {} {} pid {} port {} features {:?} cursor {:?}",
        health.name.as_deref().unwrap_or("?"),
        health.version.as_deref().unwrap_or("?"),
        health.pid,
        server.port(),
        health.features,
        health.cursor
    );
    let client = server.client().clone();
    println!("state: {:?}", client.state()?.status);

    // One stream, on its own thread, from the beginning of the log.
    let stream = EventStream::start(
        StreamTarget::coordinator(&client, Some(0)),
        StreamConfig::default(),
    );
    let deadline = Instant::now() + Duration::from_secs(300);

    if let Some(prompt) = prompt {
        let reply = client.prompt(&prompt)?;
        println!(
            "prompt \"{prompt}\": ok={} queued={:?}",
            reply.ok, reply.data
        );
        let mut settled = false;
        while !settled {
            if Instant::now() >= deadline {
                eprintln!("m0_cli: gave up waiting for the run to settle");
                break;
            }
            match next(&stream, deadline) {
                Some(StreamMsg::Event { kind, data, .. }) => match kind.as_str() {
                    // The point of §2.8: text, as it streams.
                    "text-delta" => print!(
                        "{}",
                        data.get("text").and_then(|t| t.as_str()).unwrap_or("")
                    ),
                    "message-end" => {
                        println!(
                            "\n— end of message ({} )",
                            data.get("stop_reason")
                                .and_then(|s| s.as_str())
                                .unwrap_or("?")
                        );
                        if let Some(usage) = data.get("usage") {
                            println!("  usage {usage}");
                        }
                    }
                    "tool-call-start" => println!(
                        "◆ tool {}",
                        data.get("name").and_then(|n| n.as_str()).unwrap_or("?")
                    ),
                    "output" => println!(
                        "{}",
                        data.get("text").and_then(|t| t.as_str()).unwrap_or("")
                    ),
                    "settled" => {
                        println!(
                            "settled: {}",
                            data.get("outcome").and_then(|o| o.as_str()).unwrap_or("?")
                        );
                        settled = true;
                    }
                    "gap" | "hello" => println!("[{kind}] {data}"),
                    _ => {}
                },
                Some(_) => {}
                None => break,
            }
        }
    } else {
        println!("no prompt given: streaming the log");
        while let Some(message) = next(&stream, deadline) {
            match message {
                StreamMsg::Event { id, kind, data } => println!("{id:?} {kind} {data}"),
                other => println!("{other:?}"),
            }
        }
    }
    drop(stream);

    let report = server.shutdown()?;
    println!(
        "shutdown: {:?} exit {:?} in {:?}",
        report.outcome, report.exit_code, report.waited
    );
    println!("the log is at {}", config.log_path.display());
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
