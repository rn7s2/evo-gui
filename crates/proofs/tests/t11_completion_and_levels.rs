//! t11 — the three reads this branch adopted, against a real `evo-agent`: what the
//! caret could become (`complete`), the levels each model takes (`effort_levels`),
//! and where a command's own lines are published.
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_SWARM_BIN=…/build/evo-swarm \
//!   CARGO_TARGET_DIR=target/proofs cargo test -p proofs \
//!   --test t11_completion_and_levels -- --nocapture
//! ```
//!
//! Every other test of these three is against a fixture written by hand: what a
//! server *should* answer. This one asks a server built from evo's own main.
//!
//! Three things it is here to catch:
//!
//! * **the offsets.** `complete` counts characters — Lisp characters, Unicode scalar
//!   values — and an input box works in bytes. A caret after `✓` is at a different
//!   byte than at a different character, and a client that hands over the byte
//!   offset it has asks about the wrong place: this proof drives the conversion
//!   `session::complete_request` / `session::completion` do, with a text whose
//!   caret is past two `✓`;
//! * **what the caret is on.** A path is not a command word, `/eval`'s content is
//!   symbols and never commands, and a caret inside a unit replaces the whole unit;
//! * **the duplicate lines.** A command run through `command.run` answers with its
//!   own `notices` *and* publishes the same lines as session `notice` items, which is
//!   why the app draws the transcript and not the reply.

use std::time::Instant;

use proofs::fixture::{Fixture, NOTE};
use proofs::watch::{op, snapshot};
use serde_json::{json, Value};
use session::{Completion, CompletionKind};
use store::launch::Program;

/// Ask the running server what the caret is on in `text` at `cursor` — a **byte**
/// offset, the way the box holds it — and read the answer back the way the popup does.
fn ask(client: &swarm_client::Client, text: &str, cursor: usize) -> (Completion, Value) {
    let request = session::complete_request(text, cursor);
    let result = op(client, &request.op, request.args.clone());
    (session::completion(text, &result), result)
}

#[test]
fn t11_completion_and_levels() {
    let started = Instant::now();
    let fixture = Fixture::new("t11");
    fixture.enter();
    let server = fixture.spawn(&fixture.spec(Program::Agent, 0));
    let client = server.client().clone();

    // --- what the caret could become ---------------------------------------------
    //
    // The first four are ASCII, where a byte offset and a character offset agree.
    let cases: [(&str, usize); 4] = [
        ("/", 1),
        ("run /comp now", 9),
        ("/eval (evo.eval:tok", 18),
        ("/usr/local", 6),
    ];
    for (text, cursor) in cases {
        let (completion, raw) = ask(&client, text, cursor);
        let word = completion
            .kind
            .map(|_| &text[completion.start..completion.end])
            .unwrap_or_default();
        println!(
            "{NOTE} complete {text:?} at {cursor}: kind={:?} {word:?} — {raw}",
            completion.kind
        );
    }

    // A `/command` word: the range is the word *after* the slash, since a name never
    // carries one, and the candidates are the registry's own.
    let (slash, raw) = ask(&client, "/", 1);
    assert_eq!(slash.kind, Some(CompletionKind::Command), "{raw}");
    assert_eq!(
        (slash.start, slash.end),
        (1, 1),
        "just past the slash: {raw}"
    );
    assert!(
        slash.items.iter().any(|item| item.name == "compact"),
        "the registry's own commands, with its own descriptions: {raw}"
    );
    assert!(
        slash.items.iter().all(|item| !item.name.starts_with('/')),
        "a name is typed without its slash: {raw}"
    );

    // Mid-prose, and a name that is only a prefix of the typed word's candidates:
    // what comes back is the server's answer, not the client's guess.
    let (mid, raw) = ask(&client, "run /comp now", 9);
    assert_eq!(mid.kind, Some(CompletionKind::Command), "{raw}");
    assert_eq!(
        &"run /comp now"[mid.start..mid.end],
        "comp",
        "the word the caret is in, without its slash: {raw}"
    );

    // A caret inside `/eval`'s content is on a *symbol*, and the unit is the whole
    // token the caret is in — the range reaches past the caret, because a candidate
    // replaces the token, not the part of it that has been typed.
    let (symbol, raw) = ask(&client, "/eval (evo.eval:tok", 18);
    assert_eq!(symbol.kind, Some(CompletionKind::Symbol), "{raw}");
    assert_eq!(
        &"/eval (evo.eval:tok"[symbol.start..symbol.end],
        "evo.eval:tok",
        "{raw}"
    );
    assert!(symbol.end > 18, "the caret is inside the unit: {raw}");

    // A path is not a command word, and neither is prose.
    let (path, raw) = ask(&client, "/usr/local", 6);
    assert_eq!(path.kind, None, "a path is not a command: {raw}");
    assert!(path.items.is_empty(), "{raw}");

    // --- and the caret past characters that are not one byte ---------------------
    //
    // `✓` is one character and three bytes. The box's caret is at byte 18 here —
    // after `/mo` — which is character 14; a client that sent 18 would be asking
    // about a caret two characters past the end of the word it is typing.
    let unicode = "please ✓ ✓ /mo now";
    assert_eq!(unicode.len(), 22, "the text itself, in bytes");
    let (converted, raw) = ask(&client, unicode, 18);
    assert_eq!(
        converted.kind,
        Some(CompletionKind::Command),
        "the byte offset is converted before it is sent: {raw}"
    );
    assert_eq!(
        &unicode[converted.start..converted.end],
        "mo",
        "and the range that comes back is converted back: {raw}"
    );
    // The same question asked with the byte offset a client would have *without* the
    // conversion is a question about somewhere else — the proof that the two differ.
    let naive = session::completion(
        unicode,
        &op(&client, "complete", json!({"text": unicode, "cursor": 18})),
    );
    assert_eq!(naive.kind, None, "character 18 is not in a word: {naive:?}");

    // --- the levels each model takes, as the catalog spells them -----------------
    //
    // The document the app's choosers read, through the app's own readers: the
    // catalog the *live* server answers, and — below — the two documents the offline
    // binaries print, which are what the empty tab fills its choosers from before
    // anything is spawned.
    let catalog = serde_json::to_value(server.client().catalog().expect("a catalog"))
        .expect("the catalog as JSON");
    let models = session::model_options(&catalog);
    assert!(!models.is_empty(), "the stub home registers a model");
    for model in &models {
        println!(
            "{NOTE} model {} takes {:?} — {}",
            model.key, model.effort_levels, model.detail
        );
        assert!(
            !model.effort_levels.is_empty(),
            "every model object carries the levels it takes: {}",
            model.key
        );
        assert!(
            model
                .detail
                .contains(&format!("effort {}", model.effort_levels.join(", "))),
            "and the menu says them in full: {}",
            model.detail
        );
    }
    let stub = models
        .iter()
        .find(|model| model.id == "stub-a")
        .expect("the stub home's model");
    assert_eq!(
        stub.detail, "200k ctx · vision · effort low, medium, high, xhigh, max",
        "the catalog's own line, as the real server writes it"
    );
    // The session's ladder is the same list here, but it is a *different* fact: the
    // drawer offers the model's own.
    assert_eq!(
        session::thinking_levels(&catalog),
        ["low", "medium", "high", "xhigh", "max"]
    );

    // --- the offline documents, through the offline readers ----------------------
    //
    // `evo-swarm catalog --json` and `evo-swarm check --json` are what the empty
    // tab's choosers are built from (§9), and both carry the levels: every model of
    // the catalog and every lane model, and the two models `check` reports.
    let printed = store::cli::run_json(
        &fixture.bins.swarm,
        &store::cli::args(&["catalog", "--json"]),
    )
    .expect("evo-swarm catalog --json answered");
    let offline = store::catalog::Catalog::from_json(printed.clone());
    let drawn = session::model_options(&printed);
    assert_eq!(drawn.len(), offline.models().len(), "every model, twice");
    for model in &drawn {
        println!("{NOTE} offline model {}: {}", model.key, model.detail);
        assert!(
            model.detail.contains("effort "),
            "the line the chooser draws names the levels: {}",
            model.detail
        );
    }
    let lane = offline
        .lane_models()
        .expect("the swarm's catalog says which models a lane can register");
    assert!(!lane.is_empty(), "the swarm catalog lists its lane models");
    // The lane half carries the levels too; the app draws a lane model's line from
    // the model's own entry (they are the same registration), so nothing here reads
    // them — the contract's shape is what is asserted.
    for entry in printed["lanes"]["models"].as_array().expect("lane models") {
        println!("{NOTE} lane model: {entry}");
        assert!(
            entry["effort_levels"].is_array(),
            "a lane's model carries the levels it takes: {entry}"
        );
    }
    // The check's own document: what a launch would run, and the levels beside it.
    let report = store::cli::run_json(
        &fixture.bins.swarm,
        &store::cli::args(&[
            "check",
            "--json",
            "--model",
            "stub-a@stub",
            "--workers",
            "2",
        ]),
    );
    if let Ok(checked) = report {
        let levels = checked
            .get("model")
            .and_then(|model| model.get("effort_levels"))
            .and_then(Value::as_array)
            .map(|levels| levels.len())
            .unwrap_or_default();
        println!("{NOTE} check --json: {checked}");
        assert!(
            levels > 0,
            "check names the levels its model takes: {checked}"
        );
    }

    // --- where a command's own lines are published -------------------------------
    //
    // `command.run` answers with its lines *and* publishes them: the same text, as a
    // session `notice` item, which is what the transcript already draws. That is why
    // the app reads `data.draft` out of this reply and draws nothing from `notices`.
    let reply = op(
        &client,
        "command.run",
        json!({"name": "eval", "args": "(+ 1 2)"}),
    );
    let lines: Vec<&str> = reply["notices"]
        .as_array()
        .expect("notices")
        .iter()
        .filter_map(|notice| notice["text"].as_str())
        .collect();
    println!("{NOTE} command.run /eval answered {reply}");
    assert!(
        lines.iter().any(|line| line.contains('3')),
        "the reply carries the line the command wrote, not its style keyword: {reply}"
    );

    let session = snapshot(&client, &["session"], 50);
    let notices: Vec<String> = session["topics"]["session"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == json!("notice"))
        .filter_map(|item| item["text"].as_str().map(str::to_owned))
        .collect();
    for line in &lines {
        assert!(
            notices.iter().any(|notice| notice == line),
            "the same line is a session notice too — which is why the reply's copy is \
             not drawn: {line:?} in {notices:?}"
        );
    }

    println!("{NOTE} t11 done in {:?}", started.elapsed());
}
