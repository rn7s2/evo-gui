//! agent_list_demo — the left column, live, over a scripted swarm.
//!
//! Six lanes in every state the row has (working, compacting, idle, starting, down), a
//! lane whose task is far too long for the column, a lane selected, the coordinator's own
//! row with its step clock and the `reconnecting` badge, and the Stop button a busy lane
//! grows. Everything is built from the new view model's own shapes: a lane row's clock
//! counts from the absolute step start the swarm publishes (CONTRACT §4.3), and the row's
//! activity line comes from the lane's own mirror.
//!
//! ```sh
//! cargo run --example agent_list_demo
//! ```

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    div, px, size, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _,
    Render, Styled as _, Task, WeakEntity, Window, WindowBounds, WindowOptions,
};
use session::{AgentKey, LaneList, LaneRow, LaneStatus, Status, SwarmInfo};

use agent_list::{AgentList, AgentListEvent, COLUMN_WIDTH};

const WINDOW_SIZE: (f32, f32) = (1000., 520.);
const LANE_COUNT: u32 = 6;
/// How often the live demo moves on.
const TICK: Duration = Duration::from_millis(900);
/// The lane the demo has fail, so a down row and its reason are on screen.
const FAILING_LANE: u32 = 6;

/// The tasks the demo cycles through; one is far too long for the column, so the ellipsis
/// is on screen rather than assumed.
const TASKS: [&str; 3] = [
    "port the transcript reducer to the new event shape",
    "build the readout segments from the catalog and the topic state",
    "check the Stop swarm button against the swarm's own busy flag",
];

/// What the failing lane's newest lane event says, as the row's tooltip shows it.
const DOWN_REASON: &str = "crashed — its process exited";

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as u64
}

fn status_at(step: usize, lane: u32) -> LaneStatus {
    const FIRST: [LaneStatus; 5] = [
        LaneStatus::Working,
        LaneStatus::Working,
        LaneStatus::Compacting,
        LaneStatus::Idle,
        LaneStatus::Starting,
    ];
    const MOVING: [LaneStatus; 4] = [
        LaneStatus::Working,
        LaneStatus::Compacting,
        LaneStatus::Idle,
        LaneStatus::Starting,
    ];
    if lane == FAILING_LANE {
        return if step < 3 {
            LaneStatus::Down
        } else {
            LaneStatus::Starting
        };
    }
    if step == 0 {
        FIRST[(lane - 1) as usize]
    } else {
        MOVING[((lane - 1) as usize + step) % MOVING.len()]
    }
}

fn is_busy(status: LaneStatus) -> bool {
    status.is_busy()
}

fn lane_row(step: usize, lane: u32, status: LaneStatus) -> LaneRow {
    let busy = is_busy(status);
    let now = now_millis();
    // The clock counts on for as long as the lane works: the swarm publishes a *start*,
    // and the demo hands out one a lane's step old.
    let step_started_at =
        busy.then(|| now.saturating_sub((u64::from(lane) * 41 + step as u64 * 7) * 1000));
    LaneRow {
        n: lane,
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
        task: (status != LaneStatus::Starting && status != LaneStatus::Idle)
            .then(|| TASKS[((lane - 1) as usize + step) % TASKS.len()].to_string()),
        task_started_at: busy.then(|| now.saturating_sub(4 * 60 * 1000)),
        step_started_at,
        restarts: if lane == FAILING_LANE { 2 } else { 0 },
        pid: (status != LaneStatus::Down).then(|| 4020 + u64::from(lane)),
        worktree: lane
            .is_multiple_of(2)
            .then(|| format!("/Users/you/.evo/swarm/sw-1a2b3c4d/lane-{lane}")),
        branch: lane.is_multiple_of(2).then(|| format!("evo/lane-{lane}")),
        model: Some("stub-a (stub)".to_string()),
        goal_status: Some(if lane == 2 { "active" } else { "complete" }.to_string()),
        context_tokens: Some(u64::from(lane) * 21_000),
        context_window: Some(200_000),
        reports: u64::from(lane) + 1,
        last_item: Some((
            "assistant".to_string(),
            "Reading the crate now.".to_string(),
        )),
    }
}

/// One step's swarm: the lanes, the coordinator's status and whether its stream is down.
struct Script {
    lanes: LaneList,
    coordinator: Status,
    coordinator_clock: Option<String>,
    reconnecting: bool,
    selected: AgentKey,
    failing_is_down: bool,
}

impl Script {
    fn at(step: usize) -> Script {
        let statuses: Vec<LaneStatus> =
            (1..=LANE_COUNT).map(|lane| status_at(step, lane)).collect();
        let busy = statuses.iter().filter(|status| is_busy(**status)).count() as u64;
        Script {
            lanes: LaneList {
                swarm: Some(SwarmInfo {
                    id: "sw-1a2b3c4d".to_string(),
                    workers: u64::from(LANE_COUNT),
                    busy,
                    waiting_on_lanes: busy > 0,
                    lane_model: Some("stub-a (stub)".to_string()),
                    lane_thinking: Some("medium".to_string()),
                }),
                lanes: (1..=LANE_COUNT)
                    .map(|lane| lane_row(step, lane, statuses[(lane - 1) as usize]))
                    .collect(),
            },
            coordinator: [
                Status::Idle,
                Status::Running,
                Status::Waiting,
                Status::Compacting,
            ][step % 4],
            coordinator_clock: Some(session::short_duration(12 + (step as u64) * 7)),
            reconnecting: matches!(step % 8, 3 | 4),
            selected: if step.is_multiple_of(2) {
                AgentKey::Coordinator
            } else {
                AgentKey::Lane(2)
            },
            failing_is_down: statuses[(FAILING_LANE - 1) as usize] == LaneStatus::Down,
        }
    }
}

struct Demo {
    list: Entity<AgentList>,
    step: usize,
    _timer: Option<Task<()>>,
}

impl Demo {
    fn new(cx: &mut Context<Self>, animate: bool) -> Self {
        let list = cx.new(AgentList::new);
        cx.subscribe(&list, |this, _, event: &AgentListEvent, cx| {
            // The owner's half of the contract: a click becomes a selection, and the Stop
            // button becomes the one op a person may send about a lane.
            match *event {
                AgentListEvent::Select(key) => {
                    this.list.update(cx, |list, cx| list.set_selected(key, cx));
                }
                AgentListEvent::StopLane(lane) => {
                    println!("run.interrupt scope=lane lane={lane}");
                }
            }
            cx.notify();
        })
        .detach();

        let mut demo = Self {
            list,
            step: 0,
            _timer: None,
        };
        demo.apply(cx);
        if animate {
            demo._timer = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
                cx.background_executor().timer(TICK).await;
                if this
                    .update(cx, |demo, cx| {
                        demo.step += 1;
                        demo.apply(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }));
        }
        demo
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let script = Script::at(self.step);
        self.list.update(cx, |list, cx| {
            list.set_lanes(&script.lanes, cx);
            list.set_now(now_millis(), cx);
            list.set_coordinator(script.coordinator, script.reconnecting, cx);
            list.set_coordinator_clock(script.coordinator_clock.clone(), cx);
            list.set_selected(script.selected, cx);
            for lane in 1..=LANE_COUNT {
                let reason = (lane == FAILING_LANE && script.failing_is_down)
                    .then(|| DOWN_REASON.to_string());
                list.set_down_reason(lane, reason, cx);
            }
        });
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .w(COLUMN_WIDTH)
                    .h_full()
                    .border_color(cx.theme().border)
                    .border_r_1()
                    .child(self.list.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .text_color(cx.theme().muted_foreground)
                    .child("tab page: transcript | todos | composer"),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui_kit::point(px(80.), px(80.)),
                        size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                    ))),
                    ..Default::default()
                },
                cx,
                |_window, cx| cx.new(|cx| Demo::new(cx, true)),
            )
            .expect("open the agent list window");
        });
}
