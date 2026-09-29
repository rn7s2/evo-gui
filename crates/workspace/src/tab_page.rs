//! The tab page (§7.3) and the screens around it: a swarm starting, one that
//! never came up, and one being stopped.
//!
//! The page is the layout its parts fill: the agent column on the left, the
//! selected agent's transcript and todos in the middle, the coordinator's
//! composer on the right. All three are real crates: `agent_list`, `transcript`
//! and `composer`; this module is the frame they hang in, the folder line under
//! the list, and the header that names the agent being shown.

use std::path::Path;

use agent_list::{activity_status, status_color};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, App, Context, IntoElement, SharedString, TestSupportExt as _};
use session::{lane_task_label, AgentKey, LaneStatus};
use transcript::TodoPanel;

use crate::tab::{TabContent, TabContentEvent, TabState};

/// Width of the composer column on the tab page (§7.3).
pub(crate) const COMPOSER_COLUMN_WIDTH: f32 = 360.;

/// How many characters the folder line under the agent list may take before it is
/// elided in the middle: about what fits under a 260 px column at 11 px text.
const FOLDER_LINE_CHARS: usize = 34;

/// How many characters a path may take on the boot and failure screens, where the
/// whole window is the measure.
const SCREEN_PATH_CHARS: usize = 88;

impl TabContent {
    /// The content for the tab's current state.
    pub(crate) fn render_for_state(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            TabState::Empty => self.render_empty(cx),
            TabState::Booting { folder } => self.render_booting(folder, cx),
            TabState::Running { folder } => self.render_page(folder, cx),
            TabState::Failed { folder, log_tail } => self.render_failed(folder, log_tail, cx),
            TabState::Stopping { .. } => self.render_stopping(cx),
        }
    }

    /// While the swarm starts: what is starting, and where (§3).
    fn render_booting(&self, folder: &Path, cx: &App) -> AnyElement {
        centered_region(
            "starting swarm…",
            Some(path_text(
                "boot-folder",
                folder,
                SCREEN_PATH_CHARS,
                true,
                cx,
            )),
            "the swarm's log tail appears here if it fails to come up",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
            cx,
        )
    }

    /// While §3's ladder runs on the tab's way out (§9.8).
    fn render_stopping(&self, cx: &App) -> AnyElement {
        centered_region(
            "stopping the swarm…",
            None,
            "the server is being asked to shut down, then signalled if it does not",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
            cx,
        )
    }

    /// A boot that failed: the tail of the swarm's log, and a way to try again
    /// (§9.7).
    fn render_failed(&self, folder: &Path, log_tail: &str, cx: &mut Context<Self>) -> AnyElement {
        let folder = folder.to_path_buf();
        let retry = folder.clone();
        v_flex()
            .id("boot-failure")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                div()
                    .font_semibold()
                    .child(SharedString::from("Could not start a swarm")),
            )
            .child(path_text(
                "failure-folder",
                &folder,
                SCREEN_PATH_CHARS,
                true,
                cx,
            ))
            .child(
                // The log is the evidence: its own box, monospace, and only as
                // tall as it needs to be before it scrolls (§9.7).
                div()
                    .id("boot-log-tail")
                    .test_support()
                    .w_full()
                    .max_w(px(720.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_3()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(log_tail.to_string())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("tab-retry")
                            .label("Retry")
                            .primary()
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TabContentEvent::RetryRequested(retry.clone()))
                            })),
                    )
                    .child(
                        Button::new("tab-failure-close")
                            .label("Close")
                            .ghost()
                            .on_click(cx.listener(|_this, _, _, cx| {
                                cx.emit(TabContentEvent::CloseRequested)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The tab page: agents, transcript and todos, composer (§7.3).
    fn render_page(&self, folder: &Path, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id("tab-page")
            .test_support()
            .items_stretch()
            .size_full()
            .child(self.render_agent_column(folder, cx))
            .child(self.render_center_column(cx))
            .child(self.render_composer_column(cx))
            .into_any_element()
    }

    /// The agent column: `agent_list`'s own rows, with the folder the swarm runs
    /// in pinned under them (§7.3).
    ///
    /// The list takes the room; the folder line keeps its own single row at the
    /// bottom, so a path can never push the lanes around.
    fn render_agent_column(&self, folder: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("agent-column")
            .test_support()
            .w(agent_list::COLUMN_WIDTH)
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().min_h_0().child(self.agents.clone()))
            .child(folder_line(folder, cx))
    }

    /// The slim line above the transcript: whose transcript this is, what that
    /// agent is doing, and the task it was given (§7.3).
    ///
    /// The colours are the left column's own (`agent_list`'s status table), so the
    /// header and the row agree at a glance.
    fn render_agent_header(&self, cx: &App) -> AnyElement {
        let agent = self.selected_agent();
        let Some(model) = self.model() else {
            return div().into_any_element();
        };
        let (name, status, task) = match agent {
            AgentKey::Coordinator => (
                SharedString::from("main"),
                activity_status(model.activity()),
                None,
            ),
            AgentKey::Lane(n) => {
                let lane = model.lane_rows().iter().find(|lane| lane.n as u32 == n);
                (
                    SharedString::from(format!("Lane {n}")),
                    lane.map(|lane| lane.status).unwrap_or(LaneStatus::Idle),
                    lane.and_then(|lane| lane.task.as_deref())
                        .filter(|task| !task.is_empty())
                        .map(lane_task_label),
                )
            }
        };
        h_flex()
            .id("transcript-header")
            .test_support()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_color(status_color(status, cx.theme()))
                    .child(SharedString::from(status.glyph().to_string())),
            )
            .child(div().font_medium().child(name))
            .when_some(task, |this, task| {
                // One line, elided: a task is a sentence, and the transcript below
                // is where the whole of it can be read.
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(task)),
                )
            })
            .into_any_element()
    }

    /// The selected agent's transcript, with its todos along the bottom (§7.3).
    fn render_center_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_agent();
        let view = self.transcripts.get(&selected).cloned();
        let todos = view
            .as_ref()
            .map(|view| view.read(cx).todos().to_vec())
            .unwrap_or_default();
        let empty = view
            .as_ref()
            .is_none_or(|view| view.read(cx).rows(cx).is_empty());
        v_flex()
            .id("transcript-column")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_agent_header(cx))
            .when(self.is_reconnecting(), |this| {
                this.child(reconnect_badge(cx))
            })
            .child(div().flex_1().min_h_0().child(match view {
                Some(view) if !empty => view.into_any_element(),
                _ => empty_transcript(selected, cx).into_any_element(),
            }))
            // The panel renders nothing while the agent has no todos, so this
            // row disappears rather than leaving a gap (§7.3).
            .child(TodoPanel::new(&todos))
    }

    /// The coordinator's composer: the input, and the readout and the single
    /// action button on one line beneath it (§7.3).
    fn render_composer_column(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .id("composer-column")
            .test_support()
            .w(px(COMPOSER_COLUMN_WIDTH))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .p_4()
            .child(self.composer.clone())
    }
}

/// The folder the swarm runs in: one line under the agent list, `~`-shortened
/// and elided in the middle, with the whole path on hover (§7.3).
fn folder_line(folder: &Path, cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(path_text(
            "agent-folder",
            folder,
            FOLDER_LINE_CHARS,
            true,
            cx,
        ))
}

/// A path as one line of text: elided in the middle to LIMIT characters, with the
/// whole of it on hover.
///
/// `muted` is the smaller, quieter form the boot and failure screens use.
fn path_text(id: &'static str, folder: &Path, limit: usize, muted: bool, cx: &App) -> AnyElement {
    let full = folder.display().to_string();
    let shown = shorten_path(&full, limit);
    div()
        .id(id)
        .test_support()
        .text_xs()
        .min_w_0()
        .truncate()
        .when(muted, |this| this.text_color(cx.theme().muted_foreground))
        .child(SharedString::from(shown))
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
        .into_any_element()
}

/// A path as `~` where it is the home directory: the form a person reads.
fn shorten_path(path: &str, limit: usize) -> String {
    let shortened = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    };
    elide_middle(&shortened, limit)
}

/// `head…tail`: both ends of a path are worth keeping, because the tail names the
/// folder and the head says where it is.
fn elide_middle(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit || limit < 6 {
        return text.to_string();
    }
    let head = limit / 3;
    let tail = limit - 1 - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// The center column with no rows yet: one muted line, centred (§7.3).
fn empty_transcript(agent: AgentKey, cx: &App) -> AnyElement {
    let what = match agent {
        AgentKey::Coordinator => "the coordinator has not said anything yet",
        AgentKey::Lane(_) => "this lane has not said anything yet",
    };
    v_flex()
        .id("empty-transcript")
        .test_support()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(what.to_string())),
        )
        .into_any_element()
}

/// The badge a dropped stream shows while it retries (§9.7).
fn reconnect_badge(cx: &App) -> impl IntoElement {
    h_flex()
        .id("reconnecting")
        .test_support()
        .w_full()
        .px_4()
        .py_1()
        .gap_2()
        .items_center()
        .bg(cx.theme().muted)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(IconName::Loader)
        .child("reconnecting…")
}

/// A centered message region, used while a tab has nothing to show yet.
fn centered_region(
    headline: &str,
    detail: Option<AnyElement>,
    note: &str,
    spinner: Option<Spinner>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id("booting")
        .test_support()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .when_some(spinner, |this, spinner| this.child(spinner))
        .child(
            div()
                .font_semibold()
                .child(SharedString::from(headline.to_string())),
        )
        .when_some(detail, |this, detail| this.child(detail))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(note.to_string())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path is elided in the middle: both ends survive, and the tail — the part
    /// that names the folder — gets the longer half.
    #[test]
    fn a_long_path_keeps_both_ends() {
        let path = "/var/folders/5d/bgm58_l51j52vqz4v7jrdfch0000gn/T/fixture/proj";
        let shown = elide_middle(path, 30);
        assert_eq!(shown.chars().count(), 30);
        assert!(shown.starts_with("/var/"), "{shown}");
        assert!(shown.ends_with("re/proj"), "{shown}");
        assert!(shown.contains('…'), "{shown}");

        // Short enough to fit: nothing is taken away.
        assert_eq!(elide_middle("/tmp/proj", 30), "/tmp/proj");
        assert_eq!(elide_middle("", 30), "");
    }

    /// `~` is what a person reads for their own home directory, and only there.
    #[test]
    fn the_home_directory_is_a_tilde() {
        let home = std::env::var("HOME").expect("a home directory");
        assert_eq!(
            shorten_path(&format!("{home}/work/proj"), 80),
            "~/work/proj"
        );
        assert_eq!(shorten_path("/var/tmp/proj", 80), "/var/tmp/proj");
    }
}
