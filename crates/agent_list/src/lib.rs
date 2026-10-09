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
//! set_add_lane_disabled(bool, cx)                     adding a lane is not on offer
//! → AgentListEvent::Select(AgentKey)                  the owner calls TabModel::select
//! → AgentListEvent::StopLane(u32)                     the owner sends run.interrupt{scope:lane}
//! → AgentListEvent::AddLane                           the owner sends command.run{name:"lanes"}
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_kit::base::Button;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, ClickEvent, Context, ElementId, EventEmitter, FocusHandle, FontFeatures, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use session::{AgentKey, LaneList, LaneRow, LaneStatus, Status};
use store::design::{self, Palette, Rgb, HEADER_HEIGHT, INSET, RADIUS};
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
/// The list's own inset and the gap between rows (`.ws-lane-list{padding:6px}`, and the
/// design's own `gap:2px` override in `styles.css`).
const LIST_PAD: f32 = 6.;
const LIST_GAP: f32 = 2.;
/// The band's two sizes: `.ws-head-title{font-size:13px}` and
/// `.ws-head-meta{font-size:12px}`.
const TITLE_SIZE: Pixels = px(13.);
const META_SIZE: Pixels = px(12.);
/// A row's state: `.ws-lane-state{font-size:12px}`.
const STATE_SIZE: Pixels = px(12.);
/// The Stop control and the `reconnecting` badge, which the design has no size for:
/// the chrome's own small print.
const STOP_SIZE: Pixels = px(11.);
/// The Stop control's own width, pinned to the four pixels of padding a side its label
/// takes — `px(4.) * 2` around an 11px `Stop` — so a row that can be stopped can keep
/// exactly that much room for it (see [`AgentList::row`]).
const STOP_WIDTH: f32 = 35.;

/// The row's fill on hover and while selected: the ink a few percent into the sidebar
/// (`--row-surface: color-mix(in srgb, var(--fg) 5%|9%, var(--sidebar))`). The theme
/// carries the same two mixes; the dot needs them as tokens of its own, so they are
/// named here too.
const HOVER_MIX: f32 = 5.;
const SELECTED_MIX: f32 = 9.;
/// An idle dot's ring in a workspace row: `.dot-idle{opacity:.8}` — the tab strip's
/// own ring is `IDLE_OPACITY`, and the two are not the same weight.
const WORKSPACE_IDLE: f32 = 0.8;

/// The row's own geometry, public because the lane column has one more row than this
/// list draws: the project's own line at the foot of the column is a row like these,
/// and it has to be the same row rather than a lookalike (§7.3).
pub const ROW_HEIGHT: f32 = LANE_ROW;
/// The gap between a row's own cells.
pub const ROW_GAP: f32 = LANE_GAP;
/// A row's own inset.
pub const ROW_PAD: f32 = LANE_PAD;
pub const ROW_FONT: Pixels = LANE_FONT;
/// The column's inset around its rows, and the room between two of them — what the
/// footer row is aligned by, so it stands in the same column as the lanes.
pub const LIST_INSET: f32 = LIST_PAD;
pub const ROW_SPACING: f32 = LIST_GAP;

/// The fill a row of the column wears: its surface while it is the one being shown, and
/// the sidebar otherwise, with [`row_hover_fill`] under the pointer. The project row at
/// the foot of the column is drawn in the same two, so "this is the row I am looking at"
/// reads the same wherever in the column it is.
pub fn row_fill(selected: bool, palette: &Palette) -> Rgb {
    if selected {
        paint::mix(palette.fg, SELECTED_MIX, palette.sidebar)
    } else {
        palette.sidebar
    }
}

/// What the pointer leaves under a row it is on — the fill a footer row hovers to.
pub fn row_hover_fill(palette: &Palette) -> Rgb {
    paint::mix(palette.fg, HOVER_MIX, palette.sidebar)
}

/// How wide a tooltip line is allowed to get, in characters: about one and a half columns,
/// which fits a worktree path beside its label and still keeps a long task or a long
/// failure reason inside the window.
const TOOLTIP_LINE: usize = 56;

/// The rows scroller, so a swarm with more lanes than fit can be scrolled. It is the
/// wrapper the kit's scrollbar builds around the rows; the rows themselves sit inside it,
/// under the name this one gives them with `"content"` after it (`rows_content_id`).
const ROWS_ID: &str = "agent-list-rows";

/// The id of the rows inside that scroller: what the kit's scroll area hands the element
/// it was given (`gpui_component::scroll::Scrollable`'s named `content` child). The list's
/// own name, its role and its rows are all on it, because it is the element the list
/// builds — the scroller around it is the kit's.
#[cfg(test)]
fn rows_content_id() -> ElementId {
    (ElementId::from(ROWS_ID), "content").into()
}

/// The `6 lanes · 2 busy` summary above the rows.
const SUMMARY_ID: &str = "agent-list-summary";

/// What the column is called, for a screen reader: the rows name themselves, but the list
/// around them needs a name of its own.
const LIST_LABEL: &str = "Agents";

/// The row the list ends with while this is a swarm: the one way to grow one from the
/// tab page. The design's reference draws no such row, so it borrows the lane rows' own
/// geometry and hover.
const ADD_LANE_LABEL: &str = "+ Add New Lane";

/// That row's element id, the way a lane's row is addressed.
const ADD_LANE_ID: &str = "agent-add-lane";

/// What the list asks its owner to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentListEvent {
    /// Show this agent's transcript in the center column: the owner calls
    /// `TabModel::select` and feeds the result back with [`AgentList::set_selected`].
    Select(AgentKey),
    /// Stop one lane (`run.interrupt` with scope `lane`): the one human action on a lane
    /// besides typing to the coordinator (CONTRACT §7.4).
    StopLane(u32),
    /// Grow the swarm by one lane (the owner sends `command.run` with the `lanes`
    /// command): the last row of the list, and only a swarm's — one agent has no lanes
    /// to add.
    AddLane,
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
    /// The row the pointer is on, for the dot's `--row-surface`: the dot paints a colour
    /// fixed when the row was built, so the row says where the pointer is.
    hovered: Option<AgentKey>,
    /// The add row's own focus handle. It is the caller's because the row has to be able
    /// to take the keyboard itself: an ancestor's own focus transfer wins the mouse-down
    /// that lands on it, exactly as the column's does for a click on a lane's row.
    add_focus: FocusHandle,
    /// Whether this session is a swarm (§7.2): its first row is the swarm's
    /// *coordinator*, and one agent's is simply **Main** — there is nobody to
    /// coordinate — and the column counts lanes, which a single agent has none of.
    swarm: bool,
    /// Whether adding a lane is off the table right now: a `command.run` for one is in
    /// flight, or the swarm cannot take another (the coordinator is busy, the lane cap is
    /// reached, there is no roster yet). The list's last row goes inert while it is.
    add_lane_disabled: bool,
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
            hovered: None,
            swarm: true,
            add_lane_disabled: false,
            add_focus: cx.focus_handle(),
        }
    }

    /// Which program this column is showing (§7.2): a swarm's coordinator, or the one
    /// agent a single-agent session has. It is the difference between the first row's
    /// name — `Coordinator` against `Main` — and whether the lane count is drawn at all.
    pub fn set_swarm(&mut self, swarm: bool, cx: &mut Context<Self>) {
        if self.swarm != swarm {
            self.swarm = swarm;
            cx.notify();
        }
    }

    /// What this column calls its first row: a swarm is coordinated, one agent is not
    /// ([`AgentKey::name`], the one place those names are written).
    fn main_name(&self) -> String {
        AgentKey::Coordinator.name(self.swarm)
    }

    /// Whether the list's last row answers at all: `false` while adding a lane is on
    /// offer, `true` while it is not — a request in flight, a busy coordinator, the
    /// swarm's own cap, no roster yet. The owner is the only one who knows, so it is the
    /// only one who sets it.
    pub fn set_add_lane_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.add_lane_disabled != disabled {
            self.add_lane_disabled = disabled;
            cx.notify();
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

    /// The fill a row's working dot breathes against: `.ws-lane`'s `--row-surface` —
    /// the sidebar at rest, the 5% mix under the pointer, the 9% mix while selected.
    fn dot_surface(&self, key: AgentKey, palette: &'static Palette) -> Rgb {
        if self.is_selected(key) {
            paint::mix(palette.fg, SELECTED_MIX, palette.sidebar)
        } else if self.hovered == Some(key) {
            paint::mix(palette.fg, HOVER_MIX, palette.sidebar)
        } else {
            palette.sidebar
        }
    }

    /// The lanes as the list currently draws them.
    pub fn lanes(&self) -> &LaneList {
        &self.lanes
    }

    fn is_selected(&self, key: AgentKey) -> bool {
        self.selected == key
    }

    /// The agents in the order the rows are drawn: the first row first, then the lanes.
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
        // A list with nothing highlighted starts from the top; there is always a first
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
        // A count of lanes is a swarm's own: a single-agent session has none to count,
        // and `0 of 0 busy` is not a thing to read.
        let meta = self
            .swarm
            .then(|| format!("{} of {} busy", self.lanes.busy(), lanes));
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
            .when_some(meta, |header, meta| {
                header.child(
                    div()
                        .id(SUMMARY_ID)
                        .test_support()
                        .ml_auto()
                        .text_size(META_SIZE)
                        .text_color(paint::color(palette.muted_fg))
                        .aria_label(meta.clone())
                        .child(meta),
                )
            })
    }

    /// The first row: a swarm's coordinator or one agent's `Main`, with the state cell
    /// saying what it is doing.
    fn coordinator_view(&self, palette: &'static Palette) -> RowView {
        let status = activity_status(self.activity);
        let word = activity_word(self.activity);
        let name = self.main_name();
        // The clock sits in the state cell while this row is actually working, as a
        // lane's does: an idle row says `idle`, not the seconds since a run that
        // already ended.
        let busy = matches!(self.activity, Status::Running | Status::Compacting);
        let clock = busy.then(|| self.coordinator_clock.clone()).flatten();
        let badge = self
            .coordinator_reconnecting
            .then(|| SharedString::from("reconnecting"));
        let mut tooltip = format!("{name} · {word}");
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
            name: name.clone().into(),
            busy: status.is_busy(),
            task: None,
            task_color: paint::color(palette.muted_fg),
            state,
            state_color: paint::color(palette.muted_fg),
            badge,
            stop: None,
            dot_surface: self.dot_surface(AgentKey::Coordinator, palette),
            tooltip: tooltip.into(),
            aria: format!("{name}, {word}{step}").into(),
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
            "{}, {word}{}{}",
            key.name(self.swarm),
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
            name: key.name(self.swarm).into(),
            busy: row.is_busy(),
            task: task.map(SharedString::from),
            task_color,
            state,
            state_color,
            badge: None,
            stop: row.is_busy().then_some(key),
            dot_surface: self.dot_surface(key, palette),
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
    ///
    /// `under` is the fill the row has under the pointer, which is the fill the button
    /// is drawn on: it is pinned over the state cell rather than beside it, and an
    /// opaque box in any other colour would show as a patch on the row.
    fn stop_button(
        &self,
        lane: u32,
        under: Rgb,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(stop_id(lane))
            .test_support()
            .flex_none()
            .invisible()
            .w(px(STOP_WIDTH))
            .py(px(1.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(RADIUS))
            .bg(paint::color(under))
            .border_1()
            .border_color(paint::color(palette.border))
            .text_size(STOP_SIZE)
            .text_color(paint::color(palette.muted_fg))
            // The design keeps the arrow over its chrome; only the slider asks for a
            // hand (`cursor:default` throughout `Workspace.css`).
            .cursor_default()
            .group_hover("lane", |stop| stop.visible())
            .hover(move |stop| stop.text_color(paint::color(palette.destructive)))
            .aria_label(format!("Stop Lane {lane}"))
            .on_click(cx.listener(move |_, _, _, cx| {
                // A click on the button is not a click on the row: the row must not also
                // select the lane the reader is stopping.
                cx.stop_propagation();
                cx.emit(AgentListEvent::StopLane(lane));
            }))
            .child("Stop")
    }

    /// The list's last row while this is a swarm: one lane more ([`AgentListEvent::AddLane`]).
    ///
    /// A lane row's own shape — 32px, the list's inset, a 13px line — and the quietest
    /// thing in the column: muted ink, a plus aligned with the dots, and no task or state
    /// cell. The kit's button is what makes it a keyboard's (focus, Tab stop, Enter/Space)
    /// and what makes `disabled` real. The owner disables it while adding a lane is not on
    /// offer — a request in flight, a busy coordinator, a full swarm — and gpui has no
    /// `aria-disabled` builder, so that is drawn, not named.
    fn add_lane_row(&self, palette: &'static Palette, cx: &Context<Self>) -> impl IntoElement {
        let disabled = self.add_lane_disabled;
        Button::new(ADD_LANE_ID)
            .track_focus(&self.add_focus)
            .h(px(LANE_ROW))
            .w_full()
            .flex_none()
            .px(px(LANE_PAD))
            .justify_start()
            .rounded(px(RADIUS))
            .text_size(LANE_FONT)
            .text_color(paint::color(palette.muted_fg))
            .cursor_default()
            .disabled(disabled)
            .accessibility_label(ADD_LANE_LABEL)
            .when(!disabled, |row| {
                row.hover(move |row| row.text_color(paint::color(palette.fg)))
            })
            .focus_visible(move |row| row.text_color(paint::color(palette.primary)))
            .when(disabled, |row| row.opacity(0.45))
            // The ancestor that holds the keyboard would otherwise take the press, as it
            // does for a lane's row.
            .on_click(cx.listener(|list, _: &ClickEvent, window, cx| {
                list.add_focus.focus(window, cx);
                cx.emit(AgentListEvent::AddLane);
            }))
            .child(
                h_flex()
                    .gap(px(LANE_GAP))
                    .items_center()
                    .child(
                        div()
                            .id("agent-add-lane-icon")
                            .test_support()
                            .w(px(design::DOT))
                            .flex_none()
                            .text_center()
                            .child("+"),
                    )
                    .child(
                        div()
                            .id("agent-add-lane-label")
                            .test_support()
                            .child("Add New Lane"),
                    ),
            )
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
        let stoppable = stop.is_some();
        // Match the three `--row-surface` fills used by the dot.
        let hover = paint::mix(palette.fg, HOVER_MIX, palette.sidebar);
        let active = paint::mix(palette.fg, SELECTED_MIX, palette.sidebar);
        let surface = if selected { active } else { palette.sidebar };
        // What the pointer leaves under a row it is on, which is what a control pinned
        // over that row is drawn on.
        let under_pointer = if selected { active } else { hover };
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
            // The row says where the pointer is, so its dot can breathe against that
            // row's own fill (§7.3).
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                let next = if *hovered { Some(key) } else { None };
                if next.is_some() && this.hovered != next {
                    this.hovered = next;
                    cx.notify();
                } else if next.is_none() && this.hovered == Some(key) {
                    // Only the row being left clears it: the next row's `true` may have
                    // arrived first.
                    this.hovered = None;
                    cx.notify();
                }
            }))
            .child({
                let busy = view.busy;
                let slot = format!("agent-{key:?}");
                BreathingDot::new(widgets::dot::dot_id(slot), busy)
                    .palette(palette)
                    .surface(view.dot_surface)
                    .idle_opacity(WORKSPACE_IDLE)
                    .render()
            })
            .child(
                div()
                    .flex_none()
                    // `.ws-lane.selected{font-weight:500}` — the design's medium, which
                    // `.ws-lane-task` and `.ws-lane-state` opt out of with a 400 of their
                    // own, so the name is the cell that gets heavier.
                    .when(selected, |name| name.font_weight(widgets::text::MEDIUM))
                    .child(name),
            )
            .child(
                // The flexible cell: the task, or nothing to say. A row that can be
                // stopped keeps the Stop's room at its trailing edge — this is the cell
                // the design's `flex:1` gives way — so the control lands beside the text
                // rather than over the ellipsis that ends it.
                div()
                    .id(task_id)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .when(stoppable, |task| task.pr(px(STOP_WIDTH)))
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
            //
            // It is pinned over the row's trailing edge rather than laid out in it. The
            // design puts every row's state in one place — the far end, so the eye can
            // read down the column — and a control that took room of its own there would
            // hold a working lane's clock away from that edge on every frame, whether the
            // pointer was on the row or not. Painted on the row's own fill, over the cell
            // it replaces, it moves nothing.
            .when_some(stop, |row, stop| {
                let lane = stop.lane().unwrap_or_default();
                row.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .bottom(px(0.))
                        .right(px(LANE_PAD))
                        .flex()
                        .items_center()
                        .child(self.stop_button(lane, under_pointer, palette, cx)),
                )
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
                widgets::tooltip::text("agent-row-tooltip", tooltip.clone(), px(360.), window, cx)
            })
    }
}

impl Render for AgentList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = design::palette(cx.theme().mode.is_dark());
        let coordinator = self.coordinator_view(palette);
        // §7.2: one agent has no lanes, so the column is its single row. The rows come
        // from the swarm's own topic, which an `evo-agent` server never publishes, so
        // this only ever drops what a swarm's snapshot left behind.
        let lanes: Vec<RowView> = if self.swarm {
            self.lanes
                .lanes
                .clone()
                .iter()
                .map(|row| self.lane_view(row, palette))
                .collect()
        } else {
            Vec::new()
        };

        let mut rows = v_flex()
            .id(ROWS_ID)
            .test_support()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap(px(LIST_GAP))
            .p(px(LIST_PAD))
            .role(Role::ListBox)
            .aria_label(LIST_LABEL)
            // A swarm with more lanes than the column has room for scrolls, and the kit's
            // own thumb says so — on the colours the app's theme gives it
            // (`crates/app/src/theme.rs`). The wrapper this builds around the rows keeps
            // the list's own id, and hands the rows the id this names with `"content"`
            // after it (`rows_content_id`).
            .overflow_y_scrollbar()
            .id(ROWS_ID);
        rows = rows.child(self.row(coordinator, palette, cx));
        for lane in lanes {
            rows = rows.child(self.row(lane, palette, cx));
        }
        // Grows the swarm, after the last lane — and only a swarm's (§7.2).
        if self.swarm {
            rows = rows.child(self.add_lane_row(palette, cx));
        }

        v_flex()
            .id("agent-column-list")
            .test_support()
            .w_full()
            .h_full()
            .bg(paint::color(palette.sidebar))
            // The column is the tab stop (§7.3): the frame around it is focused rather
            // than any row, so the arrows can walk the rows without a row of its own
            // having to be a control.
            .track_focus(&self.focus_handle)
            .tab_stop(true)
            .on_key_down(cx.listener(Self::key_down))
            .text_color(paint::color(palette.fg))
            .child(self.header(palette))
            .child(rows)
            // The column is a tab stop, so it says when the keyboard has it — and only
            // then: a click that selects a lane is not a reason to outline the whole
            // column, which is what `:focus-visible` means and what the design draws.
            //
            // The ring is a child laid over the column's own box, not a border on it.
            // A border is laid out inside the box, so the whole column — its band, its
            // rows, the rule under the band — would sit a pixel in from every edge, and
            // the band over the lanes would meet the conversation's band a pixel lower
            // than the design draws the one straight rule across both columns.
            .child(
                div()
                    .id("agent-column-ring")
                    .absolute()
                    .top(px(0.))
                    .left(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    // Paint only: the column itself is what the keyboard focuses, and
                    // this says so without taking a tab stop of its own.
                    .track_focus(&self.focus_handle)
                    .border_1()
                    .border_color(gpui_kit::Hsla::default())
                    .focus_visible(move |style| style.border_color(paint::color(palette.primary))),
            )
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
    /// The fill above, for the working dot: `--row-surface`.
    dot_surface: Rgb,
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
        point, AnyWindowHandle, App, AppContext as _, Bounds, Entity, InputEvent as _, Pixels,
        ScrollDelta, ScrollWheelEvent, Size, Subscription, TestAppContext, Window, WindowBounds,
        WindowOptions,
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
                    aria.contains(&format!("Lane {n}")),
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

    /// The column's own last row is where a swarm grows (§7.3 has no such row; it is the
    /// app's own). It is a lane row's own shape — 32px, the list's inset, the same width,
    /// laid out after the last lane rather than over it — and it is a button: it names
    /// itself to a screen reader, and one press is one lane.
    #[gpui_kit::test]
    fn the_swarm_grows_a_lane_after_its_last_one(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let add = window.find(ADD_LANE_ID);
            let last = window.find(row_id(AgentKey::Lane(2))).bounds();

            assert_eq!(add.role(), Some(Role::Button), "it is a control, not a row");
            assert_eq!(add.label(), Some(ADD_LANE_LABEL), "and says what it does");
            assert_eq!(add.bounds().size.height, px(LANE_ROW), "a row's own height");
            assert_eq!(add.bounds().origin.x, last.origin.x, "the list's own inset");
            assert_eq!(
                add.bounds().size.width,
                last.size.width,
                "and its own width"
            );
            assert!(
                add.bounds().origin.y > last.origin.y,
                "it comes after the last lane: {last:?} then {:?}",
                add.bounds()
            );
            assert_eq!(
                window.find("agent-add-lane-icon").bounds().origin.x,
                last.origin.x + px(LANE_PAD),
            );
            assert_eq!(
                window.find("agent-add-lane-label").bounds().origin.x,
                last.origin.x + px(LANE_PAD + design::DOT + LANE_GAP),
            );

            window.click(ADD_LANE_ID, cx);
        });
        assert_eq!(
            f.events(),
            vec![AgentListEvent::AddLane],
            "one press is one lane"
        );
    }

    /// It is the list's last row, not something laid over it: it lives inside the rows the
    /// scroller wraps, under the last lane, and the list's own inset ends the content.
    #[gpui_kit::test]
    fn the_add_row_is_the_lists_last_row(cx: &mut TestAppContext) {
        let rows: Vec<LaneRow> = (1..=30).map(|n| lane(n, LaneStatus::Idle, None)).collect();
        let f = open(cx, lanes(rows));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let add = window.find(ADD_LANE_ID).bounds();
            let content = window.find(rows_content_id()).bounds();
            assert!(
                add.origin.y > window.find(row_id(AgentKey::Lane(30))).bounds().origin.y,
                "the last lane comes first: {add:?}"
            );
            assert!(
                add.bottom() <= content.bottom(),
                "it is inside the list's own content: {add:?} in {content:?}"
            );
            assert!(
                add.bottom() + px(LIST_PAD) >= content.bottom(),
                "and nothing follows it: {add:?} in {content:?}"
            );
        });
    }

    /// §7.2: one agent has no lanes, so it has nothing to add and no row that would.
    #[gpui_kit::test]
    fn a_single_agents_column_has_nothing_to_add(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find(ADD_LANE_ID).is_some(),
                "a swarm's column ends with the row"
            );

            f.list.update(cx, |list, cx| list.set_swarm(false, cx));
            window.render_frame(cx);
            assert!(
                window.try_find(ADD_LANE_ID).is_none(),
                "one agent has no lanes to add"
            );
        });
        assert!(f.events().is_empty(), "and nothing was asked for");
    }

    /// One press is one lane: while adding one is not on offer — a request already in
    /// flight, or a swarm that cannot take another — the row is inert.
    #[gpui_kit::test]
    fn the_add_row_is_inert_while_it_is_disabled(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.list
                .update(cx, |list, cx| list.set_add_lane_disabled(true, cx));
            window.render_frame(cx);
            window.click(ADD_LANE_ID, cx);
        });
        assert!(
            f.events().is_empty(),
            "a disabled row takes no press: {:?}",
            f.events()
        );

        f.act(cx, |window, cx| {
            f.list
                .update(cx, |list, cx| list.set_add_lane_disabled(false, cx));
            window.render_frame(cx);
            window.click(ADD_LANE_ID, cx);
        });
        assert_eq!(
            f.events(),
            vec![AgentListEvent::AddLane],
            "and once it is on offer the row answers again"
        );
    }

    /// The row is a button, so the keyboard reaches it the way it reaches one: Tab from
    /// the column lands on it, a press leaves the keyboard on it, and Enter and Space each
    /// activate it once — the kit's own native button behaviour.
    #[gpui_kit::test]
    fn the_add_row_answers_the_keyboard(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // A press on a row takes the list's keyboard, as it always does; Tab from
            // there walks the column's own tab stops, and the add row is one.
            window.click(row_id(AgentKey::Coordinator), cx);
            window.press("tab", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(ADD_LANE_ID).focused(),
                Some(true),
                "Tab reaches the row"
            );
        });

        let walked = f.events().len();
        f.act(cx, |window, cx| {
            window.press("enter", cx);
            window.press("space", cx);
        });
        assert_eq!(
            f.events()[walked..],
            [AgentListEvent::AddLane, AgentListEvent::AddLane],
            "Enter and Space are each one lane: {:?}",
            f.events()
        );

        // A press on the row itself leaves the keyboard on it, the way a press on a lane
        // row leaves it on the column.
        f.act(cx, |window, cx| {
            window.click(ADD_LANE_ID, cx);
            window.render_frame(cx);
            assert_eq!(window.find(ADD_LANE_ID).focused(), Some(true));
        });
        assert_eq!(
            f.events().last(),
            Some(&AgentListEvent::AddLane),
            "and the press was one lane too"
        );
    }

    /// §7.3's rows, with the design's own numbers: the list insets them by 6px, a row is
    /// 32px with 2px between them, and the column's content reaches its edges — the
    /// focus ring is painted over the column, not laid out inside it, or the band over
    /// the lanes would meet the conversation's band a pixel low and the rows would start
    /// a pixel in.
    #[gpui_kit::test]
    fn the_column_holds_its_rows_to_the_designs_own_numbers(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Idle, None),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let main = window.find(row_id(AgentKey::Coordinator)).bounds();
            let one = window.find(row_id(AgentKey::Lane(1))).bounds();
            let two = window.find(row_id(AgentKey::Lane(2))).bounds();

            // The list's 6px inset, and a row's 32px on a 34px pitch (§7.3's rows are
            // 32px with the design's 2px gap between them).
            assert_eq!(
                main.origin.x,
                px(LIST_PAD),
                "the rows are inset by the list"
            );
            assert_eq!(
                main.size.width,
                COLUMN_WIDTH - px(2. * LIST_PAD),
                "a row fills the list, inset on both sides"
            );
            assert_eq!(main.size.height, px(LANE_ROW));
            assert_eq!(
                main.origin.y,
                px(HEADER_HEIGHT + LIST_PAD),
                "the first row sits under the column's band"
            );
            assert_eq!(one.origin.y, main.origin.y + px(LANE_ROW + LIST_GAP));
            assert_eq!(two.origin.y, one.origin.y + px(LANE_ROW + LIST_GAP));
        });
    }

    /// §7.3: the state cell is the row's far end, where the eye compares rows. The Stop a
    /// working lane's row offers is pinned over that cell rather than laid out beside it,
    /// so a lane that can be stopped holds its clock in the same place as a lane that
    /// cannot — whether the pointer is on the row or not.
    #[gpui_kit::test]
    fn the_stop_covers_the_state_cell_without_moving_it(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Working, Some("two")),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let row = window.find(row_id(AgentKey::Lane(1))).bounds();
            let state = window.find(state_id(AgentKey::Lane(1))).bounds();
            assert_eq!(
                state.right(),
                row.right() - px(LANE_PAD),
                "the clock is at the row's far end"
            );

            // An idle lane's state is in the same place — one column, one edge.
            let idle = window.find(state_id(AgentKey::Lane(2))).bounds();
            assert_eq!(idle.right(), state.right());

            // The Stop is over the cell, on the row's own fill, and its own right edge is
            // the row's padding inset too.
            let stop = window.find(stop_id(1)).bounds();
            assert_eq!(stop.right(), row.right() - px(LANE_PAD));
            assert_eq!(stop.size.width, px(STOP_WIDTH));
            assert!(
                stop.left() <= state.right() && stop.right() >= state.left(),
                "the Stop replaces the clock: {stop:?} over {state:?}"
            );
            // The task cell kept the room for the control (`flex:1` with the Stop's
            // width as trailing padding), so the text it elides ends clear of the Stop's
            // box — the cell's own box is the slack and does not move, which is why the
            // ink is what that claim is checked against (the `agent_list_states`
            // capture, and the probe's crop of a working lane's row).
            assert!(
                (stop.center().y - row.center().y).abs() <= px(1.),
                "the Stop is centred on the row: {stop:?} in {row:?}"
            );

            // The pointer on the row brings it out, and moves nothing.
            window.simulate_mouse_move(row.center(), cx);
            window.render_frame(cx);
            assert!(window.find(stop_id(1)).visible());
            assert_eq!(
                window.find(state_id(AgentKey::Lane(1))).bounds(),
                state,
                "hovering the row moves neither the state nor the Stop"
            );
            assert_eq!(window.find(stop_id(1)).bounds(), stop);
        });
    }

    /// §7.3: the dot breathes against the fill the row has under it — the design's
    /// `--row-surface`, which the pointer and the selection each step up. The three
    /// values are the design's own: `var(--sidebar)`, `color-mix(fg 5%, sidebar)` and
    /// `color-mix(fg 9%, sidebar)`, with `.selected` winning over `:hover` because its
    /// rule comes later in the sheet.
    #[gpui_kit::test]
    fn the_dot_breathes_against_the_fill_the_row_has_under_it(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Working, Some("two")),
            ]),
        );
        let palette = design::palette(false);
        let rest = palette.sidebar;
        let hovered = paint::mix(palette.fg, HOVER_MIX, palette.sidebar);
        let selected = paint::mix(palette.fg, SELECTED_MIX, palette.sidebar);
        let surface = |f: &Fixture, cx: &App, key: AgentKey| {
            let list = f.list.read(cx);
            let row = list
                .lanes()
                .lanes
                .iter()
                .find(|row| AgentKey::Lane(row.n) == key)
                .expect("the lane is in the list");
            list.lane_view(row, palette).dot_surface
        };

        // Nothing under the pointer, nothing selected: the sidebar.
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(surface(&f, cx, AgentKey::Lane(1)), rest);
        });

        // The pointer on lane 1's row: that row's fill, and only that one.
        f.act(cx, |window, cx| {
            let row = window.find(row_id(AgentKey::Lane(1))).bounds();
            window.simulate_mouse_move(row.center(), cx);
            window.render_frame(cx);
            assert_eq!(
                f.list.read(cx).hovered,
                Some(AgentKey::Lane(1)),
                "the row the pointer is on"
            );
            assert_eq!(surface(&f, cx, AgentKey::Lane(1)), hovered);
            assert_eq!(
                surface(&f, cx, AgentKey::Lane(2)),
                rest,
                "a row the pointer is not on keeps the sidebar"
            );
        });

        // The pointer on the selected row: the selection's own step, not the pointer's.
        f.act(cx, |_, cx| {
            f.list
                .update(cx, |list, cx| list.set_selected(AgentKey::Lane(1), cx))
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                surface(&f, cx, AgentKey::Lane(1)),
                selected,
                "`:selected` wins over `:hover`, as it does in the sheet"
            );
        });

        // The pointer away: back to the resting fill, whatever is selected.
        f.act(cx, |window, cx| {
            window.simulate_mouse_move(point(COLUMN_WIDTH / 2., px(HEADER_HEIGHT / 2.)), cx);
            window.render_frame(cx);
            assert_eq!(f.list.read(cx).hovered, None);
            assert_eq!(surface(&f, cx, AgentKey::Lane(1)), selected);
        });
    }

    /// §7.3: the pointer crossing from one row to the next leaves the list on the row it
    /// arrived at. Up the list the arriving row is told before the one it left, so the
    /// row being left must not clear the row the pointer is on.
    #[gpui_kit::test]
    fn the_pointer_crossing_rows_leaves_the_list_on_the_row_it_arrived_at(cx: &mut TestAppContext) {
        let f = open(
            cx,
            lanes(vec![
                lane(1, LaneStatus::Working, Some("one")),
                lane(2, LaneStatus::Working, Some("two")),
            ]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let lane1 = window.find(row_id(AgentKey::Lane(1))).bounds().center();
            let lane2 = window.find(row_id(AgentKey::Lane(2))).bounds().center();
            let away = point(COLUMN_WIDTH / 2., px(HEADER_HEIGHT / 2.));

            // Down the list, then back up it: whichever row is told first, the list is
            // left on the row the pointer is on.
            window.simulate_mouse_move(lane1, cx);
            window.render_frame(cx);
            assert_eq!(f.list.read(cx).hovered, Some(AgentKey::Lane(1)));

            window.simulate_mouse_move(lane2, cx);
            window.render_frame(cx);
            assert_eq!(f.list.read(cx).hovered, Some(AgentKey::Lane(2)));

            window.simulate_mouse_move(lane1, cx);
            window.render_frame(cx);
            assert_eq!(
                f.list.read(cx).hovered,
                Some(AgentKey::Lane(1)),
                "the row the pointer left does not clear the row it arrived at"
            );

            // Off the rows: the row the pointer was on resets.
            window.simulate_mouse_move(away, cx);
            window.render_frame(cx);
            assert_eq!(f.list.read(cx).hovered, None);
        });
    }

    /// §7.3's keyboard: the column is the tab stop and wears the focus ring while the
    /// keyboard is on it. The ring is painted over the column — the ring is the column's
    /// outermost pixel, and taking focus does not move a row or the band by one.
    #[gpui_kit::test]
    fn the_focus_ring_does_not_move_what_the_column_holds(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let before = window.find(row_id(AgentKey::Coordinator)).bounds();
            assert_eq!(before.origin.x, px(LIST_PAD));

            // A click takes the column, and the keyboard it then answers is what the
            // ring is for: `:focus-visible`, not every focus there is.
            window.click(row_id(AgentKey::Coordinator), cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("agent-column-list").focused(),
                Some(true),
                "a click takes the column"
            );
            window.press("down", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find(row_id(AgentKey::Coordinator)).bounds(),
                before,
                "the keyboard's ring moves nothing"
            );
        });
    }

    /// §7.3: a swarm with more lanes than the column has room for scrolls. The list is
    /// the kit's scroll area, so the wheel over it moves the rows — and it is that same
    /// area the thumb rides on (`agent_list_states --capture` takes the picture of it).
    #[gpui_kit::test]
    fn a_list_longer_than_the_column_scrolls(cx: &mut TestAppContext) {
        let rows: Vec<LaneRow> = (1..=30).map(|n| lane(n, LaneStatus::Idle, None)).collect();
        let f = open(cx, lanes(rows));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let content = window.find(rows_content_id()).bounds();
            assert!(
                content.size.height > COLUMN.height,
                "thirty rows are taller than the column: {content:?}"
            );
            let first = window.find(row_id(AgentKey::Lane(1))).bounds();

            // Somewhere the pointer can be: the content is taller than the window, so
            // its own centre is off it.
            window.dispatch_event(
                ScrollWheelEvent {
                    position: point(COLUMN_WIDTH / 2., px(200.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-90.))),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            let scrolled = window.find(row_id(AgentKey::Lane(1))).bounds();
            assert!(
                first.origin.y - scrolled.origin.y >= px(80.),
                "the wheel moved the rows up: {first:?} then {scrolled:?}"
            );
        });
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
            let list = window
                .find(rows_content_id())
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(list, LIST_LABEL);
            let row = window
                .find(row_id(AgentKey::Lane(2)))
                .label()
                .unwrap_or_default()
                .to_string();
            assert!(row.starts_with("Lane 2, idle"), "{row}");
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

    /// §7.2: the one-agent program. The column's first row is `Main` — there is nobody
    /// to coordinate — and the lane count is not drawn at all: a count of lanes a single
    /// agent does not have is not a thing to read. A swarm's column is unchanged: its
    /// coordinator, and its count of lanes.
    #[gpui_kit::test]
    fn a_single_agents_column_is_one_row_named_main(cx: &mut TestAppContext) {
        // A lane in the list to begin with, so the count and the lane row are both
        // there to be dropped.
        let f = open(
            cx,
            lanes(vec![lane(1, LaneStatus::Working, Some("the build"))]),
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(row_id(AgentKey::Coordinator)).label(),
                Some("Coordinator, idle"),
                "a swarm's first row is its coordinator"
            );
            assert!(
                window.find(SUMMARY_ID).visible(),
                "and the column counts its lanes"
            );

            f.list.update(cx, |list, cx| {
                list.set_swarm(false, cx);
                list.set_coordinator(Status::Running, false, cx);
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(row_id(AgentKey::Coordinator)).label(),
                Some("Main, running"),
                "one agent's first row is Main"
            );
            assert!(
                window.try_find(SUMMARY_ID).is_none(),
                "and there are no lanes to count"
            );
            assert!(
                window.try_find(row_id(AgentKey::Lane(1))).is_none(),
                "and no lane rows: whatever a swarm's snapshot left in the list, a \
                 single agent's column is one row"
            );
            // The row is still the list's own: it is the one a click selects.
            assert!(window.find(row_id(AgentKey::Coordinator)).visible());
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
                    Some("model opus-4.5 is not registered under provider :acme".to_string()),
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
            assert_eq!(main, "Coordinator, idle");
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
            assert_eq!(main, "Coordinator, running");
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
            assert_eq!(main, "Coordinator, running, step 41s");

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
            assert_eq!(main, "Coordinator, running, step 0s");
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
        assert!(flat.starts_with("Lane 2  working · step 1m"), "{tooltip}");
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
