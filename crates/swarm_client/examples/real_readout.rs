//! real_readout — the §7.3 status line, computed by `session::Readout` from a
//! **real** run: the `/state` and `/registry` bodies `m0_real` saved, plus the
//! `message-end` usages it collected. This is the app's own model doing the work
//! — the composer's status row renders exactly `Readout::text()`.
//!
//! ```text
//! CARGO_TARGET_DIR=target/real \
//!   cargo run -p swarm_client --example real_readout -- /tmp/m0-real-results
//! ```
//!
//! Arguments: the results directory `m0_real` wrote. `registry.json` may be
//! missing or a failed route's error: the readout only uses it to tell an
//! ambiguous model id (registered under several providers) from an unambiguous
//! one, so its absence is reported and survived.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use session::Readout;

fn main() {
    let Some(dir) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: real_readout <results_dir from m0_real>");
        std::process::exit(2);
    };
    let read = |name: &str| -> Option<Value> {
        let path = dir.join(name);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).ok(),
            Err(error) => {
                eprintln!("note: {name} unreadable ({error})");
                None
            }
        }
    };

    let Some(state) = read("state.json") else {
        eprintln!("real_readout: no state.json in {}", dir.display());
        std::process::exit(1);
    };
    let registry = read("registry.json");

    let mut readout = Readout::new();
    readout.apply_state(&state);
    match &registry {
        Some(registry) => readout.apply_registry(registry),
        None => eprintln!("note: no registry.json — the model segment cannot tell an ambiguous id"),
    }

    // Every message-end of the run, folded in order: the run's context and cache
    // totals as the tab's own reducer would have them.
    let mut folded = 0;
    if let Ok(text) = fs::read_to_string(dir.join("usage.jsonl")) {
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(usage) = serde_json::from_str::<Value>(line) else { continue };
            if readout.fold_message_end(&serde_json::json!({ "usage": usage })) {
                println!("folded message-end #{folded}: ctx now {}", readout.context_label());
                folded += 1;
            }
        }
    }

    println!("model:              {:?}", readout.model_id());
    println!("provider:           {:?}", readout.provider());
    println!("thinking:           {:?}", readout.thinking());
    println!("model_label:        {:?}", readout.model_label());
    println!("context_tokens:     {}", readout.context_tokens());
    println!("context_window:     {:?}", readout.context_window());
    println!("context_label:      {}", readout.context_label());
    println!("cache_label:        {:?}", readout.cache_label());
    println!("goal_label:         {:?}", readout.goal_label());
    println!("segments:           {:?}", readout.segments());
    println!("readout line:       {}", readout.text());
    println!("folded_usages:      {folded}");
}
