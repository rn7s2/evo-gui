//! composer_states — the box's own open states, as pictures.
//!
//! The screens in `docs/screens.md` are the app's states: a tab, a lane at work,
//! a report arriving. They never fold out what the box holds of its own, which is
//! where the design puts the goal's objective, the todo list, the model drawer and
//! the completion popup — so this takes those pictures, from the composer the app
//! builds, in both themes.
//!
//! ```sh
//! cargo run -p composer --example composer_states -- --capture /tmp/composer
//! ```
//!
//! Nothing here pokes a widget's fields: the states are the topic's own
//! (`set_agent` / `set_catalog` / `set_swarm_busy`), the fold-outs are opened by
//! clicking the same rows and chips a person clicks, and a word is completed by
//! putting the caret in the input and typing — the popup is raised the way a reader
//! raises it. The one thing a picture cannot do for itself is the image's answer
//! about a symbol, so the `/eval` state hands the popup the rows a real
//! `evo.eval:completions-for` returns for the token it is showing.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
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

use composer::{Answer, Candidate, CompletionKind, Composer, ComposerEvent, ModelRow};

/// The window the pictures are taken in: one conversation column, the width the app
/// gives it, so the box is docked on the reading measure it is drawn on.
const WINDOW_SIZE: (f32, f32) = (1000., 720.);

/// The topic's own state, as a server would publish it (`GET /snapshot`), run on
/// one of the catalog's registrations.
fn state(busy: bool, model: &str) -> TopicState {
    state_at(busy, model, "openai")
}

/// The same, on a registration another provider owns: `id@provider` is what a model is.
fn state_at(busy: bool, model: &str, provider: &str) -> TopicState {
    TopicState::from_json(&serde_json::json!({
        "status": if busy { "running" } else { "idle" },
        "model": {"id": model, "provider": provider, "ready": true},
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

/// The models the drawer offers, as `GET /catalog` lists them (§5.6): one chosen, one
/// ready, one that cannot be used and says why. Each carries the levels it takes — the
/// whole ladder, a provider's own three rungs, and none at all — which is what the
/// drawer's own ladder is drawn from.
fn models() -> Vec<ModelRow> {
    vec![
        ModelRow {
            id: "stub-a".to_string(),
            provider: "openai".to_string(),
            detail: "200k ctx · vision · effort low, medium, high, xhigh, max".to_string(),
            effort_levels: rungs(&["low", "medium", "high", "xhigh", "max"]),
            reason: None,
        },
        ModelRow {
            id: "stub-b".to_string(),
            provider: "proxy".to_string(),
            detail: "936k ctx · effort low, high, max".to_string(),
            effort_levels: rungs(&["low", "high", "max"]),
            reason: None,
        },
        ModelRow {
            id: "stub-c".to_string(),
            provider: "acme".to_string(),
            detail: "1M ctx".to_string(),
            effort_levels: Vec::new(),
            reason: Some("no credential".to_string()),
        },
    ]
}

fn rungs(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
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
            detail: format!(
                "{}k ctx · effort low, medium, high, xhigh, max",
                (n as u32 + 1) * 100
            ),
            effort_levels: rungs(&["low", "medium", "high", "xhigh", "max"]),
            reason: (n % 5 == 2).then(|| "no credential".to_string()),
        })
        .collect()
}

/// The commands the registry lists, as `GET /catalog` would: evo's own command
/// table, its own words, in its own order — what a `/word` completes against.
fn commands() -> Vec<Candidate> {
    [
        ("compact", "compact the context now"),
        ("eval", "evaluate one sexpr in the live image"),
        ("export", "export the transcript as markdown"),
        ("fork", "fork this session at the current leaf"),
        ("global-lore", "show lore, or add user-scope guidance"),
        (
            "global-memory",
            "show global user memory or ask the agent to refine it",
        ),
        ("goal", "show, create, refine, pause, or resume the goal"),
        ("lang", "language of the system prompt and replies"),
        ("lore", "show lore, or add project guidance"),
        (
            "memory",
            "show project memory or ask the agent to refine it",
        ),
        ("model", "pick the model from a list, or set it directly"),
        ("new", "start a new session in this folder"),
        (
            "reload",
            "re-evaluate init files, extensions and post-init files",
        ),
        ("thinking", "set the effort level of the next turn"),
    ]
    .iter()
    .map(|(name, description)| Candidate {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

/// What `evo.eval:completions-for` answers for `evo.eval:` — the package's own
/// exported names, which is what the image offers a qualified token, each with what
/// it is.
fn eval_package_symbols() -> Vec<Candidate> {
    [
        ("*eval-package-name*", "variable"),
        ("completions-for", "function"),
        ("eval-form", "function"),
        ("eval-package", "function"),
        ("single-form", "function"),
        ("symbol-kind", "function"),
        ("token-start", "function"),
    ]
    .iter()
    .map(|(name, description)| Candidate {
        name: format!("evo.eval:{name}"),
        description: description.to_string(),
    })
    .collect()
}

/// Put the caret in the box and type, as a reader does: the popup is raised by the
/// word being typed, not by a state being set.
fn type_into(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    text: &str,
) {
    let composer = composer_of(cx, page);
    cx.update_window(window, |_, window, cx| {
        composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        window.render_frame(cx);
        window.input(text, cx);
    })
    .unwrap();
}

/// Answer the box's own question the way a tab does, with what a server answers for a
/// `/command` word: the whitespace-delimited word the caret is in, when that word
/// starts with a slash, and the registry's commands as its candidates.
///
/// A real tab sends `complete` (§5.6) and hands the reply back; these pictures are of
/// the popup, so the reply is the one the picture is about — the catalog's own
/// commands, for the word being typed. A state whose answer is not a command word
/// answers with [`answer_with`] instead.
fn answer(cx: &mut HeadlessAppContext, page: &Entity<Page>) {
    // The debounce first: the box asks when the caret has rested on the word.
    cx.advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();
    let Some((text, cursor)) = page.read_with(cx, |page, _| page.question()) else {
        return;
    };
    let start = text[..cursor]
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1);
    let end = text[cursor..]
        .find(char::is_whitespace)
        .map_or(text.len(), |index| cursor + index);
    let is_command = text[start..end].starts_with('/');
    answer_with(
        cx,
        page,
        Answer {
            kind: is_command.then_some(CompletionKind::Command),
            name: if is_command {
                start + 1..end
            } else {
                start..end
            },
            items: commands(),
        },
    );
}

/// Hand the box the answer to the question it asked, as a tab hands it one.
fn answer_with(cx: &mut HeadlessAppContext, page: &Entity<Page>, answer: Answer) {
    // The caret rests first: the box asks on that rest, and a tab answers a question it
    // has been asked — the example is that tab, so it waits for the question too.
    cx.advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    let Some((text, cursor)) = page.read_with(cx, |page, _| page.question()) else {
        return;
    };
    let composer = composer_of(cx, page);
    cx.update(|cx| {
        composer.update(cx, |composer, cx| {
            composer.set_completion(&text, cursor, answer, cx)
        })
    });
}

/// Move the caret within the box by pressing a key, as a reader does.
fn press(cx: &mut HeadlessAppContext, window: AnyWindowHandle, key: &str) {
    cx.update_window(window, |_, window, cx| window.press(key, cx))
        .unwrap();
}

/// The column the box is docked at the foot of: what the app renders above it.
struct Page {
    composer: Entity<Composer>,
    /// What the box has asked, newest last: the example's stand-in for the tab, which
    /// is what sends the box's ops and hands their answers back.
    asked: Rc<RefCell<Vec<ComposerEvent>>>,
    /// Kept alive: dropping it would stop the recording.
    _subscription: gpui_kit::Subscription,
}

impl Page {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| Composer::new(window, cx));
        let asked: Rc<RefCell<Vec<ComposerEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = asked.clone();
        let subscription = cx.subscribe(&composer, move |_, _, event: &ComposerEvent, _| {
            recorded.borrow_mut().push(event.clone())
        });
        composer.update(cx, |composer, cx| {
            composer.set_pane_height(px(WINDOW_SIZE.1 - 200.), cx);
            composer.set_agent(&state(false, "stub-a"), "Coordinator", true, cx);
            composer.set_catalog(levels(), models(), commands(), cx);
        });
        Self {
            composer,
            asked,
            _subscription: subscription,
        }
    }

    /// The question the box asked last, as the tab would have sent it.
    fn question(&self) -> Option<(String, usize)> {
        self.asked.borrow().last().and_then(|event| match event {
            ComposerEvent::Complete { text, cursor } => Some((text.clone(), *cursor)),
            _ => None,
        })
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
    // models, on a registration whose ladder is a subset and on one that takes no
    // effort at all, and a catalog long enough that the models scroll — then the one
    // button's other face, then a lane's own box, which changes nothing, and says so.
    // Then the completion popup, one picture per thing it does: the commands over
    // the word at the message's start, the same list walked down its own rows, a word
    // mid-prose, and the image's own symbols inside `/eval`.
    type Setup = fn(&mut HeadlessAppContext, AnyWindowHandle, &Entity<Page>);
    let states: [(&str, Setup); 13] = [
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
                    composer.set_catalog(levels(), many_models(), commands(), cx);
                })
            });
            click(cx, window, "composer-chip-model");
        }),
        // A model whose provider takes three rungs, not the session's five: the
        // drawer's rail is the *model's* own ladder, so it is three stops long — and
        // the chip's own level (`high`) is on it.
        ("model-drawer-subset", |cx, window, page| {
            let composer = composer_of(cx, page);
            cx.update(|cx| {
                composer.update(cx, |composer, cx| {
                    composer.set_agent(
                        &state_at(false, "stub-b", "proxy"),
                        "Coordinator",
                        true,
                        cx,
                    );
                    composer.set_catalog(levels(), models(), commands(), cx);
                })
            });
            click(cx, window, "composer-chip-model");
        }),
        // A model that takes no effort setting at all: the row keeps its place and says
        // so, with no rail under it — a fact, rather than a control that vanishes.
        ("model-drawer-none", |cx, window, page| {
            let composer = composer_of(cx, page);
            cx.update(|cx| {
                composer.update(cx, |composer, cx| {
                    composer.set_agent(&state_at(false, "stub-c", "acme"), "Coordinator", true, cx);
                    composer.set_catalog(levels(), models(), commands(), cx);
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
        // The registry's fourteen commands over the word being started at the
        // message's own start: the first row highlighted, the list's own counter
        // saying how many there are, and every description whole — the list is as
        // wide as its widest row, and these are the rows evo's own registry sends.
        ("completion-command", |cx, window, page| {
            type_into(cx, window, page, "/");
            answer(cx, page);
        }),
        // The same list walked: three rows down, so the highlight — and the list
        // under it — is somewhere other than the first row.
        ("completion-walked", |cx, window, page| {
            type_into(cx, window, page, "/");
            answer(cx, page);
            for _ in 0..3 {
                press(cx, window, "down");
            }
        }),
        // A word mid-prose: the caret is at the end of `/mo`, and the popup's every
        // label starts where that word does — the popup is placed by the word's own
        // beginning, not by the caret. `mo` is what three commands begin or contain,
        // so the list is a list and not a single row, and the letters it matched
        // (`mo` of `/model`, the `m` and `o` of `/memory`) are drawn heavier.
        ("completion-mid-prose", |cx, window, page| {
            type_into(cx, window, page, "please run /mo now");
            // Four lefts put the caret at the end of `/mo`.
            for _ in 0..4 {
                press(cx, window, "left");
            }
            answer(cx, page);
        }),
        // A word started near the right-hand edge of the box: the list is pulled back
        // inside the window instead of hanging off it — its labels are no longer on the
        // word, which is the price of staying whole.
        ("completion-right-edge", |cx, window, page| {
            type_into(
                cx,
                window,
                page,
                "please check the docs and the run outcomes before you answer, then /re",
            );
            answer(cx, page);
        }),
        // `/eval` content completes against the live image: here the package's own
        // exported names, as the server answers them for the token `evo.eval:`.
        ("completion-symbols", |cx, window, page| {
            type_into(cx, window, page, "/eval (evo.eval:");
            answer_with(
                cx,
                page,
                Answer {
                    kind: Some(CompletionKind::Symbol),
                    // The token the caret is in: `evo.eval:` begins just after `(evo.`…
                    name: 7..16,
                    items: eval_package_symbols(),
                },
            );
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
