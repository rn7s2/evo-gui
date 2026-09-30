//! The resumable swarms listed under the empty tab's choosers (§7.2, §2).
//!
//! The display strings — the folder's own name, the `~`-shortened path, the meta line and
//! the tooltip — come from `session::HistoryRow`, which is where the §9.5 rules live (paths
//! shortened around the home directory, times phrased against the clock, unknown parts left
//! out). This module adds only what the workspace acts on: the journal to resume and the
//! folder to run in.

use std::path::{Path, PathBuf};

use gpui_kit::SharedString;

/// One row of the empty tab's history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    /// The folder the swarm ran in: the cwd a resumed swarm runs in again.
    pub folder: PathBuf,
    /// The journal to resume (`POST`-free: passed as `--resume` to the new swarm).
    pub session_path: PathBuf,
    /// The folder's own name — the row's primary line.
    pub title: SharedString,
    /// The `~`-shortened folder path under it.
    pub subtitle: SharedString,
    /// `4 lanes · 2h ago · coordinator: …`, with the unknown parts left out.
    pub meta: SharedString,
    /// Everything known about the session, for the row's tooltip.
    pub tooltip: SharedString,
    /// The app had this session open when it last quit: the row wears a badge for it.
    pub open_at_quit: bool,
}

impl HistoryRow {
    /// One row from the session model's own display row.
    pub fn from_session(row: &session::HistoryRow) -> HistoryRow {
        HistoryRow {
            folder: PathBuf::from(&row.folder),
            session_path: PathBuf::from(&row.session_path),
            title: SharedString::from(row.title.clone()),
            subtitle: SharedString::from(row.subtitle.clone()),
            meta: SharedString::from(row.meta.clone()),
            tooltip: SharedString::from(row.tooltip.clone()),
            open_at_quit: row.open_at_quit,
        }
    }
}

/// The rows for a whole list the session model produced, newest first (its own order).
pub(crate) fn rows_from_session(rows: &[session::HistoryRow]) -> Vec<HistoryRow> {
    rows.iter().map(HistoryRow::from_session).collect()
}

/// The label for a folder: its own name, not its whole path — the tab strip shows the same
/// name for the tab a row opens.
pub(crate) fn folder_name(folder: &Path) -> SharedString {
    folder
        .file_name()
        .map(|name| SharedString::from(name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| SharedString::from(folder.to_string_lossy().into_owned()))
}

/// The rows a fresh tab starts with: none.
///
/// The list is filled by the session index (§2) through
/// [`TabContent::set_history_entries`](crate::TabContent::set_history_entries); this exists
/// so the tab's own construction has nothing to invent in the meantime.
pub fn placeholder_history() -> Vec<HistoryRow> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_row() -> session::HistoryRow {
        session::HistoryRow {
            title: "wire the history list to the session index".to_string(),
            subtitle: "~/coding/foo".to_string(),
            meta: "4 lanes · 2h ago · coordinator: gpt-5".to_string(),
            tooltip: "/Users/you/coding/foo · 2026-09-29 09:25:44 UTC (+00:00)".to_string(),
            session_path: "/Users/you/.evo/sessions/x/1.sexp".to_string(),
            folder: "/Users/you/coding/foo".to_string(),
            coordinator_model: Some("gpt-5".to_string()),
            lanes_model: None,
            source: session::HistorySource::Index,
            open_at_quit: false,
        }
    }

    #[test]
    fn a_row_keeps_the_model_display_text_and_adds_the_two_paths() {
        let row = HistoryRow::from_session(&session_row());

        assert_eq!(row.title, "wire the history list to the session index");
        assert_eq!(row.subtitle, "~/coding/foo");
        assert_eq!(row.meta, "4 lanes · 2h ago · coordinator: gpt-5");
        assert!(row.tooltip.starts_with("/Users/you/coding/foo"));
        // What the workspace acts on: resume this journal, run in this folder.
        assert_eq!(row.folder, PathBuf::from("/Users/you/coding/foo"));
        assert_eq!(
            row.session_path,
            PathBuf::from("/Users/you/.evo/sessions/x/1.sexp")
        );
    }

    #[test]
    fn a_fresh_tab_starts_with_no_history() {
        assert!(placeholder_history().is_empty());
    }

    #[test]
    fn a_folder_is_labelled_by_its_own_name() {
        assert_eq!(folder_name(Path::new("/Users/you/coding/foo")), "foo");
        // A trailing separator is not a name.
        assert_eq!(folder_name(Path::new("/Users/you/coding/foo/")), "foo");
    }
}
