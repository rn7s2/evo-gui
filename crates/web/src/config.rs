//! `/api/config` — evo's own config files, for a browser that has no filesystem.
//!
//! The desktop app's Settings panel edits five files through `store::config_file`:
//! `init.lisp`, `post-init.lisp`, `swarm.lisp`, `memory.sexp` and `lore.sexp`, in
//! either the global evo home or one project's `.evo/`. A page cannot read or
//! write a file, so this module is the same five files over HTTP.
//!
//! Nothing about the paths, the scopes or the write rule is re-derived here: the
//! paths come from [`store::config_file::path`], the scope from
//! [`store::config_file::ConfigScope`], and the write from
//! [`store::config_file::save`] — which validates the text with a real SBCL and
//! refuses when the file changed underneath the editor. That last one is why
//! `base` travels over the wire at all: it is the text the client read, and
//! passing it through turns a concurrent edit into a refusal instead of a lost
//! write.
//!
//! ```text
//! GET  /api/config?scope=global|project&folder=<path>&file=init|post-init|swarm|memory|lore
//!      -> { "file", "scope", "path", "text", "base" }
//! POST /api/config   { scope, folder?, file, text, base? }
//!      -> { "saved": true }
//! ```

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use store::config_file::{self, ConfigFile, ConfigScope, SaveError};

use crate::tabs::ApiError;
use crate::AppState;

/// `GET /api/config?…`
#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    scope: Option<String>,
    folder: Option<String>,
    file: Option<String>,
}

/// `POST /api/config`
#[derive(Debug, Deserialize)]
pub struct SaveRequest {
    scope: Option<String>,
    folder: Option<String>,
    file: Option<String>,
    /// The text to write. Required — an absent `text` is a caller bug, not an
    /// empty file.
    #[serde(default)]
    text: Option<String>,
    /// What the client read. `Some` means "the file was there and held this";
    /// `None` (or absent) means "the file was not there". See [`base_of`].
    base: Option<String>,
}

/// One file, as the editor sees it.
#[derive(Debug, Serialize)]
pub struct ConfigView {
    /// The short key the caller asked with: `init`, `post-init`, …
    file: &'static str,
    /// `global` or `project`.
    scope: &'static str,
    /// Where it lives on the serving machine, for a person to read.
    path: String,
    /// The file's text. Empty when the file is not there yet.
    text: String,
    /// What a save must send back as `base`. Empty means "the file was not
    /// there" — the same convention the desktop editor uses (its buffer's `base`
    /// is `None` for a file that did not exist, and it saves that as no base).
    base: String,
}

/// `GET /api/config` — read one of the five files.
///
/// A file that is not there is **not** an error: a project that has never been
/// configured opens as an empty editor, and saving it creates the file. That is
/// what `store::config_file::read` already does (`Ok(None)`), and the empty
/// `text`/`base` pair is how this route says the same thing.
pub async fn read(
    State(_state): State<AppState>,
    Query(query): Query<ReadQuery>,
) -> Result<Response, ApiError> {
    let scope = scope_of(query.scope.as_deref(), query.folder.as_deref())?;
    let file = file_of(query.file.as_deref())?;
    let path = config_file::path(&scope, file);

    let read = path.clone();
    let text = tokio::task::spawn_blocking(move || config_file::read(&read))
        .await
        .map_err(|e| ApiError::internal(format!("the read task failed: {e}")))?
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    // `None` is "not there yet": an empty editor whose save will create it.
    let (text, base) = match text {
        Some(text) => (text.clone(), text),
        None => (String::new(), String::new()),
    };
    Ok(Json(ConfigView {
        file: key_of(file),
        scope: scope_key(&scope),
        path: path.to_string_lossy().into_owned(),
        text,
        base,
    })
    .into_response())
}

/// `POST /api/config` — write one, with the checks a save has always had.
///
/// Three refusals a caller can see:
///
/// * **409** when the file is no longer what `base` says it was — somebody else
///   changed it while this editor was open, and nothing is written;
/// * **400** when the text is not readable Lisp (SBCL's own words come back), or
///   the path is a symlink or not a regular file;
/// * **500** for an I/O failure.
///
/// The SBCL check is a child process. This server is native and may run one —
/// that is the whole reason the browser can have a save at all.
pub async fn save(
    State(_state): State<AppState>,
    Json(body): Json<SaveRequest>,
) -> Result<Response, ApiError> {
    let scope = scope_of(body.scope.as_deref(), body.folder.as_deref())?;
    let file = file_of(body.file.as_deref())?;
    let Some(text) = body.text else {
        return Err(ApiError::bad_request(
            "text is required: send the file's whole text (an empty string clears it)",
        ));
    };
    let base = base_of(body.base);
    let path = config_file::path(&scope, file);

    // A project's `.evo/` may not exist yet, and the browser has no way to make
    // one: the folder picker chooses a directory, not a file. Creating the scope
    // directory is the one thing this route does beyond `save`, and it is the
    // directory `ConfigScope` itself names — never a path a caller supplied.
    if let Some(dir) = path.parent() {
        if !dir.is_dir() {
            let made = dir.to_path_buf();
            tokio::task::spawn_blocking(move || store::paths::create_dir_private(&made))
                .await
                .map_err(|e| ApiError::internal(format!("the save task failed: {e}")))?
                .map_err(|e| ApiError::internal(format!("cannot make {}: {e}", dir.display())))?;
        }
    }

    tokio::task::spawn_blocking(move || config_file::save(&path, &text, base.as_deref()))
        .await
        .map_err(|e| ApiError::internal(format!("the save task failed: {e}")))?
        .map_err(save_error)?;

    Ok(Json(json!({ "saved": true })).into_response())
}

/// A save failure as the status a caller should see.
///
/// A conflict is its own status on purpose: it is the one failure the frontend
/// can offer to resolve (reload and try again), and it is not the caller's text
/// being wrong.
fn save_error(error: SaveError) -> ApiError {
    match error {
        SaveError::Conflict { .. } => ApiError::conflict(error.to_string()),
        SaveError::Check(_) | SaveError::Symlink { .. } | SaveError::NotRegular { .. } => {
            ApiError::bad_request(error.to_string())
        }
        SaveError::Io { .. } => ApiError::internal(error.to_string()),
    }
}

/// `base` as `store::config_file::save` wants it.
///
/// The wire has one string where the store has an `Option<&str>`, and the
/// convention is the desktop editor's: **an empty `base` means the file was not
/// there.** A file that exists is never empty in a way that matters here — if it
/// is empty, its text *is* the empty string, so `Some("")` and `None` compare the
/// same way against an empty file on disk, and the only case they differ in is a
/// file that was removed between the read and the save, which is a conflict
/// either way.
fn base_of(base: Option<String>) -> Option<String> {
    base.filter(|text| !text.is_empty())
}

/// The scope a request names.
fn scope_of(scope: Option<&str>, folder: Option<&str>) -> Result<ConfigScope, ApiError> {
    match scope.unwrap_or("global").trim() {
        "global" => Ok(ConfigScope::Global),
        "project" => {
            let folder = folder
                .map(str::trim)
                .filter(|folder| !folder.is_empty())
                .ok_or_else(|| {
                    ApiError::bad_request("scope=project needs folder=<the project's directory>")
                })?;
            Ok(ConfigScope::Project(std::path::PathBuf::from(folder)))
        }
        other => Err(ApiError::bad_request(format!(
            "scope {other:?} is not one of global, project"
        ))),
    }
}

/// The file a request names, by the short key the interface uses.
///
/// The key is the file's own name without its extension — `init`, `post-init`,
/// `swarm`, `memory`, `lore` — so the accepted set is exactly
/// [`ConfigFile::ALL`] and cannot drift from the files this server will open.
fn file_of(file: Option<&str>) -> Result<ConfigFile, ApiError> {
    let wanted = file.unwrap_or("").trim();
    ConfigFile::ALL
        .into_iter()
        .find(|file| key_of(*file) == wanted)
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "file {wanted:?} is not one of {}",
                ConfigFile::ALL
                    .iter()
                    .map(|file| key_of(*file))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// A file's short key: its name without the extension.
fn key_of(file: ConfigFile) -> &'static str {
    file.file_name()
        .rsplit_once('.')
        .map(|(key, _)| key)
        .unwrap_or(file.file_name())
}

/// A scope's key, as the reply spells it.
fn scope_key(scope: &ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Global => "global",
        ConfigScope::Project(_) => "project",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_the_store_opens_has_a_key_and_nothing_else_does() {
        // The accepted set *is* `ConfigFile::ALL` — this is the check that the two
        // cannot drift, because a key is derived from the file, not listed beside
        // it.
        for file in ConfigFile::ALL {
            assert_eq!(file_of(Some(key_of(file))).unwrap(), file);
        }
        assert_eq!(key_of(ConfigFile::Init), "init");
        assert_eq!(key_of(ConfigFile::PostInit), "post-init");
        assert_eq!(key_of(ConfigFile::Swarm), "swarm");
        assert_eq!(key_of(ConfigFile::Memory), "memory");
        assert_eq!(key_of(ConfigFile::Lore), "lore");

        let err = file_of(Some("init.lisp")).unwrap_err();
        assert!(err.message.contains("not one of"), "{}", err.message);
        assert!(file_of(Some("passwd")).is_err());
        assert!(file_of(Some("")).is_err());
        assert!(file_of(None).is_err());
    }

    #[test]
    fn a_project_scope_needs_a_folder_and_the_default_is_global() {
        assert_eq!(scope_of(None, None).unwrap(), ConfigScope::Global);
        assert_eq!(
            scope_of(Some("global"), Some("/x")).unwrap(),
            ConfigScope::Global
        );
        assert_eq!(
            scope_of(Some("project"), Some("/work/proj")).unwrap(),
            ConfigScope::Project(std::path::PathBuf::from("/work/proj"))
        );
        assert!(scope_of(Some("project"), None).is_err());
        assert!(scope_of(Some("project"), Some("  ")).is_err());
        assert!(scope_of(Some("elsewhere"), None).is_err());
    }

    /// An empty base is the "the file was not there" convention, and it is the
    /// one the desktop editor uses too.
    #[test]
    fn an_empty_base_means_the_file_was_not_there() {
        assert_eq!(base_of(None), None);
        assert_eq!(base_of(Some(String::new())), None);
        assert_eq!(base_of(Some("(a b)".to_owned())), Some("(a b)".to_owned()));
    }

    /// The paths are `store::config_file`'s, not this module's: a project's files
    /// live in that project's `.evo/`, and the global ones in the evo home.
    #[test]
    fn the_path_is_the_stores_own() {
        let scope = ConfigScope::Project(std::path::PathBuf::from("/work/proj"));
        assert_eq!(
            config_file::path(&scope, ConfigFile::Init),
            std::path::PathBuf::from("/work/proj/.evo/init.lisp")
        );
        assert_eq!(
            config_file::path(&ConfigScope::Global, ConfigFile::Lore),
            store::paths::evo_dir().join("lore.sexp")
        );
    }

    #[test]
    fn a_conflict_is_its_own_status_and_a_syntax_error_is_the_callers() {
        let conflict = save_error(SaveError::Conflict {
            path: std::path::PathBuf::from("/x/init.lisp"),
            reason: "changed on disk since it was opened",
        });
        assert_eq!(conflict.status, 409);
        let refused = save_error(SaveError::Check(store::lispcheck::CheckError::Syntax(
            "unbalanced".to_owned(),
        )));
        assert_eq!(refused.status, 400);
        assert!(
            refused.message.contains("unbalanced"),
            "{}",
            refused.message
        );
    }
}
