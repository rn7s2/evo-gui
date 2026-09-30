//! agent_list_states — the left column's own states, as pictures.
//!
//! The screens in `docs/screens.md` are the app's states at the app's window size: six
//! lanes fit the column with room to spare, so a picture of them never shows the list's
//! scrollbar, and nothing in them is under the pointer. This takes the states that need
//! a pointer, a wheel or an overflow — the thumb, the Stop a working lane's row offers
//! while the pointer is on it, and the ring the keyboard puts around the column — from
//! the list the app builds, in both themes.
//!
//! ```sh
//! cargo run -p agent_list --example agent_list_states -- --capture /tmp/agent-list
//! ```
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

fn lanes() -> LaneList {
    let rows: Vec<LaneRow> = (1..=LANES).map(lane_row).collect();
    let busy = rows.iter().filter(|row| row.is_busy()).count() as u64;
    LaneList {
        swarm: Some(SwarmInfo {
            id: "sw-1a2b3c4d".to_string(),
            workers: u64::from(LANES),
            busy,
            waiting_on_lanes: busy > 0,
            lane_model: Some("stub-a (stub)".to_string()),
            lane_thinking: Some("medium".to_string()),
        }),
        lanes: rows,
    }
}

/// The column, fed the way the app feeds it.
struct Column {
    list: Entity<AgentList>,
}

impl Column {
    fn new(cx: &mut Context<Self>) -> Self {
        let list = cx.new(AgentList::new);
        let now = now_millis();
        list.update(cx, |list, cx| {
            list.set_lanes(&lanes(), cx);
            list.set_now(now, cx);
            list.set_coordinator(Status::Running, false, cx);
            list.set_coordinator_clock(Some("12s".to_string()), cx);
            list.set_selected(AgentKey::Lane(1), cx);
            list.set_down_reason(3, Some("crashed — its process exited".to_string()), cx);
        });
        // The owner's half of the contract, so a click and a Stop name what they are.
        cx.subscribe(&list, |_, _, event: &AgentListEvent, _| match *event {
            AgentListEvent::Select(key) => println!("[states] select {key:?}"),
            AgentListEvent::StopLane(lane) => {
                println!("[states] run.interrupt scope=lane lane={lane}")
            }
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

fn open(cx: &mut HeadlessAppContext) -> AnyWindowHandle {
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
            |_window, cx| cx.new(Column::new),
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

/// The middle of lane N's row: the band is 38px, the list insets its rows by 6, and a
/// row is 32px with 2px between them.
fn row_center(n: u32) -> (f32, f32) {
    (WINDOW_WIDTH / 2., 38. + 6. + n as f32 * 34. + 16.)
}

fn main() {
    let dir: PathBuf = std::env::args()
        .skip_while(|arg| arg != "--capture")
        .nth(1)
        .unwrap_or_else(|| "/tmp/agent-list".to_string())
        .into();
    std::fs::create_dir_all(&dir).expect("the capture directory");

    let states: [&str; 6] = [
        "rest",
        "pointer",
        "working-pointer",
        "scrollbar-hover",
        "scrolled",
        "keyboard",
    ];

    let mut cx = context();
    for name in states {
        let window = open(&mut cx);
        match name {
            // On a lane's row, which is what the pointer does before it can do anything.
            "pointer" => point_at(&mut cx, window, row_center(5)),
            // On the row of the lane that is working: its Stop comes out.
            "working-pointer" => point_at(&mut cx, window, row_center(1)),
            // On the scroll area's own edge, which is where a scrollbar's own hover is.
            "scrollbar-hover" => point_at(&mut cx, window, (WINDOW_WIDTH - 3., 200.)),
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
            // press on a row does), and then a key. It is Tab because that is the one
            // the test window's `press` sends down the platform path that marks the
            // input as the keyboard's — which is what `:focus-visible` asks for, and
            // what a point alone is not.
            "keyboard" => cx
                .update_window(window, |_, window, cx| {
                    window.render_frame(cx);
                    window.click(agent_list::row_id(AgentKey::Coordinator), cx);
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
            let image = cx.capture_screenshot(window).expect("a frame to capture");
            let path = dir.join(format!("{name}-{suffix}.png"));
            image.save(&path).expect("write the picture");
            println!("[states] {name}: {}", path.display());
        }
    }
    let _ = COLUMN_WIDTH;
}
