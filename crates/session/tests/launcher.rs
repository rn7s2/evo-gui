//! The empty tab's view model: the choosers built from a `/catalog` body, the launch plan
//! they produce, the check's problems, and the history rows.
//!
//! `catalog.json` is the body §5.6 writes, and the shape `evo-swarm catalog --json` prints
//! (with `lanes`); `catalog-two-providers.json` is the same body with one id registered
//! under two providers, which is what `--model id@provider` exists for. Cases neither
//! capture reaches — a catalog with no `lanes`, a level list that is empty — are
//! synthesized here, and each says what shape it copies.

mod common;

use common::fixture;
use serde_json::json;
use session::{
    coordinator_chooser, history_rows, home_short, lanes_chooser, relative_time,
    thinking_chooser, workers_chooser, Choice, Chooser, HistoryEntry, HistorySource, LaunchPlan,
    Launcher, Problem, ProblemTarget, DEFAULT_KEY, WORKERS_MAX,
};

fn labels(chooser: &Chooser) -> Vec<&str> {
    chooser
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect()
}

fn entry(path: &str, folder: &str) -> HistoryEntry {
    HistoryEntry {
        session_path: path.to_string(),
        folder: folder.to_string(),
        title: String::new(),
        when: None,
        lanes: None,
        coordinator_model: None,
        lanes_model: None,
        source: HistorySource::Index,
        open_at_quit: false,
    }
}

// --- choosers ----------------------------------------------------------------------

#[test]
fn the_coordinator_chooser_lists_every_registration_after_default() {
    let chooser = coordinator_chooser(&fixture("catalog.json"));
    assert_eq!(
        labels(&chooser),
        // Sorted by provider, then id.
        vec![
            "Default",
            "ark-deepseek-v4.1-flash",
            "claude-opus-5",
            "claude-sonnet-5"
        ]
    );
    assert_eq!(chooser.options[0].key, DEFAULT_KEY);
    assert_eq!(chooser.options[0].detail, "evo's own default");
    assert_eq!(chooser.options[0].model(), None);

    let opus = chooser
        .option("claude-opus-5@anthropic")
        .expect("the key names id and provider");
    assert_eq!(opus.label, "claude-opus-5");
    assert_eq!(opus.detail, "200k ctx · vision · reasons");
    assert_eq!(
        opus.model(),
        Some(("claude-opus-5".to_string(), "anthropic".to_string()))
    );
    assert!(opus.available && opus.unavailable_reason.is_none());
    assert_eq!(chooser.models().count(), 3, "Default is not a model");
    assert_eq!(chooser.index_of("nope"), None);
}

/// §1: `--model id@provider` names one registration, so two registrations of one id are
/// two choosable options — each labelled with the provider it runs.
#[test]
fn one_id_under_two_providers_is_two_options() {
    let chooser = coordinator_chooser(&fixture("catalog-two-providers.json"));
    assert_eq!(
        labels(&chooser),
        vec!["Default", "stub-a (stub)", "stub-b", "stub-a (stub2)"]
    );
    let first = chooser.option("stub-a@stub").expect("stub");
    let second = chooser.option("stub-a@stub2").expect("stub2");
    assert_eq!(
        first.model(),
        Some(("stub-a".to_string(), "stub".to_string()))
    );
    assert_eq!(
        second.model(),
        Some(("stub-a".to_string(), "stub2".to_string()))
    );
    assert_ne!(first.key, second.key);
    assert_eq!(first.detail, "200k ctx · vision · reasons");
    assert_eq!(second.detail, "100k ctx · vision · reasons");
}

/// A model evo cannot reach is visible and not choosable: it says so in evo's own words
/// (§5.6's `ready` / `reason`).
#[test]
fn an_unready_model_says_why_and_cannot_be_chosen() {
    let chooser = coordinator_chooser(&fixture("catalog-two-providers.json"));
    let stub_b = chooser.option("stub-b@stub").expect("stub-b");
    assert!(!stub_b.available);
    assert_eq!(
        stub_b.unavailable_reason.as_deref(),
        Some("no credential for provider stub")
    );
    // A model with no reason of its own still says what is wrong.
    let bare = coordinator_chooser(&json!({"models": [
        {"id": "m", "provider": "p", "api": "x", "ready": false}
    ]}));
    assert_eq!(
        bare.option("m@p").unwrap().unavailable_reason.as_deref(),
        Some("not ready in this session")
    );
}

/// The lanes chooser greys out exactly what the catalog's own `lanes.models` says a lane
/// cannot register (§5.6, §9) — no api-set guessing.
#[test]
fn the_lanes_chooser_greys_out_what_a_lane_cannot_register() {
    let chooser = lanes_chooser(&fixture("catalog.json"));
    assert_eq!(chooser.options[0].detail, "follows the coordinator");
    assert!(chooser.option("claude-opus-5@anthropic").unwrap().available);
    assert!(chooser
        .option("ark-deepseek-v4.1-flash@aiden")
        .unwrap()
        .available);
    let blocked = chooser.option("claude-sonnet-5@proxy").unwrap();
    assert!(!blocked.available);
    assert_eq!(
        blocked.unavailable_reason.as_deref(),
        Some("api anthropic-oauth-messages is not in a lane")
    );
}

/// A catalog with no `lanes` (an `evo-agent` body) has made no judgement about lanes:
/// nothing is greyed out on its word.
#[test]
fn a_catalog_without_lanes_greys_nothing_out_on_that_account() {
    let mut body = fixture("catalog.json");
    body.as_object_mut().unwrap().remove("lanes");
    let chooser = lanes_chooser(&body);
    assert_eq!(chooser.models().count(), 3);
    // …and the only thing left that can stop an option is evo's own readiness, which is
    // the same judgement the coordinator's chooser makes.
    assert!(chooser.option("claude-opus-5@anthropic").unwrap().available);
    assert!(!chooser.option("claude-sonnet-5@proxy").unwrap().available);
}

#[test]
fn the_thinking_chooser_is_the_catalogs_levels_or_the_contracts_five() {
    let chooser = thinking_chooser(&fixture("catalog.json"));
    assert_eq!(
        labels(&chooser),
        vec!["Default", "off", "low", "medium", "high", "xhigh"]
    );
    assert_eq!(chooser.options[0].detail, "as the model is configured");
    assert_eq!(chooser.option("high").unwrap().level(), Some("high"));
    assert_eq!(chooser.option(DEFAULT_KEY).unwrap().level(), None);

    // A body that lists none falls back to §5.6's set.
    let bare = thinking_chooser(&json!({"models": [], "thinking_levels": []}));
    assert_eq!(bare.options.len(), 6);
    // …and a body with its own list is believed.
    let two = thinking_chooser(&json!({"thinking_levels": ["off", "high"]}));
    assert_eq!(labels(&two), vec!["Default", "off", "high"]);
}

#[test]
fn the_workers_chooser_runs_to_the_caps_own_limit() {
    let chooser = workers_chooser();
    assert_eq!(chooser.options.len(), WORKERS_MAX as usize + 1);
    assert_eq!(chooser.options[0].key, DEFAULT_KEY);
    assert_eq!(chooser.options[0].label, "Default");
    assert_eq!(chooser.options[1].key, "1");
    assert_eq!(chooser.options[WORKERS_MAX as usize].key, "64");
    assert!(chooser.options.iter().all(|option| option.available));
}

// --- the launch plan ----------------------------------------------------------------

#[test]
fn the_plan_names_the_flags_a_launch_passes() {
    let mut launcher = Launcher::new();
    assert!(launcher.plan().is_default(), "Default passes no flag at all");

    assert!(launcher.set_catalog(&fixture("catalog.json")));
    assert!(launcher.select(Choice::Coordinator, "claude-opus-5@anthropic"));
    assert!(launcher.select(Choice::Lanes, "ark-deepseek-v4.1-flash@aiden"));
    assert!(launcher.select(Choice::LaneThinking, "high"));
    assert!(launcher.select(Choice::Workers, "4"));
    let plan = launcher.plan();
    assert_eq!(
        plan.model,
        Some(("claude-opus-5".to_string(), "anthropic".to_string()))
    );
    assert_eq!(
        plan.lanes_model,
        Some(("ark-deepseek-v4.1-flash".to_string(), "aiden".to_string()))
    );
    assert_eq!(plan.lane_thinking.as_deref(), Some("high"));
    assert_eq!(plan.workers, Some(4));
    assert!(!plan.is_default());

    // The pair becomes the spec the flag takes (§1).
    assert_eq!(
        LaunchPlan::spec(plan.model.as_ref().unwrap()),
        "claude-opus-5@anthropic"
    );
    assert_eq!(
        LaunchPlan::spec(plan.lanes_model.as_ref().unwrap()),
        "ark-deepseek-v4.1-flash@aiden"
    );
}

#[test]
fn choosing_nothing_leaves_each_field_at_default() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    launcher.select(Choice::Coordinator, "claude-opus-5@anthropic");
    launcher.select(Choice::Coordinator, DEFAULT_KEY);
    assert_eq!(launcher.selected_key(Choice::Coordinator), DEFAULT_KEY);
    assert!(launcher.plan().is_default());
    assert!(launcher.selected(Choice::Coordinator).is_some());
    assert_eq!(launcher.selected(Choice::Coordinator).unwrap().model(), None);
}

#[test]
fn a_selection_a_rebuilt_chooser_lost_falls_back_to_default() {
    let mut launcher = Launcher::new();
    launcher.set_catalog(&fixture("catalog.json"));
    assert!(launcher.select(Choice::Coordinator, "claude-opus-5@anthropic"));
    // The next catalog does not list that registration any more.
    launcher.set_catalog(&json!({"models": [{"id": "other", "provider": "p", "ready": true}]}));
    assert_eq!(launcher.selected_key(Choice::Coordinator), DEFAULT_KEY);
    assert_eq!(launcher.chooser(Choice::Coordinator).options.len(), 2);
    // An unknown key is ignored, and so is a choice that changes nothing.
    assert!(!launcher.select(Choice::Coordinator, "nope"));
    assert!(!launcher.select(Choice::Coordinator, DEFAULT_KEY));
}

#[test]
fn setting_the_same_catalog_twice_changes_nothing() {
    let mut launcher = Launcher::new();
    let catalog = fixture("catalog.json");
    assert!(launcher.set_catalog(&catalog), "from nothing to a catalog");
    assert!(!launcher.set_catalog(&catalog), "the same body again");
}

// --- the check's problems (§9) -------------------------------------------------------

#[test]
fn a_problem_is_one_line_and_knows_where_a_click_goes() {
    let problem = Problem {
        code: "lane_model_not_ready".to_string(),
        message: "a lane cannot register claude-sonnet-5\nit needs another api".to_string(),
    };
    assert_eq!(
        problem.line(),
        "a lane cannot register claude-sonnet-5 it needs another api"
    );
    assert_eq!(problem.target(), ProblemTarget::Lanes);
    assert_eq!(problem.target().choice(), Some(Choice::Lanes));

    let cases = [
        ("model_not_ready", ProblemTarget::Coordinator),
        ("lane_model_not_ready", ProblemTarget::Lanes),
        ("lane_thinking_unknown", ProblemTarget::LaneThinking),
        ("workers_out_of_range", ProblemTarget::Workers),
        ("swarm_binary_missing", ProblemTarget::Settings),
    ];
    for (code, target) in cases {
        let problem = Problem {
            code: code.to_string(),
            message: "…".to_string(),
        };
        assert_eq!(problem.target(), target, "{code}");
    }
    assert_eq!(ProblemTarget::Settings.choice(), None);
    // A problem with no message still says something: its code.
    assert_eq!(
        Problem {
            code: "op_failed".to_string(),
            message: String::new()
        }
        .line(),
        "op_failed"
    );
}

#[test]
fn the_launcher_carries_the_problems_and_clears_them() {
    let mut launcher = Launcher::new();
    assert!(launcher.problems().is_empty(), "nothing has been checked");
    let problems = vec![Problem {
        code: "model_not_ready".to_string(),
        message: "claude-sonnet-5 has no credential".to_string(),
    }];
    assert!(launcher.set_problems(&problems));
    assert_eq!(launcher.problems().len(), 1);
    assert_eq!(
        launcher.problems()[0].line(),
        "claude-sonnet-5 has no credential"
    );
    // The same answer again is not a redraw; a clean check clears the lines.
    assert!(!launcher.set_problems(&problems));
    assert!(launcher.set_problems(&[]));
    assert!(launcher.problems().is_empty());
}

// --- history (§2) -------------------------------------------------------------------

#[test]
fn history_rows_name_a_session_by_its_title_or_its_folder() {
    let rows = history_rows(
        &[
            HistoryEntry {
                title: "make the empty tab read the index".to_string(),
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
    assert_eq!(rows[0].title, "make the empty tab read the index");
    assert_eq!(rows[0].subtitle, "~/coding/foo");
    assert_eq!(
        rows[0].meta,
        "6 lanes · 2h ago · coordinator: claude-opus-5@anthropic"
    );
    // The tooltip carries the absolute path, the instant in the caller's zone and the
    // app's own record.
    assert_eq!(
        rows[0].tooltip,
        "/Users/you/coding/foo · 1.sexp · 2026-09-29 09:29:56 +00:00 · \
         coordinator: claude-opus-5@anthropic · 6 lanes"
    );
    // No title, no time, no lanes: the folder names the row and the meta line says
    // nothing rather than guessing.
    assert_eq!(rows[1].title, "bar");
    assert_eq!(rows[1].subtitle, "~/coding/bar");
    assert_eq!(rows[1].meta, "");
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
                title: "index title".to_string(),
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
    assert_eq!(rows[0].meta, "4 lanes · just now · coordinator: m1");
    assert_eq!(rows[0].title, "index title");
    assert!(rows[0].open_at_quit);
    assert_eq!(rows[0].source, HistorySource::Index);
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
    assert_eq!(launcher.history()[0].subtitle, "~/coding/foo");
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
    assert_eq!(home_short("/Users/you/coding/foo", Some("/Users/you")), "~/coding/foo");
    assert_eq!(home_short("/Users/you", Some("/Users/you/")), "~");
    assert_eq!(home_short("/opt/x", Some("/Users/you")), "/opt/x");
    assert_eq!(home_short("/opt/x", None), "/opt/x");
    assert_eq!(home_short("/Users/y", Some("/Users/you")), "/Users/y");
}
