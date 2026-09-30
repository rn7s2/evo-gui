//! agent_list — the tab page's left column (§7.3): `main` first, then one row per lane.
//!
//! Every row is the same shape, so the column can be read down: the state glyph, the name,
//! the task the agent was given, and — right-aligned, where the eye goes to compare rows —
//! the state, with the step clock beside it while the agent is working.
//!
//! The list is a view and nothing else: it holds the coordinator's [`LaneList`] plus the
//! few things only the UI knows (the coordinator's activity, whether its stream is
//! reconnecting, the lanes' down reasons and the selection). It never talks to the server —
//! the owner pushes updates in with [`AgentList::set_lanes`] / [`AgentList::set_coordinator`]
//! and gets an [`AgentListEvent::Select`] back when a row is clicked.
//!
//! ```text
//! AgentList::new(cx)                                  the entity
//! set_lanes(&LaneList, cx)                            topic `swarm`, from TabModel
//! set_coordinator(status, reconnecting, cx)           topic `session` + the stream badge
//! set_coordinator_clock(Option<String>, cx)           the coordinator's absolute step start
//! set_selected(AgentKey, cx)                          TabModel::selected
//! set_down_reason(lane, Option<String>, cx)           TabModel::lane_down_reason
//! → AgentListEvent::Select(AgentKey)                  the owner calls TabModel::select
//! → AgentListEvent::StopLane(u32)                     the owner sends run.interrupt{scope:lane}
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_kit::component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, Context, ElementId, EventEmitter, FocusHandle, FontFeatures, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use session::{AgentKey, LaneList, LaneRow, LaneStatus, Status};
use store::design::{self, Palette, HEADER_HEIGHT, INSET, RADIUS};
use widgets::{paint, BreathingDot};

/// The column's width in the tab page (§7.3). The owner sizes the column; this is the
/// number the design was drawn against, and what the demo uses.
pub const COLUMN_WIDTH: Pixels = px(260.);

/// One row of the design's lane list: 32px, 9px between its cells, 10px of its own
/// inset, and one line of 13px text (`.ws-lane`).
const LANE_ROW: f32 = 32.;
const LANE_GAP: f32 = 9.;
const LANE_PAD: f32 = 10.;
const LANE_FONT: Pixels = px(13.);
/// The list's own inset and the gap between rows (`.ws-lane-list{padding:6px}`).
const LIST_PAD: f32 = 6.;
/// The band's two sizes: `.ws-head-title{font-size:13px}` and
/// `.ws-head-meta{font-size:12px}`.
const TITLE_SIZE: Pixels = px(13.);
const META_SIZE: Pixels = px(12.);
/// A row's state: `.ws-lane-state{font-size:12px}`.
const STATE_SIZE: Pixels = px(12.);
/// The Stop control and the `reconnecting` badge, which the design has no size for:
/// the chrome's own small print.
const STOP_SIZE: Pixels = px(11.);

/// The row's fill on hover and while selected: the ink a few percent into the sidebar
/// (`--row-surface: color-mix(in srgb, var(--fg) 5%|9%, var(--sidebar))`). The theme
/// carries the same two mixes; the dot needs them as tokens of its own, so they are
/// named here too.
const HOVER_MIX: f32 = 5.;
const SELECTED_MIX: f32 = 9.;
/// An idle dot's ring in a workspace row: `.dot-idle{opacity:.8}` — the tab strip's
/// own ring is `IDLE_OPACITY`, and the two are not the same weight.
const WORKSPACE_IDLE: f32 = 0.8;

/// How wide a tooltip line is allowed to get, in characters: about one and a half columns,
/// which fits a worktree path beside its label and still keeps a long task or a long
/// failure reason inside the window.
const TOOLTIP_LINE: usize = 56;

/// The rows scroller, so a swarm with more lanes than fit can be scrolled.
const ROWS_ID: &str = "agent-list-rows";

/// The `6 lanes · 2 busy` summary above the rows.
const SUMMARY_ID: &str = "agent-list-summary";

/// What the column is called, for a screen reader: the rows name themselves, but the list
/// around them needs a name of its own.
const LIST_LABEL: &str = "Agents";

/// What the list asks its owner to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentListEvent {
    /// Show this agent's transcript in the center column: the owner calls
    /// `TabModel::select` and feeds the result back with [`AgentList::set_selected`].
    Select(AgentKey),
    /// Stop one lane (`run.interrupt` with scope `lane`): the one human action on a lane
    /// besides typing to the coordinator (CONTRACT §7.4).
    StopLane(u32),
}

/// The tab page's agent list.
pub struct AgentList {
    lanes: LaneList,
    activity: Status,
    /// The coordinator's step clock, already formatted by the owner
    /// (`session::StepClock::clock_label`). Shown in the trailing cell while the
    /// coordinator runs or compacts, where an idle row shows its activity word.
    coordinator_clock: Option<String>,
    /// The coordinator's stream is down and retrying (§9.7): the `reconnecting` badge.
    coordinator_reconnecting: bool,
    selected: AgentKey,
    /// Why a lane is down, by lane number — the last error line of its transcript,
    /// which the owner reads from `TabModel::lane_down_reason` (§9.7).
    down_reasons: BTreeMap<u32, String>,
    /// What **now** is, as the owner stamps it (`now_millis`). A lane's step clock
    /// counts on from the moment its age was read (`LaneRow::step_clock_at`), and
    /// this is the moment it counts to: the list reads no clock of its own, so an
    /// owner that never sets one shows exactly the ages the swarm reported.
    now_millis: u64,
    /// The column's own focus (§7.3): a click takes it, and the arrows walk the rows while it
    /// is held. The list is the thing being navigated, so it is the thing that is focused.
    focus_handle: FocusHandle,
}

impl EventEmitter<AgentListEvent> for AgentList {}

impl AgentList {
    /// The context is the entity-constructor shape; there is nothing to subscribe to,
    /// because every update arrives through the setters below.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            lanes: LaneList::new(),
            activity: Status::Idle,
            coordinator_clock: None,
            coordinator_reconnecting: false,
            selected: AgentKey::Coordinator,
            down_reasons: BTreeMap::new(),
            now_millis: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    /// The moment lane clocks are counted to, stamped by the owner with its own
    /// wall clock (`now_millis`) — once per update, and again every second while
    /// anything is working.
    ///
    /// `GET /lanes` reports a step *age*, not a start, so without this a busy lane's
    /// clock would sit where the last read left it (§7.3). The owner is the one who
    /// knows the time, and it is also the one who decides how often a frame is worth
    /// paying for, so the clock moves when it says so.
    pub fn set_now(&mut self, now_millis: u64, cx: &mut Context<Self>) {
        if self.now_millis != now_millis {
            self.now_millis = now_millis;
            cx.notify();
        }
    }

    /// The lanes to draw, from `GET /lanes` plus every `lane-state` event. Replacing the
    /// list never changes the selection: the selected agent stays selected while its row
    /// updates, which is the whole point of the stable row ids.
    pub fn set_lanes(&mut self, lanes: &LaneList, cx: &mut Context<Self>) {
        if self.lanes != *lanes {
            self.lanes = lanes.clone();
            cx.notify();
        }
    }

    /// The coordinator's own row: its status, and whether its stream is reconnecting.
    pub fn set_coordinator(
        &mut self,
        activity: Status,
        reconnecting: bool,
        cx: &mut Context<Self>,
    ) {
        if self.activity != activity || self.coordinator_reconnecting != reconnecting {
            self.activity = activity;
            self.coordinator_reconnecting = reconnecting;
            cx.notify();
        }
    }

    /// The coordinator's step clock, formatted the way the owner wants it shown
    /// (`session::StepClock::clock_label(now_millis)`, which counts in the same words a
    /// lane's `step_age` does). It takes the trailing cell while the coordinator is running
    /// or compacting — the activity word is what shows the rest of the time, and while the
    /// clock is unknown — so an idle row never claims a step.
    ///
    /// `None` (or a blank string) clears it: the run ended, or its start was never stamped.
    pub fn set_coordinator_clock(&mut self, clock: Option<String>, cx: &mut Context<Self>) {
        let clock = clock.filter(|clock| !clock.trim().is_empty());
        if self.coordinator_clock != clock {
            self.coordinator_clock = clock;
            cx.notify();
        }
    }

    /// Highlight AGENT's row. The list does not select on its own: a click emits
    /// [`AgentListEvent::Select`] and this is how the owner confirms it.
    pub fn set_selected(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.selected != agent {
            self.selected = agent;
            cx.notify();
        }
    }

    /// Why lane N is down, shown in its row in place of a task; `None` clears it
    /// (the lane came back, or the reason is stale).
    pub fn set_down_reason(&mut self, lane: u32, reason: Option<String>, cx: &mut Context<Self>) {
        let reason = reason.filter(|reason| !reason.trim().is_empty());
        match reason {
            Some(reason) => {
                if self.down_reasons.get(&lane) != Some(&reason) {
                    self.down_reasons.insert(lane, reason);
                    cx.notify();
                }
            }
            None => {
                if self.down_reasons.remove(&lane).is_some() {
                    cx.notify();
                }
            }
        }
    }

    /// The agent the list is highlighting.
    pub fn selected(&self) -> AgentKey {
        self.selected
    }

    /// The lanes as the list currently draws them.
    pub fn lanes(&self) -> &LaneList {
        &self.lanes
    }

    fn is_selected(&self, key: AgentKey) -> bool {
        self.selected == key
    }

    /// The agents in the order the rows are drawn: the coordinator first, then the lanes.
    /// The arrows move through this, so the order on screen is the order they walk.
    fn keys(&self) -> Vec<AgentKey> {
        std::iter::once(AgentKey::Coordinator)
            .chain(self.lanes.lanes.iter().map(|row| AgentKey::Lane(row.n)))
            .collect()
    }

    /// The list's own keys (§7.3): the arrows walk the rows, Home and End go to the ends.
    /// The list asks and does not select — every move emits [`AgentListEvent::Select`] — so
    /// the keyboard takes the same path a click does and the owner stays the only one that
    /// knows what a selection means.
    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.keys();
        // A list with nothing highlighted starts from the top; there is always a coordinator
        // row, so `keys` is never empty and the last index is a real one.
        let at = keys
            .iter()
            .position(|key| *key == self.selected)
            .unwrap_or_default();
        let next = match event.keystroke.key.as_str() {
            "down" => at + 1,
            "up" => at.saturating_sub(1),
            "home" => 0,
            "end" => keys.len() - 1,
            _ => return,
        }
        .min(keys.len() - 1);
        // The arrows belong to the focused list even at its ends: a list that has nothing
        // further to show should not also scroll whatever is behind it.
        cx.stop_propagation();
        if keys[next] != self.selected {
            cx.emit(AgentListEvent::Select(keys[next]));
        }
    }
    /// The band the column opens with: what it holds, and how much of it is working
    /// (`.ws-head`, `.ws-head-title`, `.ws-head-meta`).
    ///
    /// It is the same band, at the same height and on the same surface, as the one over
    /// the conversation, so the rule under the two runs straight across the page.
    fn header(&self, palette: &'static Palette) -> impl IntoElement {
        let lanes = self.lanes.lanes.len();
        let meta = format!("{} of {} busy", self.lanes.busy(), lanes);
        h_flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .w_full()
            .items_center()
            .gap(px(8.))
            .px(px(INSET))
            .bg(paint::color(palette.sidebar))
            .border_b_1()
            .border_color(paint::color(palette.border))
            .child(
                div()
                    .font_semibold()
                    .text_size(TITLE_SIZE)
                    .text_color(paint::color(palette.fg))
                    .child("Lanes"),
            )
            .child(
                div()
                    .id(SUMMARY_ID)
                    .test_support()
                    .ml_auto()
                    .text_size(META_SIZE)
                    .text_color(paint::color(palette.muted_fg))
                    .aria_label(meta.clone())
                    .child(meta),
            )
    }

    /// The coordinator's row: `main`, its task cell says what it is (the swarm's
    /// coordinator), and the state cell says what it is doing.
    fn coordinator_view(&self, palette: &'static Palette) -> RowView {
        let status = activity_status(self.activity);
        let word = activity_word(self.activity);
        // The clock sits in the state cell while the coordinator is actually working,
        // as a lane's does: an idle `main` says `idle`, not the seconds since a run
        // that already ended.
        let busy = matches!(self.activity, Status::Running | Status::Compacting);
        let clock = busy.then(|| self.coordinator_clock.clone()).flatten();
        let badge = self
            .coordinator_reconnecting
            .then(|| SharedString::from("reconnecting"));
        let mut tooltip = format!("main — the coordinator · {word}");
        if let Some(clock) = &clock {
            tooltip.push_str(&format!(" · step {clock}"));
        }
        if self.coordinator_reconnecting {
            tooltip.push_str("\nstream reconnecting");
        }
        let step = clock
            .as_ref()
            .map(|clock| format!(", step {clock}"))
            .unwrap_or_default();
        let state: SharedString = clock.unwrap_or_else(|| word.to_string()).into();
        RowView {
            key: AgentKey::Coordinator,
            name: "main".into(),
            busy: status.is_busy(),
            task: Some("coordinator".into()),
            task_color: paint::color(palette.muted_fg),
            state,
            state_color: paint::color(palette.muted_fg),
            badge,
            stop: None,
            tooltip: tooltip.into(),
            aria: format!("main, {word}{step}").into(),
        }
    }

    /// One lane's row.
    fn lane_view(&self, row: &LaneRow, palette: &'static Palette) -> RowView {
        let key = AgentKey::Lane(row.n);
        let reason = (row.status == LaneStatus::Down)
            .then(|| self.down_reasons.get(&row.n))
            .flatten();
        // The row's middle cell is the task the lane was given, and only while it is
        // working: an idle lane's last task is not what it is doing now, and the state
        // cell already says `idle`. A lane that is down says why instead, which is the
        // one thing that matters about it at that moment.
        let (task, task_color) = match reason {
            Some(reason) => (Some(reason.clone()), paint::color(palette.destructive)),
            // A lane that died saying nothing still says what it had been doing.
            None if row.is_busy() || row.status == LaneStatus::Down => {
                (row.task_label(), paint::color(palette.muted_fg))
            }
            None => (None, paint::color(palette.muted_fg)),
        };
        // The clock is what tells a slow step from a wedged lane, so it is the state
        // cell while the lane works — read at the owner's `now` (`set_now`), which is
        // what keeps it moving between `/lanes` reads.
        let clock = row.step_clock(self.now_millis);
        let word = row.status.word();
        let state: SharedString = clock
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| word.into());
        let state_color = if row.status == LaneStatus::Down {
            paint::color(palette.destructive)
        } else {
            paint::color(palette.muted_fg)
        };
        let aria = format!(
            "lane {}, {word}{}{}",
            row.n,
            match &task {
                Some(task) => format!(", {task}"),
                None => String::new(),
            },
            match &clock {
                Some(clock) => format!(", step {clock}"),
                None => String::new(),
            }
        );
        RowView {
            key,
            name: format!("lane {}", row.n).into(),
            busy: row.is_busy(),
            task: task.map(SharedString::from),
            task_color,
            state,
            state_color,
            badge: None,
            stop: row.is_busy().then_some(key),
            tooltip: lane_tooltip(row, reason, self.now_millis).into(),
            aria: aria.into(),
        }
    }

    /// The Stop button of one lane's row: `run.interrupt` with scope `lane`, sent by the
    /// owner when it hears [`AgentListEvent::StopLane`].
    ///
    /// The design has no room for it on the row, so it is the row's own control while
    /// the pointer is on that row: a lane that is working is the one a reader wants to
    /// stop, and it is the only thing this column can ask the swarm to do.
    fn stop_button(
        &self,
        lane: u32,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(stop_id(lane))
            .test_support()
            .flex_none()
            .invisible()
            .px(px(4.))
            .py(px(1.))
            .rounded(px(RADIUS))
            .border_1()
            .border_color(paint::color(palette.border))
            .text_size(STOP_SIZE)
            .text_color(paint::color(palette.muted_fg))
            // The design keeps the arrow over its chrome; only the slider asks for a
            // hand (`cursor:default` throughout `Workspace.css`).
            .cursor_default()
            .group_hover("lane", |stop| stop.visible())
            .hover(move |stop| stop.text_color(paint::color(palette.destructive)))
            .aria_label(format!("Stop lane {lane}"))
            .on_click(cx.listener(move |_, _, _, cx| {
                // A click on the button is not a click on the row: the row must not also
                // select the lane the reader is stopping.
                cx.stop_propagation();
                cx.emit(AgentListEvent::StopLane(lane));
            }))
            .child("Stop")
    }

    /// Draw one row: dot, name, the task it was given, and the state at the end.
    fn row(
        &self,
        view: RowView,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let selected = self.is_selected(view.key);
        let rows = self.keys().len();
        let key = view.key;
        let (task_id, state_id, badge_id) = (task_id(key), state_id(key), badge_id(key));
        let tooltip = view.tooltip;
        let task = view.task;
        let state = view.state;
        let badge = view.badge;
        let stop = view.stop;
        // The row's own fill, and the one the dot breathes against: on hover and while
        // selected the dot has a new surface, as the design's `--row-surface` does.
        let hover = paint::mix(palette.fg, HOVER_MIX, palette.sidebar);
        let active = paint::mix(palette.fg, SELECTED_MIX, palette.sidebar);
        let surface = if selected { active } else { palette.sidebar };
        let name = view.name;

        h_flex()
            .id(row_id(key))
            .test_support()
            .group("lane")
            .h(px(LANE_ROW))
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(LANE_GAP))
            .px(px(LANE_PAD))
            .rounded(px(RADIUS))
            .bg(paint::color(surface))
            .when(!selected, |row| {
                row.hover(move |row| row.bg(paint::color(hover)))
            })
            .text_size(LANE_FONT)
            .text_color(paint::color(palette.fg))
            .child({
                let busy = view.busy;
                let slot = format!("agent-{key:?}");
                BreathingDot::new(widgets::dot::dot_id(slot), busy)
                    .palette(palette)
                    .surface(surface)
                    .idle_opacity(WORKSPACE_IDLE)
                    .render()
            })
            .child(
                div()
                    .flex_none()
                    .when(selected, |name| name.font_medium())
                    .child(name),
            )
            .child(
                // The flexible cell: the task, or nothing to say.
                div()
                    .id(task_id)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(view.task_color)
                    .children(task),
            )
            .child(
                // Right-aligned, where the eye compares rows: the state, or the clock
                // while it is working.
                div()
                    .id(state_id)
                    .test_support()
                    .flex_none()
                    .text_size(STATE_SIZE)
                    .text_color(view.state_color)
                    .font_features(tabular())
                    .aria_label(state.clone())
                    .child(state),
            )
            .when_some(badge, |row, badge| {
                row.child(
                    div()
                        .id(badge_id)
                        .test_support()
                        .flex_none()
                        .px(px(4.))
                        .rounded(px(RADIUS))
                        .bg(paint::color(palette.warning).opacity(0.18))
                        .text_color(paint::color(palette.warning))
                        .text_size(STOP_SIZE)
                        .child(badge),
                )
            })
            // The one thing a person may do to a lane (CONTRACT §7.4): stop it, while
            // the pointer is on the row that can be stopped.
            .when_some(stop, |row, stop| {
                let lane = stop.lane().unwrap_or_default();
                row.child(self.stop_button(lane, palette, cx))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                // A click is also how the list takes the keyboard: the arrows walk the
                // rows from whatever the pointer picked (§7.3).
                this.focus_handle.focus(window, cx);
                cx.emit(AgentListEvent::Select(key));
            }))
            // One option of a list box, in the order the arrows walk it: a screen reader
            // can then say where the selection is, which is what the fill says on screen.
            .role(Role::ListBoxOption)
            .aria_position_in_set(row_index(key) as usize + 1)
            .aria_size_of_set(rows)
            .aria_label(view.aria)
            .aria_selected(selected)
            .tooltip(move |window, cx| {
                // A long task would otherwise make a tooltip wider than the window.
                Tooltip::new(tooltip.clone())
                    .max_w(px(360.))
                    .build(window, cx)
            })
    }
}

impl Render for AgentList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = design::palette(cx.theme().mode.is_dark());
        let coordinator = self.coordinator_view(palette);
        let lanes: Vec<RowView> = self
            .lanes
            .lanes
            .clone()
            .iter()
            .map(|row| self.lane_view(row, palette))
            .collect();

        let mut rows = v_flex()
            .id(ROWS_ID)
            .test_support()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap(px(2.))
            .p(px(LIST_PAD))
            .overflow_y_scroll()
            .role(Role::ListBox)
            .aria_label(LIST_LABEL);
        rows = rows.child(self.row(coordinator, palette, cx));
        for lane in lanes {
            rows = rows.child(self.row(lane, palette, cx));
        }

        v_flex()
            .id("agent-column-list")
            .test_support()
            .w_full()
            .h_full()
            .bg(paint::color(palette.sidebar))
            // The column is the tab stop (§7.3): the frame around it is focused rather
            // than any row, so the arrows can walk the rows without a row of its own
            // having to be a control. The border is always laid out and only coloured in
            // when focused, so taking focus does not move the rows by a pixel.
            .track_focus(&self.focus_handle)
            .tab_stop(true)
            .on_key_down(cx.listener(Self::key_down))
            // The column is a tab stop, so it says when the keyboard has it — and only
            // then: a click that selects a lane is not a reason to outline the whole
            // column, which is what `:focus-visible` means and what the design draws.
            .border_1()
            .border_color(gpui_kit::Hsla::default())
            .focus_visible(move |style| style.border_color(paint::color(palette.primary)))
            .text_color(paint::color(palette.fg))
            .child(self.header(palette))
            .child(rows)
    }
}

/// The element id of a row, so the owner (and the tests) can address one. Lane rows are
/// keyed by lane number, so a row never loses its identity when the list updates.
pub fn row_id(key: AgentKey) -> ElementId {
    ("agent-row", row_index(key)).into()
}

/// The id of a lane's Stop control.
fn stop_id(lane: u32) -> ElementId {
    ("agent-stop", u64::from(lane)).into()
}

/// The id of a row's flexible middle, which is the part that ellipsizes.
fn task_id(key: AgentKey) -> ElementId {
    ("agent-task", row_index(key)).into()
}

/// The id of a row's right-aligned state word.
fn state_id(key: AgentKey) -> ElementId {
    ("agent-state", row_index(key)).into()
}

/// The id of the `reconnecting` badge, which only the coordinator's row can have.
fn badge_id(key: AgentKey) -> ElementId {
    ("agent-badge", row_index(key)).into()
}

fn row_index(key: AgentKey) -> u64 {
    match key {
        // `main` is row 0: the coordinator is always first (§7.3).
        AgentKey::Coordinator => 0,
        AgentKey::Lane(n) => u64::from(n),
    }
}

/// The coordinator's status in the lane vocabulary, so one glyph and one color table
/// covers both kinds of row. `waiting` — settled but held while the lanes work — is the
/// idle glyph: the coordinator itself is not doing anything.
pub fn activity_status(activity: Status) -> LaneStatus {
    match activity {
        Status::Idle | Status::Waiting => LaneStatus::Idle,
        Status::Running => LaneStatus::Working,
        Status::Compacting => LaneStatus::Compacting,
    }
}

/// The status as the coordinator's row spells it.
pub fn activity_word(activity: Status) -> &'static str {
    activity.label()
}

/// Tabular figures, so a step clock ticking from `9s` to `10s` does not shuffle the row.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_string(), 1)]))
}

/// One row's content, worked out from the model before any element is built: the shape of
/// a row is the same whatever it is showing.
struct RowView {
    key: AgentKey,
    /// `main`, or `lane 3`: what the row is called, beside its dot.
    name: SharedString,
    /// Whether this agent is working — what the dot breathes for.
    busy: bool,
    /// The flexible one line: the task it was given while it works, `coordinator` for
    /// `main`, or why a lane is down. `None` is an empty cell, not a missing state.
    task: Option<SharedString>,
    task_color: Hsla,
    /// The right-aligned state: the step clock while it works, else the word.
    state: SharedString,
    state_color: Hsla,
    /// `reconnecting`, shown beside the state while the coordinator's stream is down.
    badge: Option<SharedString>,
    /// The Stop button's lane, while the row can be stopped.
    stop: Option<AgentKey>,
    tooltip: SharedString,
    aria: SharedString,
}

/// Everything the row has no room for: the full state, the untruncated task, the worktree,
/// the restarts and the pid — the swarm's own `lane-status-line` fields, in its order.
///
/// One field group per line, each folded at [`TOOLTIP_LINE`]: a tooltip that grew to the
/// width of a long task would be wider than the window it is drawn in.
fn lane_tooltip(row: &LaneRow, reason: Option<&String>, now_millis: u64) -> String {
    // The row's own line is the swarm's: state, step, task, worktree, reports, restarts,
    // pid, goal, context. The one thing it cannot carry is why a down lane is down, and
    // that is the line this adds.
    let mut lines = vec![row.tooltip(now_millis)];
    if let Some(reason) = reason {
        lines.push(reason.clone());
    }
    lines
        .iter()
        .map(|line| fold(line, TOOLTIP_LINE))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fold a line at `width` characters, breaking between words — enough to keep a tooltip a
/// tooltip, without measuring text.
fn fold(text: &str, width: usize) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut column = 0;
    for word in text.split(' ') {
        let length = word.chars().count();
        if column > 0 {
            if column + 1 + length <= width {
                folded.push(' ');
                column += 1;
            } else {
                folded.push('\n');
                column = 0;
            }
        }
        // A word longer than the whole line is split too, so no line can run past `width`
        // — a worktree path is exactly that case.
        for (index, character) in word.chars().enumerate() {
            if index > 0 && column == width {
                folded.push('\n');
                column = 0;
            }
            folded.push(character);
            column += 1;
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Size, Subscription,
        TestAppContext, Window, WindowBounds, WindowOptions,
    };
    use session::SwarmInfo;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The column the tab page gives the list (§7.3).
    const COLUMN: Size<Pixels> = Size {
        width: COLUMN_WIDTH,
        height: px(400.),
    };

    /// The moment the tests count a lane's clock to.
    const NOW: u64 = 1_759_200_000_000;

    /// A lane as the swarm topic reports one, with only the fields a test cares about set.
    fn lane(n: u32, status: LaneStatus, task: Option<&str>) -> LaneRow {
        LaneRow {
            n,
            status,
            state: match status {
                LaneStatus::Working => "working",
                LaneStatus::Compacting => "compacting",
                LaneStatus::Idle => "idle",
                LaneStatus::Starting => "starting",
                LaneStatus::Stopped => "stopped",
                LaneStatus::Down => "down",
            }
            .to_string(),
            task: task.map(str::to_string),
            task_started_at: None,
            // A working lane counts from an absolute start: 41 n seconds before NOW.
            step_started_at: status
                .is_busy()
                .then(|| NOW.saturating_sub(u64::from(n) * 41 * 1000)),
            restarts: 0,
            pid: Some(4000 + u64::from(n)),
            worktree: None,
            branch: None,
            model: None,
            goal_status: None,
            context_tokens: None,
            context_window: None,
            reports: 0,
            last_item: None,
        }
    }

    fn lanes(rows: Vec<LaneRow>) -> LaneList {
        let busy = rows.iter().filter(|row| row.is_busy()).count() as u64;
        let workers = rows.len() as u64;
        LaneList {
            swarm: Some(SwarmInfo {
                id: "sw-test".to_string(),
                workers,
                busy,
                waiting_on_lanes: busy > 0,
                lane_model: None,
                lane_thinking: None,
            }),
            lanes: rows,
        }
    }

    struct Fixture {
        window: AnyWindowHandle,
        list: Entity<AgentList>,
        events: Rc<RefCell<Vec<AgentListEvent>>>,
        /// Kept alive: dropping it would stop the recording.
        _subscription: Subscription,
    }

    impl Fixture {
        fn events(&self) -> Vec<AgentListEvent> {
            self.events.borrow().clone()
        }

        /// Run `f` against the list's window. Events reach subscribers when the app update
        /// returns, so assertions on [`Fixture::events`] belong after this call.
        fn act<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("agent list window")
        }
    }

    fn open(cx: &mut TestAppContext, lanes: LaneList) -> Fixture {
        cx.update(gpui_kit::init);
        let (window, list) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: COLUMN,
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |_window, cx| {
                let list = cx.new(AgentList::new);
                list.update(cx, |list, cx| list.set_lanes(&lanes, cx));
                list
            })
            .expect("agent list window")
        });

        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&list, move |_, event: &AgentListEvent, _| {
                recorded.borrow_mut().push(*event);
            })
        });

        Fixture {
            window,
            list,
            events,
            _subscription: subscription,
        }
    }

    #[gpui_kit::test]
    fn rows_run_main_then_one_per_lane(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("first")),
                lane(2, LaneStatus::Idle, None),
                lane(3, LaneStatus::Down, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let main = window.find(row_id(AgentKey::Coordinator)).bounds();
            let one = window.find(row_id(AgentKey::Lane(1))).bounds();
            let two = window.find(row_id(AgentKey::Lane(2))).bounds();
            let three = window.find(row_id(AgentKey::Lane(3))).bounds();

            assert!(
                main.origin.y < one.origin.y
                    && one.origin.y < two.origin.y
                    && two.origin.y < three.origin.y,
                "rows are out of order: {main:?} {one:?} {two:?} {three:?}"
            );
            // The design's rows: every one 32px, whatever it is showing.
            assert_eq!(main.size.height, px(LANE_ROW), "a row is 32px tall");
            assert_eq!(
                window.find(row_id(AgentKey::Lane(1))).bounds().size.height,
                px(LANE_ROW)
            );
            assert!(one.size.height == two.size.height && two.size.height == three.size.height);
        });
    }

    /// Every state says its own word, and a lane that is working says its clock in
    /// that cell instead — the dot is what says busy, and the word would be the same
    /// for every working row.
    #[gpui_kit::test]
    fn every_state_says_its_own_word(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("porting the reducer")),
                lane(2, LaneStatus::Compacting, Some("compacting the context")),
                lane(3, LaneStatus::Idle, None),
                lane(4, LaneStatus::Starting, None),
                lane(5, LaneStatus::Down, Some("crashed")),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            for (n, status) in [
                (1, LaneStatus::Working),
                (2, LaneStatus::Compacting),
                (3, LaneStatus::Idle),
                (4, LaneStatus::Starting),
                (5, LaneStatus::Down),
            ] {
                let key = AgentKey::Lane(n);
                let aria = window
                    .find(row_id(key))
                    .label()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    aria.contains(&format!("lane {n}")),
                    "lane {n} should name itself: {aria}"
                );
                assert!(
                    aria.contains(status.word()),
                    "lane {n} should say {}: {aria}",
                    status.word()
                );
                let state = window
                    .find(state_id(key))
                    .label()
                    .unwrap_or_default()
                    .to_string();
                if status.is_busy() {
                    assert_ne!(
                        state,
                        status.word(),
                        "lane {n} is working: its cell is the step clock"
                    );
                    assert!(aria.contains("step "), "and the aria says the step: {aria}");
                } else {
                    assert_eq!(state, status.word(), "lane {n}'s cell is its word");
                }
            }
        });
    }

    /// CONTRACT §7.4: the one human action on a lane is stopping it. The button appears
    /// only while a lane has something to stop, and a click on it is not a click on the
    /// row — the reader is stopping the lane, not selecting it.
    #[gpui_kit::test]
    fn a_busy_lane_offers_stop_and_the_click_names_the_lane(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("porting")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            // A lane with something to stop has the control — and only while the
            // pointer is on its row, which is the room the design leaves for it.
            assert!(window.try_find(stop_id(1)).is_some());
            assert!(
                window.try_find(stop_id(2)).is_none(),
                "an idle lane has nothing to stop"
            );
            let row = window.find(row_id(AgentKey::Lane(1))).bounds();
            window.simulate_mouse_move(row.center(), cx);
            window.render_frame(cx);
            assert!(window.find(stop_id(1)).visible());

            window.click(stop_id(1), cx);
        });
        assert_eq!(
            f.events(),
            vec![AgentListEvent::StopLane(1)],
            "a click names the lane it stops"
        );
    }

    #[gpui_kit::test]
    fn the_list_names_itself_and_says_where_each_row_sits(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // The rows name themselves; the list box around them says what it is, so a screen
            // reader can announce "Agents, list box" before reading the rows out.
            let list = window.find(ROWS_ID).label().unwrap_or_default().to_string();
            assert_eq!(list, LIST_LABEL);
            let row = window
                .find(row_id(AgentKey::Lane(2)))
                .label()
                .unwrap_or_default()
                .to_string();
            assert!(row.starts_with("lane 2, idle"), "{row}");
        });
    }

    #[gpui_kit::test]
    fn clicking_a_row_gives_the_list_the_keyboard(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // Nothing has the list's focus, so the arrows are not the list's to answer.
            window.press("down", cx);
        });
        assert!(
            f.events().is_empty(),
            "the arrows belong to the focused list, not to the window"
        );

        f.act(cx, |window, cx| {
            window.click(row_id(AgentKey::Lane(1)), cx);
            window.render_frame(cx);
        });
        assert_eq!(f.events(), vec![AgentListEvent::Select(AgentKey::Lane(1))]);
        f.act(cx, |_window, cx| {
            f.list
                .update(cx, |list, cx| list.set_selected(AgentKey::Lane(1), cx))
        });

        // The list had no focus until the click gave it some, so this press proves the click
        // took the keyboard: the arrow walks on from the row the pointer picked.
        f.act(cx, |window, cx| window.press("down", cx));
        assert_eq!(
            f.events().last(),
            Some(&AgentListEvent::Select(AgentKey::Lane(2))),
            "the click left the list holding the keyboard"
        );
    }

    #[gpui_kit::test]
    fn the_arrows_walk_the_rows_and_home_and_end_go_to_the_ends(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
                lane(3, LaneStatus::Idle, None),
            ]),
        );
        // The owner's side of the contract: every selection the list asks for is confirmed,
        // the way `TabModel::select` does — the list never highlights a row on its own.
        let confirm = |key: AgentKey, cx: &mut TestAppContext| {
            f.act(cx, |_, cx| {
                f.list.update(cx, |list, cx| list.set_selected(key, cx))
            })
        };
        let press =
            |key: &str, cx: &mut TestAppContext| f.act(cx, |window, cx| window.press(key, cx));

        f.act(cx, |window, cx| {
            window.click(row_id(AgentKey::Coordinator), cx);
            window.render_frame(cx);
        });
        confirm(AgentKey::Coordinator, cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
        });

        // Down walks the rows in the order they are drawn, and stops at the last one.
        let mut walked = vec![AgentListEvent::Select(AgentKey::Coordinator)];
        for key in [AgentKey::Lane(1), AgentKey::Lane(2), AgentKey::Lane(3)] {
            press("down", cx);
            walked.push(AgentListEvent::Select(key));
            confirm(key, cx);
        }
        assert_eq!(f.events(), walked, "down walks the rows as they are drawn");

        // At the last row there is nowhere to go, and the list does not re-ask for the row it
        // is already on.
        press("down", cx);
        assert_eq!(
            f.events().len(),
            walked.len(),
            "down at the end asks for nothing"
        );

        // Up walks back, and Home and End are the ends themselves.
        press("up", cx);
        assert_eq!(
            f.events().last(),
            Some(&AgentListEvent::Select(AgentKey::Lane(2)))
        );
        confirm(AgentKey::Lane(2), cx);

        press("home", cx);
        assert_eq!(
            f.events().last(),
            Some(&AgentListEvent::Select(AgentKey::Coordinator))
        );
        confirm(AgentKey::Coordinator, cx);

        press("end", cx);
        assert_eq!(
            f.events().last(),
            Some(&AgentListEvent::Select(AgentKey::Lane(3)))
        );
    }

    #[gpui_kit::test]
    fn a_focused_list_is_the_only_thing_the_arrows_move(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Working, Some("one"))]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.press("up", cx);
            window.press("home", cx);
            window.press("end", cx);
        });
        assert!(
            f.events().is_empty(),
            "an unfocused list answers nothing: {:?}",
            f.events()
        );
    }

    #[gpui_kit::test]
    fn clicking_a_row_asks_the_owner_to_select_that_agent(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(row_id(AgentKey::Lane(2)), cx);
        });

        assert_eq!(f.events(), vec![AgentListEvent::Select(AgentKey::Lane(2))]);

        // The list does not select itself: the owner's answer is what highlights a row.
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(row_id(AgentKey::Lane(2))).selected(),
                Some(false)
            );
            f.list
                .update(cx, |list, cx| list.set_selected(AgentKey::Lane(2), cx));
            window.render_frame(cx);
            assert_eq!(
                window.find(row_id(AgentKey::Lane(2))).selected(),
                Some(true)
            );
            assert_eq!(
                window.find(row_id(AgentKey::Coordinator)).selected(),
                Some(false)
            );
        });
    }

    /// A long task is elided by the cell that holds it, and the state at the row's
    /// end — the clock while the lane works — is never pushed off it.
    #[gpui_kit::test]
    fn a_long_task_ellipsizes_and_never_pushes_the_state_out(cx: &mut TestAppContext) {
        let long = "port the transcript reducer to the new event shape and keep every row \
                    measured while it streams";
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some(long)),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            let row = window.find(row_id(AgentKey::Lane(1))).bounds();
            let task = window.find(task_id(AgentKey::Lane(1))).bounds();
            let state = window.find(state_id(AgentKey::Lane(1)));

            assert!(
                row.size.width <= COLUMN_WIDTH && row.origin.x >= px(0.),
                "the row overflows the column: {row:?}"
            );
            assert!(
                task.size.height < px(LANE_ROW),
                "the task wrapped onto another line: {task:?}"
            );
            assert!(
                task.right() <= state.bounds().left(),
                "the task runs into the state: {task:?} vs {:?}",
                state.bounds()
            );
            assert!(
                state.bounds().right() <= row.right(),
                "the state left the row: {:?} vs {row:?}",
                state.bounds()
            );
            // A working lane's state cell is its step clock; an idle one's is its word.
            assert!(
                state.label().is_some_and(|state| state != "working"),
                "the clock, not the word: {:?}",
                state.label()
            );
            assert_eq!(
                window.find(state_id(AgentKey::Lane(2))).label(),
                Some("idle"),
                "an idle lane says so in the same cell"
            );
        });
    }

    #[gpui_kit::test]
    fn a_list_update_keeps_the_selection(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, Some("two")),
                lane(3, LaneStatus::Idle, Some("three")),
            ]),
        );
        f.act(cx, |window, cx| {
            f.list
                .update(cx, |list, cx| list.set_selected(AgentKey::Lane(3), cx));
            window.render_frame(cx);
        });

        // The swarm moves on: same lanes, different states, tasks and clocks.
        f.act(cx, |window, cx| {
            let next = lanes(vec![
                lane(1, LaneStatus::Idle, Some("one")),
                lane(2, LaneStatus::Working, Some("two")),
                lane(3, LaneStatus::Working, Some("three")),
            ]);
            f.list.update(cx, |list, cx| list.set_lanes(&next, cx));
            window.render_frame(cx);
        });

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(f.list.read(cx).selected(), AgentKey::Lane(3));
            assert_eq!(
                window.find(row_id(AgentKey::Lane(3))).selected(),
                Some(true)
            );
            assert_eq!(
                window.find(row_id(AgentKey::Lane(1))).selected(),
                Some(false)
            );
            // The update did land: lane 3 is working now, so its state cell counts.
            assert_ne!(
                window.find(state_id(AgentKey::Lane(3))).label(),
                Some("working"),
                "a working lane's cell is its clock"
            );
        });
    }

    /// §7.3: a lane's step clock counts from the absolute start the swarm published, and
    /// the owner stamps the moment it counts to (`set_now`). The list reads no clock of
    /// its own, and an owner that never stamps one shows what the swarm last said.
    #[gpui_kit::test]
    fn a_lane_clock_counts_on_from_the_start_the_swarm_published(cx: &mut TestAppContext) {
        let mut row = lane(1, LaneStatus::Working, Some("build the readout segments"));
        row.step_started_at = Some(NOW - 30_000);
        let f = open(cx, lanes(vec![row]));
        f.list.update(cx, |list, cx| list.set_now(NOW, cx));
        let clock = |window: &mut Window| {
            window
                .find(row_id(AgentKey::Lane(1)))
                .label()
                .unwrap_or_default()
                .to_owned()
        };
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                clock(window).ends_with("step 30s"),
                "the age the swarm reported: {}",
                clock(window)
            );

            // Three seconds of frames later — the same rows, nothing re-read.
            f.list.update(cx, |list, cx| list.set_now(NOW + 3_000, cx));
            window.render_frame(cx);
            assert!(
                clock(window).ends_with("step 33s"),
                "the clock counted on: {}",
                clock(window)
            );
        });
    }

    #[gpui_kit::test]
    fn the_down_reason_replaces_the_task_until_the_lane_comes_back(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![lane(
                1,
                LaneStatus::Down,
                Some("build the readout segments"),
            )]),
        );
        f.act(cx, |window, cx| {
            f.list.update(cx, |list, cx| {
                list.set_down_reason(
                    1,
                    Some("model ark-opus-4.5 is not registered under provider :aiden".to_string()),
                    cx,
                )
            });
            window.render_frame(cx);
            let label = window
                .find(row_id(AgentKey::Lane(1)))
                .label()
                .unwrap_or_default()
                .to_string();
            assert!(label.contains("not registered"), "{label}");

            // An empty reason is not a reason.
            f.list.update(cx, |list, cx| {
                list.set_down_reason(1, Some("  ".into()), cx)
            });
            window.render_frame(cx);
            let label = window
                .find(row_id(AgentKey::Lane(1)))
                .label()
                .unwrap_or_default()
                .to_string();
            assert!(label.contains("build the readout segments"), "{label}");
        });
    }

    #[gpui_kit::test]
    fn the_header_counts_the_lanes_and_the_busy_ones(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Compacting, None),
                lane(3, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(SUMMARY_ID).label(),
                Some("2 of 3 busy"),
                "the band counts the lanes the list is drawing"
            );
        });
    }

    #[gpui_kit::test]
    fn the_coordinator_row_follows_the_activity_and_the_stream(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "main, idle");
            assert!(
                window.try_find(badge_id(AgentKey::Coordinator)).is_none(),
                "no badge while the stream is up"
            );

            f.list.update(cx, |list, cx| {
                list.set_coordinator(Status::Running, true, cx)
            });
            window.render_frame(cx);
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "main, running");
            assert!(window.find(badge_id(AgentKey::Coordinator)).visible());
            // The state keeps its cell: the badge is added after it, not instead.
            assert!(window.find(state_id(AgentKey::Coordinator)).visible());
        });
    }

    /// The coordinator's row as the view model has it, written out: the state cell,
    /// which is the word or the clock.
    fn coordinator_cells(f: &Fixture, cx: &App) -> (String, Option<String>) {
        let palette = design::palette(cx.theme().mode.is_dark());
        let view = f.list.read(cx).coordinator_view(palette);
        (view.state.to_string(), None)
    }

    /// The coordinator's step clock sits beside its state, as a lane's does: it is shown
    /// while the coordinator runs or compacts, and an idle `main` never claims a step —
    /// the state word is what shows then, in the cell the clock would take.
    #[gpui_kit::test]
    fn the_coordinator_clock_takes_its_cell_only_while_it_works(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            // Idle: the word, even with a clock in hand.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Status::Idle, false, cx);
                list.set_coordinator_clock(Some("41s".to_string()), cx);
            });
            assert_eq!(
                coordinator_cells(&f, cx).0,
                "idle",
                "an idle main shows its state, not the seconds since a run that ended"
            );

            // Running with a clock: the clock, in the cell the lanes use, and the row's aria
            // says the step as a lane row's does.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Status::Running, false, cx);
            });
            assert_eq!(
                coordinator_cells(&f, cx).0,
                "41s",
                "working: the step clock"
            );
            window.render_frame(cx);
            assert_eq!(
                window.find(state_id(AgentKey::Coordinator)).label(),
                Some("41s"),
                "the coordinator's own step, in the state cell"
            );
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "main, running, step 41s");

            // A compaction is work too.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Status::Compacting, false, cx);
                list.set_coordinator_clock(Some("3m".to_string()), cx);
            });
            assert_eq!(
                coordinator_cells(&f, cx).0,
                "3m",
                "a compaction is work too"
            );

            // No clock (the step was never stamped, or the run just ended): the word is what
            // shows, and a blank string is no clock either.
            f.list
                .update(cx, |list, cx| list.set_coordinator_clock(None, cx));
            assert_eq!(coordinator_cells(&f, cx), ("compacting".to_string(), None));
            f.list.update(cx, |list, cx| {
                list.set_coordinator_clock(Some("   ".to_string()), cx)
            });
            assert_eq!(coordinator_cells(&f, cx), ("compacting".to_string(), None));

            // A step that just began is a real clock, not an empty cell.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Status::Running, false, cx);
                list.set_coordinator_clock(Some("0s".to_string()), cx);
            });
            assert_eq!(coordinator_cells(&f, cx).0, "0s", "a step that just began");
            window.render_frame(cx);
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "main, running, step 0s");
        });
    }

    #[test]
    fn the_coordinator_activity_maps_onto_the_row_states() {
        assert_eq!(activity_status(Status::Idle), LaneStatus::Idle);
        assert_eq!(activity_status(Status::Running), LaneStatus::Working);
        assert_eq!(activity_status(Status::Compacting), LaneStatus::Compacting);
        assert_eq!(activity_word(Status::Running), "running");
    }

    #[test]
    fn a_tooltip_line_never_runs_past_its_width() {
        let long = "worktree /Users/you/.evo/swarm/sw-1a2b3c4d/lane-6 (evo/lane-6) · 2 restarts";
        for line in fold(long, TOOLTIP_LINE).lines() {
            assert!(
                line.chars().count() <= TOOLTIP_LINE,
                "{line:?} is wider than {TOOLTIP_LINE}"
            );
        }
        // A single word longer than the line is split rather than left to run on.
        let word = "a".repeat(100);
        for line in fold(&word, 10).lines() {
            assert_eq!(line.chars().count(), 10);
        }
        assert_eq!(fold("short", 10), "short");
    }

    #[test]
    fn the_lane_tooltip_carries_what_the_row_has_no_room_for() {
        let mut row = lane(2, LaneStatus::Working, Some("build the readout segments"));
        row.worktree = Some("/tmp/sw/lane-2".to_string());
        row.branch = Some("evo/lane-2".to_string());
        row.restarts = 2;
        row.goal_status = Some("active".to_string());
        let tooltip = lane_tooltip(&row, None, NOW);
        // Folded for the tooltip's width, so the checks ignore where the line breaks land.
        let flat = tooltip.replace('\n', " ");
        assert!(flat.starts_with("lane 2  working · step 1m"), "{tooltip}");
        assert!(
            flat.contains("task: build the readout segments"),
            "{tooltip}"
        );
        assert!(flat.contains("/tmp/sw/lane-2 (evo/lane-2)"), "{tooltip}");
        assert!(flat.contains("2 restarts"), "{tooltip}");
        assert!(flat.contains("pid 4002"), "{tooltip}");
        assert!(flat.contains("goal active"), "{tooltip}");

        // The reason gets a line of its own, and a lane with no restarts does not brag
        // about them.
        row.restarts = 0;
        let tooltip = lane_tooltip(&row, Some(&"crashed on startup".to_string()), 0);
        assert!(tooltip.ends_with("crashed on startup"), "{tooltip}");
        assert!(!tooltip.contains("restart"), "{tooltip}");
    }
}
