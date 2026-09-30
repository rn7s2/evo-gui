//! t07 — the empty tab's choosers, without a server: the catalog and the launch
//! check come from two processes that print one document each.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t07_catalog_choosers
//! ```
//!
//! §9's whole point: `evo-swarm catalog --json` and `evo-swarm check --json`
//! answer before anything is spawned, so the empty tab can offer models, grey out
//! the ones a lane cannot register, and say what is wrong with a launch — without
//! the two throwaway servers the old probe needed, and without a listener of any
//! kind. Nothing in this file starts a server.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE};
use store::catalog::{Catalog, CheckReport, ModelRef};
use store::cli;
use store::launch::Program;

/// The model the stub home registers.
const MODEL: &str = "stub-a";

#[test]
fn t07_catalog_choosers() {
    let started = Instant::now();
    let fixture = Fixture::new("t07");
    fixture.enter();

    // --- the catalog, as one process prints it ----------------------------------
    let body = cli::run_json(&fixture.bins.swarm, &cli::args(&["catalog", "--json"]))
        .expect("evo-swarm catalog --json answered");
    let catalog = Catalog::from_json(body);
    let models: Vec<String> = catalog.models().into_iter().map(|model| model.id).collect();
    assert!(
        models.iter().any(|id| id == MODEL),
        "the choosers are offered the stub home's own model: {models:?}"
    );
    let lanes = catalog
        .lane_models()
        .expect("the swarm's catalog says which models a lane can register");
    assert!(
        lanes.iter().any(|model| model.id == MODEL && model.ok),
        "and the lanes chooser can offer it: {lanes:?}"
    );
    let registered = catalog
        .model(MODEL, Some("stub"))
        .expect("the stub home registers it with its provider");
    assert!(registered.ready, "and it is reachable: {registered:?}");
    println!(
        "{NOTE} catalog after {:?}: {} model(s), {} lane model(s)",
        started.elapsed(),
        models.len(),
        lanes.len()
    );

    // --- the check of the launch those choosers describe -------------------------
    let spec = fixture.spec(Program::Swarm, 2);
    let report = check(&fixture, &spec);
    assert!(
        report.ok,
        "the launch the empty tab would spawn is valid: {:?} — argv {:?}",
        report.problems,
        spec.argv()
    );
    assert_eq!(
        report.model.as_ref().and_then(|model| model.id.as_deref()),
        Some(MODEL),
        "and it says which model it judged: {report:?}"
    );

    // --- a model nothing registered is refused, with evo's own words -------------
    let mut unknown = fixture.spec(Program::Swarm, 2);
    unknown.model = Some(ModelRef::new("t07-not-a-model", Option::<String>::None));
    let report = check(&fixture, &unknown);
    assert!(
        !report.ok,
        "a model that is not registered is not a launch: {report:?}"
    );
    let lines = report.lines();
    assert!(
        !lines.is_empty(),
        "and the tab has a line to show: {report:?}"
    );
    println!("{NOTE} the check refused it: {lines:?}");
    println!("{NOTE} t07 done in {:?}", started.elapsed());
}

/// `evo-swarm check --json` for one spec: the same argv the empty tab builds.
fn check(fixture: &Fixture, spec: &store::launch::LaunchSpec) -> CheckReport {
    let body = cli::run_json(&fixture.bins.swarm, &spec.check_argv())
        .expect("evo-swarm check --json answered");
    CheckReport::from_json(&body)
}
