//! The empty tab's view model: the choosers built from the captured registries, the launch
//! plan they produce, the `swarm.lisp` note, and the history rows.
//!
//! The two registries are real captures (`registry.json` from the swarm's coordinator,
//! `registry-two-providers.json` from a swarm whose two providers serve the same model id).
//! Cases no capture reaches — a model whose API a lane does not have, a worker count the
//! registry configures, a session with no timestamp — are synthesized here, and each says
//! what shape it copies and from where.

mod common;

use common::fixture;
use serde_json::json;
use session::{
    coordinator_chooser, history_rows, home_short, lanes_chooser, lanes_model_note, relative_time,
    swarm_lisp_path, swarm_workers_setting, workers_chooser, Choice, Chooser, HistoryEntry,
    HistorySource, LaunchPlan, Launcher, When, DEFAULT_KEY, NEEDS_EXTENSION_API, WORKERS_MAX,
};

/// The kernel's own API set, as a `--no-userspace` probe reports it — the capture's
/// coordinator registry carries it too (`/registry.apis`, `src/serve/routes.lisp:472`).
fn kernel_apis(registry: &serde_json::Value) -> serde_json::Value {
    registry["apis"].clone()
}

fn labels(chooser: &Chooser) -> Vec<&str> {
    chooser
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect()
}

fn entry(path: &str, folder: &str, when: When) -> HistoryEntry {
    HistoryEntry {
        session_path: path.to_string(),
        folder: folder.to_string(),
        when,
        lanes: None,
        coordinator_model: None,
        lanes_model: None,
        source: HistorySource::Scan,
        open_at_quit: false,
    }
}

// --- choosers ----------------------------------------------------------------------

#[test]
fn the_coordinator_chooser_lists_every_model_after_default() {
    let chooser = coordinator_chooser(&fixture("registry.json"));
    assert_eq!(labels(&chooser), vec!["Default", "stub-a", "stub-b"]);
    assert_eq!(chooser.options[0].key, DEFAULT_KEY);
    assert_eq!(chooser.options[0].detail, "evo's own default");
    assert!(
        !chooser.uncertain,
        "the coordinator can use whatever its registry lists"
    );
    assert!(chooser.options.iter().all(|option| option.available));
    assert!(chooser
        .options
        .iter()
        .all(|option| option.unavailable_reason.is_none()));

    let stub_a = chooser
        .option("stub-a@stub")
        .expect("the key names id and provider");
    assert_eq!(stub_a.label, "stub-a");
    assert_eq!(stub_a.detail, "200k ctx · vision · effort low–max");
    assert_eq!(
        stub_a.model(),
        Some(("stub-a".to_string(), "stub".to_string()))
    );
    assert!(stub_a.available && stub_a.unavailable_reason.is_none());

    // Sorted by provider then id, and detail follows the registry's own fields.
    let stub_b = chooser.option("stub-b@stub").expect("stub-b");
    assert_eq!(stub_b.detail, "100k ctx · vision · effort low–max");
    assert_eq!(chooser.models().count(), 2, "Default is not a model");
    assert_eq!(chooser.index_of("stub-b@stub"), Some(2));
    assert_eq!(chooser.index_of("nope"), None);
    assert!(chooser.option(DEFAULT_KEY).unwrap().model().is_none());
}

/// §7.3's rule: an id under more than one provider is named with its provider, because the
/// bare id no longer identifies the endpoint.
#[test]
fn an_id_under_two_providers_names_its_provider() {
    let chooser = coordinator_chooser(&fixture("registry-two-providers.json"));
    assert_eq!(
        labels(&chooser),
        vec!["Default", "stub-a (stub)", "stub-a (stub2)"]
    );
    assert_eq!(
        chooser.options[1].model(),
        Some(("stub-a".to_string(), "stub".to_string())),
        "the two options are distinct models, not a duplicate"
    );
    assert_eq!(
        chooser.options[2].model(),
        Some(("stub-a".to_string(), "stub2".to_string()))
    );
    assert_ne!(chooser.options[1].key, chooser.options[2].key);
}

/// F4 of `docs/review-1.md`: the coordinator's model reaches the swarm as `--model <id>`,
/// which is a bare id, so only the registration a bare id resolves to is a real choice.
///
/// The order evidence: evo's `*models*` is documented "in registration order"
/// (`src/provider/registry.lisp`), `/registry.models` is that list walked in order
/// (`src/serve/routes.lisp`), and `find-model` for a bare id takes the **first** entry
/// (`src/provider/registry.lisp`) — which is why the capture, whose two providers registered
/// `stub` then `stub2`, resolves `stub-a` to `stub`.
#[test]
fn the_coordinator_offers_only_the_registration_a_bare_id_reaches() {
    let registry = fixture("registry-two-providers.json");
    assert_eq!(
        registry["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["provider"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["stub", "stub2"],
        "the capture lists the two registrations in registration order"
    );

    let chooser = coordinator_chooser(&registry);
    assert_eq!(
        labels(&chooser),
        vec!["Default", "stub-a (stub)", "stub-a (stub2)"]
    );
    let reached = chooser
        .option("stub-a@stub")
        .expect("the first registration");
    assert!(reached.available && reached.unavailable_reason.is_none());
    assert_eq!(
        reached.model(),
        Some(("stub-a".to_string(), "stub".to_string()))
    );

    let other = chooser
        .option("stub-a@stub2")
        .expect("the second registration is still listed");
    assert!(!other.available, "a bare id never reaches it");
    assert_eq!(
        other.unavailable_reason.as_deref(),
        Some("evo-swarm --model resolves this id to stub")
    );

    // The rule follows the registry's order, not the alphabet: register stub2 first and the
    // other registration is the reachable one.
    let reversed = json!({ "models": [
        { "id": "stub-a", "provider": "stub2", "api": "anthropic-messages", "context_window": 200000 },
        { "id": "stub-a", "provider": "stub", "api": "anthropic-messages", "context_window": 200000 },
    ]});
    let chooser = coordinator_chooser(&reversed);
    assert!(chooser.option("stub-a@stub2").unwrap().available);
    let stub = chooser.option("stub-a@stub").unwrap();
    assert!(!stub.available);
    assert_eq!(
        stub.unavailable_reason.as_deref(),
        Some("evo-swarm --model resolves this id to stub2")
    );

    // Nothing else is affected: an unambiguous id, and the one-provider capture, stay whole.
    assert!(coordinator_chooser(&fixture("registry.json"))
        .models()
        .all(|option| option.available));

    // The lanes chooser is untouched (§9.6 writes the provider into swarm.lisp, so every
    // registration is its own choice there), and the labels keep naming the provider.
    let lanes = lanes_chooser(&registry, Some(&kernel_apis(&registry)));
    assert_eq!(
        labels(&lanes),
        vec!["Default", "stub-a (stub)", "stub-a (stub2)"]
    );
    assert!(
        lanes
            .models()
            .all(|option| option.available && option.unavailable_reason.is_none()),
        "lanes: {:?}",
        lanes.options
    );
}

#[test]
fn the_lanes_chooser_measures_models_against_the_kernel_api_set() {
    let registry = fixture("registry.json");
    let apis = kernel_apis(&registry);
    assert_eq!(apis, json!(["anthropic-messages"]));

    // Every captured model speaks the kernel's API, so a lane registers it.
    let chooser = lanes_chooser(&registry, Some(&apis));
    assert!(!chooser.uncertain);
    assert_eq!(labels(&chooser), vec!["Default", "stub-a", "stub-b"]);
    assert!(chooser.models().all(|option| option.available));

    // A model whose API is not in the lane: a lane calling `find-api` for it errors
    // (`src/provider/api.lisp`), and only `swarm.lisp`'s `(evo.swarm:in-lanes …)` can load
    // the extension that defines it. The shape below is a captured model's, with the API
    // and a second model's missing `api` swapped in.
    let mixed = json!({ "models": [
        { "id": "stub-a", "provider": "stub", "api": "anthropic-messages", "context_window": 200000,
          "vision": true, "effort": ["low", "medium", "max"] },
        { "id": "gpt-9", "provider": "openai", "api": "openai-chat", "context_window": 1000000,
          "vision": true, "effort": ["low"] },
        { "id": "mystery", "provider": "acme", "context_window": 8000 },
    ]});
    let chooser = lanes_chooser(&mixed, Some(&apis));
    assert!(!chooser.uncertain);
    let gpt = chooser.option("gpt-9@openai").expect("gpt-9");
    assert!(!gpt.available);
    assert_eq!(gpt.unavailable_reason.as_deref(), Some(NEEDS_EXTENSION_API));
    assert_eq!(
        gpt.detail, "1M ctx · vision · effort low",
        "a whole million reads 1M, and a single effort level has no range"
    );
    assert!(
        chooser
            .option("mystery@acme")
            .is_some_and(|option| !option.available),
        "a model that names no API cannot be checked, so a lane is not promised it"
    );
    assert!(chooser
        .option("stub-a@stub")
        .is_some_and(|option| option.available));
    assert_eq!(
        chooser.option("stub-a@stub").unwrap().detail,
        "200k ctx · vision · effort low–max"
    );

    // No probe has reported the set: nothing is claimed unavailable, and the chooser says
    // its availability is unverified (§9.4).
    let unknown = lanes_chooser(&registry, None);
    assert!(unknown.uncertain);
    assert!(unknown
        .models()
        .all(|option| option.available && option.unavailable_reason.is_none()));
    // A null `apis` is "no probe has said", not "no API exists".
    let null = lanes_chooser(&registry, Some(&json!(null)));
    assert!(null.uncertain);
    assert!(null.models().all(|option| option.available));

    // The lanes' Default is the rule, not a model: a lane runs the coordinator's model
    // unless the project's swarm.lisp says otherwise (§9.6).
    assert_eq!(chooser.options[0].key, DEFAULT_KEY);
    assert_eq!(chooser.options[0].detail, "follows the coordinator");
}

/// The detail's window figure is evo's own picker formatting (`format-context-window`,
/// `src/command/command.lisp:241`) — above a million it is `M`, because "1000k reads worse
/// than 1M". Every expected string below was produced by evaluating that function on the
/// same number in a real Common Lisp.
#[test]
fn the_detail_shows_windows_the_way_evos_picker_does() {
    let windows = [
        (8_000u64, "8k"),
        (100_000, "100k"),
        (200_000, "200k"),
        (936_000, "936k"),
        (999_999, "1000k"),
        (1_000_000, "1M"),
        (1_000_001, "1.0M"),
        (1_048_576, "1.0M"),
        (1_050_000, "1.1M"),
        (1_150_000, "1.1M"),
        (1_200_000, "1.2M"),
        (1_250_000, "1.3M"),
        (1_350_000, "1.4M"),
        (1_500_000, "1.5M"),
        (1_650_000, "1.6M"),
        (1_750_000, "1.8M"),
        (1_999_999, "2.0M"),
        (2_000_000, "2M"),
        (2_250_000, "2.3M"),
        (2_500_000, "2.5M"),
        (10_000_000, "10M"),
    ];
    let models: Vec<serde_json::Value> = windows
        .iter()
        .map(|(window, _)| json!({ "id": format!("m{}", window), "provider": "stub", "api": "anthropic-messages", "context_window": window }))
        .collect();
    let chooser = coordinator_chooser(&json!({ "models": models }));
    for (window, expected) in windows {
        let option = chooser
            .option(&format!("m{}@stub", window))
            .expect("every model is listed");
        assert_eq!(option.detail, format!("{} ctx", expected), "{window}");
    }

    // A model the registry says nothing about has no detail at all, rather than a zero.
    let chooser = coordinator_chooser(&json!({ "models": [
        { "id": "quiet", "provider": "stub", "api": "anthropic-messages", "context_window": 0 }
    ]}));
    assert_eq!(chooser.option("quiet@stub").unwrap().detail, "");
}

#[test]
fn the_workers_chooser_spans_one_to_sixty_four() {
    let chooser = workers_chooser(None);
    assert_eq!(labels(&chooser)[0], "Default");
    assert_eq!(chooser.options[0].detail, "evo's own default, else 6");
    assert_eq!(chooser.options.len(), 1 + WORKERS_MAX as usize);
    assert_eq!(chooser.options[1].key, "1");
    assert_eq!(chooser.options.last().unwrap().key, "64");
    assert!(chooser.options.iter().all(|option| option.available));
    assert!(chooser
        .options
        .iter()
        .all(|option| option.model().is_none()));

    // The configured value is what Default means, so the label says it: `Default (6)`
    // (§7.2's row 3, where Default = evo's own `:swarm-workers`, else 6).
    let chooser = workers_chooser(Some(6));
    assert_eq!(chooser.options[0].label, "Default (6)");
    assert_eq!(chooser.options[0].detail, ":swarm-workers 6");
    assert_eq!(chooser.options[0].key, DEFAULT_KEY);

    // A configured count the list does not hold: the label still tells the truth.
    let chooser = workers_chooser(Some(99));
    assert_eq!(chooser.options[0].label, "Default (99)");
    assert_eq!(chooser.options.len(), 1 + WORKERS_MAX as usize);
}

#[test]
fn the_configured_swarm_workers_comes_from_the_registry_settings() {
    // The capture's settings carry only the model; `:swarm-workers` appears once a project
    // or the user sets it (`docs/swarm.md`), and crosses the wire as `swarm_workers`.
    assert_eq!(swarm_workers_setting(&fixture("registry.json")), None);
    assert_eq!(
        swarm_workers_setting(&json!({ "settings": { "swarm_workers": 6 } })),
        Some(6)
    );
    assert_eq!(
        swarm_workers_setting(&json!({ "settings": { "swarm_workers": null } })),
        None
    );
    assert_eq!(
        swarm_workers_setting(&json!({ "settings": { "swarm_workers": 0 } })),
        None
    );
    assert_eq!(
        swarm_workers_setting(&json!({ "settings": { "swarm_workers": 99 } })),
        Some(99),
        "outside the chooser's 1–64 range, but still what Default would mean"
    );
    assert_eq!(swarm_workers_setting(&json!({ "settings": {} })), None);
    assert_eq!(swarm_workers_setting(&json!({})), None);
}

// --- the plan and the note ---------------------------------------------------------

#[test]
fn the_plan_passes_only_what_was_chosen() {
    let mut tab = Launcher::new();
    assert_eq!(
        tab.plan(),
        LaunchPlan::default(),
        "an untouched tab passes nothing"
    );
    assert_eq!(tab.selected_key(Choice::Coordinator), DEFAULT_KEY);

    assert!(tab.set_registry(&fixture("registry.json")));
    assert!(tab.set_kernel_apis(Some(&kernel_apis(&fixture("registry.json")))));

    assert!(tab.select(Choice::Coordinator, "stub-b@stub"));
    assert!(tab.select(Choice::Lanes, "stub-a@stub"));
    assert!(tab.select(Choice::Workers, "6"));
    assert_eq!(
        tab.plan(),
        LaunchPlan {
            model: Some(("stub-b".to_string(), "stub".to_string())),
            lanes_model: Some(("stub-a".to_string(), "stub".to_string())),
            workers: Some(6),
        }
    );
    assert_eq!(tab.selected(Choice::Lanes).unwrap().label, "stub-a");

    // Choosing the same thing again is not a change, and an unknown key is ignored: the
    // choosers come from `/registry`, so a key that is not there selects nothing.
    assert!(!tab.select(Choice::Coordinator, "stub-b@stub"));
    assert!(!tab.select(Choice::Coordinator, "not-a-model"));
    assert_eq!(
        tab.plan().model,
        Some(("stub-b".to_string(), "stub".to_string()))
    );
    assert_eq!(
        tab.selected(Choice::Coordinator).unwrap().key,
        "stub-b@stub"
    );

    // Back to Default: nothing is passed.
    assert!(tab.select(Choice::Coordinator, DEFAULT_KEY));
    assert_eq!(tab.plan().model, None);
    assert!(tab.select(Choice::Workers, DEFAULT_KEY));
    assert_eq!(tab.plan().workers, None);

    // A selection that the next registry does not have falls back to Default, and the tab
    // says it changed.
    assert!(tab.select(Choice::Lanes, "stub-b@stub"));
    assert!(tab.set_registry(&json!({ "models": [
        { "id": "other", "provider": "stub", "api": "anthropic-messages" }
    ]})));
    assert_eq!(tab.selected_key(Choice::Lanes), DEFAULT_KEY);
    assert_eq!(tab.plan().lanes_model, None);
    // ...and a registry that still has the chosen model keeps it: the tab is only reset
    // when the option itself is gone.
    assert!(tab.set_registry(&fixture("registry.json")));
    assert!(tab.select(Choice::Lanes, "stub-b@stub"));
    assert!(
        !tab.set_registry(&fixture("registry.json")),
        "the same catalog is not a change"
    );
    assert_eq!(tab.selected_key(Choice::Lanes), "stub-b@stub");
    assert_eq!(
        tab.plan().lanes_model,
        Some(("stub-b".to_string(), "stub".to_string()))
    );
}

#[test]
fn the_empty_tab_reports_which_updates_changed_it() {
    let mut tab = Launcher::new();
    assert!(
        tab.set_registry(&fixture("registry.json")),
        "the first registry is news"
    );

    // A probe registry is both halves at once: the catalog and the kernel's API set.
    let mut probe = Launcher::new();
    assert!(probe.set_probe_registry(&fixture("registry.json")));
    assert!(!probe.lanes().uncertain);
    assert_eq!(labels(probe.lanes()), vec!["Default", "stub-a", "stub-b"]);

    // The same registry again changes nothing.
    assert!(!probe.set_probe_registry(&fixture("registry.json")));
    assert!(!probe.set_kernel_apis(Some(&kernel_apis(&fixture("registry.json")))));

    // The kernel set arriving later still counts as a change.
    let mut tab = Launcher::new();
    tab.set_registry(&fixture("registry.json"));
    assert!(tab.lanes().uncertain);
    assert!(tab.set_kernel_apis(Some(&kernel_apis(&fixture("registry.json")))));
    assert!(!tab.lanes().uncertain);
    assert!(
        tab.set_kernel_apis(None),
        "forgetting the set is a change too"
    );

    // The configured worker count is a change once, and Default keeps meaning it.
    let mut tab = Launcher::new();
    assert!(tab.set_swarm_workers(Some(4)));
    assert!(!tab.set_swarm_workers(Some(4)));
    assert_eq!(tab.workers().options[0].label, "Default (4)");
    assert!(tab.set_swarm_workers(None));
    assert_eq!(tab.workers().options[0].label, "Default");
    assert_eq!(tab.workers().options[0].detail, "evo's own default, else 6");
}

#[test]
fn the_note_names_the_folders_swarm_lisp() {
    assert_eq!(
        swarm_lisp_path("/Users/me/coding/foo"),
        "/Users/me/coding/foo/.evo/swarm.lisp"
    );
    assert_eq!(
        swarm_lisp_path("/Users/me/coding/foo/"),
        "/Users/me/coding/foo/.evo/swarm.lisp",
        "a folder's trailing separator (evo namestrings carry one) does not double up"
    );
    assert_eq!(
        lanes_model_note("/Users/me/coding/foo", Some("/Users/me")),
        "Saved to ~/coding/foo/.evo/swarm.lisp — shared by every tab"
    );
    // Without a home to shorten against, the path is written whole — never a wrong `~`.
    assert_eq!(
        lanes_model_note("/Users/me/coding/foo", None),
        "Saved to /Users/me/coding/foo/.evo/swarm.lisp — shared by every tab"
    );
    let tab = Launcher::new();
    assert_eq!(
        tab.lanes_model_note("/Users/me/coding/foo", Some("/Users/me")),
        lanes_model_note("/Users/me/coding/foo", Some("/Users/me"))
    );
}

#[test]
fn paths_shorten_around_the_home_directory_only() {
    assert_eq!(
        home_short("/Users/me/coding/foo", Some("/Users/me")),
        "~/coding/foo"
    );
    assert_eq!(home_short("/Users/me", Some("/Users/me")), "~");
    assert_eq!(
        home_short("/Users/me/", Some("/Users/me/")),
        "~/",
        "as written, minus the home"
    );
    assert_eq!(
        home_short("/Users/melanie/x", Some("/Users/me")),
        "/Users/melanie/x",
        "a sibling whose name merely starts with the home path is left alone"
    );
    assert_eq!(
        home_short("/other/place", Some("/Users/me")),
        "/other/place"
    );
    assert_eq!(home_short("/Users/me/x", None), "/Users/me/x");
}

// --- history -----------------------------------------------------------------------

#[test]
fn history_rows_merge_by_session_newest_first() {
    let now = 1790683200; // 2026-09-29T12:00:00Z
    let mut scanned = entry(
        "/sessions/a.sexp",
        "/Users/me/coding/foo",
        When::Epoch(now - 2 * 3600),
    );
    scanned.lanes = Some(4);
    scanned.lanes_model = Some("claude-4".to_string());
    scanned.source = HistorySource::Scan;

    // The same session from the app's own recents: same path, and it knows the model the
    // scan could not (and no lane count) — and, unlike the scan, that the tab was open when
    // the app last quit.
    let mut recent = entry(
        "/sessions/a.sexp",
        "/Users/me/coding/foo",
        When::Epoch(now - 2 * 3600),
    );
    recent.coordinator_model = Some("gpt-5".to_string());
    recent.source = HistorySource::Recent;
    recent.open_at_quit = true;

    // Another session, older, and a third whose header timestamp is an RFC3339 string.
    let mut older = entry(
        "/sessions/b.sexp",
        "/Users/me/coding/bar/",
        When::Text("2026-09-27T09:00:00Z".into()),
    );
    older.lanes = Some(1);
    let third = entry(
        "/sessions/c.sexp",
        "/Users/me/coding/baz",
        When::Text("2026-09-29T11:59:30Z".into()),
    );

    let rows = history_rows(&[scanned, older, recent, third], now, 0, Some("/Users/me"));
    assert_eq!(rows.len(), 3, "the duplicate session is one row");
    assert_eq!(
        rows.iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>(),
        vec!["baz", "foo", "bar"],
        "newest first, and a `just now` row leads"
    );

    let baz = &rows[0];
    assert_eq!(baz.subtitle, "~/coding/baz");
    assert_eq!(baz.meta, "just now", "nothing else is known about it");
    assert_eq!(baz.session_path, "/sessions/c.sexp");
    assert_eq!(baz.coordinator_model, None);
    assert_eq!(baz.lanes_model, None);
    // The tooltip is the whole entry: the absolute folder, the session file, the instant
    // with its offset spelled out — and no invented models or counts.
    assert_eq!(
        baz.tooltip,
        "/Users/me/coding/baz · c.sexp · 2026-09-29 11:59:30 +00:00"
    );

    let foo = &rows[1];
    assert_eq!(foo.title, "foo");
    assert_eq!(foo.subtitle, "~/coding/foo");
    assert_eq!(foo.meta, "4 lanes · 2h ago · coordinator: gpt-5");
    assert_eq!(foo.session_path, "/sessions/a.sexp");
    assert_eq!(
        foo.folder, "/Users/me/coding/foo",
        "the absolute folder comes with the row"
    );
    assert_eq!(
        foo.source,
        HistorySource::Scan,
        "the newest entry's source is the row's"
    );
    // Both models survive the merge: the scan knew the lanes', the app's own recents the
    // coordinator's.
    assert_eq!(foo.coordinator_model.as_deref(), Some("gpt-5"));
    assert_eq!(foo.lanes_model.as_deref(), Some("claude-4"));
    // The scan found the session; only the app's recents could say it was open at quit, and
    // one true copy makes the merged row true.
    assert!(foo.open_at_quit);
    assert_eq!(
        foo.tooltip,
        "/Users/me/coding/foo · a.sexp · 2026-09-29 10:00:00 +00:00 · open at last quit \
         · coordinator: gpt-5 · lanes: claude-4 · 4 lanes"
    );

    let bar = &rows[2];
    assert_eq!(
        bar.title, "bar",
        "a trailing separator is not part of the name"
    );
    assert_eq!(
        bar.meta, "1 lane · 2d ago",
        "`lane` is singular for one, and no model is known"
    );
    assert!(
        !bar.open_at_quit,
        "the scan alone cannot know it was open at quit"
    );
    // The RFC3339 header timestamp reads back as the same instant, at the offset asked for.
    assert_eq!(
        bar.tooltip,
        "/Users/me/coding/bar · b.sexp · 2026-09-27 09:00:00 +00:00 · 1 lane"
    );
}

#[test]
fn a_history_entry_with_no_usable_time_sorts_last_and_says_nothing() {
    let now = 1790683200;
    let mut unknown = entry(
        "/sessions/x.sexp",
        "/Users/me/coding/x",
        When::Text("not a timestamp".into()),
    );
    unknown.lanes = Some(2);
    let known = entry(
        "/sessions/y.sexp",
        "/Users/me/coding/y",
        When::Epoch(now - 30),
    );
    let rows = history_rows(&[unknown, known], now, 0, Some("/Users/me"));
    assert_eq!(
        rows[0].title, "y",
        "a session with a time sorts above one without"
    );
    assert_eq!(
        rows[1].meta, "2 lanes",
        "the unknown time is left out entirely"
    );
    assert_eq!(
        rows[1].tooltip, "/Users/me/coding/x · x.sexp · 2 lanes",
        "and the tooltip says nothing about the time either"
    );
}

#[test]
fn relative_time_says_what_the_list_says() {
    let now = 1790683200; // 2026-09-29T12:00:00Z
    for (label, when, expected) in [
        ("this second", now, "just now"),
        ("30s", now - 30, "just now"),
        ("59s", now - 59, "just now"),
        ("1m", now - 60, "1m ago"),
        ("5m", now - 5 * 60, "5m ago"),
        ("59m", now - 59 * 60, "59m ago"),
        ("1h", now - 3600, "1h ago"),
        ("2h", now - 2 * 3600, "2h ago"),
        ("23h", now - 23 * 3600, "23h ago"),
        ("24h", now - 24 * 3600, "yesterday"),
        // 12:00 minus 47h is 13:00 two calendar days ago: the day rule, not the hour rule.
        ("47h", now - 47 * 3600, "2d ago"),
        ("48h", now - 48 * 3600, "2d ago"),
        ("3d", now - 3 * 86_400, "3d ago"),
        ("6d", now - 6 * 86_400, "6d ago"),
        // A week back: the date instead of a count (2026-09-22, still this year).
        ("7d", 1790078400, "22 Sep"),
        ("12d", 1789646400, "17 Sep"),
        ("200d", 1773403200, "13 Mar"),
        // Another year says which.
        ("2025-09-12", 1757635200, "12 Sep 2025"),
        // A clock ahead of ours is not "in the future" to a list: it is just now.
        ("30s ahead", now + 30, "just now"),
    ] {
        assert_eq!(relative_time(when, now, 0), expected, "{label}");
    }
}

/// A desktop shows a session's time where the person is: the rows take the caller's local
/// UTC offset, the tooltip prints the instant in it and always spells the offset out, and
/// the day words ("yesterday", the dates) are the caller's calendar.
#[test]
fn the_rows_read_in_the_callers_own_offset() {
    let now = When::from("2026-09-29T12:00:00Z").epoch_seconds().unwrap();
    // The captured journal header, seen from four places.
    let header = When::from("2026-09-29T09:25:44Z");
    for (offset, expected) in [
        (0, "2026-09-29 09:25:44 +00:00"),
        (8 * 3600, "2026-09-29 17:25:44 +08:00"),
        (-5 * 3600, "2026-09-29 04:25:44 -05:00"),
        (5 * 3600 + 1800, "2026-09-29 14:55:44 +05:30"),
        (14 * 3600, "2026-09-29 23:25:44 +14:00"),
    ] {
        let mut session = entry("/sessions/a.sexp", "/Users/me/coding/foo", header.clone());
        session.lanes = Some(2);
        let rows = history_rows(&[session], now, offset, Some("/Users/me"));
        assert_eq!(
            rows[0].tooltip,
            format!("/Users/me/coding/foo · a.sexp · {} · 2 lanes", expected)
        );
    }

    // The day words follow the local calendar, not a fixed 24-hour bucket: the same 25-hour
    // gap is two days back at UTC and only yesterday at +08:00, because at +08:00 the local
    // clock says 08:30 rather than 00:30.
    let midnight = When::from("2026-09-29T00:30:00Z").epoch_seconds().unwrap();
    let earlier = midnight - 90_000;
    assert_eq!(relative_time(earlier, midnight, 0), "2d ago");
    assert_eq!(relative_time(earlier, midnight, 8 * 3600), "yesterday");
    // ...and the same holds through the meta line.
    let mut session = entry(
        "/sessions/a.sexp",
        "/Users/me/coding/foo",
        When::Epoch(earlier),
    );
    session.lanes = Some(2);
    let utc = history_rows(std::slice::from_ref(&session), midnight, 0, None);
    let perth = history_rows(std::slice::from_ref(&session), midnight, 8 * 3600, None);
    assert_eq!(utc[0].meta, "2 lanes · 2d ago");
    assert_eq!(perth[0].meta, "2 lanes · yesterday");

    // The date the list prints is the local one too: the same instant is 12 September in
    // UTC and the 13th eight hours east.
    let evening = When::from("2026-09-12T20:00:00Z").epoch_seconds().unwrap();
    assert_eq!(relative_time(evening, now, 0), "12 Sep");
    assert_eq!(relative_time(evening, now, 8 * 3600), "13 Sep");
    // ...including which year it is.
    let last_year = When::from("2025-09-12T20:00:00Z").epoch_seconds().unwrap();
    assert_eq!(relative_time(last_year, now, 0), "12 Sep 2025");
    assert_eq!(relative_time(last_year, now, 8 * 3600), "13 Sep 2025");
}

#[test]
fn when_reads_both_shapes_evo_hands_out() {
    // The capture's own journal header, and the same instant as an epoch.
    let header = "2026-09-29T09:25:44Z";
    assert_eq!(When::Text(header.into()).epoch_seconds(), Some(1790673944));
    assert_eq!(When::Epoch(1790673944).epoch_seconds(), Some(1790673944));
    assert_eq!(When::from(1790673944).epoch_seconds(), Some(1790673944));
    assert_eq!(When::from(header).epoch_seconds(), Some(1790673944));
    assert_eq!(
        When::from(header.to_string()).epoch_seconds(),
        Some(1790673944)
    );

    // Seconds and a fractional part, and an offset that is not UTC: RFC3339 allows both,
    // and both have to mean the same instant.
    assert_eq!(
        When::Text(format!("{}.500Z", &header[..19])).epoch_seconds(),
        Some(1790673944)
    );
    assert_eq!(
        When::Text("2026-09-29T11:25:44+02:00".into()).epoch_seconds(),
        Some(1790673944)
    );
    assert_eq!(
        When::Text("2026-09-29T04:25:44-05:00".into()).epoch_seconds(),
        Some(1790673944)
    );

    // The epoch itself, and a leap day.
    assert_eq!(
        When::Text("1970-01-01T00:00:00Z".into()).epoch_seconds(),
        Some(0)
    );
    // A space where RFC3339 wants a `T` is what a hand-written header may look like.
    assert_eq!(
        When::Text("2026-09-29 09:25:44Z".into()).epoch_seconds(),
        Some(1790673944)
    );
    assert_eq!(
        When::Text("2000-02-29T12:00:00Z".into()).epoch_seconds(),
        Some(951825600)
    );

    // Anything else is not a timestamp, and the row then says nothing about the time.
    for bad in [
        "",
        "2026-09-29",
        "2026-13-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-09-29T24:00:00Z",
        "2026-09-29T09:25:60Z",
        "2026-09-29T09:25:44",
        "2026-09-29T09:25:44+2:00",
        "yesterday",
    ] {
        assert_eq!(When::Text(bad.into()).epoch_seconds(), None, "{bad:?}");
    }
}
