//! composer_states — the box's own open states, as pictures.
//!
//! The screens in `docs/screens.md` are the app's states: a tab, a lane at work,
//! a report arriving. They never fold out what the box holds of its own, which is
//! where the design puts the goal's objective, the todo list and the model drawer
//! — so this takes those pictures, from the composer the app builds, in both themes.
//!
//! ```sh
//! cargo run -p composer --example composer_states -- --capture /tmp/composer
//! ```
//!
//! Nothing here pokes a widget's fields: the states are the topic's own
//! (`set_agent` / `set_catalog` / `set_swarm_busy`) and the fold-outs are opened
//! by clicking the same rows and chips a person clicks.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, px, size, AnyWindowHandle, AppContext as _, Bounds, Context, Entity, HeadlessAppContext,
    IntoElement, ParentElement as _, Render, Styled as _, Window, WindowBounds, WindowOptions,
};
use session::TopicState;

use composer::{Composer, ModelRow};

/// The window the pictures are taken in: one conversation column, the width the app
/// gives it, so the box is docked on the reading measure it is drawn on.
const WINDOW_SIZE: (f32, f32) = (1000., 720.);

/// The topic's own state, as a server would publish it (`GET /snapshot`), run on
/// one of the catalog's registrations.
fn state(busy: bool, model: &str) -> TopicState {
    TopicState::from_json(&serde_json::json!({
        "status": if busy { "running" } else { "idle" },
        "model": {"id": model, "provider": "openai", "ready": true},
        "thinking": "high",
        "context": {"tokens": 48000, "window": 936000, "source": "usage"},
        "goal": {"goal_id": "a1b2c3d4", "objective": "Ship the redesign: every screen taken \
                  from the design, every state bound to the real view model, and the gate \
                  green.", "status": "active", "budget": 50000, "tokens": 12000},
        "todos": [
            {"text": "port the view model", "status": "done"},
            {"text": "trim the workspace", "status": "in_progress"},
            {"text": "re-take the screens", "status": "pending"},
        ],
        "segments": [
            {"name": "model", "order": 100, "side": "left", "text": model, "data": {}},
            {"name": "thinking", "order": 200, "side": "left", "text": "high", "data": {}},
            {"name": "context", "order": 300, "side": "left",
             "text": "ctx 48k/936k (5%)", "data": {}},
            {"name": "cache_stats", "order": 350, "side": "left", "text": "97% cached",
             "data": {}},
            {"name": "goal", "order": 400, "side": "left",
             "text": "goal a1b2c3d4 (active) 12k/50k", "data": {}},
        ],
    }))
}

/// The effort ladder evo's own registration declares (`/catalog.thinking_levels`).
fn levels() -> Vec<String> {
    ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|level| level.to_string())
        .collect()
}

/// The models the drawer offers, as `GET /catalog` lists them (§5.6): one chosen,
/// one ready, one that cannot be used and says why.
fn models() -> Vec<ModelRow> {
    vec![
        ModelRow {
            id: "stub-a".to_string(),
            provider: "openai".to_string(),
            detail: "200k ctx · vision · effort low–max".to_string(),
            reason: None,
        },
        ModelRow {
            id: "stub-b".to_string(),
            provider: "proxy".to_string(),
            detail: "936k ctx · effort low–xhigh".to_string(),
            reason: None,
        },
        ModelRow {
            id: "stub-c".to_string(),
            provider: "acme".to_string(),
            detail: "1M ctx".to_string(),
            reason: Some("no credential".to_string()),
        },
    ]
}

/// A catalog long enough to need the drawer's region: ten registrations, so the
/// list scrolls inside its seven rows whatever the window's height. The state that
/// runs it is on `stub-j` — the last row, which is the one a region of seven rows
/// would not show without the reveal.
fn many_models() -> Vec<ModelRow> {
    (0..10u8)
        .map(|n| ModelRow {
            id: format!("stub-{}", char::from(b'a' + n)),
            provider: ["openai", "proxy", "acme"][n as usize % 3].to_string(),
            detail: format!("{}k ctx · effort low–max", (n as u32 + 1) * 100),
            reason: (n % 5 == 2).then(|| "no credential".to_string()),
        })
        .collect()
}

/// The column the box is docked at the foot of: what the app renders above it.
struct Page {
    composer: Entity<Composer>,
}

impl Page {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        composer.update(cx, |composer, cx| {
            composer.set_pane_height(px(WINDOW_SIZE.1 - 200.), cx);
            composer.set_agent(&state(false, "stub-a"), "Coordinator", true, cx);
            composer.set_catalog(levels(), models(), cx);
        });
        Self { composer }
    }
}

impl Render for Page {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .p(px(16.))
                            .text_color(cx.theme().muted_foreground)
                            .child("the transcript is above; the box is docked at its foot"),
                    )
                    .child(self.composer.clone()),
            )
    }
}

/// A headless app of its own: one window per state, so nothing a state folds open
/// is left open for the next picture and nothing has to be folded back.
fn context() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx
}

fn open(cx: &mut HeadlessAppContext) -> (AnyWindowHandle, Entity<Page>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    gpui_kit::point(px(0.), px(0.)),
                    size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Page::new(window, cx)),
        )
    })
    .expect("open the capture window")
}

fn composer_of(cx: &mut HeadlessAppContext, page: &Entity<Page>) -> Entity<Composer> {
    cx.update(|cx| page.read(cx).composer.clone())
}

fn click(cx: &mut HeadlessAppContext, window: AnyWindowHandle, id: &'static str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .unwrap();
}

fn main() {
    let dir: PathBuf = std::env::args()
        .skip_while(|arg| arg != "--capture")
        .nth(1)
        .unwrap_or_else(|| "/tmp/composer-states".to_string())
        .into();
    std::fs::create_dir_all(&dir).expect("the capture directory");

    // The states, in the order the design reads them: the goal's objective folded
    // out, then the todo list, then the model drawer — the catalog's own three
    // models, and a catalog long enough that the models scroll — then the one
    // button's other face, then a lane's own box, which changes nothing, and says so.
    type Setup = fn(&mut HeadlessAppContext, AnyWindowHandle, &Entity<Page>);
    let states: [(&str, Setup); 6] = [
        ("goal-open", |cx, window, _| {
            click(cx, window, "goal-strip-row")
        }),
        ("todos-open", |cx, window, _| {
            click(cx, window, "todo-strip-row")
        }),
        ("model-drawer", |cx, window, _| {
            click(cx, window, "composer-chip-model")
        }),
        ("model-drawer-many", |cx, window, page| {
            let composer = composer_of(cx, page);
            cx.update(|cx| {
                composer.update(cx, |composer, cx| {
                    // The last registration, so the drawer opens on the row that
                    // only the region's own scroll can show.
                    composer.set_agent(&state(false, "stub-j"), "Coordinator", true, cx);
                    composer.set_catalog(levels(), many_models(), cx);
                })
            });
            click(cx, window, "composer-chip-model");
        }),
        ("busy", |cx, _window, page| {
            let composer = composer_of(cx, page);
            cx.update(|cx| composer.update(cx, |composer, cx| composer.set_swarm_busy(true, cx)));
        }),
        ("lane-drawer", |cx, window, page| {
            let composer = composer_of(cx, page);
            cx.update(|cx| {
                composer.update(cx, |composer, cx| {
                    composer.set_agent(&state(false, "stub-a"), "lane 1", false, cx)
                })
            });
            click(cx, window, "composer-chip-model");
        }),
    ];

    let mut cx = context();
    for (name, setup) in states {
        let (window, page) = open(&mut cx);
        setup(&mut cx, window, &page);
        // Let anything the state set in motion — the chevron's own 120ms turn —
        // come to rest before the picture is taken.
        std::thread::sleep(Duration::from_millis(220));
        cx.run_until_parked();
        for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            cx.update_window(window, |_, window, cx| {
                gpui_kit::component::Theme::change(mode, Some(window), cx);
            })
            .unwrap();
            // The theme change is one frame, and the assets are decoded off the
            // render thread: two frames is what the app's own capture waits for.
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.render_frame(cx);
            })
            .unwrap();
            let image = cx.capture_screenshot(window).expect("a frame to capture");
            let path = dir.join(format!("{name}-{suffix}.png"));
            image.save(&path).expect("write the picture");
            println!(
                "[states] {}x{} -> {}",
                image.width(),
                image.height(),
                path.display()
            );
        }
    }
}
