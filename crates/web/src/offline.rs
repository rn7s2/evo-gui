//! `/api/cli` — one of the two binaries' offline reads, run on the serving machine.
//!
//! The empty tab's cards ask two questions about the machine evo runs on: *what
//! models does this program have* (`<bin> catalog --json`) and *is this launch
//! sound* (`evo-swarm check --json`, §9). The desktop app answers both by running
//! the binary ([`store::cli::run_json`], [`store::cli::run_json_reporting`]). A
//! browser cannot: a page has no process to spawn, so both call sites reach
//! `crate::host`'s wasm arm and every card reads "… could not be run — fix it in
//! Settings…" about a binary that is perfectly fine. This route is the server
//! answering instead.
//!
//! ```text
//! GET /api/cli?name=<evo-swarm|evo-agent>&read=<catalog|check>
//!              [&workers=N][&model=ID@P][&lane-model=ID@P]
//!   -> 200  the JSON document the read printed
//!   -> 400  { "error": … }  a name, a read, or a flag outside the closed sets
//!   -> 502  { "error": … }  the binary could not be run at all
//! ```
//!
//! # The two sets are closed, and that is the whole design
//!
//! This route runs a process, so what it will run is spelled out rather than
//! accepted from the caller. Both halves of "what runs" are enumerated:
//!
//! * **which binary** — `name` is `evo-swarm` or `evo-agent` and nothing else, and
//!   it is *not a path*. The server runs its own configured binary for that name
//!   (the same two `GET /api/health` publishes), exactly as [`crate::probe`]
//!   restricts the same parameter. A caller cannot point this route at a file of
//!   its choosing, so there is no path to validate and nothing to escape.
//! * **which read** — `read` is `catalog` or `check`, and each admits a fixed set
//!   of flags: `catalog` takes **none**, and `check` takes `--workers`, `--model`
//!   and `--lane-model`. That is the whole of what these two call sites need, and
//!   the table below is the authority. There is no way to pass a subcommand, a
//!   `--flag`, or an argv position this route does not name itself, which is what
//!   keeps it from being "run any evo subcommand for me".
//!
//! | read      | `evo-swarm` | `evo-agent` | flags                              |
//! |-----------|-------------|-------------|------------------------------------|
//! | `catalog` | yes         | yes         | none                               |
//! | `check`   | yes         | no          | `--workers`, `--model`, `--lane-model` |
//!
//! `check` is `evo-swarm`'s alone because `evo-agent` has no such subcommand — the
//! desktop app asks the swarm binary too ([`store::launch::check_spec`] builds a
//! `Program::Swarm` spec), so refusing it here refuses nothing a caller needs.
//!
//! The argv is built here, from validated pieces, and handed to
//! [`store::cli::run_json`] as a list. **There is no shell**, so there is no string
//! to interpolate into and nothing to quote; a value that reaches the argv is
//! checked for emptiness and control characters the way `POST /api/tabs` checks a
//! flag value, and `workers` must parse as a count.
//!
//! # `check` exits 1 and prints its document anyway
//!
//! That is the entire reason [`store::cli::run_json_reporting`] exists (§2): a
//! launch with problems is a *document* about those problems, not a failed read.
//! So `check` answers **200 with the document** on a non-zero exit, mirroring
//! `run_json_reporting` exactly — a 502 here would tell the empty tab that its
//! launch could not be checked when what it actually has is a list of problems to
//! draw. A binary that could not be *run* is still a 502, which is what
//! [`crate::offline`] produces for a missing or unreadable file.
//!
//! # What bounds it
//!
//! The same as every other offline read: the run happens off the reactor
//! ([`crate::offline`]'s `spawn_blocking`), it is bounded by [`store::cli`]'s own
//! timeouts, and a read that never answers is stopped group-and-all by the store's
//! ladder rather than left behind. It sits under `/api`, so it is loopback by
//! default and behind the token middleware like everything else — and whoever holds
//! the token can already `POST /api/tabs` and start processes as this user, so
//! running one of the two binaries grants them nothing new.

use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::tabs::ApiError;
use crate::AppState;

/// The two binaries this route can run. Closed on purpose: see the module docs.
const NAMES: [&str; 2] = ["evo-swarm", "evo-agent"];

/// `GET /api/cli?name=…&read=…`
#[derive(Debug, Deserialize)]
pub struct CliQuery {
    name: Option<String>,
    read: Option<String>,
    workers: Option<String>,
    model: Option<String>,
    #[serde(rename = "lane-model")]
    lane_model: Option<String>,
}

/// Which of the two reads a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    /// `<bin> catalog --json` — read with [`store::cli::run_json`].
    Catalog,
    /// `evo-swarm check --json` — read with [`store::cli::run_json_reporting`],
    /// because it exits 1 while still printing its document.
    Check,
}

/// A request that has been read against the closed sets: which binary, which read,
/// and the argv that follows from them.
#[derive(Debug)]
struct Plan {
    name: &'static str,
    read: Read,
    workers: Option<u16>,
    model: Option<String>,
    lane_model: Option<String>,
}

impl Plan {
    /// Read a request, or refuse it in words the caller can act on.
    fn of(query: &CliQuery) -> Result<Plan, ApiError> {
        let name = name_of(query.name.as_deref())?;
        let read = read_of(query.read.as_deref())?;

        // The table in the module docs, as code: `check` is the swarm's, and
        // `catalog` is either's.
        if read == Read::Check && name != "evo-swarm" {
            return Err(ApiError::bad_request(format!(
                "{name} has no `check`: it is evo-swarm's read"
            )));
        }

        let workers = match flag(query.workers.as_deref(), "workers")? {
            // A count, so it is parsed rather than passed through: a `--workers`
            // that is not a number is a launch flag evo would refuse, and a caller
            // should hear that here.
            Some(text) => Some(text.parse::<u16>().ok().filter(|n| *n > 0).ok_or_else(|| {
                ApiError::bad_request(format!("workers {text:?} is not a count of at least 1"))
            })?),
            None => None,
        };
        let model = flag(query.model.as_deref(), "model")?;
        let lane_model = flag(query.lane_model.as_deref(), "lane-model")?;

        // `catalog` takes no flags at all — the read is `<bin> catalog --json` and
        // nothing else. A flag here is a caller asking for something this route
        // does not do, which is worth saying rather than silently dropping.
        if read == Read::Catalog {
            for (given, name) in [
                (workers.is_some(), "workers"),
                (model.is_some(), "model"),
                (lane_model.is_some(), "lane-model"),
            ] {
                if given {
                    return Err(ApiError::bad_request(format!(
                        "{name} is not a flag of `catalog`: it reads {NAME_FLAG_LESS}"
                    )));
                }
            }
        }

        Ok(Plan {
            name,
            read,
            workers,
            model,
            lane_model,
        })
    }

    /// The argv, built here and handed over as a list — never a shell string.
    ///
    /// The order is the one [`store::launch::LaunchSpec::check_argv`] uses, so what
    /// this route runs is the argv the desktop app would have run for the same
    /// launch.
    fn argv(&self) -> Vec<String> {
        let mut argv = vec![
            match self.read {
                Read::Catalog => "catalog".to_owned(),
                Read::Check => "check".to_owned(),
            },
            "--json".to_owned(),
        ];
        if let Some(workers) = self.workers {
            argv.push("--workers".to_owned());
            argv.push(workers.to_string());
        }
        for (flag, value) in [("--model", &self.model), ("--lane-model", &self.lane_model)] {
            if let Some(value) = value {
                argv.push(flag.to_owned());
                argv.push(value.clone());
            }
        }
        argv
    }

    /// The server's own binary for this name — never a caller's path.
    fn bin(&self, state: &AppState) -> PathBuf {
        match self.name {
            "evo-agent" => state.agent_bin.clone(),
            _ => state.swarm_bin.clone(),
        }
    }
}

/// The words `catalog` takes nothing more than, for a refusal that says what it
/// does take.
const NAME_FLAG_LESS: &str = "a model list and no launch flags";

/// The binary a request names, which must be one of the two.
fn name_of(name: Option<&str>) -> Result<&'static str, ApiError> {
    let wanted = name.unwrap_or("").trim();
    NAMES
        .into_iter()
        .find(|known| *known == wanted)
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "name {wanted:?} is not one of {}",
                NAMES.join(", ")
            ))
        })
}

/// The read a request names, which must be one of the two.
fn read_of(read: Option<&str>) -> Result<Read, ApiError> {
    match read.unwrap_or("").trim() {
        "catalog" => Ok(Read::Catalog),
        "check" => Ok(Read::Check),
        other => Err(ApiError::bad_request(format!(
            "read {other:?} is not one of catalog, check"
        ))),
    }
}

/// One optional flag value, checked the way `POST /api/tabs` checks one.
///
/// Empty is absent rather than an error — an empty query parameter is what a form
/// sends for a field nobody filled in — and a control character is refused because
/// it has no business in a flag: it cannot reach a shell (there is none), but it
/// can reach a log line.
fn flag(value: Option<&str>, name: &str) -> Result<Option<String>, ApiError> {
    let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{name} contains a control character"
        )));
    }
    Ok(Some(text.to_owned()))
}

/// `GET /api/cli` — run one offline read of one of the two binaries.
pub async fn read(
    State(state): State<AppState>,
    Query(query): Query<CliQuery>,
) -> Result<Response, ApiError> {
    let plan = Plan::of(&query)?;
    let bin = plan.bin(&state);
    let argv = plan.argv();
    // `check` prints its document on a non-zero exit, so it is read with the
    // reporting half; `catalog` exits 0 or not at all.
    crate::offline(move || match plan.read {
        Read::Catalog => store::cli::run_json(&bin, &argv),
        Read::Check => store::cli::run_json_reporting(&bin, &argv),
    })
    .await
    .map(|response| response.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> AppState {
        AppState {
            tabs: std::sync::Arc::new(crate::tabs::Tabs::new(
                store::paths::Root::at(std::env::temp_dir().join("evo-web-cli-root")),
                PathBuf::from("/usr/local/bin/evo-agent"),
                PathBuf::from("/usr/local/bin/evo-swarm"),
                None,
            )),
            agent_bin: PathBuf::from("/usr/local/bin/evo-agent"),
            swarm_bin: PathBuf::from("/usr/local/bin/evo-swarm"),
            token: None,
            bind: "127.0.0.1:0".to_owned(),
        }
    }

    fn query(pairs: &[(&str, &str)]) -> CliQuery {
        let mut query = CliQuery {
            name: None,
            read: None,
            workers: None,
            model: None,
            lane_model: None,
        };
        for (key, value) in pairs {
            let slot = match *key {
                "name" => &mut query.name,
                "read" => &mut query.read,
                "workers" => &mut query.workers,
                "model" => &mut query.model,
                "lane-model" => &mut query.lane_model,
                other => panic!("no such parameter {other}"),
            };
            *slot = Some((*value).to_owned());
        }
        query
    }

    /// The two names and nothing else. A path is not accepted here at all, which is
    /// what keeps this route from being "run this file for me".
    #[test]
    fn only_the_two_binaries_can_be_named() {
        assert_eq!(name_of(Some("evo-swarm")).unwrap(), "evo-swarm");
        assert_eq!(name_of(Some(" evo-agent ")).unwrap(), "evo-agent");
        assert_eq!(name_of(None).unwrap_err().status, 400);

        for bad in [
            "",
            "  ",
            "evo",
            "sh",
            "/bin/sh",
            "/usr/local/bin/evo-swarm",
            "EVO-SWARM",
            "evo-swarm2",
        ] {
            let err = name_of(Some(bad)).unwrap_err();
            assert_eq!(err.status, 400, "{bad:?}");
            assert!(err.message.contains("is not one of"), "{}", err.message);
        }
    }

    /// The two reads and nothing else: no subcommand of evo's is reachable that
    /// these two call sites do not need.
    #[test]
    fn only_the_two_reads_can_be_asked_for() {
        assert_eq!(read_of(Some("catalog")).unwrap(), Read::Catalog);
        assert_eq!(read_of(Some(" check ")).unwrap(), Read::Check);
        assert_eq!(read_of(None).unwrap_err().status, 400);

        for bad in [
            "",
            "serve",
            "sessions",
            "--help",
            "catalog --json",
            "check; rm -rf /",
            "CATALOG",
        ] {
            let err = read_of(Some(bad)).unwrap_err();
            assert_eq!(err.status, 400, "{bad:?}");
            assert!(err.message.contains("is not one of"), "{}", err.message);
        }
    }

    /// `check` is the swarm's; `evo-agent` has no such subcommand.
    #[test]
    fn check_is_the_swarms_read_alone() {
        let ok = Plan::of(&query(&[("name", "evo-swarm"), ("read", "check")])).unwrap();
        assert_eq!(ok.read, Read::Check);

        let err = Plan::of(&query(&[("name", "evo-agent"), ("read", "check")])).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("has no `check`"), "{}", err.message);
    }

    /// The argv is built from validated pieces, in the order the desktop app uses —
    /// and a `catalog` carries no flag at all.
    #[test]
    fn the_argv_is_the_one_the_desktop_app_would_have_run() {
        let catalog = Plan::of(&query(&[("name", "evo-agent"), ("read", "catalog")])).unwrap();
        assert_eq!(catalog.argv(), vec!["catalog", "--json"]);

        let bare = Plan::of(&query(&[("name", "evo-swarm"), ("read", "check")])).unwrap();
        assert_eq!(bare.argv(), vec!["check", "--json"]);

        let full = Plan::of(&query(&[
            ("name", "evo-swarm"),
            ("read", "check"),
            ("workers", "4"),
            ("model", "m@acme"),
            ("lane-model", "l@acme"),
        ]))
        .unwrap();
        assert_eq!(
            full.argv(),
            vec![
                "check",
                "--json",
                "--workers",
                "4",
                "--model",
                "m@acme",
                "--lane-model",
                "l@acme"
            ]
        );

        // The same shape `store::launch` builds for one launch, so the two cannot
        // drift: every token is a separate element, and there is no string to
        // interpolate into.
        let spec = store::launch::LaunchSpec::new(store::launch::Program::Swarm, PathBuf::new());
        assert_eq!(spec.check_argv(), vec!["check", "--json"]);
    }

    /// `catalog` reads a model list, so a launch flag is a caller asking for
    /// something this route does not do.
    #[test]
    fn catalog_refuses_a_launch_flag() {
        for (key, value) in [
            ("workers", "4"),
            ("model", "m@acme"),
            ("lane-model", "l@acme"),
        ] {
            let err = Plan::of(&query(&[
                ("name", "evo-swarm"),
                ("read", "catalog"),
                (key, value),
            ]))
            .unwrap_err();
            assert_eq!(err.status, 400, "{key}");
            assert!(
                err.message.contains("not a flag of `catalog`"),
                "{}",
                err.message
            );
        }
    }

    /// `workers` is a count: a value that is not one is refused here rather than
    /// handed to evo as a flag it would reject.
    #[test]
    fn workers_must_be_a_count_of_at_least_one() {
        for bad in ["0", "-1", "four", "1.5", "99999999999"] {
            let err = Plan::of(&query(&[
                ("name", "evo-swarm"),
                ("read", "check"),
                ("workers", bad),
            ]))
            .unwrap_err();
            assert_eq!(err.status, 400, "{bad:?}");
            assert!(err.message.contains("is not a count"), "{}", err.message);
        }
        let one = Plan::of(&query(&[
            ("name", "evo-swarm"),
            ("read", "check"),
            ("workers", "1"),
        ]))
        .unwrap();
        assert_eq!(one.workers, Some(1));
    }

    /// A flag value is checked the way `POST /api/tabs` checks one: empty is absent,
    /// and a control character is refused because it can reach a log line.
    #[test]
    fn a_flag_value_is_checked_like_a_launch_flag() {
        assert_eq!(flag(None, "model").unwrap(), None);
        assert_eq!(flag(Some("   "), "model").unwrap(), None);
        assert_eq!(
            flag(Some(" m@acme "), "model").unwrap(),
            Some("m@acme".to_owned())
        );

        let err = flag(Some("m\u{0}del"), "model").unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("control character"), "{}", err.message);
    }

    /// The binary is the server's own, chosen by name — the caller's only influence
    /// is which of the two, and a path can never be it.
    #[test]
    fn the_binary_is_the_servers_own() {
        let state = state();
        let swarm = Plan::of(&query(&[("name", "evo-swarm"), ("read", "check")])).unwrap();
        assert_eq!(swarm.bin(&state), PathBuf::from("/usr/local/bin/evo-swarm"));
        let agent = Plan::of(&query(&[("name", "evo-agent"), ("read", "catalog")])).unwrap();
        assert_eq!(agent.bin(&state), PathBuf::from("/usr/local/bin/evo-agent"));
    }

    /// The refusal body is the crate's one error shape, so a frontend reads it the
    /// same way it reads any other.
    #[test]
    fn a_refusal_is_the_crates_own_error_shape() {
        let err = Plan::of(&query(&[("name", "sh"), ("read", "catalog")])).unwrap_err();
        let body = err.body();
        assert_eq!(body["error"], json!(err.message));
        // A 400 is a caller bug, and carries no `detail` — that is `bad_gateway`'s,
        // for the binary's own words. The shape is the crate's either way.
        assert!(body.get("detail").is_none(), "{body}");
        assert_eq!(body.as_object().map(|o| o.len()), Some(1), "{body}");
    }
}
