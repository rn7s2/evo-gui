//! §7.2, seen from the app: the View menu's zoom is the *transcript's* text size.
//!
//! The menu's own rules — the five menus in macOS's order, the keys, and what
//! `app.json` remembers — are `crates/app/src/menus.rs`' tests. What this one is
//! about is the action's effect on a live window: dispatching Zoom In draws the
//! same rows taller in *every* transcript on screen, at once and without being
//! asked, and leaves the rest of the window — a real composer, beside them —
//! exactly where it was.

use evo_desktop::{install_menus, AppLog, Shell, ZoomIn, ZoomOut};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, px, size, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    ParentElement as _, Pixels, Point, Render, Styled as _, TestAppContext, Window, WindowBounds,
    WindowOptions,
};
use serde_json::json;
use session::Item;
use store::app_state::AppState;
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use transcript::{TranscriptView, TranscriptZoom};

/// The transcripts sit over a fixed part of the window, as a tab page's
/// conversation does, so what moves when the zoom changes is the text and not the
/// pane it grew into.
const TRANSCRIPT_HEIGHT: f32 = 320.;

/// A window with what a tab page has: the transcripts of the agents on screen —
/// two, as the coordinator's and a lane's would be — and the composer's box at the
/// foot.
struct Page {
    transcripts: Vec<Entity<TranscriptView>>,
    composer: Entity<composer::Composer>,
}

impl Page {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Page {
            transcripts: vec![cx.new(TranscriptView::new), cx.new(TranscriptView::new)],
            composer: cx.new(|cx| composer::Composer::new(window, cx)),
        }
    }
}

impl Render for Page {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(TRANSCRIPT_HEIGHT))
                    .flex_none()
                    .flex()
                    .flex_col()
                    // A row of its own for each, so both are on screen at once and
                    // each is measured where it is drawn.
                    .children(
                        self.transcripts
                            .iter()
                            .map(|view| div().h(px(TRANSCRIPT_HEIGHT / 2.)).child(view.clone())),
                    ),
            )
            .child(self.composer.clone())
    }
}

/// What the page shows: one user turn and one message in each transcript. The two
/// hold items of their own (`u_1`, `u_2`), which is what makes each row findable:
/// a row's element id is its kind and its item's id.
fn seed(cx: &mut TestAppContext, page: &Entity<Page>) {
    let views = cx.update(|cx| page.read(cx).transcripts.clone());
    for (n, view) in views.iter().enumerate() {
        let n = n + 1;
        let items = vec![
            Item::from_json(&json!({
                "id": format!("u_{n}"), "ts": 1, "kind": "user",
                "text": "what the reader asked for", "status": "sent"
            }))
            .expect("a user turn"),
            Item::from_json(&json!({
                "id": format!("a_{n}"), "ts": 2, "kind": "assistant",
                "text": "and what came back", "status": "final"
            }))
            .expect("a message"),
        ];
        view.update(cx, |view, cx| view.replace(items, cx));
    }
}

/// The app's own wiring, minus the background loads: a `Shell` on a temp app root
/// and the menu bar with its zoom keys, then the window.
fn open(cx: &mut TestAppContext, root: &AppRoot) -> (gpui_kit::AnyWindowHandle, Entity<Page>) {
    let root = root.clone();
    cx.update(gpui_kit::init);
    cx.update(move |cx| {
        let log = AppLog::open(&root);
        let state = AppState::default();
        Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
        install_menus(cx);
        let (window, page) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1200.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Page::new(window, cx)),
        )
        .expect("the window");
        (window.into(), page)
    })
}

/// Render `n` frames, so the views have measured what they draw.
fn frames(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle, n: usize) {
    for _ in 0..n {
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .expect("the window");
    }
    cx.run_until_parked();
}

/// The box an element was drawn in, as the frame that just ran left it.
fn box_of(
    cx: &mut TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    id: ElementId,
) -> Bounds<Pixels> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.find(id.clone()).bounds()
    })
    .expect("the window")
}

/// A row's own cell: the measure a row's builder gives it.
fn row(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
}

/// What one of the View menu's items does: the action the item fires.
fn dispatch(
    cx: &mut TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    action: impl gpui_kit::Action,
) {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_action(Box::new(action), cx);
    })
    .expect("the window");
    cx.run_until_parked();
}

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-zoom-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

/// The reader's transcript text, and only it. Zoom In draws the same rows taller —
/// a user card's own padding is fixed, so what grows is the line box its text sits
/// on — in **both** transcripts at once, while the composer's box beside them does
/// not move at all; Zoom Out is the way back, and the size is `app.json`'s to
/// remember.
#[gpui_kit::test]
fn the_view_menu_zooms_every_transcripts_text_and_nothing_else(cx: &mut TestAppContext) {
    let root = temp_root("view-menu");
    let (window, page) = open(cx, &root);
    seed(cx, &page);
    frames(cx, window, 3);

    let cards = [row("transcript-user", "u_1"), row("transcript-user", "u_2")];
    let composer = ElementId::from("composer-box");
    let at_one: Vec<f32> = cards
        .iter()
        .map(|card| f32::from(box_of(cx, window, card.clone()).size.height))
        .collect();
    let composer_at_one = box_of(cx, window, composer.clone()).size.height;
    assert!(
        composer_at_one > px(0.),
        "the composer's box is drawn beside them"
    );
    for height in &at_one {
        // `USER_LINE`: 21px of line box under the row's 8 + 8 of padding.
        assert_eq!(
            *height,
            8. + 21. + 8.,
            "the design's own size, 1.0: `.user-row{{line-height:21px}}`"
        );
    }

    dispatch(cx, window, ZoomIn);
    frames(cx, window, 3);
    for (card, was) in cards.iter().zip(&at_one) {
        // The line box is rounded to whole pixels by the text system, as every
        // line box this app draws is.
        let line = (21. * 1.1_f32).round();
        assert_eq!(
            f32::from(box_of(cx, window, card.clone()).size.height),
            8. + line + 8.,
            "one step in: the row's text is a tenth larger, its padding is not \
             ({was} was the design's own)"
        );
    }
    assert_eq!(
        cx.update(|cx| TranscriptZoom::get(cx)),
        TranscriptZoom(1.1),
        "and the whole app agrees"
    );
    assert_eq!(
        box_of(cx, window, composer.clone()).size.height,
        composer_at_one,
        "the composer's own type is its own: the zoom is the transcript's (§7.2)"
    );
    assert_eq!(
        AppState::load(&root).zoom,
        1.1,
        "and the reader's size is what the next launch opens with"
    );

    dispatch(cx, window, ZoomOut);
    frames(cx, window, 3);
    for (card, was) in cards.iter().zip(&at_one) {
        assert_eq!(
            f32::from(box_of(cx, window, card.clone()).size.height),
            *was,
            "Zoom Out is the way back"
        );
    }
    assert_eq!(
        cx.update(|cx| TranscriptZoom::get(cx)),
        TranscriptZoom::default()
    );
    assert_eq!(AppState::load(&root).zoom, 1.0);
    let _ = std::fs::remove_dir_all(root.path());
}

/// A zoom is a *drawing* change: the markdown a message was laid out at is drawn
/// again at the new size on the next frame, without the view being asked — it
/// observes the global — and without the record being rebuilt.
#[gpui_kit::test]
fn a_zoom_redraws_the_message_that_is_already_on_screen(cx: &mut TestAppContext) {
    let root = temp_root("live");
    let (window, page) = open(cx, &root);
    seed(cx, &page);
    frames(cx, window, 3);

    let message = row("transcript-message", "a_1");
    let before = box_of(cx, window, message.clone()).size.height;
    assert!(before > px(0.), "the message is drawn at 1.0");

    dispatch(cx, window, ZoomIn);
    frames(cx, window, 3);
    let after = box_of(cx, window, message).size.height;
    assert!(
        after > before,
        "the message's markdown is drawn at the new size: {before:?} then {after:?}"
    );

    // Asked for its own state, the view is the same view: what changed is the size
    // it draws at, not what it holds.
    let transcript = cx.update(|cx| page.read(cx).transcripts[0].clone());
    let items = cx.update(|cx| transcript.read(cx).items(cx).len());
    assert_eq!(items, 2, "the record is the same record");
    let _ = std::fs::remove_dir_all(root.path());
}
