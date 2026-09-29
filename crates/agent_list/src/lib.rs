//! agent_list — the tab page's left column (§7.3): `main` first, then one row per lane,
//! with the status glyph, the current task, the step clock and the selection.
//!
//! The list is a view and nothing else: it holds the coordinator's [`LaneList`] plus the
//! few things only the UI knows (the activity, whether the coordinator's stream is
//! reconnecting, the lanes' down reasons and the selection). It never talks to the server —
//! the owner pushes updates in with [`AgentList::set_lanes`] / [`AgentList::set_coordinator`]
//! and gets an [`AgentListEvent::Select`] back when a row is clicked.
//!
//! ```text
//! AgentList::new(cx)                                  the entity
//! set_lanes(&LaneList, cx)                            GET /lanes + lane-state events
//! set_coordinator(activity, reconnecting, cx)         /state.status + the stream badge
//! set_coordinator_clock(Option<String>, cx)           TabModel::coordinator_step_started()
//! set_selected(AgentKey, cx)                          TabModel::selected
//! set_down_reason(lane, Option<String>, cx)           §9.7: why a lane is down
//! → AgentListEvent::Select(AgentKey)                  the owner calls TabModel::select
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui_kit::component::{h_flex, tooltip::Tooltip, v_flex, ActiveTheme as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, Context, ElementId, EventEmitter, FontFeatures, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window,
};
use session::{Activity, AgentKey, LaneList, LaneRow, LaneStatus};

/// The column's width in the tab page (§7.3). The owner sizes the column; this is the
/// number the design was drawn against, and what the demo uses.
pub const COLUMN_WIDTH: Pixels = px(260.);

/// One row is one line of 13 px text: compact enough that a whole swarm fits without
/// scrolling, tall enough to click (§7.3).
const ROW_HEIGHT: Pixels = px(28.);
const TEXT_SIZE: Pixels = px(13.);
const SMALL_TEXT_SIZE: Pixels = px(11.);

/// The glyph cell and the lead cell are fixed, so a task always starts at the same x
/// whatever the state of the row — and the step clock never gets pushed off the row.
const GLYPH_WIDTH: Pixels = px(14.);
const LEAD_WIDTH: Pixels = px(16.);

/// Rows are inset by this much; the owner aligns its title bar's own inset to it, so the
/// first row reads as the same column as the tab labels above it.
const ROW_INSET: Pixels = px(8.);

/// How wide a tooltip line is allowed to get, in characters: about one and a half columns,
/// which fits a worktree path beside its label and still keeps a long task or a long
/// failure reason inside the window.
const TOOLTIP_LINE: usize = 56;

/// The rows scroller, so a swarm with more lanes than fit can be scrolled.
const ROWS_ID: &str = "agent-list-rows";

/// The `6 lanes · 2 busy` summary above the rows.
const SUMMARY_ID: &str = "agent-list-summary";

/// What the list asks its owner to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentListEvent {
    /// Show this agent's transcript in the center column: the owner calls
    /// `TabModel::select` and feeds the result back with [`AgentList::set_selected`].
    Select(AgentKey),
}

/// The tab page's agent list.
pub struct AgentList {
    lanes: LaneList,
    activity: Activity,
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
}

impl EventEmitter<AgentListEvent> for AgentList {}

impl AgentList {
    /// The context is the entity-constructor shape; there is nothing to subscribe to,
    /// because every update arrives through the setters below.
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            lanes: LaneList::new(),
            activity: Activity::Idle,
            coordinator_clock: None,
            coordinator_reconnecting: false,
            selected: AgentKey::Coordinator,
            down_reasons: BTreeMap::new(),
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

    /// The coordinator's own row: its activity, and whether its stream is reconnecting.
    pub fn set_coordinator(
        &mut self,
        activity: Activity,
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

    /// The small muted summary above the rows: how many lanes, how many busy.
    fn header(&self, theme: &Theme) -> Option<impl IntoElement> {
        if self.lanes.swarm.is_none() && self.lanes.lanes.is_empty() {
            // Nothing is known yet: a tab that has not fetched `/lanes` shows no header
            // rather than a wrong `0 lanes`.
            return None;
        }
        let count = self.lanes.lanes.len();
        let text = format!(
            "{} {} · {} busy",
            count,
            if count == 1 { "lane" } else { "lanes" },
            self.lanes.busy()
        );
        Some(
            h_flex()
                .id(SUMMARY_ID)
                .test_support()
                .w_full()
                .flex_none()
                .px(ROW_INSET)
                .pb_1()
                .text_size(SMALL_TEXT_SIZE)
                .text_color(theme.muted_foreground)
                .aria_label(text.clone())
                .child(text),
        )
    }

    fn coordinator_view(&self, theme: &Theme) -> RowView {
        let status = activity_status(self.activity);
        let word = activity_word(self.activity);
        // The step clock takes the trailing cell a lane's own clock sits in, but only while
        // the coordinator is actually doing something: an idle `main` says "idle", not the
        // seconds since a run that already ended.
        let busy = matches!(self.activity, Activity::Running | Activity::Compacting);
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
        let aria_step = match &clock {
            Some(clock) => format!(", step {clock}"),
            None => String::new(),
        };
        RowView {
            key: AgentKey::Coordinator,
            glyph: status.glyph(),
            glyph_color: status_color(status, theme),
            // No lane number: `main` sits in the label column, so it lines up with the
            // tasks below it, and the clock (or the activity word) takes the trailing cell.
            lead: SharedString::default(),
            label: "main".into(),
            label_color: theme.foreground,
            trailing: Some(clock.unwrap_or_else(|| word.to_string()).into()),
            badge,
            tooltip: tooltip.into(),
            aria: format!("{} main, {word}{aria_step}", status.glyph()).into(),
        }
    }

    fn lane_view(&self, row: &LaneRow, theme: &Theme) -> RowView {
        let key = AgentKey::Lane(row.n as u32);
        let reason = (row.status == LaneStatus::Down)
            .then(|| self.down_reasons.get(&(row.n as u32)))
            .flatten();
        // A down lane says why instead of what it was doing; a lane that has never been
        // given anything says where it is in the swarm's own vocabulary.
        let (label, label_color) = match (reason, row.task_label()) {
            (Some(reason), _) => (reason.clone(), theme.danger),
            (None, Some(task)) => (task, theme.foreground),
            (None, None) => (row.state.clone(), theme.muted_foreground),
        };
        // The clock is what tells a slow step from a wedged lane, so it is only worth a
        // cell while the lane is actually working.
        let trailing = if row.is_busy() {
            row.step_clock()
        } else {
            None
        };
        let aria = format!(
            "{} lane {} {}, {}{}",
            row.glyph(),
            row.n,
            row.state,
            label,
            match &trailing {
                Some(clock) => format!(", step {clock}"),
                None => String::new(),
            }
        );
        RowView {
            key,
            glyph: row.glyph(),
            glyph_color: status_color(row.status, theme),
            lead: row.n.to_string().into(),
            label: label.into(),
            label_color,
            trailing: trailing.map(SharedString::from),
            badge: None,
            tooltip: lane_tooltip(row, reason).into(),
            aria: aria.into(),
        }
    }

    /// Draw one row. The whole row is the click target, so a click anywhere selects.
    fn row(&self, view: RowView, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.is_selected(view.key);
        let key = view.key;
        let (label_id, clock_id) = (label_id(key), clock_id(key));
        let tooltip = view.tooltip;
        let glyph = view.glyph.to_string();
        let trailing = view.trailing;
        let badge = view.badge;

        h_flex()
            .id(row_id(key))
            .h(ROW_HEIGHT)
            .w_full()
            .flex_none()
            .px(ROW_INSET)
            .gap_1()
            .items_center()
            .rounded(theme.radius)
            .text_size(TEXT_SIZE)
            .when(selected, |row| row.bg(theme.list_active))
            .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
            .child(
                // Fixed glyph cell: the icon column stays put while states change.
                h_flex()
                    .w(GLYPH_WIDTH)
                    .flex_none()
                    .justify_center()
                    .items_center()
                    .text_color(view.glyph_color)
                    .child(glyph),
            )
            .child(
                h_flex()
                    .w(LEAD_WIDTH)
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child(view.lead),
            )
            .child(
                div()
                    .id(label_id)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(view.label_color)
                    .child(view.label),
            )
            .when_some(trailing, |row, trailing| {
                row.child(
                    div()
                        .id(clock_id)
                        .test_support()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .font_features(tabular())
                        .child(trailing),
                )
            })
            .when_some(badge, |row, badge| {
                row.child(
                    div()
                        .id(badge_id(key))
                        .test_support()
                        .flex_none()
                        .px_1()
                        .rounded(theme.radius)
                        .bg(theme.warning.opacity(0.18))
                        .text_color(theme.warning)
                        .text_size(SMALL_TEXT_SIZE)
                        .child(badge),
                )
            })
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(AgentListEvent::Select(key))))
            .aria_label(view.aria)
            .aria_selected(selected)
            .tooltip(move |window, cx| {
                // A long task would otherwise make a tooltip wider than the window.
                Tooltip::new(tooltip.clone())
                    .max_w(px(360.))
                    .build(window, cx)
            })
            .test_support()
    }
}

impl Render for AgentList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let coordinator = self.coordinator_view(&theme);
        let lanes: Vec<RowView> = self
            .lanes
            .lanes
            .clone()
            .iter()
            .map(|row| self.lane_view(row, &theme))
            .collect();

        let mut rows = v_flex()
            .id(ROWS_ID)
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_0p5()
            .overflow_y_scroll();
        rows = rows.child(self.row(coordinator, cx));
        for lane in lanes {
            rows = rows.child(self.row(lane, cx));
        }

        v_flex()
            .w_full()
            .h_full()
            .py_1()
            .text_color(theme.foreground)
            .children(self.header(&theme))
            .child(rows)
    }
}

/// The element id of a row, so the owner (and the tests) can address one. Lane rows are
/// keyed by lane number, so a row never loses its identity when the list updates.
pub fn row_id(key: AgentKey) -> ElementId {
    ("agent-row", row_index(key)).into()
}

/// The id of a row's flexible middle, which is the part that ellipsizes.
fn label_id(key: AgentKey) -> ElementId {
    ("agent-task", row_index(key)).into()
}

/// The id of a row's step clock.
fn clock_id(key: AgentKey) -> ElementId {
    ("agent-clock", row_index(key)).into()
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

/// The coordinator's activity in the lane vocabulary, so one glyph and one color table
/// covers both kinds of row.
pub fn activity_status(activity: Activity) -> LaneStatus {
    match activity {
        Activity::Idle => LaneStatus::Idle,
        Activity::Running => LaneStatus::Working,
        Activity::Compacting => LaneStatus::Compacting,
    }
}

/// The activity as the coordinator's row spells it.
pub fn activity_word(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Running => "running",
        Activity::Compacting => "compacting",
    }
}

/// The status glyph's color (§7.3): work is the live accent, compaction a warning, idle
/// muted, a starting lane the faint outline of a row that is not up yet, and down the
/// danger red. The glyph shape carries the meaning; the color only speeds it up.
pub fn status_color(status: LaneStatus, theme: &Theme) -> Hsla {
    match status {
        LaneStatus::Working => theme.success,
        LaneStatus::Compacting => theme.warning,
        LaneStatus::Idle => theme.muted_foreground,
        // The dashed glyph is what says "not up yet"; the color stays legible (§7.3).
        LaneStatus::Starting => theme.muted_foreground.opacity(0.85),
        LaneStatus::Down => theme.danger,
    }
}

/// Tabular figures, so a step clock ticking from `9s` to `10s` does not shuffle the row.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_string(), 1)]))
}

/// One row's content, worked out from the model before any element is built: the shape of
/// a row is the same whatever it is showing.
struct RowView {
    key: AgentKey,
    glyph: char,
    glyph_color: Hsla,
    /// `main`, or the lane's number.
    lead: SharedString,
    /// The flexible one line: the task, why the lane is down, or its state.
    label: SharedString,
    label_color: Hsla,
    /// The trailing cell: a lane's step clock, or the coordinator's activity.
    trailing: Option<SharedString>,
    /// `reconnecting`, shown where the activity would go while the stream is down.
    badge: Option<SharedString>,
    tooltip: SharedString,
    aria: SharedString,
}

/// Everything the row has no room for: the full state, the untruncated task, the worktree,
/// the restarts and the pid — the swarm's own `lane-status-line` fields, in its order.
///
/// One field group per line, each folded at [`TOOLTIP_LINE`]: a tooltip that grew to the
/// width of a long task would be wider than the window it is drawn in.
fn lane_tooltip(row: &LaneRow, reason: Option<&String>) -> String {
    let mut head = format!("lane {} · {}", row.n, row.state);
    if let Some(clock) = row.step_clock() {
        head.push_str(&format!(" · step {clock}"));
    }
    let mut lines = vec![head];
    if let Some(task) = &row.task {
        lines.push(format!("task: {task}"));
    }
    let mut facts: Vec<String> = Vec::new();
    if let Some(worktree) = &row.worktree {
        let branch = match &row.branch {
            Some(branch) => format!(" ({branch})"),
            None => String::new(),
        };
        facts.push(format!("worktree {worktree}{branch}"));
    }
    if row.restarts > 0 {
        // The swarm's `lane-status-line` stays quiet about zero restarts; so does this.
        facts.push(format!(
            "{} {}",
            row.restarts,
            if row.restarts == 1 {
                "restart"
            } else {
                "restarts"
            }
        ));
    }
    if let Some(pid) = row.pid {
        facts.push(format!("pid {pid}"));
    }
    if let Some(goal) = &row.goal_status {
        facts.push(format!("goal {goal}"));
    }
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
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

    /// A lane as `/lanes` reports one, with only the fields a test cares about set.
    fn lane(n: u64, status: LaneStatus, task: Option<&str>) -> LaneRow {
        LaneRow {
            n,
            status,
            state: match status {
                LaneStatus::Working => "working",
                LaneStatus::Compacting => "compacting",
                LaneStatus::Idle => "idle",
                LaneStatus::Starting => "starting",
                LaneStatus::Down => "down",
            }
            .to_string(),
            task: task.map(str::to_string),
            task_age: None,
            // A working lane has a step clock; anything else has already stopped.
            step_age: matches!(status, LaneStatus::Working | LaneStatus::Compacting)
                .then(|| 41 * n),
            pid: Some(4000 + n),
            worktree: None,
            branch: None,
            restarts: 0,
            reports: 0,
            goal_status: None,
        }
    }

    fn lanes(rows: Vec<LaneRow>) -> LaneList {
        let busy = rows.iter().filter(|row| row.is_busy()).count() as u64;
        let workers = rows.len() as u64;
        LaneList {
            swarm: Some(SwarmInfo {
                id: "sw-test".to_string(),
                dir: "/tmp/sw-test".to_string(),
                cwd: "/tmp".to_string(),
                workers,
                busy,
                stopping: false,
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
            // Compact rows (§7.3), and every row the same height.
            assert!(
                main.size.height == ROW_HEIGHT,
                "row height {} is not {}",
                main.size.height,
                ROW_HEIGHT
            );
            assert!(one.size.height == two.size.height && two.size.height == three.size.height);
        });
    }

    #[gpui_kit::test]
    fn every_status_gets_its_glyph_and_its_word(cx: &mut TestAppContext) {
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
                let label = window
                    .find(row_id(AgentKey::Lane(n)))
                    .label()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    label.starts_with(status.glyph()),
                    "lane {n} should lead with {}: {label}",
                    status.glyph()
                );
                assert!(
                    label.contains(status_from_state_word(status)),
                    "lane {n} should say {}: {label}",
                    status_from_state_word(status)
                );
                assert!(
                    label.contains(&format!("lane {n}")),
                    "lane {n} should name itself: {label}"
                );
            }
        });
    }

    /// The word the aria label carries for each status, which is the swarm's own state
    /// vocabulary.
    fn status_from_state_word(status: LaneStatus) -> &'static str {
        match status {
            LaneStatus::Working => "working",
            LaneStatus::Compacting => "compacting",
            LaneStatus::Idle => "idle",
            LaneStatus::Starting => "starting",
            LaneStatus::Down => "down",
        }
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

    #[gpui_kit::test]
    fn a_long_task_ellipsizes_and_never_pushes_the_clock_out(cx: &mut TestAppContext) {
        let long = "port the transcript reducer to the new event shape and keep every row \
                    measured while it streams";
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Working, Some(long))]));
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            let row = window.find(row_id(AgentKey::Lane(1))).bounds();
            let label = window.find(label_id(AgentKey::Lane(1))).bounds();
            let clock_row = window.find(clock_id(AgentKey::Lane(1)));
            let clock = clock_row.bounds();

            assert!(
                row.size.width <= COLUMN_WIDTH && row.origin.x >= px(0.),
                "the row overflows the column: {row:?}"
            );
            assert!(
                label.size.height <= px(22.),
                "the task wrapped onto another line: {label:?}"
            );
            assert!(
                label.right() <= clock.origin.x,
                "the task runs into the clock: {label:?} vs {clock:?}"
            );
            assert!(
                clock.right() <= row.right(),
                "the clock left the row: {clock:?} vs {row:?}"
            );
            assert!(clock_row.visible());

            // A lane that is not working has no clock to show.
            assert!(window.try_find(clock_id(AgentKey::Lane(2))).is_none());
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
            // The update did land: lane 3 is working now, so it has a clock.
            assert!(window.try_find(clock_id(AgentKey::Lane(3))).is_some());
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
                Some("3 lanes · 2 busy"),
                "the summary should count the rows the list is drawing"
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
            assert_eq!(main, "○ main, idle");
            assert!(
                window.try_find(badge_id(AgentKey::Coordinator)).is_none(),
                "no badge while the stream is up"
            );

            f.list.update(cx, |list, cx| {
                list.set_coordinator(Activity::Running, true, cx)
            });
            window.render_frame(cx);
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "● main, running");
            assert!(window.find(badge_id(AgentKey::Coordinator)).visible());
            // The activity keeps its cell: the badge is added after it, not instead.
            assert!(window.try_find(clock_id(AgentKey::Coordinator)).is_some());
        });
    }

    /// The coordinator's row as the view model has it, written out.
    fn coordinator_trailing(f: &Fixture, cx: &App) -> Option<String> {
        f.list
            .read(cx)
            .coordinator_view(cx.theme())
            .trailing
            .map(|trailing| trailing.to_string())
    }

    /// The coordinator's step clock shares the trailing cell with the activity word: it is
    /// shown while the coordinator runs or compacts, and the word is what shows otherwise —
    /// so an idle `main` never claims a step. Lanes keep their own clock in that cell, and
    /// the owner formats both with the same words.
    #[gpui_kit::test]
    fn the_coordinator_clock_takes_the_trailing_cell_only_while_it_works(cx: &mut TestAppContext) {
        let f = open(cx, lanes(vec![lane(1, LaneStatus::Idle, None)]));
        f.act(cx, |window, cx| {
            // Idle: the word, even with a clock in hand.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Activity::Idle, false, cx);
                list.set_coordinator_clock(Some("41s".to_string()), cx);
            });
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("idle"));

            // Running with a clock: the clock, in the cell the lanes use, and the row's aria
            // says the step as a lane row's does.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Activity::Running, false, cx);
            });
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("41s"));
            window.render_frame(cx);
            assert!(window.find(clock_id(AgentKey::Coordinator)).visible());
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "● main, running, step 41s");

            // A compaction is work too.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Activity::Compacting, false, cx);
                list.set_coordinator_clock(Some("3m".to_string()), cx);
            });
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("3m"));

            // No clock (the step was never stamped, or the run just ended): the word is what
            // shows, and a blank string is no clock either.
            f.list.update(cx, |list, cx| list.set_coordinator_clock(None, cx));
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("compacting"));
            f.list.update(cx, |list, cx| {
                list.set_coordinator_clock(Some("   ".to_string()), cx)
            });
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("compacting"));

            // A step that just began is a real clock, not an empty cell.
            f.list.update(cx, |list, cx| {
                list.set_coordinator(Activity::Running, false, cx);
                list.set_coordinator_clock(Some("0s".to_string()), cx);
            });
            assert_eq!(coordinator_trailing(&f, cx).as_deref(), Some("0s"));
            window.render_frame(cx);
            let main = window
                .find(row_id(AgentKey::Coordinator))
                .label()
                .unwrap_or_default()
                .to_string();
            assert_eq!(main, "● main, running, step 0s");
        });
    }

    #[gpui_kit::test]
    fn the_status_colors_come_from_the_theme(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.read(|cx| {
            let theme = cx.theme();
            assert_eq!(status_color(LaneStatus::Working, theme), theme.success);
            assert_eq!(status_color(LaneStatus::Compacting, theme), theme.warning);
            assert_eq!(
                status_color(LaneStatus::Idle, theme),
                theme.muted_foreground
            );
            assert_eq!(status_color(LaneStatus::Down, theme), theme.danger);
            // A starting lane is the muted outline of a row that is not up yet.
            let starting = status_color(LaneStatus::Starting, theme);
            assert!(starting.a > 0.);
            assert!(starting.a < theme.muted_foreground.a);
            assert_ne!(starting, theme.muted_foreground);
            // The five states do not collapse into each other.
            let colors: Vec<Hsla> = [
                LaneStatus::Working,
                LaneStatus::Compacting,
                LaneStatus::Idle,
                LaneStatus::Starting,
                LaneStatus::Down,
            ]
            .iter()
            .map(|status| status_color(*status, theme))
            .collect();
            for (index, color) in colors.iter().enumerate() {
                assert!(!colors[..index].contains(color), "duplicate status color");
            }
        });
    }

    #[test]
    fn the_coordinator_activity_maps_onto_the_row_states() {
        assert_eq!(activity_status(Activity::Idle), LaneStatus::Idle);
        assert_eq!(activity_status(Activity::Running), LaneStatus::Working);
        assert_eq!(
            activity_status(Activity::Compacting),
            LaneStatus::Compacting
        );
        assert_eq!(activity_word(Activity::Running), "running");
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
        let tooltip = lane_tooltip(&row, None);
        // Folded for the tooltip's width, so the checks ignore where the line breaks land.
        let flat = tooltip.replace('\n', " ");
        assert!(flat.starts_with("lane 2 · working · step 1m"), "{tooltip}");
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
        let tooltip = lane_tooltip(&row, Some(&"crashed on startup".to_string()));
        assert!(tooltip.ends_with("crashed on startup"), "{tooltip}");
        assert!(!tooltip.contains("restart"), "{tooltip}");
    }
}
