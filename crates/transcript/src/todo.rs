//! The todo panel of the selected agent (§7.3, center bottom).

use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::{
    div, point, px, AnyElement, App, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window,
};
use session::{Todo, TodoStatus};
use widgets::glyph;

use crate::style::{Palette, MEASURE};

/// How tall the list may grow before it scrolls on its own, so a long todo
/// list never pushes the transcript away.
pub(crate) const MAX_LIST_HEIGHT: gpui_kit::Pixels = px(156.);
/// Width of the glyph column: one column, whatever the glyph in it.
const GLYPH_COLUMN: gpui_kit::Pixels = px(18.);
/// The one glyph size: `Workspace.css`'s `.todo-box` is 14px square, and ☑, ◐
/// and ☐ are drawn at this size and no other, so the three read as one column
/// rather than three weights of font glyph.
const GLYPH_SIZE: gpui_kit::Pixels = px(14.);
/// The box's corner: `.todo-box .frame{rx:3}`.
const GLYPH_RADIUS: gpui_kit::Pixels = px(3.);
/// The frame's stroke: `.todo-box .frame{stroke-width:1.2}`.
const FRAME_STROKE: f32 = 1.2;
/// The pending frame is drawn at 70% (`opacity:.7`); a done or in-progress one
/// is the ink at full weight.
const FRAME_OPACITY: f32 = 0.7;
/// The tick's stroke: `.todo-box.done .tick{stroke-width:1.6}`.
const TICK_STROKE: f32 = 1.6;
/// The panel's scroll state: one list, whose scroll position is remembered
/// across frames by the window.
const LIST_ID: &str = "todo-list";
const SCROLLBAR_ID: &str = "todo-scrollbar";

/// A compact list of the selected agent's todos: status glyph + text, under a
/// `Todos done/total` header, aligned with the transcript's reading measure.
///
/// This is the design's `.todo-strip` minus its fold: the 32px `.todo-strip-row`
/// with the chevron that turns belongs to whoever owns the layout around it (the
/// composer draws that row for its own drawer, `crates/composer`), and the strip
/// reads the same either way. Nothing draws this panel yet — `workspace`'s tab
/// page feeds it through [`crate::TranscriptView::set_todos`], and where the
/// strip sits is the tab page's to decide (the design puts it between the
/// transcript and the composer).
///
/// The panel renders nothing while the agent has no todos, so a caller can
/// place it unconditionally; [`TodoPanel::is_empty`] exposes the same fact.
///
/// A list longer than [`MAX_LIST_HEIGHT`] scrolls inside the panel, and the
/// panel draws the scrollbar itself: it owns the list's [`ScrollHandle`] — kept
/// in the window's keyed state for as long as the panel is on screen — and
/// overlays the thumb on the list's own box, so nothing outside the panel has
/// to hand it a handle.
#[derive(IntoElement)]
pub struct TodoPanel {
    todos: Vec<Todo>,
}

impl TodoPanel {
    pub fn new(todos: &[Todo]) -> Self {
        Self {
            todos: todos.to_vec(),
        }
    }

    /// Whether this panel renders nothing.
    pub fn is_empty(&self) -> bool {
        self.todos.is_empty()
    }
}

impl RenderOnce for TodoPanel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.todos.is_empty() {
            return div().into_any_element();
        }

        let palette = Palette::from_app(cx);
        let done = self
            .todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Done)
            .count();
        let total = self.todos.len();
        // The list's own scroll state, kept for as long as the panel is on
        // screen, so the thumb and the offset survive a re-render.
        let scroll = window
            .use_keyed_state(LIST_ID, cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();

        div()
            .id("todo-panel")
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            // `.todo-strip{border-bottom:1px solid var(--border);background:
            // var(--sidebar)}`: the strip sits on the chrome surface with a rule
            // under it, and its own list carries the padding.
            .bg(palette.sidebar)
            .border_b_1()
            .border_color(palette.border)
            .pb(px(4.))
            // The same measure as the rows above, so the panel's text starts on
            // the same left edge as the transcript's.
            .child(
                div().w_full().min_w_0().flex().justify_center().child(
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w(px(MEASURE))
                        .flex()
                        .flex_col()
                        // `.todo-strip-list{gap:4px;padding:8px 14px 4px}`.
                        .gap_1()
                        .px(px(14.))
                        .pt(px(8.))
                        .child(
                            div()
                                .id("todo-header")
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(palette.foreground)
                                .child(format!("Todos {done}/{total}"))
                                .test_support(),
                        )
                        .child(
                            // The list and its scrollbar share a positioned box:
                            // the thumb is drawn as a sibling of the list, so it
                            // stays put while the list scrolls under it.
                            div()
                                .relative()
                                .w_full()
                                .min_w_0()
                                .child(
                                    div()
                                        .id(LIST_ID)
                                        .track_scroll(&scroll)
                                        .w_full()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .max_h(MAX_LIST_HEIGHT)
                                        .overflow_y_scroll()
                                        .children(
                                            self.todos.iter().enumerate().map(|(index, todo)| {
                                                todo_item(index, todo, &palette)
                                            }),
                                        )
                                        .test_support(),
                                )
                                .child(
                                    div()
                                        .id(SCROLLBAR_ID)
                                        .absolute()
                                        .inset_0()
                                        .child(
                                            Scrollbar::vertical(&scroll)
                                                .id(SCROLLBAR_ID)
                                                .viewport_from_layout(),
                                        )
                                        .test_support(),
                                ),
                        ),
                ),
            )
            .test_support()
            .into_any_element()
    }
}

/// One todo: a fixed-width, vertically centred glyph column and its text.
fn todo_item(index: usize, todo: &Todo, palette: &Palette) -> impl IntoElement {
    // Only the work in hand is coloured; everything else stays quiet.
    let text_color = match todo.status {
        TodoStatus::InProgress => palette.foreground,
        TodoStatus::Done | TodoStatus::Pending => palette.muted_foreground,
    };

    div()
        .id(("todo-item", index))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .line_height(px(18.))
        .child(
            div()
                .id(("todo-glyph", index))
                .w(GLYPH_COLUMN)
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(todo_glyph(todo.status, palette))
                .test_support(),
        )
        .child({
            // `.todo-item.done .todo-text{text-decoration:line-through}` with the
            // design's own decoration colour, 55% of the muted ink.
            let text = div()
                .min_w_0()
                .text_color(text_color)
                .child(todo.text.clone());
            if todo.status == TodoStatus::Done {
                text.line_through().text_decoration_color(gpui_kit::Hsla {
                    a: 0.55,
                    ..palette.muted_foreground
                })
            } else {
                text
            }
        })
        .test_support()
}

/// The state drawn as a shape rather than a font glyph: ☑ a checked disc, ◐ a
/// half disc, ☐ an empty one — all at [`GLYPH_SIZE`], so they line up and
/// carry the same weight.
fn todo_glyph(status: TodoStatus, palette: &Palette) -> AnyElement {
    let glyph = match status {
        TodoStatus::Done => done_box(palette),
        in_progress => open_box(in_progress, palette),
    };

    div()
        .size(GLYPH_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .child(glyph)
        .into_any_element()
}

/// The done box: the frame filled with the ink, the tick cut out of it in the
/// sidebar colour — `Workspace.css`'s `.todo-box.done`.
fn done_box(palette: &Palette) -> AnyElement {
    // The design's path, `M4 7.2 6.1 9.2 10 4.9` in a 14-unit box, at 14px.
    let tick = vec![
        (point(px(4.), px(7.2)), point(px(6.1), px(9.2))),
        (point(px(6.1), px(9.2)), point(px(10.), px(4.9))),
    ];
    div()
        .size(GLYPH_SIZE)
        .rounded(GLYPH_RADIUS)
        .bg(palette.muted_foreground)
        .flex()
        .items_center()
        .justify_center()
        .child(glyph::stroked(
            GLYPH_SIZE.into(),
            TICK_STROKE,
            &tick,
            palette.sidebar,
        ))
        .into_any_element()
}

/// An open box: the frame, at 70% of the muted ink when there is nothing to say
/// about it, at the foreground's full weight while it is the work in hand, and
/// with the inner square the design fills it with.
///
/// The frame is a 1.2px border on a 3px corner; the inner square is 6×6 with a
/// 1.5px corner (`x:4 y:4 width:6 height:6 rx:1.5`).
fn open_box(status: TodoStatus, palette: &Palette) -> AnyElement {
    let (ink, opacity) = match status {
        TodoStatus::InProgress => (palette.foreground, 1.),
        _ => (palette.muted_foreground, FRAME_OPACITY),
    };
    let box_ = div()
        .size(GLYPH_SIZE)
        .rounded(GLYPH_RADIUS)
        .border(px(FRAME_STROKE))
        .border_color(ink)
        .opacity(opacity)
        .flex()
        .items_center()
        .justify_center();
    match status {
        TodoStatus::InProgress => box_.child(
            div()
                .size(GLYPH_SIZE - px(2. * 4.))
                .rounded(px(1.5))
                .bg(ink),
        ),
        _ => box_,
    }
    .into_any_element()
}
