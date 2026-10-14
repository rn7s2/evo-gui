//! t14 — a saved config file takes effect without restarting the app, against a real
//! `evo-agent`: the `/reload` a tab sends after a save (§13), and the documents the
//! empty tab re-reads.
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_SWARM_BIN=…/build/evo-swarm \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs \
//!   --test t14_config_reload -- --nocapture
//! ```
//!
//! The home's `init.lisp` is rewritten while a server is running — a model whose
//! ladder is `low, high` and a `thinking` setting of `xhigh`, the shape of a person's
//! edit in the Settings tab — and then:
//!
//! * the running server is sent exactly the op the tab sends (`command.run` with
//!   `reload`), and its live `/catalog` answers with the new registration: no process
//!   was restarted;
//! * the offline `catalog --json` and `check --json` — what the empty tab reads again
//!   after a save — say the same, and the launcher built from them offers only the two
//!   levels that model takes, opening on the configured level clamped onto them.

use std::fs;
use std::time::Instant;

use proofs::fixture::{Fixture, NOTE};
use proofs::watch::op;
use serde_json::{json, Value};
use session::{Launcher, Role};
use store::launch::Program;

/// The live server's own catalog, as JSON.
fn live_catalog(client: &swarm_client::Client) -> Value {
    serde_json::to_value(client.catalog().expect("a catalog")).expect("the catalog as JSON")
}

/// One registration's levels in a catalog body.
fn levels_of(catalog: &Value, id: &str) -> Option<Vec<String>> {
    session::model_options(catalog)
        .into_iter()
        .find(|model| model.id == id)
        .map(|model| model.effort_levels)
}

#[test]
fn t14_config_reload() {
    let started = Instant::now();
    let fixture = Fixture::new("t14");
    fixture.enter();
    let server = fixture.spawn(&fixture.spec(Program::Agent, 0));
    let client = server.client().clone();

    let before = live_catalog(&client);
    assert_eq!(
        levels_of(&before, "stub-b"),
        None,
        "the home registers no stub-b yet"
    );
    assert_eq!(
        levels_of(&before, "stub-a").as_deref(),
        Some(&["low", "medium", "high", "xhigh", "max"].map(String::from)[..])
    );

    // The person's edit, as the Settings tab writes it: the whole file, saved.
    let init = fixture.home.join(".evo").join("init.lisp");
    let mut text = fs::read_to_string(&init).expect("the stub home's init.lisp");
    text.push_str(
        "(evo:register-model \"stub-b\" :provider :stub :context-window 100000 \
         :max-output 8000 :effort '(:low :high))\n\
         (evo:set-setting :model '(\"stub-b\" :stub))\n\
         (evo:set-setting :thinking :xhigh)\n",
    );
    fs::write(&init, text).expect("the edited init.lisp");

    // The running server has not seen it: init files are read at boot.
    assert_eq!(levels_of(&live_catalog(&client), "stub-b"), None);

    // What the tab sends after the save — and nothing is restarted.
    let request = session::OpRequest::command_run("reload", "");
    let reply = op(&client, &request.op, request.args.clone());
    println!("{NOTE} reload answered: {reply}");
    let after = live_catalog(&client);
    assert_eq!(
        levels_of(&after, "stub-b").as_deref(),
        Some(&["low", "high"].map(String::from)[..]),
        "the reloaded server registers the saved file's model"
    );
    assert_eq!(after["default_model"]["id"], json!("stub-b"), "{after}");
    println!(
        "{NOTE} live catalog after /reload: default {} thinking {}",
        after["default_model"], after["default_thinking"]
    );

    // The empty tab's own reads, asked again after the save.
    let printed = store::cli::run_json(
        &fixture.bins.swarm,
        &store::cli::args(&["catalog", "--json"]),
    )
    .expect("evo-swarm catalog --json answered");
    let checked = store::cli::run_json_reporting(
        &fixture.bins.swarm,
        &store::cli::args(&["check", "--json", "--workers", "1"]),
    )
    .expect("evo-swarm check --json answered");
    println!(
        "{NOTE} check: model {} thinking {} lane {} lane_thinking {}",
        checked["model"], checked["thinking"], checked["lane_model"], checked["lane_thinking"]
    );
    let mut launcher = Launcher::new();
    launcher.set_catalog(&printed);
    launcher.set_check(&checked);
    assert_eq!(launcher.chosen_key(Role::Coordinator), Some("stub-b@stub"));
    assert_eq!(
        launcher.ladder(Role::Coordinator),
        ["low", "high"],
        "the slider offers only what the model takes"
    );
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("high"),
        "the configured xhigh, clamped onto the model the way evo clamps it"
    );
    assert!(launcher.plan().is_default(), "shown, never passed");
    println!("{NOTE} t14: reload and re-read in {:?}", started.elapsed());
}
