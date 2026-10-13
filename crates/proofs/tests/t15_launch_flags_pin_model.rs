//! t15 — the contract the empty tab's launch plan depends on: a model passed as
//! `--model` is journaled by evo, so a later `--resume` keeps it even after the
//! default in `init.lisp` has changed; with no flag, resume follows the new
//! default instead.
//!
//! The GUI pins only the model evo itself resolved (`check --json`'s answer or the
//! catalog's `default_model`) — never its own first-usable fallback — and asks
//! `check` with by-hand flags alone. This proof guards the binary half of that
//! design: the flag is what makes the choice durable for a resume.
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_SWARM_BIN=…/build/evo-swarm \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs \
//!   --test t15_launch_flags_pin_model -- --nocapture
//! ```

use std::fs;
use std::time::{Duration, Instant};

use proofs::fixture::{Fixture, NOTE};
use proofs::watch::{op, snapshot};
use serde_json::json;
use store::catalog::ModelRef;
use store::launch::Program;

/// Wait for the session state to satisfy `f`.
fn wait<T>(deadline: Instant, what: &str, f: impl Fn() -> Option<T>) -> T {
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run one turn to completion so the journal exists, then return its path.
fn journal_path(client: &swarm_client::Client, deadline: Instant) -> String {
    op(client, "input.send", json!({"text": "hello"}));
    wait(deadline, "the turn", || {
        let body = snapshot(client, &["session"], 1);
        (body["topics"]["session"]["state"]["status"].as_str() == Some("idle"))
            .then(|| {
                body["topics"]["session"]["state"]["session"]["path"]
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten()
    })
}

#[test]
fn t15_launch_flags_pin_model_for_resume() {
    let started = Instant::now();
    let fixture = Fixture::new("t15");
    fixture.enter();
    let init_path = fixture.home.join(".evo").join("init.lisp");
    let base = fs::read_to_string(&init_path).expect("the stub home's init.lisp");
    fs::write(
        &init_path,
        format!(
            "{base}(evo:register-model \"stub-b\" :provider :stub :context-window 200000 \
             :max-output 8000 :effort t)\n"
        ),
    )
    .expect("register stub-b; default stays stub-a");
    let deadline = Instant::now() + Duration::from_secs(90);
    let model = |client: &swarm_client::Client| {
        snapshot(client, &["session"], 1)["topics"]["session"]["state"]["model"]["id"]
            .as_str()
            .unwrap_or("<none>")
            .to_owned()
    };

    // A launch with no --model: the session runs on the init default.
    let tab = fixture.new_tab();
    let server = fixture.spawn(&fixture.spec_in(Program::Agent, 0, &tab));
    let client = server.client().clone();
    wait(deadline, "stub-a", || {
        (model(&client) == "stub-a").then_some(())
    });
    let unpinned = journal_path(&client, deadline);
    drop(server);
    std::thread::sleep(Duration::from_millis(500));

    // A launch that pins the resolved model, the way the GUI now does.
    let tab = fixture.new_tab();
    let mut spec = fixture.spec_in(Program::Agent, 0, &tab);
    spec.model = Some(ModelRef::new("stub-a", Some("stub")));
    let server = fixture.spawn(&spec);
    let client = server.client().clone();
    wait(deadline, "pinned stub-a", || {
        (model(&client) == "stub-a").then_some(())
    });
    let pinned = journal_path(&client, deadline);
    drop(server);
    std::thread::sleep(Duration::from_millis(500));

    // The default in init.lisp is now stub-b, the way a config edit changes it.
    fs::write(
        &init_path,
        format!(
            "{base}(evo:register-model \"stub-b\" :provider :stub :context-window 200000 \
             :max-output 8000 :effort t)\n(evo:set-setting :model \"stub-b\")\n"
        ),
    )
    .expect("the default is now stub-b");

    let resume = |session: &str| {
        let tab = fixture.new_tab();
        let mut spec = fixture.resume_spec(Program::Agent, std::path::Path::new(session));
        spec.ready_file = Some(store::launch::LaunchSpec::ready_file_in(
            &fixture.root,
            &tab,
        ));
        let server = fixture.spawn(&spec);
        wait(deadline, "the resumed session", || {
            (model(server.client()) != "<none>").then(|| model(server.client()))
        })
    };

    let unpinned_model = resume(&unpinned);
    let pinned_model = resume(&pinned);
    println!(
        "{NOTE} after the init default moved to stub-b: resume without a launch flag -> \
         {unpinned_model}, resume with --model stub-a -> {pinned_model}"
    );
    assert_eq!(
        unpinned_model, "stub-b",
        "without the flag the journal carries no model, so resume takes the current default"
    );
    assert_eq!(
        pinned_model, "stub-a",
        "the launch flag was journaled, so resume keeps the model the session started on"
    );
    println!("{NOTE} t15 done in {:?}", started.elapsed());
}
