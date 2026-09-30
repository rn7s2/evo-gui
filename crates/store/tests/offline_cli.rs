//! The offline CLIs, end to end through a real process (§2, §9).
//!
//! The three commands the app runs are stubbed by a shell script that prints the
//! committed fixtures — the same JSON shapes the contract writes — so what is
//! tested here is the whole path: the argv store builds, the process, the
//! document, and the rows and models that come out the other end. No evo binary,
//! no network, no home directory.
//!
//! Exactly one test touches the environment (`EVO_AGENT_BIN` / `EVO_SWARM_BIN`);
//! every other one hands the stub's path in, so the tests do not race on a
//! process-global.

use std::fs;
use std::path::{Path, PathBuf};

use store::catalog::{self, Catalog, CheckReport};
use store::cli::{self, CliError};
use store::history::{self, SessionsQuery};
use store::launch::{LaunchSpec, Program};
use store::model_cache::ModelCache;
use store::paths::{Root, TabId};

/// `crates/store/tests/fixtures/`.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(name: &str) -> serde_json::Value {
    let text = fs::read_to_string(fixtures().join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// A fixture with its session paths moved into a directory that really exists —
/// a row whose journal is not there is not resumable, so the merge tests need
/// files on disk.
fn fixture_with_journals(dir: &Path, days_old: &[u64]) -> Vec<history::Session> {
    let mut body = fixture("sessions.json");
    for (index, session) in body["sessions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        let name = session["path"].as_str().unwrap();
        let name = Path::new(name)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let path = dir.join(&name);
        fs::write(&path, "(:type :session :version 1)").unwrap();
        session["path"] = serde_json::Value::String(path.display().to_string());
        let _ = days_old.get(index);
    }
    history::parse(&body)
}

/// A directory with one fake binary in it, and the argv log it appends to.
struct Stub {
    dir: PathBuf,
    log: PathBuf,
}

impl Stub {
    /// A script that answers `sessions`, `catalog` and `check` out of the
    /// fixtures, and records every argv it was called with.
    fn new(name: &str) -> Stub {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("store-stub-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("argv.log");
        let fixtures = fixtures();
        let bin = dir.join("evo-stub.sh");
        fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/argv.log"
case "$1" in
  sessions) cat "{fixtures}/sessions.json" ;;
  catalog) cat "{fixtures}/catalog.json" ;;
  check)
    case "$*" in
      *claude-sonnet-5@proxy*) cat "{fixtures}/check-bad.json" ;;
      *) cat "{fixtures}/check-ok.json" ;;
    esac ;;
  *) echo "no such command: $1" >&2; exit 64 ;;
esac
"#,
                fixtures = fixtures.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        // The script logs beside itself, so two stubs never share a log.
        Stub { dir, log }
    }

    fn bin(&self) -> PathBuf {
        self.dir.join("evo-stub.sh")
    }

    fn argvs(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The one test that exercises the environment override, for both binaries.
#[test]
fn the_environment_names_the_binaries() {
    let stub = Stub::new("bins");
    let before = (
        std::env::var_os(cli::AGENT_BIN_ENV),
        std::env::var_os(cli::SWARM_BIN_ENV),
    );
    std::env::remove_var(cli::AGENT_BIN_ENV);
    std::env::remove_var(cli::SWARM_BIN_ENV);
    assert_eq!(cli::agent_bin(), PathBuf::from(cli::INSTALLED_AGENT));
    assert_eq!(cli::swarm_bin(), PathBuf::from(cli::INSTALLED_SWARM));

    std::env::set_var(cli::AGENT_BIN_ENV, stub.bin());
    std::env::set_var(cli::SWARM_BIN_ENV, stub.bin());
    assert_eq!(cli::agent_bin(), stub.bin());
    assert_eq!(cli::swarm_bin(), stub.bin());
    // The launch's own binary follows the same rule.
    assert_eq!(Program::Swarm.binary(), stub.bin());
    assert_eq!(Program::Agent.binary(), stub.bin());

    // An empty value is not an override.
    std::env::set_var(cli::AGENT_BIN_ENV, "");
    assert_eq!(cli::agent_bin(), PathBuf::from(cli::INSTALLED_AGENT));

    std::env::remove_var(cli::AGENT_BIN_ENV);
    std::env::remove_var(cli::SWARM_BIN_ENV);
    if let Some(value) = before.0 {
        std::env::set_var(cli::AGENT_BIN_ENV, value);
    }
    if let Some(value) = before.1 {
        std::env::set_var(cli::SWARM_BIN_ENV, value);
    }
}

#[test]
fn sessions_json_becomes_history_rows() {
    let stub = Stub::new("sessions");
    let sessions = history::fetch(&stub.bin(), &SessionsQuery::swarms()).unwrap();
    assert_eq!(sessions.len(), 3, "the fixture lists three sessions");
    assert_eq!(sessions[0].id, "ed99c60d1dee3c3f");
    assert_eq!(sessions[0].program, "evo-swarm");
    assert_eq!(
        sessions[0].swarm_id.as_deref(),
        Some("20260929T090956-ed99")
    );
    assert_eq!(
        sessions[0].title,
        "make the empty tab read the session index"
    );
    assert_eq!(sessions[0].updated_epoch(), 1_790_674_196);
    assert_eq!(sessions[0].updated_text(), "2026-09-29T09:29:56Z");
    // The argv is exactly the contract's (§2): the resumable swarms, wherever
    // they are.
    assert_eq!(
        stub.argvs(),
        vec!["sessions --json --all --program evo-swarm".to_string()]
    );
}

#[test]
fn a_session_whose_journal_is_gone_is_not_a_row() {
    let stub = Stub::new("gone");
    // The fixture's paths point at /Users/x/…, which this machine does not have.
    let sessions = history::fetch(&stub.bin(), &SessionsQuery::swarms()).unwrap();
    assert_eq!(sessions.len(), 3, "the index lists them all");
    assert!(history::merge(sessions, &[]).is_empty());
}

#[test]
fn the_index_and_the_apps_recents_become_one_list() {
    let stub = Stub::new("merge");
    let dir = stub.dir.join("journals");
    fs::create_dir_all(&dir).unwrap();
    let sessions = fixture_with_journals(&dir, &[0, 1, 2]);
    assert_eq!(sessions.len(), 3);

    // The app remembers the first session: its models, its lane count, and that
    // the tab was open at the last quit.
    let recents = vec![store::Recent {
        session: sessions[0].path.clone(),
        folder: sessions[0].cwd.clone(),
        when: "2026-09-30T08:00:00Z".to_owned(),
        models: store::TabModels {
            coordinator: Some("claude-opus-5".to_owned()),
            lanes: Some("ark-deepseek-v4.1-flash".to_owned()),
        },
        lanes: 6,
        open_at_quit: true,
    }];
    let entries = history::merge(sessions, &recents);
    assert_eq!(
        entries.len(),
        3,
        "the recent is the same session, not a fourth"
    );
    let first = &entries[0];
    assert_eq!(first.session_id, "ed99c60d1dee3c3f");
    assert_eq!(first.label(), "make the empty tab read the session index");
    assert_eq!(first.lanes, 6);
    assert_eq!(first.models.coordinator.as_deref(), Some("claude-opus-5"));
    assert_eq!(
        first.models.lanes.as_deref(),
        Some("ark-deepseek-v4.1-flash")
    );
    assert_eq!(first.source, history::HistorySource::Index);
    assert!(first.open_at_quit, "only the app can say this");
    // Newest first, and a row with no title falls back to the folder's name.
    assert!(entries[0].mtime() >= entries[1].mtime());
    assert_eq!(entries[1].label(), "wire the lanes chooser to the catalog");
    assert_eq!(
        entries[2].label(),
        "bar",
        "a session with no title falls back to the folder's name"
    );
    assert_eq!(
        entries[1].resume_args().1,
        entries[1].session,
        "resume gets the exact journal path"
    );
}

#[test]
fn catalog_json_fills_the_cache_the_choosers_read() {
    let stub = Stub::new("catalog");
    let body = cli::run_json(&stub.bin(), &cli::args(&["catalog", "--json"])).unwrap();
    assert_eq!(
        stub.argvs(),
        vec!["catalog --json".to_string()],
        "one process, no probe server"
    );

    let cache = ModelCache::from_catalog("evo-swarm", body);
    assert_eq!(cache.models().len(), 3);
    assert_eq!(cache.program, "evo-swarm");
    let lanes = cache
        .catalog()
        .lane_models()
        .expect("the swarm catalog has lanes");
    assert_eq!(lanes.len(), 3);
    // §5.6: a model a lane cannot register says why, and that is what greys the
    // chooser option out.
    let blocked: Vec<String> = lanes.iter().filter(|m| !m.ok).map(|m| m.spec()).collect();
    assert_eq!(blocked, ["claude-sonnet-5@proxy"]);
    assert_eq!(
        lanes[2].reason.as_deref(),
        Some("api anthropic-oauth-messages is not in a lane")
    );
    // The catalog's own warnings are carried, not swallowed.
    assert_eq!(cache.warnings(), ["mcp[2] could not be encoded"]);
    assert_eq!(
        Catalog::from_json(fixture("catalog.json")).raw()["thinking_levels"][0],
        serde_json::Value::String("off".to_string())
    );
    // The cache survives the round trip to disk and back.
    let root = Root::at(stub.dir.join("root"));
    cache.save(&root).unwrap();
    let loaded = ModelCache::load(&root);
    assert_eq!(loaded.models().len(), 3);
    assert_eq!(loaded.catalog().lane_models().unwrap().len(), 3);
}

#[test]
fn check_json_answers_about_the_launch_it_is_given() {
    let stub = Stub::new("check");
    let mut launch = LaunchSpec::new(Program::Swarm, stub.dir.clone());
    launch.model = Some(catalog::ModelRef::new("claude-opus-5", Some("anthropic")));
    launch.lane_model = Some(catalog::ModelRef::new(
        "ark-deepseek-v4.1-flash",
        Some("aiden"),
    ));
    launch.workers = Some(4);

    let report = CheckReport::from_json(&cli::run_json(&stub.bin(), &launch.check_argv()).unwrap());
    assert!(report.ok, "no model here is the blocked one");
    assert!(report.problems.is_empty());
    // The three resolved values come back with it.
    assert_eq!(report.thinking.as_deref(), Some("high"));
    assert_eq!(report.lane_thinking.as_deref(), Some("medium"));
    assert_eq!(report.workers, Some(6));
    assert_eq!(
        stub.argvs().last().unwrap(),
        "check --json --workers 4 --model claude-opus-5@anthropic --lane-model ark-deepseek-v4.1-flash@aiden"
    );

    // The same launch, with a model the fixture says a lane cannot register:
    // the report is not ok, and its problems are one line each.
    launch.lane_model = Some(catalog::ModelRef::new("claude-sonnet-5", Some("proxy")));
    let report = CheckReport::from_json(&cli::run_json(&stub.bin(), &launch.check_argv()).unwrap());
    assert!(!report.ok);
    assert_eq!(report.problems.len(), 2);
    assert_eq!(report.problems[0].code, "model_not_ready");
    assert_eq!(
        report.problems[1].message,
        "a lane cannot register claude-sonnet-5\nit needs the anthropic-oauth-messages api"
    );
    assert_eq!(
        report.lines(),
        vec![
            "claude-sonnet-5 has no credential; pick another model".to_string(),
            "a lane cannot register claude-sonnet-5 it needs the anthropic-oauth-messages api"
                .to_string()
        ]
    );
}

#[test]
fn a_failing_command_is_an_error_with_the_childs_own_words() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("store-stub-fail-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("evo-fail.sh");
    fs::write(
        &bin,
        "#!/bin/sh\necho 'model nope is not registered' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    let error = cli::run_json(&bin, &cli::args(&["sessions", "--json"])).unwrap_err();
    match &error {
        CliError::Failed { status, stderr, .. } => {
            assert_eq!(*status, Some(1));
            assert_eq!(stderr, "model nope is not registered");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(
        error.detail().as_deref(),
        Some("model nope is not registered")
    );
    assert!(error.summary().contains("exited 1"));

    // Nothing at the path: the app says so rather than waiting.
    let error = cli::run_json(&dir.join("not-here.sh"), &[]).unwrap_err();
    assert!(matches!(error, CliError::NotFound { .. }));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_launchs_argv_is_the_contracts_flags() {
    let root = Root::at("/tmp/evo-desktop");
    let id = TabId::parse("t1").unwrap();
    let mut launch = LaunchSpec::new(Program::Swarm, "/Users/x/coding/foo");
    launch.ready_file = Some(LaunchSpec::ready_file_in(&root, &id));
    launch.watch_stdin = true;
    launch.port = Some(0);
    launch.resume = Some(PathBuf::from(
        "/Users/x/.evo/sessions/-Users-x-coding-foo/20260929T090956Z_ed99c60d1dee3c3f.sexp",
    ));
    launch.model = Some(catalog::ModelRef::new("claude-opus-5", Some("anthropic")));
    launch.thinking = Some("high".to_owned());
    launch.lane_model = Some(catalog::ModelRef::new(
        "ark-deepseek-v4.1-flash",
        Some("aiden"),
    ));
    launch.lane_thinking = Some("medium".to_owned());
    launch.workers = Some(6);
    launch.agent_bin = Some(PathBuf::from("/usr/local/bin/evo-agent"));
    assert_eq!(
        launch.argv(),
        vec![
            "serve",
            "--ready-file",
            "/tmp/evo-desktop/tabs/t1/ready.json",
            "--watch-stdin",
            "--port",
            "0",
            "--resume",
            "/Users/x/.evo/sessions/-Users-x-coding-foo/20260929T090956Z_ed99c60d1dee3c3f.sexp",
            "--model",
            "claude-opus-5@anthropic",
            "--thinking",
            "high",
            "--evo",
            "/usr/local/bin/evo-agent",
            "--workers",
            "6",
            "--lane-model",
            "ark-deepseek-v4.1-flash@aiden",
            "--lane-thinking",
            "medium",
        ]
    );
}

/// The catalogs the two binaries print differ only in `lanes`: an agent's body
/// is still a catalog.
#[test]
fn an_agent_catalog_is_read_without_the_lanes() {
    let mut body = fixture("catalog.json");
    body.as_object_mut().unwrap().remove("lanes");
    let catalog = Catalog::from_json(body);
    assert!(!catalog.is_empty());
    assert_eq!(catalog.models().len(), 3);
    assert!(catalog.lane_models().is_none());
    assert!(catalog.lane_model_ok("claude-opus-5", Some("anthropic")));
    assert!(!catalog.lane_model_ok("claude-sonnet-5", Some("proxy")));
}

/// The fixtures are the contract's shapes, so a change to one shows up as a
/// failing test here rather than as an empty chooser at launch.
#[test]
fn the_fixtures_are_the_contract_shapes() {
    let sessions = fixture("sessions.json");
    let first = &sessions["sessions"][0];
    for key in [
        "id",
        "path",
        "cwd",
        "program",
        "title",
        "created_at",
        "updated_at",
        "entries",
    ] {
        assert!(first.get(key).is_some(), "Session has no {key}");
    }
    assert!(
        first.get("swarm_id").is_some(),
        "swarm_id may be null, not absent"
    );
    for key in ["created_at", "updated_at"] {
        // Epoch milliseconds, not seconds: a seconds-shaped value would sort a
        // session into 1970.
        assert!(first[key].as_u64().unwrap() > 1_000_000_000_000);
    }

    let catalog = fixture("catalog.json");
    for key in [
        "models",
        "providers",
        "thinking_levels",
        "languages",
        "warnings",
    ] {
        assert!(catalog.get(key).is_some(), "catalog has no {key}");
    }
    for model in catalog["models"].as_array().unwrap() {
        for key in [
            "id",
            "provider",
            "name",
            "api",
            "context_window",
            "reasoning",
            "images",
            "ready",
        ] {
            assert!(model.get(key).is_some(), "model has no {key}");
        }
    }
    for lane in catalog["lanes"]["models"].as_array().unwrap() {
        assert!(lane.get("ok").is_some(), "lane model has no ok");
        assert!(lane.get("reason").is_some(), "lane model has no reason");
    }

    let check = fixture("check-bad.json");
    assert!(!check["ok"].as_bool().unwrap());
    assert!(check["problems"][0]["code"].is_string());
    assert!(check["problems"][0]["message"].is_string());
    // What a launch from here would resolve with no flags (§2): the empty tab's controls
    // open on these rather than on a rung of its own.
    assert!(check["thinking"].is_string(), "check has no thinking");
    assert!(
        check["lane_thinking"].is_string(),
        "check has no lane_thinking"
    );
    assert!(check["workers"].is_u64(), "check has no workers");
}
