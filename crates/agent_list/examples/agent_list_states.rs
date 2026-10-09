//! agent_list_states — the left column's own states, as pictures.
//!
//! The screens in `docs/screens.md` are the app's states at the app's window size: six
//! lanes fit the column with room to spare, so a picture of them never shows the list's
//! scrollbar, and nothing in them is under the pointer. This takes the states that need
//! a pointer, a wheel or an overflow — the thumb, the Stop a working lane's row offers
//! while the pointer is on it, the ring the keyboard puts around the column, and the
//! `+ Add New Lane` row the list ends with — at rest, under the pointer, under the
//! keyboard, and disabled — from the list the app builds, in both themes. It also draws
//! the one-agent column, where that row is absent.
//!
//! ```sh
//! cargo run -p agent_list --example agent_list_states -- --capture /tmp/agent-list
//! cargo run -p agent_list --example agent_list_states -- --capture /tmp/agent-list --burst 24
//! ```
//!
//! `--burst N` takes N frames of every state, 80ms apart, named `<state>-<i>-<theme>.png`:
//! the working dot breathes on a clock of its own, so the ends of the breath — the fill it
//! sits on, and the ink — need more than one frame to be seen.
//!
//! Nothing here pokes a widget's fields: the column is fed the topic's own shapes
//! (`LaneList` / `LaneRow`), and the pointer, the wheel and the keyboard are the paths a
//! person's own input takes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_list::{AgentList, AgentListEvent, COLUMN_WIDTH};
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::{ActiveTheme as _, Theme as ComponentTheme};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InputEvent as _, IntoElement, MouseMoveEvent, ParentElement as _, Render,
    ScrollDelta, ScrollWheelEvent, Styled as _, Window, WindowBounds, WindowOptions,
};
use session::{AgentKey, LaneList, LaneRow, LaneStatus, Status, SwarmInfo};

/// The window: one agent column (`COLUMN_WIDTH`, which is 260), and the height a
/// smaller window would give it. Six lanes fit it; the twelve below do not, which is
/// what the scrollbar is for.
const WINDOW_WIDTH: f32 = 260.;
const WINDOW_HEIGHT: f32 = 320.;

/// How many lanes the pictures show — twice what the app's own window fits.
const LANES: u32 = 12;

/// How many lanes the add row's own pictures show: few enough that the list's last row
/// is on screen without a wheel, which is where the pointer can reach it.
const SHORT_LANES: u32 = 3;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as u64
}

/// A lane as `GET /lanes` reports one. Lane 1 is working, 2 is compacting, 3 went down
/// saying why, 4 is starting, and the rest are idle.
fn lane_row(n: u32) -> LaneRow {
    let status = match n {
        1 => LaneStatus::Working,
        2 => LaneStatus::Compacting,
        3 => LaneStatus::Down,
        4 => LaneStatus::Starting,
        _ => LaneStatus::Idle,
    };
    let busy = status.is_busy();
    let now = now_millis();
    LaneRow {
        n,
        status,
        state: status.word().to_string(),
        task: busy.then(|| {
            "port the transcript reducer to the new event shape and keep every row measured"
                .to_string()
        }),
        task_started_at: busy.then(|| now - 4 * 60 * 1000),
        step_started_at: busy.then(|| now - u64::from(n) * 41 * 1000),
        restarts: u64::from(n == 3),
        pid: Some(4020 + u64::from(n)),
        worktree: Some(format!("/Users/you/.evo/swarm/sw-1a2b3c4d/lane-{n}")),
        branch: Some(format!("evo/lane-{n}")),
        model: Some("stub-a (stub)".to_string()),
        goal_status: Some("active".to_string()),
        context_tokens: Some(u64::from(n) * 21_000),
        context_window: Some(200_000),
        reports: u64::from(n),
        last_item: Some((
            "assistant".to_string(),
            "Reading the crate now.".to_string(),
        )),
    }
}

fn lanes(count: u32) -> LaneList {
    let rows: Vec<LaneRow> = (1..=count).map(lane_row).collect();
    let busy = rows.iter().filter(|row| row.is_busy()).count() as u64;
    LaneList {
        swarm: Some(SwarmInfo {
            id: "sw-1a2b3c4d".to_string(),
            workers: u64::from(count),
            busy,
            waiting_on_lanes: busy > 0,
            lane_model: Some("stub-a (stub)".to_string()),
            lane_thinking: Some("medium".to_string()),
        }),
        lanes: rows,
    }
}

/// Which column a state draws: the app's own swarm, a swarm short enough to show the
/// list's last row without a wheel, that same swarm with the add row disabled, or the
/// single-agent program's one row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Swarm,
    Short,
    ShortPending,
    Single,
}

/// The column, fed the way the app feeds it.
struct Column {
    list: Entity<AgentList>,
}

impl Column {
    fn new(cx: &mut Context<Self>, kind: Kind) -> Self {
        let list = cx.new(AgentList::new);
        let now = now_millis();
        let count = match kind {
            Kind::Swarm => LANES,
            Kind::Short | Kind::ShortPending => SHORT_LANES,
            // §7.2: one agent has no lanes at all, and so nothing to count or to add.
            Kind::Single => 0,
        };
        list.update(cx, |list, cx| {
            list.set_lanes(&lanes(count), cx);
            list.set_swarm(kind != Kind::Single, cx);
            list.set_add_lane_disabled(kind == Kind::ShortPending, cx);
            list.set_now(now, cx);
            list.set_coordinator(Status::Running, false, cx);
            list.set_coordinator_clock(Some("12s".to_string()), cx);
            list.set_selected(AgentKey::Lane(1), cx);
            list.set_down_reason(3, Some("crashed — its process exited".to_string()), cx);
        });
        // The owner's half of the contract, so a click, a Stop and the add row name what
        // they are.
        cx.subscribe(&list, |_, _, event: &AgentListEvent, _| match *event {
            AgentListEvent::Select(key) => println!("[states] select {key:?}"),
            AgentListEvent::StopLane(lane) => {
                println!("[states] run.interrupt scope=lane lane={lane}")
            }
            AgentListEvent::AddLane => println!("[states] command.run name=lanes"),
        })
        .detach();
        Self { list }
    }
}

impl Render for Column {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.list.clone())
    }
}

/// A headless app of its own: one window per state, so nothing a state moved is left
/// moved for the next picture.
fn context() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The app's own scrollbar behaviour (`crates/app/src/theme.rs`): a thin overlay thumb
    // while the pointer is on a scroll area and while one is scrolling. The colours that
    // app picks are its theme's; this example draws the kit's, because it is the list's
    // crate and cannot see the app's.
    cx.update(|cx| ComponentTheme::set_scrollbar_mode(ScrollbarMode::Hover, cx));
    cx
}

fn open(cx: &mut HeadlessAppContext, kind: Kind) -> AnyWindowHandle {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |_window, cx| cx.new(|cx| Column::new(cx, kind)),
        )
    })
    .expect("open the capture window")
    .0
}

/// Put the pointer somewhere, the way a person's does: twice, because it is the second
/// move that is routed to what the first one found under it.
fn point_at(cx: &mut HeadlessAppContext, window: AnyWindowHandle, at: (f32, f32)) {
    for _ in 0..2 {
        cx.update_window(window, |_, window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position: point(px(at.0), px(at.1)),
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
    }
}

/// The middle of the row at INDEX, where the band is 38px, the list insets its rows by 6,
/// and a row is 32px with 2px between them — `main` is 0 and lane N is N.
fn row_center_at(index: u32) -> (f32, f32) {
    (WINDOW_WIDTH / 2., 38. + 6. + index as f32 * 34. + 16.)
}

/// The middle of lane N's row.
fn row_center(n: u32) -> (f32, f32) {
    row_center_at(n)
}

/// The middle of the list's last row — the add row, which follows the lanes.
fn add_row_center() -> (f32, f32) {
    row_center_at(SHORT_LANES + 1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir: PathBuf = args
        .iter()
        .skip_while(|arg| arg.as_str() != "--capture")
        .nth(1)
        .map(String::as_str)
        .unwrap_or("/tmp/agent-list")
        .into();
    std::fs::create_dir_all(&dir).expect("the capture directory");
    let burst: usize = args
        .iter()
        .skip_while(|arg| arg.as_str() != "--burst")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
        .max(1);

    let states: [&str; 12] = [
        "rest",
        "pointer",
        "working-resting-pointer",
        "working-pointer",
        "scrollbar-hover",
        "scrolled",
        "keyboard",
        "add-lane",
        "add-lane-pointer",
        "add-lane-keyboard",
        "add-lane-disabled",
        // §7.2: one agent's column has one row — and none of these.
        "single",
    ];

    let mut cx = context();
    for name in states {
        // A column of its own per state: the add row's own pictures draw a swarm short
        // enough to reach its last row, and one picture is the single-agent column.
        let kind = match name {
            "add-lane" | "add-lane-pointer" | "add-lane-keyboard" => Kind::Short,
            "add-lane-disabled" => Kind::ShortPending,
            "single" => Kind::Single,
            _ => Kind::Swarm,
        };
        let window = open(&mut cx, kind);
        match name {
            // On a lane's row, which is what the pointer does before it can do anything.
            "pointer" => point_at(&mut cx, window, row_center(5)),
            // On the row of a working lane that is not the selected one: its dot breathes
            // against the fill the pointer put under it.
            "working-resting-pointer" => point_at(&mut cx, window, row_center(2)),
            // On the row of the lane that is working and selected: the dot breathes
            // against the selected fill.
            "working-pointer" => point_at(&mut cx, window, row_center(1)),
            // On the scroll area's own edge, which is where a scrollbar's own hover is.
            "scrollbar-hover" => point_at(&mut cx, window, (WINDOW_WIDTH - 3., 200.)),
            // The add row under the pointer: the one hover the new row has.
            "add-lane-pointer" => point_at(&mut cx, window, add_row_center()),
            // A wheel over the list: the rows move, and the thumb is out while they do.
            "scrolled" => {
                cx.update_window(window, |_, window, cx| {
                    let at = point(px(WINDOW_WIDTH / 2.), px(200.));
                    window.dispatch_event(
                        ScrollWheelEvent {
                            position: at,
                            delta: ScrollDelta::Pixels(point(px(0.), px(-90.))),
                            ..Default::default()
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                })
                .unwrap();
                point_at(&mut cx, window, (WINDOW_WIDTH / 2., 200.));
            }
            // The keyboard takes the column: a press on a row takes it first (as any
            // press on a row does), and then a key. Tab is what marks the input as the
            // keyboard's — which is what `:focus-visible` asks for, and what a point
            // alone is not — but the list's last row is a tab stop of it now, so Tab
            // walks out to that row (`add-lane-keyboard` below). The arrow it uses is the
            // one that moves nothing here: the row above the selection, which this
            // example's owner reads and does not confirm.
            "keyboard" => cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.click(agent_list::row_id(AgentKey::Coordinator), cx);
                    window.press("up", cx);
                    window.render_frame(cx);
                })
                .unwrap(),
            // The same keys, stopping one tab stop earlier: the add row wearing the
            // keyboard's own fill.
            "add-lane-keyboard" => cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.click(agent_list::row_id(AgentKey::Lane(1)), cx);
                    window.press("tab", cx);
                    window.render_frame(cx);
                })
                .unwrap(),
            _ => {}
        }
        // Let anything the state set in motion come to rest before the picture.
        std::thread::sleep(Duration::from_millis(260));
        cx.run_until_parked();
        for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            cx.update_window(window, |_, window, cx| {
                gpui_kit::component::Theme::change(mode, Some(window), cx);
            })
            .unwrap();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
            for frame in 0..burst {
                // The dot's clock is real time: a moment of it has to pass between two
                // pictures of it.
                if frame > 0 {
                    std::thread::sleep(Duration::from_millis(80));
                }
                cx.update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                })
                .unwrap();
                let image = cx.capture_screenshot(window).expect("a frame to capture");
                let path = if burst == 1 {
                    dir.join(format!("{name}-{suffix}.png"))
                } else {
                    dir.join(format!("{name}-{frame:02}-{suffix}.png"))
                };
                image.save(&path).expect("write the picture");
                if burst == 1 {
                    println!("[states] {name}: {}", path.display());
                }
            }
            if burst > 1 {
                println!(
                    "[states] {name}-{suffix}: {burst} frames in {}",
                    dir.display()
                );
            }
        }
    }
    let _ = COLUMN_WIDTH;
}
