//! The tab page (§7.3) and the screens around it: a swarm starting, one that
//! never came up, and one being stopped.
//!
//! The page is the layout its parts fill: the agent column on the left, the
//! selected agent's transcript and todos in the middle, the coordinator's
//! composer on the right. The transcript and the composer are the real ones from
//! their crates; the agent column is drawn here from the tab's [`TabModel`], and
//! swaps for `agent_list` when that crate lands.
//!
//! [`TabModel`]: session::TabModel

use std::path::Path;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, App, Context, IntoElement, SharedString, TestSupportExt as _};
use session::{Activity, AgentKey, LaneStatus, TabModel};
use transcript::TodoPanel;

use crate::tab::{TabContent, TabContentEvent, TabState};

/// Width of the agent column on the tab page (§7.3).
pub(crate) const AGENT_COLUMN_WIDTH: f32 = 260.;
/// Width of the composer column on the tab page (§7.3).
pub(crate) const COMPOSER_COLUMN_WIDTH: f32 = 360.;

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
            &folder.display().to_string(),
            "the swarm's log tail appears here if it fails to come up",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
            cx,
        )
    }

    /// While §3's ladder runs on the tab's way out (§9.8).
    fn render_stopping(&self, cx: &App) -> AnyElement {
        centered_region(
            "stopping the swarm…",
            "",
            "the server is being asked to shut down, then signalled if it does not",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
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
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("tab-retry").label("Retry").outline().on_click(
                                cx.listener(move |_this, _, _, cx| {
                                    cx.emit(TabContentEvent::RetryRequested(folder.clone()))
                                }),
                            ))
                            .child(
                                Button::new("tab-failure-close")
                                    .label("Close")
                                    .ghost()
                                    .on_click(cx.listener(|_this, _, _, cx| {
                                        cx.emit(TabContentEvent::CloseRequested)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("boot-log-tail")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
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
            .child(self.render_center_column(cx))
            .child(self.render_composer_column(cx))
            .into_any_element()
    }

    /// The agent list: `main` first, then one row per lane (§7.3).
    fn render_agent_column(&self, folder: &Path, cx: &mut Context<Self>) -> impl IntoElement {
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
                self.agent_rows(cx)
                    .into_iter()
                    .map(|row| self.render_agent_row(row, cx)),
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

    /// The rows of the agent column, from the tab's own model (§7.3).
    fn agent_rows(&self, cx: &App) -> Vec<AgentRow> {
        let Some(model) = self.model() else {
            return Vec::new();
        };
        let selected = model.selected();
        let mut rows = vec![AgentRow {
            key: AgentKey::Coordinator,
            glyph: coordinator_status(model.activity()).glyph(),
            label: SharedString::from("main"),
            detail: self.coordinator_detail(),
            tooltip: SharedString::from(format!(
                "main — the coordinator\n{}",
                model.readout_text()
            )),
            selected: selected == AgentKey::Coordinator,
        }];
        rows.extend(model.lane_rows().iter().map(|lane| {
            let key = AgentKey::Lane(lane.n as u32);
            AgentRow {
                key,
                glyph: lane.glyph(),
                label: SharedString::from(format!("Lane {}", lane.n)),
                detail: lane_detail(model, lane.n as u32),
                tooltip: lane_tooltip(model, lane.n as u32, &lane.state),
                selected: selected == key,
            }
        }));
        let _ = cx;
        rows
    }

    fn render_agent_row(&self, row: AgentRow, cx: &mut Context<Self>) -> impl IntoElement {
        let key = row.key;
        h_flex()
            .id(gpui_kit::ElementId::NamedInteger(
                "agent-row".into(),
                u64::from(key.lane().unwrap_or(u64::MAX as u32)) % u64::MAX,
            ))
            .test_support()
            .mx_2()
            .px_2()
            .py_1()
            .gap_2()
            .rounded(cx.theme().radius)
            .when(row.selected, |row| row.bg(cx.theme().secondary))
            .on_click(cx.listener(move |this, _, _window, cx| this.select_agent(key, cx)))
            .tooltip({
                let tooltip = row.tooltip.clone();
                move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                }
            })
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(row.glyph.to_string())),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(row.label))
                    .when_some(row.detail, |this, detail| {
                        this.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(detail),
                        )
                    }),
            )
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

/// One row of the agent column (§7.3).
struct AgentRow {
    key: AgentKey,
    glyph: char,
    label: SharedString,
    /// The task and the step clock, while the agent is working.
    detail: Option<SharedString>,
    tooltip: SharedString,
    selected: bool,
}

impl TabContent {
    /// The coordinator's line: what it is doing, and the step clock that
    /// `turn-start`/`run-start` began (§7.3, §5).
    fn coordinator_detail(&self) -> Option<SharedString> {
        let model = self.model()?;
        let word = match model.activity() {
            Activity::Idle => return None,
            Activity::Running => "working",
            Activity::Compacting => "compacting",
        };
        let clock = self
            .step_seconds()
            .map(|seconds| format!(" · {}", session::short_duration(seconds)))
            .unwrap_or_default();
        Some(SharedString::from(format!("{word}{clock}")))
    }
}

/// The coordinator's glyph: its activity, in the lane vocabulary (§7.3).
fn coordinator_status(activity: Activity) -> LaneStatus {
    match activity {
        Activity::Idle => LaneStatus::Idle,
        Activity::Running => LaneStatus::Working,
        Activity::Compacting => LaneStatus::Compacting,
    }
}

/// A working lane's line: what it was given, and how long its current step has
/// been running (§7.3).
fn lane_detail(model: &TabModel, n: u32) -> Option<SharedString> {
    let lane = model.lane_rows().iter().find(|lane| lane.n as u32 == n)?;
    let task = lane.task.as_deref().filter(|task| !task.is_empty())?;
    let mut detail = session::lane_task_label(task);
    if let Some(step) = lane.step_age {
        detail.push_str(" · ");
        detail.push_str(&session::short_duration(step));
    }
    Some(SharedString::from(detail))
}

/// A lane's tooltip: its state, what it is doing, and why it is down when it is
/// (§7.3, §9.7).
fn lane_tooltip(model: &TabModel, n: u32, state: &str) -> SharedString {
    let mut tooltip = format!("Lane {n} — {state}");
    if let Some(lane) = model.lane_rows().iter().find(|lane| lane.n as u32 == n) {
        if let Some(task) = lane.task.as_deref().filter(|task| !task.is_empty()) {
            tooltip.push('\n');
            tooltip.push_str(&session::lane_task_label(task));
        }
        if lane.restarts > 0 {
            tooltip.push_str(&format!("\nrestarts: {}", lane.restarts));
        }
    }
    if let Some(reason) = model.lane_down_reason(n) {
        tooltip.push('\n');
        tooltip.push_str(&reason);
    }
    SharedString::from(tooltip)
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
    detail: &str,
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
        .when(!detail.is_empty(), |this| {
            this.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(detail.to_string())),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(note.to_string())),
        )
        .into_any_element()
}
