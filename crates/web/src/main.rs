//! evo-web — the browser UI's host and the thing that drives evo on this machine.
//!
//! One `evo-web` process serves the frontend's static bundle and answers
//! `/api/…`. A **tab** is one `evo-swarm serve` (or `evo-agent serve`) in a folder
//! on this machine, started the way the desktop app starts one — the same argv
//! ([`store::launch`]), the same ready file, the same tab directory under
//! `~/.evo/desktop/tabs/` — and this server is what talks to it: the browser sends
//! `/api/tabs/<id>/ops`, this process adds the `Authorization: Bearer …` header and
//! hands the reply back. The bearer token of a serve process never reaches the
//! browser, and neither does its port.
//!
//! ```text
//! GET    /                          the frontend (dist/, index.html fallback)
//! GET    /api/health                this server: version, tabs, bind
//! GET    /api/tabs                  every tab, oldest first
//! POST   /api/tabs                  open one (folder, program, model/effort/…)
//! GET    /api/tabs/<id>             one tab's state
//! DELETE /api/tabs/<id>             stop it, and forget it
//! GET    /api/tabs/<id>/snapshot    the tab's whole view, atomically
//! GET    /api/tabs/<id>/stream      the tab's op stream (SSE, unbuffered)
//! POST   /api/tabs/<id>/ops         one operation (input.send, goal.*, …)
//! GET    /api/tabs/<id>/items       older items, paging back
//! GET    /api/tabs/<id>/items/<i>   one item, whole
//! GET    /api/tabs/<id>/media/<i>/<n>  image bytes, with their content type
//! GET    /api/tabs/<id>/catalog     the running session's own registry
//! GET    /api/catalog               the machine's registry (offline CLI read)
//! GET    /api/sessions              the machine's session history (offline read)
//! GET    /api/config                evo's own config files, for the browser's editor
//! POST   /api/config                write one, through the same save the app uses
//! GET    /api/fs                    the serving machine's directories, for a picker
//! GET    /api/probe                 whether this machine can run evo-swarm/evo-agent
//! ```
//!
//! # Security
//!
//! This listens on a port and can start and stop processes, so:
//!
//! * **Loopback by default.** Binding anything else is refused unless
//!   `--auth-token` (or `EVO_WEB_TOKEN`) is given — the refusal is in
//!   [`cli`], and it is a startup error, not a warning.
//! * **The token never leaves.** A tab's bearer token is read from its 0600 ready
//!   file and used by this process's own client; no reply, log line or error body
//!   carries it, and `/api/tabs/<id>` reports the port and pid but not the token.
//! * **No shell, ever.** A folder or a model name goes into an argv **list**
//!   ([`store::launch::LaunchSpec::argv`]) handed to `Command`, never into a
//!   command string. There is no `sh -c` anywhere in this crate.
//! * **Same-origin only.** No CORS headers are sent, so a browser refuses to let
//!   another origin read a reply. When a token is configured, `/api` requires it
//!   — as `Authorization: Bearer`, or as the `evo_web_token` cookie **and** a
//!   mutating request whose `Origin` matches this server, so a page on another
//!   site cannot drive a tab through the cookie alone.
//! * **Nothing is forwarded blindly.** Every proxied route is one route of the
//!   serve protocol; there is no catch-all. A path segment (a tab id, an item id,
//!   a topic) is validated, and `swarm_client` percent-encodes what it puts in a
//!   request line, so a segment cannot become part of the request.
//! * **The static bundle is served by `tower-http`'s `ServeDir`**, which rejects
//!   a path that climbs out of the directory it was given.

mod cli;
mod config;
mod fs;
mod offline;
mod probe;
mod proxy;
mod tabs;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::tabs::{ApiError, OpenRequest, Tabs};

/// What every handler is given: the tabs, and the machine they run on.
#[derive(Clone)]
pub struct AppState {
    pub tabs: Arc<Tabs>,
    /// The binaries the offline reads (`catalog`, `sessions`) run.
    pub agent_bin: PathBuf,
    pub swarm_bin: PathBuf,
    /// The token `/api` requires, when one was configured.
    pub token: Option<Arc<String>>,
    /// What this server is bound to, for `/api/health`.
    pub bind: String,
}

/// The most a request body may be. A pasted screenshot travels as base64 in
/// `input.send`, and the protocol takes bodies of a few megabytes; axum's own
/// default of 2 MB would refuse one the session would have accepted.
const MAX_BODY: usize = 16 * 1024 * 1024;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match cli::from_args(args, &|name| std::env::var(name).ok()) {
        Ok(cli::Outcome::Run(config)) => *config,
        Ok(cli::Outcome::Help) => {
            print!("{}", cli::USAGE);
            return Ok(());
        }
        Err(message) => {
            eprintln!("evo-web: {message}");
            eprintln!("\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(config))
}

async fn serve(config: cli::Config) -> anyhow::Result<()> {
    let listener = bind(&config).await?;
    let local = listener.local_addr()?;

    let state = AppState {
        tabs: Arc::new(Tabs::new(
            store::paths::Root::at(config.root.clone()),
            config.agent_bin.clone(),
            config.swarm_bin.clone(),
            config.prompt_note.clone(),
        )),
        agent_bin: config.agent_bin.clone(),
        swarm_bin: config.swarm_bin.clone(),
        token: config.token.clone().map(Arc::new),
        bind: local.to_string(),
    };

    let index = config.dist.join("index.html");
    let app = router(&config, &state);

    // The one line a caller (a person, or a supervisor reading stdout) waits for.
    println!(
        "evo-web ready — http://{local}  (dist {}, root {})",
        config.dist.display(),
        config.root.display()
    );
    if state.token.is_some() {
        println!("evo-web: /api requires the configured token");
    }
    if !index.is_file() {
        eprintln!(
            "evo-web: no frontend at {} — `/` will 404 until the bundle is built there \
             (or point --dist at it)",
            index.display()
        );
    }

    let shutdown = {
        let state = state.clone();
        async move {
            let signal = stop_signal().await;
            println!(
                "evo-web: {signal}; {} tab(s) to stop",
                state.tabs.len()
            );
            let tabs = state.tabs.clone();
            // The ladder is blocking (it waits on processes), so it runs off the
            // runtime — and the runtime is not torn down until this returns.
            let _ = tokio::task::spawn_blocking(move || tabs.stop_all()).await;
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Bind the listener, and make a taken port say something a person can act on.
///
/// A bare `Address already in use (os error 48)` names neither the port nor what
/// to do about it, and the default port is fixed on purpose — so a run that finds
/// it taken explains itself instead of passing the OS's words along.
///
/// **Nothing is retried and nothing falls back to another port.** A predictable
/// default is the whole point of having one, and a silent fallback would leave two
/// runs of this server disagreeing about the URL the browser should open. `--port
/// 0` is the deliberate way to ask for a free one, and the chosen URL is printed
/// once the server is up.
async fn bind(config: &cli::Config) -> anyhow::Result<tokio::net::TcpListener> {
    let addr = SocketAddr::new(config.bind, config.port);
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err(anyhow::anyhow!(
            "port {} on {} is already in use: something else on this machine holds it \
             (another evo-web, or an unrelated service).\n\
             Start on a free port with `--port 0` — the OS picks one and the chosen \
             URL is printed — or pass `--port <N>` for one you know is free.",
            config.port,
            config.bind
        )),
        Err(error) => Err(anyhow::Error::new(error).context(format!("could not bind {addr}"))),
    }
}

/// Wait for the signal that stops this server, and name it.
///
/// **SIGTERM as well as SIGINT, and the first one wins.** A server that runs other
/// people's processes has to stop them when it is asked to stop, and the two ways
/// it is asked are a person's `Ctrl-C` (SIGINT) and a supervisor's or an init
/// system's `SIGTERM`. Handling only the first — which this did — means a
/// `kill <pid>` or a service manager's stop skips the tab ladder entirely: the
/// tabs still die, because each `serve` watches the stdin that closes with this
/// process, but nothing gives them the graceful stop they would have had, and no
/// supervisor can tell the difference.
///
/// On a platform with no SIGTERM the listener cannot be built; SIGINT alone is
/// then the whole answer rather than a reason to fail.
async fn stop_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "interrupted",
                _ = term.recv() => "terminated",
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                "interrupted"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "interrupted"
    }
}

fn router(config: &cli::Config, state: &AppState) -> Router {
    let dist = config.dist.clone();
    let index = dist.join("index.html");
    // `ServeDir` resolves and percent-decodes the path itself and refuses one that
    // climbs out of `dist`. Its own 404 — a path that is not a file there — is
    // turned into `index.html` for a route that is a *page* (the last segment has
    // no extension, which is what a client-side route looks like) and left a 404
    // otherwise, so a missing script or stylesheet fails the way a browser
    // expects instead of arriving as HTML.
    let frontend = ServeDir::new(&dist).fallback(page_fallback(index));

    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/tabs", get(list_tabs).post(open_tab))
        .route("/api/tabs/{id}", get(tab_state).delete(stop_tab))
        .merge(proxy::routes())
        .route("/api/catalog", get(catalog))
        .route("/api/sessions", get(sessions))
        // A browser cannot run a binary, so the two offline reads the empty tab's
        // cards need — `<bin> catalog --json` and `evo-swarm check --json` — are run
        // here, from a closed set of binaries and reads. See `offline.rs`.
        .route("/api/cli", get(offline::read))
        // The browser's two filesystem needs: evo's own config files (which a
        // page cannot read or write) and a directory listing (which a native
        // folder dialog would otherwise give it). Both are on `/api`, so both
        // are behind the token check below.
        .route("/api/config", get(config::read).post(config::save))
        .route("/api/fs", get(fs::list))
        // A browser cannot run a binary, so it asks the machine that has one — and
        // gets the same answer the desktop app's own probe computes.
        .route("/api/probe", get(probe::probe))
        // An op may carry a pasted picture; axum's default limit is smaller than
        // the protocol's own.
        .layer(DefaultBodyLimit::max(MAX_BODY))
        // The token check is always in place and is a no-op when no token was
        // configured: one router, and no way for a route to be added to the wrong
        // half of it.
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .merge(api)
        // One fallback for everything the router above did not answer: an /api
        // path gets JSON, anything else is the static bundle (and, for a page
        // route, the app). A fallback *inside* the /api router would be shadowed
        // by this one — the outer router's fallback is the one that runs — so the
        // choice is made here, where the path is still visible.
        .fallback_service(bundle_and_api(frontend))
        // Cross-origin isolation, which this page needs rather than prefers.
        //
        // gpui's web platform spawns background workers on a *dedicated thread*
        // (`gpui-pre-scheduler`'s `spawn_dedicated_os_thread`), and on wasm that
        // is `wasm_thread`, which requires `SharedArrayBuffer` — available only
        // to a cross-origin isolated page. Without these two headers the page is
        // not isolated, the spawn fails, and the panic unwinds out of whatever
        // first drives a session: the window then never leaves the empty tab, so
        // a person who opens a session watches nothing happen.
        //
        // `require-corp` is what makes the bundle's own subresources (the wasm
        // module, the icons) loadable from this origin, so nothing else changes
        // for a page served by this server.
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("cross-origin-opener-policy"),
            header::HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("cross-origin-embedder-policy"),
            header::HeaderValue::from_static("require-corp"),
        ))
        .with_state(state.clone())
}

/// The body a fallback answers with.
type FallbackBody = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Response, std::convert::Infallible>> + Send>,
>;

/// The single fallback: `/api/…` is JSON, everything else is the bundle.
fn bundle_and_api<F>(
    frontend: ServeDir<F>,
) -> tower::util::ServiceFn<impl FnMut(Request<Body>) -> FallbackBody + Clone>
where
    F: tower::Service<Request<Body>, Response = Response, Error = std::convert::Infallible>
        + Clone
        + Send
        + 'static,
    F::Future: Send + 'static,
{
    use tower::ServiceExt;
    tower::service_fn(move |request: Request<Body>| {
        let frontend = frontend.clone();
        // The future is boxed because the two branches have different body types
        // (`ServeDir`'s own, and this crate's `Response`).
        let future: FallbackBody = Box::pin(async move {
            let path = request.uri().path().to_owned();
            if path == "/api" || path.starts_with("/api/") {
                return Ok(unknown_api(&request));
            }
            match frontend.oneshot(request).await {
                Ok(response) => Ok(response.map(Body::new)),
                Err(_) => Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            }
        });
        future
    })
}

/// The static bundle's own 404 rule: a page route gets the app, an asset does
/// not.
///
/// A `service_fn` rather than a handler: this is what `ServeDir` falls back *to*,
/// so it has to be a service, and a service is the one shape axum's own routing
/// types cannot be here (they carry this router's state until the whole tree is
/// built).
fn page_fallback(
    index: PathBuf,
) -> tower::util::ServiceFn<
    impl FnMut(Request<Body>) -> std::future::Ready<Result<Response, std::convert::Infallible>> + Clone,
> {
    tower::service_fn(move |request: Request<Body>| {
        let index = index.clone();
        std::future::ready(Ok(page_for(&request, &index)))
    })
}

/// `index.html` for a page route, a 404 for anything that names a file.
fn page_for(request: &Request<Body>, index: &std::path::Path) -> Response {
    let path = request.uri().path();
    let looks_like_a_file = path
        .rsplit('/')
        .next()
        .map(|last| last.contains('.'))
        .unwrap_or(false);
    if looks_like_a_file {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(index) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => ApiError::not_found(format!(
            "no frontend at {} — build the bundle, or point --dist at it",
            index.display()
        ))
        .into_response(),
    }
}

/// Anything under `/api` this server does not serve.
fn unknown_api(request: &Request<Body>) -> Response {
    ApiError::not_found(format!(
        "no such endpoint: {} /{}",
        request.method(),
        request.uri().path().trim_start_matches('/')
    ))
    .into_response()
}

// ---------------------------------------------------------------------------
// /api, unproxied
// ---------------------------------------------------------------------------

/// `GET /api/health` — what this server is, for a frontend that just loaded and
/// for a person checking whether it is up.
async fn health(State(state): State<AppState>) -> Response {
    Json(json!({
        "ok": true,
        "program": "evo-web",
        "version": env!("CARGO_PKG_VERSION"),
        "bind": state.bind,
        "tabs": state.tabs.len(),
        "auth": state.token.is_some(),
        "binaries": {
            "agent": state.agent_bin,
            "swarm": state.swarm_bin,
        },
    }))
    .into_response()
}

/// `GET /api/tabs` — every tab this server has open, oldest first.
async fn list_tabs(State(state): State<AppState>) -> Response {
    let tabs: Vec<serde_json::Value> = state.tabs.list().iter().map(|tab| tab.view()).collect();
    Json(json!({ "tabs": tabs })).into_response()
}

/// `POST /api/tabs` — open one: a folder, a program, and the model and effort
/// choices the frontend's empty tab offers.
///
/// 201 with the tab's own state when it came up; 400 for a request this server
/// will not run, and **502 with the child's own words** (its log tail included)
/// when the process never came ready. The tab stays in the list, `failed`, so the
/// frontend can show what happened instead of a tab that vanished.
async fn open_tab(
    State(state): State<AppState>,
    Json(body): Json<OpenRequest>,
) -> Result<Response, ApiError> {
    let tab = state.tabs.open(body).await?;
    Ok((StatusCode::CREATED, Json(tab.view())).into_response())
}

/// `GET /api/tabs/<id>` — one tab's state.
async fn tab_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let tab = state
        .tabs
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("no tab {id}")))?;
    Ok(Json(tab.view()).into_response())
}

/// `DELETE /api/tabs/<id>` — stop it (the full ladder) and forget it.
async fn stop_tab(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(Json(state.tabs.stop(&id).await?).into_response())
}

/// `GET /api/catalog` — the machine's registry, from `evo-swarm catalog --json`.
///
/// An offline read: one process, one JSON document, no listener. The same command
/// the desktop app fills its choosers from, so the browser's choosers and the
/// app's agree.
async fn catalog(State(state): State<AppState>) -> Result<Response, ApiError> {
    let bin = state.swarm_bin.clone();
    offline(move || store::cli::run_json(&bin, &store::cli::args(&["catalog", "--json"]))).await
}

/// `GET /api/sessions` — the machine's session history, from
/// `evo-agent sessions --json --all`.
async fn sessions(State(state): State<AppState>) -> Result<Response, ApiError> {
    let bin = state.agent_bin.clone();
    let query = store::history::SessionsQuery::resumable();
    offline(move || {
        let body = store::cli::run_json_within(&bin, &query.argv(), store::cli::SESSIONS_TIMEOUT)?;
        Ok(body)
    })
    .await
}

/// A bounded offline read, off the runtime, with a failure a frontend can show.
///
/// A missing or unreadable binary is the common failure here (this is the one
/// place the two binaries are run by path), and `store::cli` reports it as one
/// line: which binary, and what it said.
pub(crate) async fn offline<F>(read: F) -> Result<Response, ApiError>
where
    F: FnOnce() -> Result<serde_json::Value, store::cli::CliError> + Send + 'static,
{
    let body = tokio::task::spawn_blocking(read)
        .await
        .map_err(|e| ApiError::internal(format!("the read task failed: {e}")))?
        .map_err(|e| ApiError::bad_gateway(e.summary(), json!({ "detail": e.detail() })))?;
    Ok(Json(body).into_response())
}

// ---------------------------------------------------------------------------
// the token
// ---------------------------------------------------------------------------

/// The cookie the browser carries a configured token in.
const COOKIE: &str = "evo_web_token";

/// `/api` when a token is configured: the header, or the cookie plus a same-origin
/// mutating request.
///
/// The two forms are not equivalent on purpose. A cookie is attached by the
/// browser to *any* request to this origin, including one a page on another site
/// made, so a state-changing request that authenticates by cookie alone has to
/// prove it is same-origin — and the proof is the `Origin` header the browser
/// sends, which a cross-site page cannot forge. A request with the token in
/// `Authorization` needs no proof: no other site can read this server's token, so
/// one that carries it was written by something that knows it.
async fn require_token(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(token) = state.token.clone() else {
        // No token configured: loopback only, and the bind rule in `cli` is what
        // keeps it that way.
        return next.run(request).await;
    };
    let method = request.method().clone();
    let headers = request.headers().clone();
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let cookie = cookie_value(&headers, COOKIE);

    let authorized = match (bearer, cookie) {
        (Some(given), _) if constant_eq(given, &token) => true,
        (_, Some(given)) if constant_eq(given, &token) => {
            !mutating(&method) || same_origin(&headers)
        }
        _ => false,
    };
    if authorized {
        return next.run(request).await;
    }
    ApiError::unauthorized(
        "this server requires a token: send `Authorization: Bearer …`, or the \
         evo_web_token cookie from a same-origin page",
    )
    .into_response()
}

fn mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether a request says it came from this server.
///
/// A browser sets `Origin` on every request whose method is not GET/HEAD — a
/// same-origin one included — so a mutating request that authenticates by cookie
/// and carries no `Origin` at all is not one a browser made, and is refused. The
/// authority has to be this server's: `Origin` is a scheme and an authority and
/// nothing else.
fn same_origin(headers: &HeaderMap) -> bool {
    let (Some(origin), Some(host)) = (
        headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()),
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
    ) else {
        return false;
    };
    origin
        .rsplit_once("://")
        .map(|(_, authority)| authority == host)
        .unwrap_or(false)
}

/// A named cookie's value, if the request carries it.
fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let want = format!("{name}=");
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|pair| pair.strip_prefix(&want))
}

/// Compare two secrets without leaking their length or where they differ.
fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn a_cookie_is_read_out_of_the_header() {
        let map = headers(&[("cookie", "a=1; evo_web_token=s3cret; b=2")]);
        assert_eq!(cookie_value(&map, "evo_web_token"), Some("s3cret"));
        assert_eq!(cookie_value(&map, "nope"), None);
        assert_eq!(cookie_value(&HeaderMap::new(), COOKIE), None);
    }

    #[test]
    fn a_mutating_request_by_cookie_has_to_be_same_origin() {
        let same = headers(&[
            ("origin", "http://127.0.0.1:8787"),
            ("host", "127.0.0.1:8787"),
        ]);
        assert!(same_origin(&same));
        let other = headers(&[
            ("origin", "http://evil.example"),
            ("host", "127.0.0.1:8787"),
        ]);
        assert!(!same_origin(&other));
        // No Origin at all: a browser sets it on every mutating request, so this
        // is not one a browser made — and it is not proved same-origin.
        assert!(!same_origin(&HeaderMap::new()));
        // An Origin with no authority in it proves nothing either.
        let opaque = headers(&[("origin", "null"), ("host", "127.0.0.1:8787")]);
        assert!(!same_origin(&opaque));
    }

    /// The bundle's 404 rule: a page route gets the app, a missing asset does not.
    #[test]
    fn only_a_page_route_falls_back_to_the_app() {
        let dir = std::env::temp_dir().join(format!("evo-web-page-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let index = dir.join("index.html");
        std::fs::write(&index, "<!doctype html><h1>app</h1>").unwrap();

        let page = |path: &str| {
            page_for(
                &Request::builder().uri(path).body(Body::empty()).unwrap(),
                &index,
            )
        };
        assert_eq!(page("/").status(), StatusCode::OK);
        assert_eq!(page("/tab/7f3a").status(), StatusCode::OK);
        assert_eq!(page("/settings").status(), StatusCode::OK);
        // A name with an extension is an asset: a 404, never the HTML shell.
        assert_eq!(page("/evo_webapp.js").status(), StatusCode::NOT_FOUND);
        assert_eq!(page("/assets/app.css").status(), StatusCode::NOT_FOUND);
        assert_eq!(
            page("/dist/web/evo_webapp_bg.wasm").status(),
            StatusCode::NOT_FOUND
        );

        // And with no bundle at all, a page route says so rather than 404ing
        // silently.
        let missing = dir.join("nope/index.html");
        let body = page_for(
            &Request::builder()
                .uri("/tab/1")
                .body(Body::empty())
                .unwrap(),
            &missing,
        );
        assert_eq!(body.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_state_changing_methods_are_mutating() {
        assert!(mutating(&Method::POST));
        assert!(mutating(&Method::DELETE));
        assert!(!mutating(&Method::GET));
        assert!(!mutating(&Method::HEAD));
    }

    #[test]
    fn secrets_are_compared_whole() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "abcd"));
        assert!(!constant_eq("", "a"));
        assert!(constant_eq("", ""));
    }

    // -----------------------------------------------------------------------
    // The router itself: a request in, a reply out. The two things worth proving
    // here are the ones a helper test cannot — that the token check is really in
    // front of every /api route, and that a path this server does not serve is
    // answered rather than falling through to the frontend.
    // -----------------------------------------------------------------------

    use tower::ServiceExt;

    /// A server over a temp bundle, with `token` required if it is given.
    fn test_router(token: Option<&str>) -> (Router, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "evo-web-router-{}-{}",
            std::process::id(),
            token.unwrap_or("none")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<!doctype html><h1>app</h1>").unwrap();
        let mut args = vec![
            "--dist".to_owned(),
            dir.display().to_string(),
            "--root".to_owned(),
            dir.join("root").display().to_string(),
        ];
        if let Some(token) = token {
            args.push("--auth-token".to_owned());
            args.push(token.to_owned());
        }
        let cli::Outcome::Run(config) =
            cli::from_args(args, &|_| None).expect("the arguments parse")
        else {
            panic!("not help");
        };
        let state = AppState {
            tabs: Arc::new(Tabs::new(
                store::paths::Root::at(config.root.clone()),
                config.agent_bin.clone(),
                config.swarm_bin.clone(),
                None,
            )),
            agent_bin: config.agent_bin.clone(),
            swarm_bin: config.swarm_bin.clone(),
            token: config.token.clone().map(Arc::new),
            bind: "127.0.0.1:0".to_owned(),
        };
        let app = router(&config, &state);
        (app, dir)
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[test]
    fn without_a_token_every_api_route_is_reachable_and_the_bundle_is_served() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (app, dir) = test_router(None);
            let (status, body) = call(&app, "GET", "/api/health", &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("\"program\":\"evo-web\""), "{body}");

            let (status, body) = call(&app, "GET", "/api/tabs", &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, "{\"tabs\":[]}");

            // The frontend, and a client-side route through it.
            let (status, body) = call(&app, "GET", "/", &[]).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("<h1>app</h1>"), "{body}");
            let (status, _) = call(&app, "GET", "/tab/7f3a", &[]).await;
            assert_eq!(status, StatusCode::OK);

            // A path under /api that is not a route is JSON, and not the SPA.
            let (status, body) = call(&app, "GET", "/api/nope", &[]).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(body.starts_with("{\"error\""), "{body}");

            std::fs::remove_dir_all(&dir).unwrap();
        });
    }

    #[test]
    fn with_a_token_the_api_is_closed_and_the_bundle_is_not() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (app, dir) = test_router(Some("hunter2"));

            // No token, a wrong token, a token that is a prefix of the real one.
            for headers in [
                vec![],
                vec![("authorization", "Bearer nope")],
                vec![("authorization", "Bearer hunter")],
                vec![("authorization", "hunter2")],
            ] {
                let (status, body) = call(&app, "GET", "/api/tabs", &headers).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{headers:?} {body}");
            }
            let (status, body) = call(
                &app,
                "GET",
                "/api/tabs",
                &[("authorization", "Bearer hunter2")],
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");

            // The bundle is not behind the token: a browser has to load the page
            // before it can be asked for anything.
            let (status, _) = call(&app, "GET", "/", &[]).await;
            assert_eq!(status, StatusCode::OK);

            // A cookie read is allowed; a cookie write has to be same-origin.
            let cookie = [("cookie", "evo_web_token=hunter2")];
            let (status, _) = call(&app, "GET", "/api/tabs", &cookie).await;
            assert_eq!(status, StatusCode::OK);
            let (status, _) = call(&app, "POST", "/api/tabs", &cookie).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "no Origin at all");
            let (status, _) = call(
                &app,
                "POST",
                "/api/tabs",
                &[
                    ("cookie", "evo_web_token=hunter2"),
                    ("origin", "http://evil.example"),
                    ("host", "127.0.0.1:8787"),
                ],
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "another origin");
            // Same-origin gets past the token check and fails on its own merits
            // (an empty body is not an open request), which is what proves it
            // was let through.
            let (status, body) = call(
                &app,
                "POST",
                "/api/tabs",
                &[
                    ("cookie", "evo_web_token=hunter2"),
                    ("origin", "http://127.0.0.1:8787"),
                    ("host", "127.0.0.1:8787"),
                    ("content-type", "application/json"),
                ],
            )
            .await;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "{body}");

            std::fs::remove_dir_all(&dir).unwrap();
        });
    }
}
