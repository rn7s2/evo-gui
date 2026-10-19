//! The config files, read and written through the server that served this page.
//!
//! A browser has no filesystem: `~/.evo/init.lisp` and a project's `.evo/` are on
//! the **serving** machine, and a page can neither open nor write them. What it
//! can do is ask the `evo-web` server that served it, which holds the files and
//! does the reading and the writing on its side:
//!
//! ```text
//! GET  /api/config?scope=global|project&folder=<path>&file=init
//!      -> { file, scope, path, text, base }
//! POST /api/config   { scope, folder?, file, text, base? }
//!      -> { saved: true }
//! ```
//!
//! The two answers are shaped like the two calls they stand in for —
//! [`store::config_file::read`]'s `Result<Option<String>, ConfigError>` and
//! [`store::config_file::save`]'s `Result<(), SaveError>` — so
//! [`ConfigEditor`](crate::ConfigEditor) does not branch on the target: it calls
//! these two functions, which are `store::config_file` on a host and this on a
//! browser. That is the whole point of the seam.
//!
//! The same server is also the authority for the two binaries, which a page cannot
//! run either:
//!
//! ```text
//! GET  /api/probe?name=<evo-swarm|evo-agent>&path=<path>
//!      -> { check: ready|missing|not-executable|not-evo|timed-out, line? }
//! ```
//!
//! That answer is [`store::cli::Verdict`]'s vocabulary — the same one the desktop
//! app's own probe produces — so a Settings row says the same thing in a browser as
//! it does in the app. See [`probe`].
//!
//! `base` is what makes a browser's save safe. The server refuses a write whose
//! `base` is not the text the file holds now, so an edit made in another window —
//! or by evo itself — is a refusal rather than a silent overwrite, exactly as
//! [`store::config_file::save`] refuses one on a host.

use std::path::PathBuf;

use store::config_file::{ConfigError, ConfigFile, SaveError};
use store::ConfigScope;

// The two wasm-bindgen traits the answers arrive through: `dyn_into` to see a
// `fetch` reply as a `Response`, and `as_string` to read a `JsValue` that is one.
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;

/// A config file, named the way the route names one.
///
/// The wire spelling is the file's own stem with dashes for the two words:
/// `init.lisp` is `init`, `post-init.lisp` is `post-init`. The server maps it back
/// to a name inside the scope's directory, so nothing here sends a path.
pub fn file_token(file: ConfigFile) -> &'static str {
    match file {
        ConfigFile::Init => "init",
        ConfigFile::PostInit => "post-init",
        ConfigFile::Swarm => "swarm",
        ConfigFile::Memory => "memory",
        ConfigFile::Lore => "lore",
    }
}

/// Read one config file: `Ok(Some(text))` when it is there, `Ok(None)` when it is
/// not yet — the same two answers `store::config_file::read` gives.
///
/// A refusal the server reports is an `Err` with the server's own words, which is
/// what the editor shows; a file that is simply not there is `Ok(None)`, which is
/// an empty editor that saving creates.
pub async fn read(scope: &ConfigScope, file: ConfigFile) -> Result<Option<String>, ConfigError> {
    let query = query_for(scope, file);
    let body = get(&format!("/api/config?{query}"))
        .await
        .map_err(|reason| ConfigError::Io {
            path: store::config_file::path(scope, file),
            source: std::io::Error::other(reason),
        })?;
    // The route sends `base` as a **string** whose empty value means "the file was
    // not there" — its own documented convention, and the same one the desktop
    // editor's buffer uses (its `base` is `None` for a file that did not exist). So
    // an empty base is `Ok(None)` here: the "write it" path a first run takes.
    let base = body
        .get("base")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if base.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        body.get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    ))
}

/// Write one config file, refusing a write whose `base` is not what the file holds
/// now — the same refusal `store::config_file::save` makes on a host.
///
/// The server validates the text as Lisp before it writes, so a typo is refused
/// here rather than at the next boot; what it reports is what the editor shows.
pub async fn save(
    scope: &ConfigScope,
    file: ConfigFile,
    text: &str,
    base: Option<&str>,
) -> Result<(), SaveError> {
    let path = store::config_file::path(scope, file);
    let mut payload = serde_json::json!({
        "scope": scope_token(scope),
        "file": file_token(file),
        "text": text,
    });
    if let Some(base) = base {
        payload["base"] = serde_json::Value::String(base.to_owned());
    }
    if let Some(folder) = folder_of(scope) {
        payload["folder"] = serde_json::Value::String(folder.display().to_string());
    }

    let refused = path.clone();
    let body = post("/api/config", &payload).await.map_err(|reason| {
        // The server refuses a save for the reasons a host does — a conflict, a
        // symlink, text that is not Lisp — and says which in its own words. The
        // editor shows the message, so the classification only has to be honest
        // about the one thing it can tell from here.
        SaveError::Io {
            path: refused,
            source: std::io::Error::other(reason),
        }
    })?;
    match body.get("saved").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(SaveError::Io {
            path,
            source: std::io::Error::other("the server did not report a save"),
        }),
    }
}

/// `global`, or `project` — the two scopes the route knows.
fn scope_token(scope: &ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Global => "global",
        ConfigScope::Project(_) => "project",
    }
}

/// The folder a project scope names, which the route needs to find `.evo/`.
fn folder_of(scope: &ConfigScope) -> Option<PathBuf> {
    match scope {
        ConfigScope::Global => None,
        ConfigScope::Project(folder) => Some(folder.clone()),
    }
}

/// The query for one read. `folder` is percent-encoded: a path holds spaces and
/// `#`, and a query that broke on one would be a file nobody could open.
fn query_for(scope: &ConfigScope, file: ConfigFile) -> String {
    let mut query = format!("scope={}&file={}", scope_token(scope), file_token(file));
    if let Some(folder) = folder_of(scope) {
        query.push_str(&format!(
            "&folder={}",
            encode(&folder.display().to_string())
        ));
    }
    query
}

/// Everything outside the unreserved set becomes `%XX`, so a value cannot break
/// out of the query it is in.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Ask the server whether the machine behind this page can run a binary, and what
/// it says about itself.
///
/// `name` is `evo-swarm` or `evo-agent` — the route accepts exactly those two, so
/// that it is not a general "run this path for me" oracle — and `path` is the path
/// the row holds.
///
/// `None` is "no answer": the server could not be reached, or refused the request.
/// [`probe`](crate::probe::probe) turns that into `Check::Missing`, which is the
/// honest thing to show — the row must not hang at `checking…` because a request
/// failed, and it must not claim a binary is fine on no evidence.
pub async fn probe(name: &str, path: &std::path::Path) -> Option<serde_json::Value> {
    let query = format!(
        "name={}&path={}",
        encode(name),
        encode(&path.display().to_string())
    );
    get(&format!("/api/probe?{query}")).await.ok()
}

// ---------------------------------------------------------------------------
// The transport
// ---------------------------------------------------------------------------

/// One `GET`, its body as JSON, or the reason it failed.
///
/// `fetch` resolves a relative URL against the page's own origin, which is exactly
/// the server that served this page — so the routes above need no host and no
/// configuration.
async fn get(path: &str) -> Result<serde_json::Value, String> {
    request("GET", path, None).await
}

async fn post(path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    request("POST", path, Some(body)).await
}

async fn request(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let options = web_sys::RequestInit::new();
    options.set_method(method);
    if let Some(body) = body {
        let text = serde_json::to_string(body).map_err(|e| e.to_string())?;
        options.set_body(&JsValue::from_str(&text));
    }
    let request = web_sys::Request::new_with_str_and_init(path, &options)
        .map_err(|_| format!("cannot ask the server for {path}"))?;
    request
        .headers()
        .set("content-type", "application/json")
        .map_err(|_| "cannot set the request's content type".to_owned())?;

    let response = wasm_bindgen_futures::JsFuture::from(
        web_sys::window()
            .ok_or("this page has no window")?
            .fetch_with_request(&request),
    )
    .await
    .map_err(|_| format!("the server did not answer {path}"))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| "the answer was not a response".to_owned())?;

    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|_| "cannot read the answer".to_owned())?,
    )
    .await
    .map_err(|_| "cannot read the answer".to_owned())?
    .as_string()
    .unwrap_or_default();

    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if !response.ok() {
        // The server's own words, which is what the editor shows. `{"error": …}`
        // is the shape every refusal of that server has.
        let reason = body
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("the server refused {method} {path}"));
        return Err(reason);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire spelling of each file: what the route maps back to a name inside
    /// the scope's directory.
    #[test]
    fn a_file_is_named_by_its_stem() {
        assert_eq!(file_token(ConfigFile::Init), "init");
        assert_eq!(file_token(ConfigFile::PostInit), "post-init");
        assert_eq!(file_token(ConfigFile::Swarm), "swarm");
        assert_eq!(file_token(ConfigFile::Memory), "memory");
        assert_eq!(file_token(ConfigFile::Lore), "lore");
        // Every file has one, so a new one cannot be added without a spelling.
        for file in ConfigFile::ALL {
            assert!(!file_token(file).is_empty());
            assert!(file_token(file).is_ascii());
        }
    }

    #[test]
    fn the_scope_is_the_route_s_word_and_a_project_names_its_folder() {
        assert_eq!(scope_token(&ConfigScope::Global), "global");
        let project = ConfigScope::Project(PathBuf::from("/tmp/foo"));
        assert_eq!(scope_token(&project), "project");
        assert_eq!(folder_of(&ConfigScope::Global), None);
        assert_eq!(folder_of(&project), Some(PathBuf::from("/tmp/foo")));
    }

    /// A folder with a space or a `#` in it has to survive the query, and a value
    /// has to be unable to break out of it.
    #[test]
    fn a_query_encodes_what_would_break_it() {
        let query = query_for(
            &ConfigScope::Project(PathBuf::from("/tmp/my folder#2")),
            ConfigFile::Init,
        );
        assert_eq!(
            query,
            "scope=project&file=init&folder=%2Ftmp%2Fmy%20folder%232"
        );
        assert_eq!(
            query_for(&ConfigScope::Global, ConfigFile::Lore),
            "scope=global&file=lore"
        );
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(
            encode("/Users/me/coding/foo"),
            "%2FUsers%2Fme%2Fcoding%2Ffoo"
        );
    }

    /// The probe's query has to survive a path with a space, a `#` or a `?` in it:
    /// a query that broke on one would be a row that could not be answered.
    #[test]
    fn a_probe_query_encodes_the_path_it_asks_about() {
        let query = format!(
            "name={}&path={}",
            encode("evo-swarm"),
            encode("/Applications/My App#2/evo-swarm")
        );
        assert_eq!(
            query,
            "name=evo-swarm&path=%2FApplications%2FMy%20App%232%2Fevo-swarm"
        );
        assert_eq!(encode("evo-agent"), "evo-agent");
    }

    /// The route's own convention, and the one thing about it a caller can get
    /// wrong: `base` is a **string**, and empty means the file was not there.
    #[test]
    fn an_empty_base_is_a_file_that_is_not_there() {
        let present = serde_json::json!({"text": "hi", "base": "hi"});
        let base = present
            .get("base")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert_eq!(base, "hi");
        assert!(!base.is_empty(), "a file that is there has a base");

        let absent = serde_json::json!({"text": "", "base": ""});
        let base = absent
            .get("base")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(base.is_empty(), "a file that is not there has none");
    }
}
