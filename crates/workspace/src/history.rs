//! The resumable swarms listed under the empty tab's choosers (§7.2, §9.5).
//!
//! The real rows come from a background scan of `~/.evo/sessions` merged with
//! the app's own recents; until that lands, [`placeholder_history`] supplies the
//! shape the view renders.

use std::path::{Path, PathBuf};

use gpui_kit::SharedString;

/// One swarm that can be opened again: where it ran, how wide it was, when, and
/// — when the journal records them — the models it used.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HistoryRow {
    /// The folder the swarm ran in; opening the row resumes it there.
    pub folder: PathBuf,
    /// Lanes the journal records. Read-only: a resumed swarm keeps its size.
    pub lanes: usize,
    /// When it ran, already phrased for display ("2h ago", "yesterday").
    pub when: SharedString,
    /// The coordinator's model, when the journal records one.
    pub models: Option<SharedString>,
}

impl HistoryRow {
    /// The label on the row: the folder's own name, not its whole path.
    pub fn folder_name(&self) -> SharedString {
        folder_name(&self.folder)
    }

    /// The row's secondary text: `4 lanes · 2h ago · coordinator: gpt-…`.
    pub fn summary(&self) -> SharedString {
        let lanes = if self.lanes == 1 { "lane" } else { "lanes" };
        let mut summary = format!("{} {lanes} · {}", self.lanes, self.when);
        if let Some(models) = &self.models {
            summary.push_str(" · coordinator: ");
            summary.push_str(models);
        }
        summary.into()
    }
}

/// The label for a folder: its own name, not its whole path.
pub(crate) fn folder_name(folder: &Path) -> SharedString {
    folder
        .file_name()
        .map(|name| SharedString::from(name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| SharedString::from(folder.to_string_lossy().into_owned()))
}

/// Stand-in rows, so the empty tab's history region has real content to lay out
/// before the session scan exists.
pub fn placeholder_history() -> Vec<HistoryRow> {
    vec![
        HistoryRow {
            folder: PathBuf::from("/Users/bytedance/coding/evo-desktop"),
            lanes: 4,
            when: "2h ago".into(),
            models: Some("ark-deepseek-v4.1-flash".into()),
        },
        HistoryRow {
            folder: PathBuf::from("/Users/bytedance/coding/evo-gui"),
            lanes: 6,
            when: "yesterday".into(),
            models: None,
        },
        HistoryRow {
            folder: PathBuf::from("/Users/bytedance/coding/evo-agent"),
            lanes: 2,
            when: "3 days ago".into(),
            models: Some("claude-sonnet-4".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_labelled_by_its_folder_name_and_summarised_for_display() {
        let row = HistoryRow {
            folder: PathBuf::from("/Users/bytedance/coding/foo"),
            lanes: 4,
            when: "2h ago".into(),
            models: Some("gpt-5".into()),
        };

        assert_eq!(row.folder_name(), "foo");
        assert_eq!(row.summary(), "4 lanes · 2h ago · coordinator: gpt-5");
    }

    #[test]
    fn a_row_without_a_recorded_model_says_so_by_omission() {
        let row = HistoryRow {
            folder: PathBuf::from("/tmp/bar"),
            lanes: 1,
            when: "today".into(),
            models: None,
        };

        assert_eq!(row.summary(), "1 lane · today");
    }
}
