//! The transcript's code blocks under the app's own theme, as pictures.
//!
//! ```sh
//! cargo run -p evo-desktop --example code_snapshot -- --capture /tmp/code-snapshot
//! ```
//!
//! A fenced block's *tokens* are the one ink the app's palette does not carry: they
//! come from the theme's highlight palette, and a theme file that says nothing about
//! code leaves the kit's highlight theme where it was — its light one, in either mode.
//! That is what dark mode drew with: `#333333` keys on the `#262626` block (1.2:1, a
//! grey smudge), `#036A07` strings, `#0433FF` numbers.
//!
//! The record below is one answer per language — JSON, Rust, Lisp and a shell session
//! — each with the tokens a reader reads code by: keys and properties, strings,
//! numbers, booleans, keywords, comments. Every picture is the app's own window: the
//! `Shell`, the design's two palettes, and `follow_appearance`, the call the real
//! window makes.
//!
//! Three pictures come out of *one* window, each after one `Theme::change`: `-light`,
//! then `-dark` — the switch a person makes when the machine's appearance changes —
//! and `-light-again`. So what they show is a live switch, and that switching back
//! re-inks what was already drawn, rather than two windows opened apart.

use std::sync::Arc;

use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, AnyView, AnyWindowHandle, AppContext as _, Bounds, Context, Entity,
    HeadlessAppContext, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Window, WindowBounds, WindowOptions,
};
use serde_json::json;
use session::Item;
use transcript::TranscriptView;

use evo_desktop::{AppLog, Shell};
use store::app_state::{AppState, Theme};
use store::model_cache::ModelCache;
use store::paths::Root;

/// The window the pictures are taken in: the conversation column, tall enough that
/// all four answers are on screen at once.
const WINDOW_SIZE: (f32, f32) = (900., 1020.);

/// One answer: what the reader said, and the four languages.
fn answers() -> [(&'static str, &'static str); 4] {
    [
        (
            "The endpoint as the catalog publishes it — a key, a string, a number, a \
             boolean and a null, which is where the light theme's greys disappeared:",
            r#"{
  "id": "demo-all",
  "provider": "demo",
  "api": "anthropic-messages",
  "context_window": 200000,
  "reasoning": true,
  "effort_levels": ["low", "medium", "high", "xhigh", "max"],
  "images": true,
  "ready": true,
  "reason": null
}"#,
        ),
        (
            "The row the pane builds, with the one comment a reader needs:",
            r#"/// A row's own words, in the measure the column gives it.
fn row(&self, id: &str) -> Option<Row> {
    let index = self.rows.get(id)?; // measured once, kept across a splice
    Some(Row::new(index, 1024, 0.75_f32))
}"#,
        ),
        (
            "And the same state from a shell — a comment, a variable, a command:",
            r#"# the reader's own place, in both themes
cargo run -p evo-desktop --example code_snapshot -- \
  --capture /tmp/code-snapshot
echo "done: $?""#,
        ),
        (
            "An extension's command, in the language the config files are written in:",
            r#"(evo:register-command "notice-probe"
  (lambda (ctx)
    (let ((host (getf ctx :host)))
      (evo.command:host-notice host "a warning" :severity :warn)
      nil)))"#,
        ),
    ]
}

/// The record: one turn, then one answer per language, each in a fence.
fn record() -> Vec<Item> {
    let mut values = vec![json!({
        "id": "e_turn", "ts": 1, "kind": "user", "status": "sent",
        "text": "show me the palette in every language this window has to draw",
    })];
    for (n, (prose, code)) in answers().into_iter().enumerate() {
        let lang = match n {
            0 => "json",
            1 => "rust",
            2 => "bash",
            _ => "lisp",
        };
        values.push(json!({
            "id": format!("e_{n}"), "ts": 1, "kind": "assistant", "status": "final",
            "model": "stub-a", "provider": "stub",
            "usage": { "input": 1200, "output": 300, "cache_read": 4000, "cache_write": 0 },
            "text": format!("{prose}\n\n```{lang}\n{code}\n```\n"),
        }));
    }
    values
        .into_iter()
        .map(|value| Item::from_json(&value).expect("a fixture item has an id"))
        .collect()
}

/// The app's own embedding of the transcript: one cached view in a `flex_1` box.
struct Page {
    transcript: Entity<TranscriptView>,
}

impl Page {
    fn new(items: &[Item], cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(TranscriptView::new);
        transcript.update(cx, |view, cx| view.replace(items.to_vec(), cx));
        Self { transcript }
    }
}

impl Render for Page {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("code-page")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .id("code-transcript-box")
                    .flex_1()
                    .min_h_0()
                    .children(Some(
                        AnyView::from(self.transcript.clone())
                            .cached(gpui_kit::StyleRefinement::default().size_full()),
                    )),
            )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = match (args.next().as_deref(), args.next()) {
        (Some("--capture"), Some(dir)) => std::path::PathBuf::from(dir),
        _ => {
            eprintln!("usage: code_snapshot --capture <dir>");
            std::process::exit(2);
        }
    };
    if let Err(error) = capture(&dir) {
        eprintln!("capture failed: {error}");
        std::process::exit(1);
    }
}

fn capture(dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let root =
        Root::at(std::env::temp_dir().join(format!("evo-desktop-code-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(root.path());

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);

    let (window, _page) = cx.update({
        let root = root.clone();
        move |cx| {
            let log = AppLog::open(&root);
            let state = AppState {
                theme: Theme::System,
                ..AppState::default()
            };
            Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    // The app's own light or dark, and the design's two palettes under
                    // it — the call the real window makes. The subscription is not
                    // kept: nothing here changes the system's appearance, and the
                    // switches below are asked for outright.
                    let _appearance = evo_desktop::follow_appearance(cx, window);
                    cx.new(|cx| Page::new(&record(), cx))
                },
            )
        }
    })?;
    let window: AnyWindowHandle = window;

    for (mode, name) in [
        (ThemeMode::Light, "code-light.png"),
        // The switch a person makes, on the window already drawn: if a block kept the
        // ink it was laid out with, this is the picture that shows it.
        (ThemeMode::Dark, "code-dark.png"),
        (ThemeMode::Light, "code-light-again.png"),
    ] {
        cx.update_window(window, |_, window, cx| {
            gpui_kit::component::Theme::change(mode, Some(window), cx);
        })?;
        settle(&mut cx, window);
        shot(&mut cx, window, dir, name)?;
    }
    let _ = std::fs::remove_dir_all(root.path());
    Ok(())
}

/// Let anything the switch set in motion come to rest before the picture is taken.
fn settle(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    for _ in 0..6 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| window.render_frame(cx));
    }
}

fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &std::path::Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let (theme, mode, inks) = cx.update_window(window, |_, _window, cx| {
        let theme = cx.theme().highlight_theme.clone();
        let ink = |token: &str| {
            theme
                .style(token)
                .and_then(|style| style.color)
                .map(|colour| {
                    let rgba = gpui_kit::Rgba::from(colour);
                    format!(
                        "#{:02X}{:02X}{:02X}",
                        (rgba.r * 255.).round() as u8,
                        (rgba.g * 255.).round() as u8,
                        (rgba.b * 255.).round() as u8
                    )
                })
                .unwrap_or_else(|| "-".into())
        };
        (
            theme.name.clone(),
            theme.appearance,
            [
                ("key", "property"),
                ("string", "string"),
                ("number", "number"),
            ]
            .map(|(label, token)| format!("{label} {}", ink(token))),
        )
    })?;
    let image = cx.capture_screenshot(window)?;
    let path = dir.join(name);
    image.save(&path)?;
    println!(
        "[code] {name}: {mode:?} highlight theme {theme:?}, {} -> {}",
        inks.join(", "),
        path.display()
    );
    Ok(())
}
