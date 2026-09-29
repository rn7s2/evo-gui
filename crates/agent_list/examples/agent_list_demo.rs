//! The tab page's left column on its own: `main` plus six lanes covering every state the
//! swarm reports, with the tasks and the step clocks moving on a timer.
//!
//! ```sh
//! cargo run --example agent_list_demo                    # live window
//! cargo run --example agent_list_demo -- --capture <dir> # render frames to PNGs
//! ```
//!
//! Capture mode drives the same model through GPUI's headless renderer, so the pictures do
//! not depend on a window being on screen — which is what makes them reproducible on a
//! locked machine.

use std::path::Path;
use std::time::Duration;

use agent_list::{row_id, AgentList, AgentListEvent, COLUMN_WIDTH};
use gpui_kit::component::{h_flex, ActiveTheme as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InputEvent as _, IntoElement, MouseMoveEvent, ParentElement as _, Render,
    Styled as _, Subscription, Task, WeakEntity, Window, WindowBounds, WindowOptions,
};
use session::{short_duration, Activity, AgentKey, LaneList, LaneRow, LaneStatus, SwarmInfo};

/// The live window: the column, and room for the middle and right columns beside it.
const WINDOW_SIZE: (f32, f32) = (900., 420.);
/// A capture is tall enough for the header and every row, with the page's margin.
const CAPTURE_SIZE: (f32, f32) = (760., 400.);

/// How often the live demo moves on.
const TICK: Duration = Duration::from_millis(900);

/// How long to let the app clock settle before a capture, so a hover fill is at full
/// strength rather than mid-transition.
const SETTLE: Duration = Duration::from_millis(400);

const LANE_COUNT: u32 = 6;
/// The lane the demo has fail, so a down row and its reason are on screen.
const FAILING_LANE: u32 = 6;

/// The tasks the demo cycles through: one of them is far too long for the column, so the
/// ellipsis is on screen rather than assumed.
const TASKS: [&str; 3] = [
    "port the transcript reducer to the new event shape",
    "build the readout segments from /state, the registry and the cache seed",
    "check the Send/Stop button against the TUI's esc",
];

/// What the failing lane's transcript says, as §9.7 shows it in the lane's row.
const DOWN_REASON: &str =
    "model ark-opus-4.5 is not registered under provider :aiden — the lane failed to initialize";

/// The swarm the demo draws: the app's own folder, six lanes, and an id like the real one.
fn swarm(busy: u64) -> SwarmInfo {
    SwarmInfo {
        id: "sw-1a2b3c4d".to_string(),
        dir: "/Users/you/.evo/swarm/sw-1a2b3c4d".to_string(),
        cwd: "/Users/you/coding/evo-gui".to_string(),
        workers: u64::from(LANE_COUNT),
        busy,
        stopping: false,
    }
}

/// The first frame shows every state at once; later frames rotate them, the way a live
/// swarm moves, and the failing lane comes back as the supervisor restarts it.
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
    matches!(status, LaneStatus::Working | LaneStatus::Compacting)
}

fn lane_row(step: usize, lane: u32, status: LaneStatus) -> LaneRow {
    let busy = is_busy(status);
    LaneRow {
        n: u64::from(lane),
        status,
        state: match status {
            LaneStatus::Working => "working",
            LaneStatus::Compacting => "compacting",
            LaneStatus::Idle => "idle",
            LaneStatus::Starting => "starting",
            LaneStatus::Down => "down",
        }
        .to_string(),
        // A lane keeps the task it was last given, which is what `/lanes` still reports
        // while it is idle — only a lane that has never had one shows nothing.
        task: (status != LaneStatus::Starting && status != LaneStatus::Idle)
            .then(|| TASKS[((lane - 1) as usize + step) % TASKS.len()].to_string()),
        task_age: busy.then(|| 60 * 4 + u64::from(lane) * 37),
        // The clock only moves while the lane is working, which is what makes a slow step
        // visible.
        step_age: busy.then(|| u64::from(lane) * 41 + (step as u64) * 7),
        pid: Some(4020 + u64::from(lane)),
        worktree: lane
            .is_multiple_of(2)
            .then(|| format!("/Users/you/.evo/swarm/sw-1a2b3c4d/lane-{lane}")),
        branch: lane.is_multiple_of(2).then(|| format!("evo/lane-{lane}")),
        restarts: if lane == FAILING_LANE { 2 } else { 0 },
        reports: u64::from(lane) + 1,
        goal_status: Some(if lane == 2 { "active" } else { "complete" }.to_string()),
    }
}

/// The demo's script: what the swarm looks like at each step. Everything is a function of
/// the step, so a capture can ask for exactly the frame it wants and a live window moves
/// by advancing it.
struct Script {
    lanes: LaneList,
    activity: Activity,
    /// The coordinator's step clock, as the owner formats it from
    /// `TabModel::coordinator_step_started()` — the same `short_duration` a lane's
    /// `step_age` goes through, so the two kinds of row count in one voice.
    coordinator_clock: Option<String>,
    reconnecting: bool,
    selected: AgentKey,
    down_reason: Option<String>,
}

impl Script {
    fn at(step: usize) -> Script {
        let statuses: Vec<LaneStatus> =
            (1..=LANE_COUNT).map(|lane| status_at(step, lane)).collect();
        let busy = statuses.iter().filter(|status| is_busy(**status)).count() as u64;
        let lanes = (1..=LANE_COUNT)
            .map(|lane| lane_row(step, lane, statuses[(lane - 1) as usize]))
            .collect();
        Script {
            lanes: LaneList {
                swarm: Some(swarm(busy)),
                lanes,
            },
            // The coordinator's own row: idle before the first run, then a run, then a
            // compaction, like the TUI's status line. The clock runs with the step, and the
            // list keeps it out of the idle frames.
            activity: [
                Activity::Idle,
                Activity::Running,
                Activity::Running,
                Activity::Compacting,
            ][step % 4],
            coordinator_clock: Some(short_duration(12 + (step as u64) * 7)),
            // The stream drops for a while, so the badge and its effect on the row are
            // both on screen.
            reconnecting: matches!(step % 8, 3 | 4),
            // The selection moves between the coordinator and a lane, and both are
            // captured: the pill is the same either way.
            selected: if step.is_multiple_of(2) {
                AgentKey::Coordinator
            } else {
                AgentKey::Lane(2)
            },
            down_reason: (statuses[(FAILING_LANE - 1) as usize] == LaneStatus::Down)
                .then(|| DOWN_REASON.to_string()),
        }
    }
}

struct Demo {
    list: Entity<AgentList>,
    step: usize,
    /// The live ticker; kept so it is not cancelled.
    _timer: Option<Task<()>>,
    _subscription: Subscription,
}

impl Demo {
    fn new(cx: &mut Context<Self>, animate: bool) -> Self {
        let list = cx.new(AgentList::new);
        let subscription = cx.subscribe(&list, |this, _, event: &AgentListEvent, cx| {
            // The owner's half of the contract: a click becomes a selection, and the
            // highlight follows the model rather than the widget's own idea of it.
            let AgentListEvent::Select(key) = *event;
            this.list.update(cx, |list, cx| list.set_selected(key, cx));
            cx.notify();
        });
        let mut demo = Self {
            list,
            step: 0,
            _timer: None,
            _subscription: subscription,
        };
        demo.apply(cx);
        if animate {
            let timer = cx.spawn(async move |this: WeakEntity<Self>, cx| loop {
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
            });
            demo._timer = Some(timer);
        }
        demo
    }

    /// Push the step's script into the list, the way the owner pushes `/lanes`, `/state`
    /// and the down reasons.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let script = Script::at(self.step);
        self.list.update(cx, |list, cx| {
            list.set_lanes(&script.lanes, cx);
            list.set_coordinator(script.activity, script.reconnecting, cx);
            list.set_coordinator_clock(script.coordinator_clock.clone(), cx);
            list.set_selected(script.selected, cx);
            for lane in 1..=LANE_COUNT {
                // The reason is the failing lane's, and only while it is down.
                let reason = (lane == FAILING_LANE)
                    .then(|| script.down_reason.clone())
                    .flatten();
                list.set_down_reason(lane, reason, cx);
            }
        });
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .items_stretch()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                // The tab page's left column (§7.3).
                div()
                    .w(COLUMN_WIDTH)
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
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

fn window_options(window_size: (f32, f32)) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(80.), px(80.)),
            size(px(window_size.0), px(window_size.1)),
        ))),
        ..Default::default()
    }
}

fn run_window() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(window_options(WINDOW_SIZE), cx, |_window, cx| {
                cx.new(|cx| Demo::new(cx, true))
            })
            .expect("open the agent list window");
        });
}

/// Render the column to one PNG per interesting step.
fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        std::sync::Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, demo) = cx.update(|cx| {
        gpui_kit::open_window(window_options(CAPTURE_SIZE), cx, |_window, cx| {
            cx.new(|cx| Demo::new(cx, false))
        })
    })?;

    // Step 0: every state at once, the failing lane's reason, a long task ellipsized and
    // the coordinator's row selected.
    shot(&mut cx, window, dir, "01-all-states.png")?;

    // The selection follows the model, so asking the list for a lane highlights that lane.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 1;
        demo.apply(cx);
    });
    shot(&mut cx, window, dir, "02-lane-selected.png")?;

    // Step 3: the coordinator's stream is down, so the badge takes the tick's place and
    // the counter has moved on.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 3;
        demo.apply(cx);
    });
    shot(&mut cx, window, dir, "03-coordinator-reconnecting.png")?;

    // The coordinator's own step clock: `main` selected, running, its trailing cell counting
    // in the same words the lane rows below it use.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 2;
        demo.apply(cx);
    });
    shot(&mut cx, window, dir, "07-coordinator-step-clock.png")?;

    // A hovered row, so the hover fill is on screen too.
    hover(&mut cx, window, AgentKey::Lane(5))?;
    shot(&mut cx, window, dir, "04-hover.png")?;

    // The failing lane's tooltip: the full state, the untruncated task, the worktree, the
    // restarts, the pid — and the reason, on its own line.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 0;
        demo.apply(cx);
    });
    hover(&mut cx, window, AgentKey::Lane(6))?;
    shot(&mut cx, window, dir, "05-down-lane-tooltip.png")?;

    // The failing lane restarts: the same row turns from down to starting and the reason
    // goes away — the supervisor's own behaviour.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 4;
        demo.apply(cx);
    });
    shot(&mut cx, window, dir, "06-lane-restarted.png")?;

    // The focus ring (§7.3): the click that selects a row is also what hands the column the
    // keyboard, so the row under the pointer and the ring around the list are both on screen.
    demo.update(&mut cx, |demo, cx| {
        demo.step = 0;
        demo.apply(cx);
    });
    click(&mut cx, window, AgentKey::Lane(3))?;
    // The pointer leaves before the shot: the ring is the focus, not the hover.
    park_pointer(&mut cx, window)?;
    shot(&mut cx, window, dir, "08-list-focus.png")?;

    Ok(())
}

/// Click a row's centre, so the list takes the keyboard the same way a user's click does.
fn click(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    key: AgentKey,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.click(row_id(key), cx);
        window.render_frame(cx);
    })?;
    Ok(())
}

/// Park the pointer off the column, so no row is hovered and no tooltip is mid-show.
fn park_pointer(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let corner = cx
        .update_window(window, |_, window, _| window.bounds())?
        .bottom_right()
        - point(px(8.), px(8.));
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: corner,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    Ok(())
}

/// Move the pointer over a row's centre, so its hover style is what the next frame paints.
fn hover(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    key: AgentKey,
) -> Result<(), Box<dyn std::error::Error>> {
    let position = cx
        .update_window(window, |_, window, _| window.find(row_id(key)).bounds())?
        .center();
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    Ok(())
}

/// Let the app clock settle, draw a frame, and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Two advances: the first lets a hover fill settle, the second is longer than the
    // tooltip's 500 ms show delay, so a hovered row's tooltip is in the picture too.
    for _ in 0..2 {
        cx.advance_clock(SETTLE);
        cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    }
    let image = cx.capture_screenshot(window)?;
    let path = dir.join(name);
    image.save(&path)?;
    println!(
        "[capture] {}x{} -> {}",
        image.width(),
        image.height(),
        path.display()
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => run_window(),
        [flag, dir] if flag == "--capture" => {
            if let Err(error) = capture(Path::new(dir)) {
                eprintln!("capture failed: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: agent_list_demo [--capture <dir>]");
            std::process::exit(2);
        }
    }
}
