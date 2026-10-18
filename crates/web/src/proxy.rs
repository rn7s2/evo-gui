//! The proxy: `/api/tabs/<id>/…` is the evo protocol, with the bearer token added
//! here and never handed to the browser.
//!
//! Every route below is a route on the serve protocol and nothing else — there is
//! no catch-all that forwards whatever path a caller typed. The tab's
//! [`Client`](swarm_client::Client) already owns the address, the port and the
//! token, and it builds each request path itself (percent-encoding every value it
//! interpolates), so a path segment from a URL can never become part of the
//! request line, a header, or a shell.
//!
//! The client is blocking — a plain `TcpStream`, because serve's HTTP is small and
//! an SSE stream is read line by line anyway — so every call runs on
//! `spawn_blocking`. The one exception is the stream, which is read on its own
//! thread and relayed as it arrives: an SSE body that was collected first would
//! not be a stream.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::Value;
use swarm_client::{
    Client, Cursor, SseConnection, TOPIC_LANE_WILDCARD, TOPIC_SESSION, TOPIC_SWARM,
};

use crate::tabs::{ApiError, Tab};
use crate::AppState;

/// The topics a tab always reads: the coordinator's, the swarm's, and every
/// lane's — the same set the desktop app subscribes to, so the browser and the
/// app see one session the same way.
fn tab_topics() -> Vec<String> {
    vec![
        TOPIC_SESSION.to_owned(),
        TOPIC_SWARM.to_owned(),
        TOPIC_LANE_WILDCARD.to_owned(),
    ]
}

/// `?topics=a,b` — a comma-separated list, or this server's default.
fn topics_from(param: Option<&String>) -> Result<Vec<String>, ApiError> {
    let Some(text) = param else {
        return Ok(tab_topics());
    };
    let topics: Vec<String> = text
        .split(',')
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .map(|topic| {
            if topic.len() > 128 || topic.chars().any(char::is_control) {
                Err(ApiError::bad_request("a topic name is not usable"))
            } else {
                Ok(topic.to_owned())
            }
        })
        .collect::<Result<_, _>>()?;
    if topics.is_empty() {
        return Err(ApiError::bad_request("topics is empty"));
    }
    Ok(topics)
}

/// A path segment this server passes on: an item id, a topic. It is never
/// concatenated into anything without encoding (`swarm_client` percent-encodes
/// it), but a value that cannot be one is refused here rather than sent.
fn segment(name: &str, value: &str) -> Result<String, ApiError> {
    if value.is_empty() {
        return Err(ApiError::bad_request(format!("{name} is required")));
    }
    if value.len() > 256 {
        return Err(ApiError::bad_request(format!("{name} is too long")));
    }
    if value
        .chars()
        .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(ApiError::bad_request(format!(
            "{name} contains a character that cannot be part of one"
        )));
    }
    Ok(value.to_owned())
}

/// The tab a path names, or a 404 that says which id was not found.
async fn tab(state: &AppState, id: &str) -> Result<std::sync::Arc<Tab>, ApiError> {
    state
        .tabs
        .get(id)
        .ok_or_else(|| ApiError::not_found(format!("no tab {id}")))
}

/// A blocking client call, off the runtime.
async fn on_client<T, F>(call: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(call)
        .await
        .map_err(|e| ApiError::internal(format!("the request task failed: {e}")))?
}

/// The client of a running tab.
async fn client(state: &AppState, id: &str) -> Result<Client, ApiError> {
    tab(state, id).await?.client()
}

// ---------------------------------------------------------------------------
// GET /snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotQuery {
    topics: Option<String>,
    items: Option<u32>,
}

/// `GET /api/tabs/<id>/snapshot?topics=&items=` — the whole view, atomically.
pub async fn snapshot(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Response, ApiError> {
    let topics = topics_from(query.topics.as_ref())?;
    // A cap, not a preference: this body is the whole transcript and it goes to a
    // browser. The protocol's own default is 200.
    let items = query.items.map(|n| n.clamp(1, 1000));
    let client = client(&state, &id).await?;
    let body = on_client(move || client.snapshot(&topics, items).map_err(ApiError::from)).await?;
    Ok(json_response(
        serde_json::to_value(body).map_err(json_error)?,
    ))
}

// ---------------------------------------------------------------------------
// GET /stream (SSE, relayed as it arrives)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct StreamQuery {
    topics: Option<String>,
    since: Option<String>,
}

/// `GET /api/tabs/<id>/stream?topics=&since=` — the op stream, relayed line by
/// line and never buffered.
///
/// The cursor is the protocol's own (`<epoch>.<seq>`); a caller that has one
/// resumes where it was, and one that does not starts from now. A reconnection
/// after a server restart is the protocol's business (`stream.reset`), not this
/// server's.
pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let topics = topics_from(query.topics.as_ref())?;
    let since = match query.since.as_deref() {
        Some(text) => Some(
            Cursor::decode(text)
                .ok_or_else(|| ApiError::bad_request(format!("since {text:?} is not a cursor")))?,
        ),
        None => None,
    };
    let client = client(&state, &id).await?;
    // Connecting is blocking (a socket and its response head), so it happens off
    // the runtime; reading the lines does not touch the runtime at all.
    let connection = on_client(move || {
        client
            .open_stream(&topics, since.as_ref())
            .map_err(ApiError::from)
    })
    .await?;
    Ok(sse_response(connection))
}

/// An SSE body relayed from a live connection.
///
/// The reader is a plain thread — the connection is a blocking `TcpStream` — and
/// what it reads is handed to the body one line at a time through a bounded
/// channel, so a browser that stops reading applies back pressure instead of
/// letting this process grow a buffer. A `: ping` comment is relayed as it is: it
/// is what keeps the connection warm.
struct SseStream {
    lines: tokio::sync::mpsc::Receiver<Result<String, String>>,
    /// A second handle on the same socket, so dropping this body unblocks the
    /// reader thread at once instead of letting it sit out its read timeout.
    closer: std::net::TcpStream,
}

impl Drop for SseStream {
    fn drop(&mut self) {
        let _ = self.closer.shutdown(std::net::Shutdown::Both);
    }
}

impl Stream for SseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.lines.poll_recv(cx) {
            Poll::Ready(Some(Ok(line))) => Poll::Ready(Some(Ok(Bytes::from(format!("{line}\n"))))),
            Poll::Ready(Some(Err(reason))) => {
                Poll::Ready(Some(Err(std::io::Error::other(reason))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn sse_response(mut connection: SseConnection) -> Response {
    let closer = match connection.socket().try_clone() {
        Ok(socket) => socket,
        Err(error) => {
            connection.close();
            return ApiError::bad_gateway(format!("cannot hold the stream: {error}"), Value::Null)
                .into_response();
        }
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, String>>(256);
    std::thread::spawn(move || {
        loop {
            match connection.read_line() {
                // `None` is the end of the stream: the server stopped, or the
                // browser went away and the socket was shut down under us.
                Ok(None) => break,
                Ok(Some(line)) => {
                    if tx.blocking_send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.blocking_send(Err(error.to_string()));
                    break;
                }
            }
        }
    });

    let body = axum::body::Body::from_stream(SseStream { lines: rx, closer });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap_or_else(|_| ApiError::internal("cannot build the stream response").into_response())
}

// ---------------------------------------------------------------------------
// POST /ops
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct OpsBody {
    rid: Option<String>,
    op: String,
    #[serde(default)]
    args: Value,
}

/// `POST /api/tabs/<id>/ops` — one operation, its reply relayed whole.
///
/// The body is the protocol's `{rid, op, args}`; `rid` may be left out and this
/// server mints one (the server dedupes retries by rid, so a caller that *does*
/// send one gets the free retry). An answered op is always HTTP 200 in this
/// protocol — a refusal is `{"ok":false,"error":{…}}` in the body — so this
/// handler does not turn one into a status code, and the frontend branches on
/// `ok` the way every other client does.
pub async fn ops(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<OpsBody>,
) -> Result<Response, ApiError> {
    let op = segment("op", &body.op)?;
    let rid = match body.rid {
        Some(rid) => segment("rid", &rid)?,
        None => swarm_client::new_rid(),
    };
    let client = client(&state, &id).await?;
    let reply = on_client(move || {
        client
            .op_with_rid(&rid, &op, body.args)
            .map_err(ApiError::from)
    })
    .await?;
    Ok(json_response(
        serde_json::to_value(reply).map_err(json_error)?,
    ))
}

// ---------------------------------------------------------------------------
// GET /items, /items/<id>, /media/<id>/<n>, /catalog
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ItemsQuery {
    topic: Option<String>,
    before: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/tabs/<id>/items?topic=&before=&limit=` — older items, paging back.
pub async fn items(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Response, ApiError> {
    let topic = segment("topic", query.topic.as_deref().unwrap_or_default())?;
    let before = query.before.map(|b| segment("before", &b)).transpose()?;
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let client = client(&state, &id).await?;
    let body = on_client(move || {
        client
            .items(&topic, before.as_deref(), limit)
            .map_err(ApiError::from)
    })
    .await?;
    Ok(json_response(body))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemQuery {
    topic: Option<String>,
}

/// `GET /api/tabs/<id>/items/<item_id>?topic=` — one item, whole.
pub async fn item(
    State(state): State<AppState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ItemQuery>,
) -> Result<Response, ApiError> {
    let topic = segment("topic", query.topic.as_deref().unwrap_or_default())?;
    let item_id = segment("item id", &item_id)?;
    let client = client(&state, &id).await?;
    let body = on_client(move || client.item(&topic, &item_id).map_err(ApiError::from)).await?;
    Ok(json_response(body))
}

/// `GET /api/tabs/<id>/media/<item_id>/<n>?topic=` — image bytes, relayed with
/// their own content type.
///
/// This is the one route that hands back bytes rather than JSON, so the item id
/// is checked here — it is a path segment of *our* URL and it must not be one
/// that could be read as a path anywhere else. It reaches serve only as a
/// percent-encoded segment of a request line this process builds itself.
pub async fn media(
    State(state): State<AppState>,
    Path((id, item_id, n)): Path<(String, String, String)>,
    Query(query): Query<ItemQuery>,
) -> Result<Response, ApiError> {
    let topic = segment("topic", query.topic.as_deref().unwrap_or_default())?;
    let item_id = segment("item id", &item_id)?;
    let n = n
        .parse::<u32>()
        .map_err(|_| ApiError::bad_request(format!("media index {n:?} is not a number")))?;
    let client = client(&state, &id).await?;
    let (bytes, content_type) =
        on_client(move || client.media(&topic, &item_id, n).map_err(ApiError::from)).await?;
    let content_type = HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| ApiError::internal("cannot build the media response").into_response()))
}

/// `GET /api/tabs/<id>/catalog` — the running session's own registry.
pub async fn catalog(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let client = client(&state, &id).await?;
    let body = on_client(move || client.catalog().map_err(ApiError::from)).await?;
    Ok(json_response(body))
}

// ---------------------------------------------------------------------------

fn json_response(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn json_error(error: serde_json::Error) -> ApiError {
    ApiError::internal(format!("cannot encode the reply: {error}"))
}

/// Every route a tab's own protocol lives on. Registered flat, under the tab's
/// own id, so there is no catch-all that forwards a path this server does not
/// know.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tabs/{id}/snapshot", axum::routing::get(snapshot))
        .route("/api/tabs/{id}/stream", axum::routing::get(stream))
        .route("/api/tabs/{id}/ops", axum::routing::post(ops))
        .route("/api/tabs/{id}/items", axum::routing::get(items))
        .route("/api/tabs/{id}/items/{item_id}", axum::routing::get(item))
        .route(
            "/api/tabs/{id}/media/{item_id}/{n}",
            axum::routing::get(media),
        )
        .route("/api/tabs/{id}/catalog", axum::routing::get(catalog))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topic_list_defaults_to_the_tabs_own_topics() {
        assert_eq!(
            topics_from(None).unwrap(),
            vec!["session", "swarm", "lane:*"]
        );
        assert_eq!(
            topics_from(Some(&"session,lane:3".to_owned())).unwrap(),
            vec!["session", "lane:3"]
        );
        assert!(topics_from(Some(&"".to_owned())).is_err());
        assert!(topics_from(Some(&"session,\u{7}".to_owned())).is_err());
    }

    /// A path segment is refused when it cannot be one — a traversal attempt
    /// never becomes part of a request.
    #[test]
    fn a_segment_with_a_slash_is_refused() {
        assert!(segment("item id", "../../etc/passwd").is_err());
        assert!(segment("item id", "a/b").is_err());
        assert!(segment("item id", "a\\b").is_err());
        assert!(segment("item id", "").is_err());
        assert!(segment("item id", "e_9c1f").is_ok());
    }

    #[test]
    fn the_media_index_has_to_be_a_number() {
        assert!("12".parse::<u32>().is_ok());
        assert!("1/2".parse::<u32>().is_err());
        assert!("-1".parse::<u32>().is_err());
    }
}
