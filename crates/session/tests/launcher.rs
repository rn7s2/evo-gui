//! The empty tab's view model: what the controls resolve to, the plan they produce, and
//! the history rows.
//!
//! `catalog.json` is the body §5.6 writes, and the shape `evo-swarm catalog --json` prints
//! (with `lanes`); `catalog-two-providers.json` is the same body with one id registered
//! under two providers, which is what `--model id@provider` exists for. Cases neither
//! capture reaches — a catalog with no `lanes`, a ladder that lists only the retired rung —
//! are synthesized here, and each says what shape it copies.

mod common;

use common::fixture;
use serde_json::json;
use session::{
    history_rows, home_short, k_tokens, model_options, relative_time, thinking_levels,
    HistoryEntry, HistorySource, LaunchPlan, Launcher, Role, DEFAULT_WORKERS, WORKERS_MAX,
    WORKERS_MIN,
};

fn entry(path: &str, folder: &str) -> HistoryEntry {
    HistoryEntry {
        session_path: path.to_string(),
        folder: folder.to_string(),
        when: None,
        lanes: None,
        coordinator_model: None,
        lanes_model: None,
        swarm: true,
        source: HistorySource::Index,
        open_at_quit: false,
    }
}

/// A single agent's own session, as the history list takes it.
fn agent_entry(path: &str, folder: &str) -> HistoryEntry {
    HistoryEntry {
        swarm: false,
        ..entry(path, folder)
    }
}

/// §2, §7.2: which program a listed session is decides what a row's glyph says and what
/// resuming it opens — `evo-agent` for one agent, `evo-swarm` for a swarm — so the row
/// keeps it, whatever else it says.
#[test]
fn a_row_says_which_program_wrote_the_session() {
    let rows = history_rows(
        &[
            entry("/s/1.sexp", "/Users/you/coding/foo"),
            agent_entry("/s/2.sexp", "/Users/you/coding/bar"),
        ],
        0,
        0,
        Some("/Users/you"),
    );
    assert_eq!(rows.len(), 2);
    assert!(rows[0].swarm && !rows[1].swarm, "{rows:?}");
}

// --- the registrations ---------------------------------------------------------------

#[test]
fn every_registration_is_an_option_named_by_id_and_provider() {
    let options = model_options(&fixture("catalog.json"));
    assert_eq!(
        keys_of(&options),
        vec![
            "claude-opus-5@anthropic",
            "deepseek-v4.1-flash@acme",
            "claude-sonnet-5@proxy"
        ],
        "the catalog's own order"
    );
    let opus = &options[0];
    assert_eq!(opus.id, "claude-opus-5");
    assert_eq!(opus.provider, "anthropic");
    assert_eq!(
        opus.detail,
        "200k ctx · vision · effort low, medium, high, xhigh, max"
    );
    assert!(opus.ready && opus.lane_ok);
    // A model evo cannot reach says so in its own words, and a lane cannot register it
    // either.
    let sonnet = &options[2];
    assert!(!sonnet.ready);
    assert_eq!(sonnet.ready_reason.as_deref(), Some("no credential"));
    assert!(!sonnet.lane_ok);
    assert_eq!(
        sonnet.lane_reason.as_deref(),
        Some("api anthropic-oauth-messages is not in a lane")
    );
    // A registration without a provider is not one: `--model id@provider` needs both.
    let bare = model_options(&json!({"models": [{"id": "m"}, {"id": "n", "provider": "p"}]}));
    assert_eq!(keys_of(&bare), vec!["n@p"]);
}

fn keys_of(options: &[session::ModelOption]) -> Vec<&str> {
    options.iter().map(|m| m.key.as_str()).collect()
}

/// The menu's second line: the ctx window, the modalities, and the levels that
/// registration takes — `200k ctx · vision · effort low, high, max`. The `reasoning` flag
/// is not a word, so it still prints as nothing.
#[test]
fn a_models_detail_is_its_ctx_window_its_modalities_and_its_levels() {
    let options = model_options(&fixture("catalog.json"));
    assert_eq!(
        options[0].detail,
        "200k ctx · vision · effort low, medium, high, xhigh, max"
    );
    assert_eq!(
        options[1].detail, "936k ctx",
        "a model with no effort parameter names no levels, and nothing is invented for it"
    );
    assert_eq!(
        options[2].detail, "1M ctx · vision · effort low, high, max",
        "the model's own ladder, in full: not `1000k ctx`, and not a range"
    );
    // Nothing else in the body is a second line: no window, no detail.
    let bare = model_options(&json!({"models": [
        {"id": "m", "provider": "p", "reasoning": true},
        {"id": "n", "provider": "p", "context_window": 0}
    ]}));
    assert_eq!(bare[0].detail, "", "no API says a window was `0 ctx`");
    assert_eq!(bare[1].detail, "");
    // A server that predates `effort_levels` names none for any model: the line ends at
    // the modalities, exactly as a model with no effort parameter does.
    let old = model_options(&json!({"models": [
        {"id": "m", "provider": "p", "context_window": 200000, "images": true}
    ]}));
    assert_eq!(old[0].detail, "200k ctx · vision");
    assert!(old[0].effort_levels.is_empty());
}

/// A model's own ladder is carried beside its detail line, in the catalog's order, so a
/// chooser can offer the levels *that* model takes rather than the session's.
#[test]
fn a_model_carries_the_levels_it_takes() {
    let options = model_options(&fixture("catalog.json"));
    assert_eq!(
        options[0].effort_levels,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(
        options[1].effort_levels.is_empty(),
        "a model with no effort parameter offers none"
    );
    assert_eq!(
        options[2].effort_levels,
        ["low", "high", "max"],
        "a ladder that is not a run of levels is carried as it is"
    );
}

/// §5.6: a token count reads the way the design's own rows print it — in thousands, and
/// in whole millions once it is a thousand thousand (`1M ctx`, never `1000k ctx`). Both
/// steps round half to even, the TUI's own rule.
#[test]
fn a_token_count_reads_in_thousands_and_then_in_whole_millions() {
    for (tokens, reads) in [
        (0, "0k"),
        (200_000, "200k"),
        (372_000, "372k"),
        (936_000, "936k"),
        (999_000, "999k"),
        (999_499, "999k"),
        (999_500, "1M"),
        (1_000_000, "1M"),
        (1_048_576, "1M"),
        (1_500_000, "2M"),
    ] {
        assert_eq!(k_tokens(tokens), reads, "{tokens} tokens");
    }
}

/// Without a `lanes` list (an `evo-agent` body) evo has made no judgement about lanes,
/// and a model's own readiness answers for one too.
#[test]
fn a_catalog_without_lanes_judges_a_lane_by_the_models_own_readiness() {
    let mut body = fixture("catalog.json");
    body.as_object_mut().unwrap().remove("lanes");
    let options = model_options(&body);
    assert!(options[0].lane_ok, "ready");
    assert!(!options[2].lane_ok, "not ready");
}

/// The ladder a **launch** may carry: `/catalog.thinking_levels` without the retired rung,
/// and evo's own five when the catalog lists none.
#[test]
fn the_ladder_is_the_catalogs_without_the_retired_rung() {
    assert_eq!(
        thinking_levels(&fixture("catalog.json")),
        vec!["low", "medium", "high", "xhigh", "max"],
        "the catalog's own list"
    );
    // A body that lists none — or only the retired rung — is evo's own ladder.
    assert_eq!(
        thinking_levels(&json!({"thinking_levels": []})),
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        thinking_levels(&json!({"thinking_levels": ["off"]})),
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    // A body with its own list is believed.
    assert_eq!(
        thinking_levels(&json!({"thinking_levels": ["off", "high", "max"]})),
        vec!["high", "max"]
    );
}

// --- the resolved controls -----------------------------------------------------------

/// §7.2: the controls open on what evo would run now, and nothing is called "Default".
#[test]
fn the_controls_open_on_the_catalogs_own_resolution() {
    let mut launcher = Launcher::new();
    assert_eq!(launcher.workers(), DEFAULT_WORKERS);
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));
    assert_eq!(launcher.level(Role::Lanes), Some("medium"));

    launcher.set_catalog(&fixture("catalog.json"));
    // The catalog's own `default_model`, which both cards can run.
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("deepseek-v4.1-flash@acme")
    );
    assert_eq!(
        launcher.chosen_key(Role::Lanes),
        Some("deepseek-v4.1-flash@acme")
    );
    assert_eq!(launcher.unresolved_note(Role::Coordinator), None);
}

/// Without a `default_model` the first registration each card can run is the answer — and
/// the two cards answer differently, because a lane cannot run everything.
#[test]
fn without_a_default_each_card_takes_the_first_it_can_run() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&json!({
        "models": [
            {"id": "m", "provider": "p", "ready": true},
            {"id": "l", "provider": "p", "ready": true}
        ],
        "lanes": {"models": [
            {"id": "m", "provider": "p", "ok": false, "reason": "not an api a lane has"},
            {"id": "l", "provider": "p", "ok": true}
        ]}
    }));
    assert_eq!(launcher.chosen_key(Role::Coordinator), Some("m@p"));
    assert_eq!(launcher.chosen_key(Role::Lanes), Some("l@p"));
    assert_eq!(
        launcher
            .model("m@p")
            .and_then(|model| model.reason(Role::Lanes)),
        Some("not an api a lane has")
    );
}

/// What `check --json` resolved is what the launch would run, so it wins over the
/// catalog's own default — `ok` or not, with the check's own line saying why.
#[test]
fn what_the_check_resolved_is_what_the_fields_show() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": false,
        "model": {"id": "claude-opus-5", "provider": "anthropic", "ok": true},
        "lane_model": {"id": "claude-sonnet-5", "provider": "proxy", "ok": false,
                       "reason": "api anthropic-oauth-messages is not in a lane"},
        "problems": []
    }));
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("claude-opus-5@anthropic")
    );
    assert_eq!(
        launcher.chosen_key(Role::Lanes),
        Some("claude-sonnet-5@proxy"),
        "a model the check judged unusable is still the model it resolved"
    );
}

/// §7.2: the workers card's own switch. A tab opens on a swarm; off, the check's answer
/// is dropped — `--workers` and `--lane-model` are questions a single agent does not ask
/// — and the coordinator's field falls back to what the catalog resolves. Back on, the
/// check is asked again and resolves the fields the swarm's way.
#[test]
fn the_workers_switch_off_drops_the_checks_own_answer() {
    let mut launcher = Launcher::new();
    assert!(launcher.swarm(), "a tab opens on a swarm");
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": false,
        "model": {"id": "claude-sonnet-5", "provider": "proxy", "ok": false,
                  "reason": "no credential"},
        "lane_model": {"id": "claude-opus-5", "provider": "anthropic", "ok": true},
        "thinking": "xhigh",
        "lane_thinking": "low",
        "workers": 12,
        "problems": []
    }));
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("claude-sonnet-5@proxy"),
        "the check resolved the coordinator, unusable or not"
    );
    assert_eq!(launcher.workers(), 12);

    assert!(launcher.set_swarm(false));
    assert!(!launcher.swarm());
    assert!(
        !launcher.set_swarm(false),
        "setting it where it already is changes nothing"
    );
    // What the catalog resolves on its own: no check to read, and no lane judgement to
    // consult, so the coordinator is the catalog's own default registration.
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("deepseek-v4.1-flash@acme"),
        "the check's answer is gone with the swarm"
    );
    assert_eq!(
        launcher.workers(),
        DEFAULT_WORKERS,
        "and the count it resolved too"
    );
    // A check that arrives after the switch moved — the answer to a question asked
    // before it — is dropped rather than resolved against.
    assert!(!launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "claude-sonnet-5", "provider": "proxy", "ok": true},
        "lane_model": {"id": "deepseek-v4.1-flash", "provider": "acme", "ok": true},
        "workers": 12,
        "problems": []
    })));
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("deepseek-v4.1-flash@acme")
    );

    // Back on: the swarm's own answer resolves the fields again.
    assert!(launcher.set_swarm(true));
    assert!(launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "claude-sonnet-5", "provider": "proxy", "ok": false},
        "workers": 4,
        "problems": []
    })));
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("claude-sonnet-5@proxy")
    );
    assert_eq!(launcher.workers(), 4);
}

/// §7.2: the workers card's switch is part of the plan — which binary runs — and none of
/// the five flags: a single agent takes the coordinator's `--model` and `--thinking`
/// exactly as a swarm does, and no lane flag at all.
#[test]
fn the_plan_carries_the_program_the_switch_picked() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    assert!(launcher.plan().swarm, "a tab opens on a swarm");
    assert!(launcher.plan().is_default(), "which is still nobody's hand");

    launcher.set_swarm(false);
    assert!(
        !launcher.plan().swarm,
        "and the switch is the plan's program"
    );
    assert!(
        launcher.plan().is_default(),
        "a switch is not one of the five: a check is asked about the same bare launch"
    );

    launcher.choose(Role::Coordinator, "claude-opus-5@anthropic");
    launcher.set_workers(3);
    let agent = launcher.plan();
    assert!(!agent.swarm);
    assert_eq!(
        agent.model,
        Some(("claude-opus-5".to_string(), "anthropic".to_string())),
        "the coordinator's own pick is passed either way"
    );
    assert_eq!(
        agent.workers,
        Some(3),
        "and what the person typed is the plan's"
    );
}

/// §2: `check --json` resolves the two efforts and the count as a launch with no flags
/// would — evo's own chains, including a resumed swarm's record — so the controls open on
/// those rather than on a rung or a count this app picked.
#[test]
fn a_check_opens_the_sliders_and_the_count_on_what_it_resolved() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "deepseek-v4.1-flash", "provider": "acme", "ok": true},
        "lane_model": {"id": "deepseek-v4.1-flash", "provider": "acme", "ok": true},
        "thinking": "xhigh",
        "lane_thinking": "low",
        "workers": 12,
        "problems": []
    }));
    assert_eq!(launcher.level(Role::Coordinator), Some("xhigh"));
    assert_eq!(launcher.level(Role::Lanes), Some("low"));
    assert_eq!(launcher.workers(), 12);
    // The controls show what the check resolved, and pass it to nobody: the launch and the
    // next check go out with no flags, and evo resolves these same values itself (§9).
    assert!(launcher.plan().is_default(), "shown, not passed");

    // A count off the wire is clamped like one typed: a launch may carry 1–64.
    launcher.set_check(&json!({"ok": true, "workers": 999, "thinking": "max"}));
    assert_eq!(launcher.workers(), WORKERS_MAX);
    assert_eq!(launcher.level(Role::Coordinator), Some("max"));

    // A rung the ladder does not list leaves the slider on its middle, rather than on a
    // level no flag could carry.
    launcher.set_check(&json!({"ok": true, "thinking": "extreme"}));
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));

    // And a ladder that changed under a resolved rung moves the slider onto it.
    launcher.set_catalog(&json!({"models": [], "thinking_levels": ["low", "high"]}));
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("high"),
        "the middle of two"
    );
}

/// The workers card's switch and the coordinator's effort (§7.2): off, the level is the one
/// a single agent's own catalog publishes for a fresh session — `default_thinking`, which
/// `evo-agent catalog --json` now carries. It used to be the ladder's middle rung, which is
/// what a client with nothing to read would guess.
#[test]
fn the_switch_off_opens_the_coordinator_on_the_agents_own_level() {
    let swarm = |level: &str| {
        json!({
            "models": [{"id": "m", "provider": "p", "ready": true}],
            "default_model": {"id": "m", "provider": "p"},
            "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
            "default_thinking": level
        })
    };
    let mut launcher = Launcher::new();
    launcher.set_catalog(&swarm("high"));
    launcher.set_check(&json!({"ok": true, "thinking": "low", "lane_thinking": "low"}));
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("low"),
        "on, the swarm's own `check` resolution"
    );

    // Off: the page reads `evo-agent catalog --json`, and the card opens on what it says.
    assert!(launcher.set_swarm(false));
    launcher.set_catalog(&swarm("max"));
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("max"),
        "a single agent opens on the level its own catalog publishes"
    );
    assert_eq!(
        launcher.level(Role::Lanes),
        Some("medium"),
        "a lane's level is the swarm's question, which a single agent does not answer"
    );
    assert!(launcher.plan().is_default(), "shown, not passed");

    // A check that arrives while the switch is off is not this program's answer.
    assert!(
        !launcher.set_check(&json!({"ok": true, "thinking": "xhigh"})),
        "a single agent has no check to ask"
    );
    assert_eq!(launcher.level(Role::Coordinator), Some("max"));

    // Back on: the swarm's own resolution, asked for again.
    assert!(launcher.set_swarm(true));
    launcher.set_catalog(&swarm("high"));
    launcher.set_check(&json!({"ok": true, "thinking": "xhigh", "lane_thinking": "low"}));
    assert_eq!(launcher.level(Role::Coordinator), Some("xhigh"));
    assert_eq!(launcher.level(Role::Lanes), Some("low"));

    // And off once more, which is the toggle the person did.
    assert!(launcher.set_swarm(false));
    launcher.set_catalog(&swarm("max"));
    assert_eq!(launcher.level(Role::Coordinator), Some("max"));
}

/// A program that predates the field says nothing, and nothing is invented from it: the
/// middle rung stands, exactly as it did before `default_thinking` existed.
#[test]
fn a_catalog_without_a_default_level_leaves_the_middle_rung() {
    let mut launcher = Launcher::new();
    launcher.set_swarm(false);
    launcher.set_catalog(&json!({
        "models": [{"id": "m", "provider": "p", "ready": true}],
        "default_model": {"id": "m", "provider": "p"},
        "thinking_levels": ["low", "medium", "high"]
    }));
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));

    // A level the ladder does not list is not a rung either: the slider stays on the
    // middle rather than on a level no flag could carry.
    launcher.set_catalog(&json!({
        "models": [{"id": "m", "provider": "p", "ready": true}],
        "thinking_levels": ["low", "medium", "high"],
        "default_thinking": "extreme"
    }));
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));
}

/// A slider the person moved is theirs: neither the switch nor a catalog that says
/// something else moves it, in either direction — and it is what the launch passes.
#[test]
fn a_slider_the_person_moved_survives_the_switch_and_the_catalog() {
    let agent = json!({
        "models": [{"id": "m", "provider": "p", "ready": true}],
        "default_model": {"id": "m", "provider": "p"},
        "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
        "default_thinking": "max"
    });
    let mut launcher = Launcher::new();
    launcher.set_swarm(false);
    launcher.set_catalog(&agent);
    assert_eq!(launcher.level(Role::Coordinator), Some("max"));
    // A person drags it down to `high`.
    assert!(launcher.set_effort(Role::Coordinator, 2));
    assert_eq!(launcher.level(Role::Coordinator), Some("high"));

    // The catalog answers again — another program's level, and this one's — and the
    // slider does not move.
    launcher.set_catalog(&json!({
        "models": [{"id": "m", "provider": "p", "ready": true}],
        "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
        "default_thinking": "low"
    }));
    assert_eq!(launcher.level(Role::Coordinator), Some("high"));

    // On, where the swarm resolves a level of its own.
    launcher.set_swarm(true);
    launcher.set_check(&json!({"ok": true, "thinking": "xhigh", "lane_thinking": "xhigh"}));
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("high"),
        "the person's rung stands over the check's"
    );
    assert_eq!(
        launcher.plan().thinking.as_deref(),
        Some("high"),
        "and it is what the launch passes as `--thinking`"
    );
    assert_eq!(
        launcher.level(Role::Lanes),
        Some("xhigh"),
        "the other card follows"
    );

    // Back off: still theirs.
    launcher.set_swarm(false);
    launcher.set_catalog(&agent);
    assert_eq!(launcher.level(Role::Coordinator), Some("high"));
    assert_eq!(launcher.plan().thinking.as_deref(), Some("high"));
}

/// A control the person moved is theirs: a later check leaves it where they put it, while
/// the one beside it follows.
#[test]
fn a_control_the_person_moved_is_not_re_resolved() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher
        .set_check(&json!({"ok": true, "thinking": "low", "lane_thinking": "low", "workers": 3}));
    assert_eq!(launcher.level(Role::Lanes), Some("low"));

    launcher.set_effort(Role::Coordinator, 4);
    launcher.set_workers(9);
    launcher
        .set_check(&json!({"ok": true, "thinking": "xhigh", "lane_thinking": "max", "workers": 6}));
    assert_eq!(
        launcher.level(Role::Coordinator),
        Some("max"),
        "the slider nobody touched follows the check; the moved one stays"
    );
    assert_eq!(launcher.workers(), 9);
    assert_eq!(launcher.level(Role::Lanes), Some("max"));

    // And the values the launcher resolved are still re-resolved after a refresh.
    launcher
        .set_check(&json!({"ok": true, "thinking": "high", "lane_thinking": "high", "workers": 4}));
    assert_eq!(launcher.level(Role::Lanes), Some("high"));
    assert_eq!(launcher.level(Role::Coordinator), Some("max"));
    assert_eq!(launcher.workers(), 9);
}

/// A check from an evo that did not carry those three — or none at all — leaves the
/// controls on evo's own last values for that frame.
#[test]
fn a_check_without_resolved_values_leaves_evos_own() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "claude-opus-5", "provider": "anthropic", "ok": true},
        "problems": []
    }));
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));
    assert_eq!(launcher.level(Role::Lanes), Some("medium"));
    assert_eq!(launcher.workers(), DEFAULT_WORKERS);

    // Those three are part of what the window renders: a check that carries them tells it
    // to paint again.
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    assert!(launcher.set_check(&json!({
        "ok": true,
        "thinking": "high",
        "lane_thinking": "high",
        "workers": 2
    })));
    assert!(!launcher.set_check(&json!({
        "ok": true,
        "thinking": "high",
        "lane_thinking": "high",
        "workers": 2
    })));
}

/// A check that resolved nothing — the empty home — leaves the fields empty, and they say
/// so in evo's own words rather than inventing a model.
#[test]
fn a_check_that_resolved_nothing_says_so_in_evos_words() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&json!({"models": [], "thinking_levels": []}));
    launcher.set_check(&json!({
        "ok": null,
        "model": {"id": null, "provider": null, "ok": null, "reason": "no model is configured"},
        "lane_model": {"id": null, "provider": null, "ok": null, "reason": "no model is configured"},
        "problems": [
            {"code": "model_unresolved", "message": "the model the swarm would run is not usable: no model is configured"}
        ]
    }));
    assert_eq!(launcher.chosen_key(Role::Coordinator), None);
    assert_eq!(
        launcher.unresolved_note(Role::Coordinator).as_deref(),
        Some("no model is configured")
    );
    // A field with nothing at all to go on still says something.
    let mut bare = Launcher::new();
    bare.set_check(&json!({"ok": null, "problems": []}));
    assert_eq!(
        bare.unresolved_note(Role::Lanes).as_deref(),
        Some("No models are registered")
    );
}

/// A choice stands until the registration leaves the catalog: a refresh that still lists
/// it leaves the choice alone, and one that dropped it re-resolves.
#[test]
fn a_choice_outlives_a_refresh_and_falls_back_when_it_must() {
    let mut launcher = Launcher::new();
    let catalog = fixture("catalog.json");
    launcher.set_catalog(&catalog);
    assert!(launcher.choose(Role::Coordinator, "claude-opus-5@anthropic"));
    assert!(!launcher.choose(Role::Coordinator, "nope"), "unknown key");
    assert!(
        !launcher.choose(Role::Coordinator, "claude-opus-5@anthropic"),
        "the same choice again"
    );
    assert!(
        !launcher.set_catalog(&catalog),
        "the same body changes nothing"
    );
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("claude-opus-5@anthropic")
    );

    launcher.set_catalog(&json!({"models": [{"id": "other", "provider": "p", "ready": true}]}));
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("other@p"),
        "the choice left the catalog, so the card re-resolved"
    );
}

/// The count clamps to what a launch may carry, and the slider clamps to its ladder.
#[test]
fn the_count_and_the_slider_clamp_to_what_a_launch_allows() {
    let mut launcher = Launcher::new();
    assert!(launcher.set_workers(4));
    assert_eq!(launcher.workers(), 4);
    assert!(!launcher.set_workers(4));
    launcher.set_workers(0);
    assert_eq!(launcher.workers(), WORKERS_MIN, "a count below one is one");
    launcher.set_workers(u16::MAX);
    assert_eq!(launcher.workers(), WORKERS_MAX, "and above 64 is 64");

    let levels = launcher.levels().len();
    assert!(launcher.set_effort(Role::Lanes, levels - 1));
    assert_eq!(launcher.level(Role::Lanes), Some("max"));
    assert!(!launcher.set_effort(Role::Lanes, levels - 1));
    launcher.set_effort(Role::Lanes, levels + 9);
    assert_eq!(
        launcher.level(Role::Lanes),
        Some("max"),
        "an index past the ladder is its end"
    );
    // The two sliders are their own: one card's rung is not the other's.
    assert_eq!(launcher.effort(Role::Coordinator), 1, "medium");
}

/// §1, §9: the plan is the flags a **person** set, and only those. A control evo resolved
/// passes nothing, so a launch (and the check about it) leaves it to evo — which is what
/// keeps the page's numbers evo's own rather than this app's guesses echoed back.
#[test]
fn the_plan_carries_only_what_a_person_set() {
    let mut launcher = Launcher::new();
    assert!(
        launcher.plan().is_default(),
        "a fresh tab passes nothing: evo resolves all five itself"
    );

    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "claude-opus-5", "provider": "anthropic", "ok": true},
        "lane_model": {"id": "deepseek-v4.1-flash", "provider": "acme", "ok": true},
        "thinking": "low",
        "lane_thinking": "medium",
        "workers": 9,
        "problems": []
    }));
    // The check has answered and the controls show what it resolved — exactly what a
    // launch with no flags gets, so not one of them becomes a flag.
    assert_eq!(
        launcher.chosen_key(Role::Coordinator),
        Some("claude-opus-5@anthropic")
    );
    assert_eq!(launcher.workers(), 9);
    assert!(
        launcher.plan().is_default(),
        "the shown values are not flags"
    );

    // One control at a time, and only that one appears.
    assert!(
        launcher.choose(Role::Coordinator, "claude-opus-5@anthropic"),
        "a hand on the value the card already showed is still a hand"
    );
    let plan = launcher.plan();
    assert_eq!(
        plan.model,
        Some(("claude-opus-5".to_string(), "anthropic".to_string()))
    );
    assert_eq!(plan.thinking, None);
    assert_eq!(
        plan.workers, None,
        "the count evo resolved is not handed back"
    );
    assert_eq!(plan.lanes_model, None);
    assert_eq!(plan.lane_thinking, None);

    launcher.set_effort(Role::Lanes, 4);
    let plan = launcher.plan();
    assert_eq!(plan.lane_thinking.as_deref(), Some("max"));
    assert_eq!(plan.thinking, None, "the card nobody moved is left to evo");

    launcher.set_workers(4);
    let plan = launcher.plan();
    assert_eq!(plan.workers, Some(4));
    assert_eq!(plan.lanes_model, None, "still nobody's pick");

    launcher.choose(Role::Lanes, "deepseek-v4.1-flash@acme");
    launcher.set_effort(Role::Coordinator, 2);
    let plan = launcher.plan();
    assert_eq!(
        plan.lanes_model,
        Some(("deepseek-v4.1-flash".to_string(), "acme".to_string()))
    );
    assert_eq!(plan.thinking.as_deref(), Some("high"));
    assert!(!plan.is_default());

    // The pair becomes the spec the flag takes (§1).
    assert_eq!(
        LaunchPlan::spec(plan.model.as_ref().unwrap()),
        "claude-opus-5@anthropic"
    );
    assert_eq!(
        LaunchPlan::spec(plan.lanes_model.as_ref().unwrap()),
        "deepseek-v4.1-flash@acme"
    );

    // A person's pick stays theirs: a later check moves what it resolved and leaves the
    // four flags where they were put.
    launcher.set_check(&json!({
        "ok": true,
        "model": {"id": "deepseek-v4.1-flash", "provider": "acme"},
        "lane_model": {"id": "deepseek-v4.1-flash", "provider": "acme"},
        "thinking": "low",
        "workers": 9
    }));
    let plan = launcher.plan();
    assert_eq!(
        plan.model,
        Some(("claude-opus-5".to_string(), "anthropic".to_string()))
    );
    assert_eq!(plan.thinking.as_deref(), Some("high"));
    assert_eq!(plan.workers, Some(4));
    assert_eq!(plan.lane_thinking.as_deref(), Some("max"));
}

/// A check that failed resolves nothing, so the controls show evo's own last fallback —
/// and still pass nothing, which is the same thing evo would do with no flags (§9).
#[test]
fn a_failed_check_leaves_the_shown_fallbacks_out_of_the_plan() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.set_check(&json!({
        "ok": null,
        "model": {"id": null, "provider": null, "reason": "no model is configured"},
        "problems": [{"code": "model_unresolved", "message": "no model is configured"}]
    }));
    // The fallbacks are on show...
    assert_eq!(launcher.workers(), DEFAULT_WORKERS);
    assert_eq!(launcher.level(Role::Coordinator), Some("medium"));
    // ...and out of the plan: evo runs its own default, which is what they stand for.
    assert!(launcher.plan().is_default());
}

// --- history (§2) -------------------------------------------------------------------

#[test]
fn history_rows_name_a_session_by_its_folder() {
    let rows = history_rows(
        &[
            HistoryEntry {
                when: Some(1_790_674_196),
                lanes: Some(6),
                coordinator_model: Some("claude-opus-5@anthropic".to_string()),
                ..entry("/j/1.sexp", "/Users/you/coding/foo/")
            },
            entry("/j/2.sexp", "/Users/you/coding/bar/"),
        ],
        1_790_674_196 + 7200,
        0,
        Some("/Users/you"),
    );
    assert_eq!(rows[0].title, "foo");
    assert_eq!(rows[0].folder_short, "~/coding/foo");
    assert_eq!(rows[0].when, "2h ago");
    // The tooltip carries the absolute path, the instant in the caller's zone and the
    // app's own record.
    assert_eq!(
        rows[0].tooltip,
        "/Users/you/coding/foo · 1.sexp · 2026-09-29 09:29:56 +00:00 · \
         coordinator: claude-opus-5@anthropic · 6 lanes"
    );
    // No time, no lanes: the folder names the row and the time says nothing rather than
    // guessing.
    assert_eq!(rows[1].title, "bar");
    assert_eq!(rows[1].folder_short, "~/coding/bar");
    assert_eq!(rows[1].when, "");
    assert_eq!(rows[1].tooltip, "/Users/you/coding/bar · 2.sexp");
}

#[test]
fn history_rows_merge_the_index_with_the_apps_own_recents() {
    let rows = history_rows(
        &[
            HistoryEntry {
                source: HistorySource::Recent,
                when: Some(100),
                lanes: Some(4),
                coordinator_model: Some("m1".to_string()),
                open_at_quit: true,
                ..entry("/j/1.sexp", "/f")
            },
            HistoryEntry {
                when: Some(200),
                ..entry("/j/1.sexp", "/f")
            },
        ],
        200,
        0,
        None,
    );
    assert_eq!(rows.len(), 1, "one session, not two rows");
    // The newer of the two is the base, and what only one side knows survives.
    assert_eq!(rows[0].when, "just now");
    assert_eq!(rows[0].title, "f");
    assert!(rows[0].open_at_quit);
    assert_eq!(rows[0].source, HistorySource::Index);
    assert!(rows[0].tooltip.contains("4 lanes"), "{}", rows[0].tooltip);
}

#[test]
fn the_launcher_keeps_the_rows_it_was_given() {
    let mut launcher = Launcher::new();
    assert!(launcher.history().is_empty());
    let entries = vec![HistoryEntry {
        when: Some(1_790_674_196),
        ..entry("/j/1.sexp", "/Users/you/coding/foo")
    }];
    assert!(launcher.set_history(&entries, 1_790_674_196, 8 * 3600, Some("/Users/you")));
    assert_eq!(launcher.history().len(), 1);
    assert_eq!(launcher.history()[0].title, "foo");
    assert_eq!(launcher.history()[0].folder_short, "~/coding/foo");
    assert!(!launcher.set_history(&entries, 1_790_674_196, 8 * 3600, Some("/Users/you")));
}

#[test]
fn relative_times_are_phrased_against_the_callers_clock() {
    let now = 1_790_674_196;
    assert_eq!(relative_time(now, now, 0), "just now");
    assert_eq!(relative_time(now - 59, now, 0), "just now");
    assert_eq!(relative_time(now - 300, now, 0), "5m ago");
    assert_eq!(relative_time(now - 7200, now, 0), "2h ago");
    assert_eq!(relative_time(now - 90_000, now, 0), "yesterday");
    assert_eq!(relative_time(now - 3 * 86_400, now, 0), "3d ago");
    let week = relative_time(now - 8 * 86_400, now, 0);
    assert_eq!(week, "21 Sep", "{week}");
    assert_eq!(relative_time(now - 400 * 86_400, now, 0), "25 Aug 2025");
}

#[test]
fn home_shortening_keeps_a_path_that_is_not_under_home() {
    assert_eq!(
        home_short("/Users/you/coding/foo", Some("/Users/you")),
        "~/coding/foo"
    );
    assert_eq!(home_short("/Users/you", Some("/Users/you/")), "~");
    assert_eq!(home_short("/opt/x", Some("/Users/you")), "/opt/x");
    assert_eq!(home_short("/opt/x", None), "/opt/x");
    assert_eq!(home_short("/Users/y", Some("/Users/you")), "/Users/y");
}

#[test]
fn a_folder_outside_home_reads_without_its_trailing_slash() {
    assert_eq!(
        home_short("/private/tmp/run/", Some("/Users/me")),
        "/private/tmp/run"
    );
    assert_eq!(
        home_short("/Users/me/coding/evo/", Some("/Users/me")),
        "~/coding/evo"
    );
    assert_eq!(home_short("/Users/me/", Some("/Users/me")), "~");
    assert_eq!(home_short("/", Some("/Users/me")), "/");
    assert_eq!(home_short("/opt/x/", None), "/opt/x");
}
