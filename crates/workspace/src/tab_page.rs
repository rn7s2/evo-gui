//! The tab page (§7.3) and the two screens around it: a swarm starting, and one
//! that never came up.
//!
//! The page is the layout its parts fill: the agent column on the left, the
//! selected agent's transcript and todos in the middle, the coordinator's
//! composer on the right. The transcript and the composer are the real ones from
//! their crates; the agent column is still a placeholder, because the lane list
//! it draws is not wired to `GET /lanes` yet (§7.3).

use std::path::Path;

use gpui_kit::component::button::Button;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, App, Context, IntoElement, SharedString, TestSupportExt as _};
use transcript::TodoPanel;

use crate::tab::{TabContent, TabContentEvent, TabState};

/// Width of the agent column on the tab page (§7.3).
pub(crate) const AGENT_COLUMN_WIDTH: f32 = 260.;
/// Width of the composer column on the tab page (§7.3).
pub(crate) const COMPOSER_COLUMN_WIDTH: f32 = 360.;

/// The agents the lane column will list, as placeholders: the coordinator first,
/// then one row per lane with its status glyph (§7.3).
const PLACEHOLDER_AGENTS: [(&str, &str); 4] = [
    ("●", "main"),
    ("◐", "Lane 1"),
    ("○", "Lane 2"),
    ("✗", "Lane 3"),
];

impl TabContent {
    /// The content for the tab's current state.
    pub(crate) fn render_for_state(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            TabState::Empty => self.render_empty(cx),
            TabState::Booting { folder } => self.render_booting(folder, cx),
            TabState::Running { folder } => self.render_page(folder, cx),
            TabState::Failed { folder, log_tail } => self.render_failed(folder, log_tail, cx),
        }
    }

    /// While the swarm starts: what is starting, and where (§3).
    fn render_booting(&self, folder: &Path, cx: &App) -> AnyElement {
        centered_region(
            "Starting evo-swarm…",
            &folder.display().to_string(),
            "the swarm's log tail appears here if it fails to come up",
            cx,
        )
    }

    /// A boot that failed: the tail of the swarm's log, and a way to try again
    /// (§9.7).
    fn render_failed(&self, folder: &Path, log_tail: &str, cx: &mut Context<Self>) -> AnyElement {
        let folder = folder.to_path_buf();
        v_flex()
            .id("boot-failure")
            .test_support()
            .size_full()
            .p_6()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .font_semibold()
                            .child(format!("Could not start a swarm in {}", folder.display())),
                    )
                    .child(Button::new("tab-retry").label("Retry").outline().on_click(
                        cx.listener(move |_this, _, _, cx| {
                            cx.emit(TabContentEvent::RetryRequested(folder.clone()))
                        }),
                    )),
            )
            .child(
                div()
                    .id("boot-log-tail")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_3()
                    .text_xs()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(log_tail.to_string())),
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
            .child(self.render_transcript_column(cx))
            .child(self.render_composer_column(cx))
            .into_any_element()
    }

    /// The agent list: `main` first, then one row per lane (§7.3).
    fn render_agent_column(&self, folder: &Path, cx: &App) -> impl IntoElement {
        v_flex()
            .id("agent-column")
            .test_support()
            .w(px(AGENT_COLUMN_WIDTH))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().border)
            .py_2()
            .gap_0p5()
            .children(
                PLACEHOLDER_AGENTS
                    .iter()
                    .enumerate()
                    .map(|(ix, (glyph, name))| self.render_agent_row(ix == 0, glyph, name, cx)),
            )
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(folder.display().to_string()),
            )
    }

    fn render_agent_row(
        &self,
        selected: bool,
        glyph: &'static str,
        name: &'static str,
        cx: &App,
    ) -> impl IntoElement {
        h_flex()
            .mx_2()
            .px_2()
            .py_1()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(selected, |row| row.bg(cx.theme().secondary))
            .child(div().text_color(cx.theme().muted_foreground).child(glyph))
            .child(div().truncate().child(name))
    }

    /// The selected agent's transcript, with its todos along the bottom (§7.3).
    fn render_transcript_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let todos = self.transcript.read(cx).todos().to_vec();
        v_flex()
            .id("transcript-column")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
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

/// A centered message region, used while a tab has nothing to show yet.
fn centered_region(headline: &str, detail: &str, note: &str, cx: &App) -> AnyElement {
    v_flex()
        .id("booting")
        .test_support()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .font_semibold()
                .child(SharedString::from(headline.to_string())),
        )
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(detail.to_string())),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(note.to_string())),
        )
        .into_any_element()
}
