//! `/api/probe` — whether the serving machine can run one of the two binaries.
//!
//! The desktop app's Settings rows ask whether *this* machine can run
//! `/usr/local/bin/evo-swarm`, and they answer it by running it
//! ([`settings::probe`]). A browser cannot: a page has no process to spawn, and
//! `std::thread::spawn` panics on `wasm32-unknown-unknown`, so `store::cli` is not
//! reachable from it at all. But the binaries are not the page's to run — they
//! belong to the machine that served it, which is already the authority for this
//! page's filesystem ([`crate::config`], [`crate::fs`]). This route makes it the
//! authority for the binaries too.
//!
//! ```text
//! GET /api/probe?name=<evo-swarm|evo-agent>&path=<path>
//!   -> { "check": "ready", "line": "evo-swarm 0.1.0 …" }
//!   |  { "check": "missing" | "not-executable" | "not-evo" | "timed-out" }
//! ```
//!
//! # The answer is the same one the desktop app gives
//!
//! [`store::cli::verdict`] is the mapping, and it is *shared*: the desktop app's
//! probe calls it locally, this route calls it here, and both therefore read the
//! name in the binary's output rather than in its file name, take either stream,
//! and treat a timeout the same way. That is the point — a row must not say one
//! thing in the app and another in the browser about one path.
//!
//! # What keeps this from being a way to run things
//!
//! * **`name` is a closed set.** It must be exactly `evo-swarm` or `evo-agent`, and
//!   it is not cosmetic: the name is what the binary has to *lead* its output with,
//!   so a path that is not that binary cannot answer `ready`. Without the check this
//!   would be a general "run this path and tell me what it printed" oracle, which is
//!   a different and much worse route.
//! * **`path` is optional, and defaults to this server's own configured binary** —
//!   the same `agent_bin`/`swarm_bin` that `GET /api/health` already publishes. A
//!   caller that supplies one must give an absolute path, so nothing here resolves
//!   a relative path against this process's working directory.
//! * **The run is bounded** — [`store::cli::VERSION_TIMEOUT`] (5 s) — and
//!   `version_within` kills what it started, its whole process group included. A
//!   binary that hangs is a row that says `timed-out`, not a request that never
//!   answers.
//! * **It is behind the same door as everything else**: `/api`, so loopback by
//!   default, the token middleware off loopback, and no CORS. Whoever holds the
//!   token can already `POST /api/tabs` and run processes as this user, so probing
//!   one grants them nothing new.
//!
//! A refusal is a normal **200** carrying a `check` that is not `ready`. Only a
//! malformed request — a `name` that is not one of the two, a `path` that is not
//! absolute — is a 4xx, because that is a caller bug rather than an answer about a
//! binary.

use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::tabs::ApiError;
use crate::AppState;

/// The two binaries a row can ask about. The set is closed on purpose: see the
/// module docs.
const NAMES: [&str; 2] = ["evo-swarm", "evo-agent"];

/// `GET /api/probe?name=…&path=…`
#[derive(Debug, Deserialize)]
pub struct ProbeQuery {
    name: Option<String>,
    path: Option<String>,
}

/// What the row is told. One variant per [`store::cli::Verdict`], plus the line a
/// `ready` binary introduced itself with.
#[derive(Debug, Serialize)]
struct ProbeView {
    /// `ready`, `missing`, `not-executable`, `not-evo` or `timed-out`.
    check: &'static str,
    /// The line it introduced itself with — present exactly when `check` is
    /// `ready`, because that line *is* what `ready` means.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    /// The path that was asked about, resolved — so a caller that omitted `path`
    /// can see which binary this server actually ran.
    path: String,
}

/// `GET /api/probe` — ask whether the serving machine can run one of the two
/// binaries.
pub async fn probe(
    State(state): State<AppState>,
    Query(query): Query<ProbeQuery>,
) -> Result<Response, ApiError> {
    let name = name_of(query.name.as_deref())?;
    let path = path_of(query.path.as_deref(), name, &state)?;

    // A real process, so it runs off the reactor: this server's runtime is what
    // every other request is waiting on, and a binary that takes its whole bound to
    // answer must not hold one of its threads while it does.
    let asked = path.clone();
    let verdict = tokio::task::spawn_blocking(move || {
        store::cli::verdict(&asked, name, store::cli::VERSION_TIMEOUT)
    })
    .await
    .map_err(|e| ApiError::internal(format!("the probe task failed: {e}")))?;

    Ok(Json(ProbeView {
        check: verdict.token(),
        line: verdict.line().map(str::to_owned),
        path: path.to_string_lossy().into_owned(),
    })
    .into_response())
}

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

/// The path to probe: the caller's, or this server's own binary for that name.
///
/// An absolute path is required when one is given. A relative one would be resolved
/// against this process's working directory, which is not a thing a caller should
/// be able to steer.
fn path_of(path: Option<&str>, name: &str, state: &AppState) -> Result<PathBuf, ApiError> {
    let Some(given) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(match name {
            "evo-agent" => state.agent_bin.clone(),
            _ => state.swarm_bin.clone(),
        });
    };
    let path = PathBuf::from(given);
    if !path.is_absolute() {
        return Err(ApiError::bad_request(format!(
            "path {given:?} is not absolute: give the path this server should run"
        )));
    }
    Ok(path)
}

/// The `check` tokens this route can send — the contract, in one place, so a test
/// can assert the set rather than spell it out twice.
#[cfg(test)]
const TOKENS: [&str; 5] = ["ready", "missing", "not-executable", "not-evo", "timed-out"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use store::cli::Verdict;

    fn state() -> AppState {
        AppState {
            tabs: std::sync::Arc::new(crate::tabs::Tabs::new(
                store::paths::Root::at(std::env::temp_dir().join("evo-web-probe-root")),
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

    /// The two names, and nothing else — this is what keeps the route from being a
    /// "run this path for me" oracle.
    #[test]
    fn only_the_two_binaries_can_be_asked_about() {
        assert_eq!(name_of(Some("evo-swarm")).unwrap(), "evo-swarm");
        assert_eq!(name_of(Some("evo-agent")).unwrap(), "evo-agent");
        assert_eq!(name_of(Some(" evo-agent ")).unwrap(), "evo-agent");

        for bad in ["", "  ", "evo", "sh", "/bin/sh", "EVO-SWARM", "evo-swarm2"] {
            let err = name_of(Some(bad)).unwrap_err();
            assert_eq!(err.status, 400, "{bad:?}");
            assert!(err.message.contains("is not one of"), "{}", err.message);
        }
        assert_eq!(name_of(None).unwrap_err().status, 400);
    }

    /// With no path, the server runs the binary it is configured with — the same
    /// two `GET /api/health` publishes.
    #[test]
    fn a_missing_path_is_the_servers_own_binary() {
        let state = state();
        assert_eq!(
            path_of(None, "evo-swarm", &state).unwrap(),
            PathBuf::from("/usr/local/bin/evo-swarm")
        );
        assert_eq!(
            path_of(Some("  "), "evo-agent", &state).unwrap(),
            PathBuf::from("/usr/local/bin/evo-agent")
        );
        assert_eq!(
            path_of(Some("/opt/evo-swarm"), "evo-swarm", &state).unwrap(),
            PathBuf::from("/opt/evo-swarm")
        );
    }

    /// A relative path is refused: it would be resolved against this process's
    /// working directory, which a caller must not be able to steer.
    #[test]
    fn a_relative_path_is_refused() {
        let state = state();
        for bad in ["evo-swarm", "./evo-swarm", "../evo-swarm", "bin/evo"] {
            let err = path_of(Some(bad), "evo-swarm", &state).unwrap_err();
            assert_eq!(err.status, 400, "{bad:?}");
            assert!(err.message.contains("not absolute"), "{}", err.message);
        }
    }

    /// The wire vocabulary is [`store::cli::Verdict`]'s, and a `ready` is the only
    /// answer that carries a line — the reply's shape and the mapping cannot drift
    /// from the type they are made of.
    #[test]
    fn the_reply_speaks_the_verdicts_vocabulary() {
        let verdicts = [
            Verdict::Ready("evo-swarm 0.1.0".to_owned()),
            Verdict::Missing,
            Verdict::NotExecutable,
            Verdict::NotEvo,
            Verdict::TimedOut,
        ];
        let tokens: Vec<&str> = verdicts.iter().map(Verdict::token).collect();
        assert_eq!(tokens, TOKENS.to_vec(), "the whole set, and no sixth");

        for verdict in &verdicts {
            let view = ProbeView {
                check: verdict.token(),
                line: verdict.line().map(str::to_owned),
                path: "/usr/local/bin/evo-swarm".to_owned(),
            };
            let body = serde_json::to_value(&view).unwrap();
            assert_eq!(body["check"], json!(verdict.token()));
            // `line` is present exactly when there is one, and it is the whole of
            // what `ready` means.
            match verdict.line() {
                Some(line) => assert_eq!(body["line"], json!(line)),
                None => assert!(body.get("line").is_none(), "{body}"),
            }
            // And a caller can round-trip it: what the page reads back is the
            // verdict the server computed.
            let token = body["check"].as_str().unwrap();
            let line = body.get("line").and_then(|v| v.as_str());
            assert_eq!(
                Verdict::from_wire(token, line).as_ref(),
                Some(verdict),
                "{token} did not survive the wire"
            );
        }
    }

    /// A `ready` with no line is not a verdict the page can show, and a token this
    /// crate does not mint is not one either — so the page's fallback is exercised
    /// rather than assumed.
    #[test]
    fn an_answer_that_is_not_a_verdict_is_not_read_as_one() {
        assert_eq!(Verdict::from_wire("ready", None), None);
        assert_eq!(Verdict::from_wire("checking", None), None);
        assert_eq!(Verdict::from_wire("", None), None);
    }
}
