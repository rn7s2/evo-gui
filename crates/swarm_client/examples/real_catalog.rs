//! real_catalog — the empty tab's catalog, learned from the **real** `HOME`
//! (docs/PROMPT.md §9.4): `tab_engine::catalog::learn` starts the two throwaway
//! `evo-agent serve` probes (`probe_dir` under `$TMPDIR`, never the user's home),
//! and this prints what they said — models, providers, the kernel's own wire API
//! set — plus which models the lanes chooser would mark unavailable
//! (`session::lanes_chooser`).
//!
//! ```text
//! CARGO_TARGET_DIR=target/real cargo run -p swarm_client --example real_catalog
//! CARGO_TARGET_DIR=target/real cargo run -p swarm_client --example real_catalog -- --home /tmp/alt-home
//! ```
//!
//! `--home <dir>` overrides `EVO_HOME` for the probes only (the same switch
//! `catalog::learn_with` exists for): useful when the real `init.lisp` cannot be
//! probed at all, to still read the rest of the catalog. `EVO_AGENT_BIN` picks
//! the binary, `EVO_CATALOG_TIMEOUT_SECS` the patience.
//!
//! Names only — a model id, a provider key, an API name. No secret is printed,
//! and the probe's own `/registry` reply never carries one.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use session::{lanes_chooser, DEFAULT_KEY};
use swarm_client::redact;

fn main() {
    let mut home: Option<String> = None;
    let mut probe_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--home" => home = args.next(),
            "--probe-dir" => probe_dir = args.next().map(PathBuf::from),
            other => {
                eprintln!("usage: real_catalog [--home <dir>] [--probe-dir <dir>] (got {other})");
                std::process::exit(2);
            }
        }
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let probe_dir = probe_dir.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("evo-desktop-real-catalog-{nanos}"))
    });
    let bin = swarm_client::default_agent_bin();
    let env: Vec<(String, String)> = home
        .iter()
        .map(|home| ("EVO_HOME".to_string(), home.clone()))
        .collect();

    println!("=== real_catalog ===");
    println!("agent_bin:          {}", bin.display());
    println!("probe_dir:          {}", probe_dir.display());
    println!("home:               {}", home.as_deref().unwrap_or("(the real one)"));
    println!(
        "patience:           {}s (EVO_CATALOG_TIMEOUT_SECS)",
        std::env::var("EVO_CATALOG_TIMEOUT_SECS").ok().unwrap_or_else(|| "120".to_string())
    );

    let started = std::time::Instant::now();
    let updates = tab_engine::catalog::learn_with(bin, &probe_dir, env);
    let update = match updates.recv_blocking() {
        Ok(update) => update,
        Err(error) => {
            eprintln!("real_catalog: no update from the probe ({error})");
            std::process::exit(1);
        }
    };
    let (registry, kernel_apis) = match update {
        tab_engine::catalog::CatalogUpdate::Done { registry, kernel_apis } => {
            (registry, kernel_apis)
        }
        tab_engine::catalog::CatalogUpdate::Failed { message, log_tail } => {
            println!("catalog:            FAILED after {:?}: {}", started.elapsed(), redact(&message));
            for line in log_tail.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
                println!("  log: {}", redact(line));
            }
            std::process::exit(1);
        }
    };

    let models = registry.get("models").and_then(Value::as_array).cloned().unwrap_or_default();
    let providers = registry.get("providers").and_then(Value::as_array).cloned().unwrap_or_default();
    // The chooser and the report both read the probe's own array; `None` (the
    // no-userspace probe did not answer) falls back to the registry's own list.
    let apis = kernel_apis
        .as_ref()
        .map(|apis| serde_json::json!(apis))
        .or_else(|| registry.get("apis").cloned());
    println!("registry models:    {}", models.len());
    println!("registry providers: {}", providers.len());
    println!(
        "providers:          {}",
        providers
            .iter()
            .map(|provider| {
                let key = provider.get("key").and_then(Value::as_str).unwrap_or("?");
                let has_key = match provider.get("has_api_key").and_then(Value::as_bool) {
                    Some(true) => "has-key",
                    Some(false) => "no-key",
                    None => "key-unknown",
                };
                format!("{key} ({has_key})")
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("kernel apis:        {}", api_names(&apis));
    println!("models (id · provider · api):");
    for model in &models {
        println!(
            "  {} · {} · {}",
            model.get("id").and_then(Value::as_str).unwrap_or("?"),
            model.get("provider").and_then(Value::as_str).unwrap_or("-"),
            model.get("api").and_then(Value::as_str).unwrap_or("-")
        );
    }

    // What the empty tab's lanes row would show.
    let lanes = lanes_chooser(&registry, apis.as_ref());
    let coordinator = session::coordinator_chooser(&registry);
    println!(
        "lanes chooser:      uncertain={} {} option(s) ({} model(s) + Default)",
        lanes.uncertain,
        lanes.options.len(),
        lanes.models().count()
    );
    println!("lanes default:      {}", describe(lanes.option(DEFAULT_KEY)));
    let unavailable: Vec<String> = lanes
        .models()
        .filter(|option| !option.available)
        .map(|option| {
            format!(
                "{} ({}) — {}",
                option.label,
                option.provider.as_deref().unwrap_or("-"),
                option.unavailable_reason.as_deref().unwrap_or("no reason given")
            )
        })
        .collect();
    println!("lanes unavailable:  {}", unavailable.len());
    for line in &unavailable {
        println!("  {line}");
    }
    println!(
        "lanes available:    {}",
        lanes.models().filter(|option| option.available).count()
    );
    println!(
        "coordinator chooser: {} option(s), {} unavailable",
        coordinator.options.len(),
        coordinator.models().filter(|option| !option.available).count()
    );
    println!("--- lanes chooser options ---");
    for option in &lanes.options {
        println!("  {}", describe(Some(option)));
    }
}

fn describe(option: Option<&session::ChooserOption>) -> String {
    match option {
        None => "(missing)".to_string(),
        Some(option) => format!(
            "key={:?} label={:?} available={} provider={:?} model={:?} reason={:?}",
            option.key,
            option.label,
            option.available,
            option.provider,
            option.model_id,
            option.unavailable_reason
        ),
    }
}

/// The API names in a `kernel_apis` (or `/registry.apis`) body.
fn api_names(apis: &Option<Value>) -> String {
    match apis.as_ref().and_then(Value::as_array) {
        Some(names) => names
            .iter()
            .filter_map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        None => "(none reported: the no-userspace probe did not answer)".to_string(),
    }
}
