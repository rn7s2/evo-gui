//! The todo panel of the selected agent (§7.3, center bottom).

use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    div, px, AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, ScrollHandle, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window,
};
use session::{Todo, TodoStatus};

use crate::style::{Palette, MEASURE};

/// How tall the list may grow before it scrolls on its own, so a long todo
/// list never pushes the transcript away.
pub(crate) const MAX_LIST_HEIGHT: gpui_kit::Pixels = px(132.);
/// Width of the glyph column: one column, whatever the glyph in it.
const GLYPH_COLUMN: gpui_kit::Pixels = px(18.);
/// The one glyph size: ☑, ◐ and ☐ are drawn at this size and no other, so the
/// three read as one column rather than three weights of font glyph.
const GLYPH_SIZE: gpui_kit::Pixels = px(12.);
/// The panel's scroll state: one list, whose scroll position is remembered
/// across frames by the window.
const LIST_ID: &str = "todo-list";
const SCROLLBAR_ID: &str = "todo-scrollbar";

/// A compact list of the selected agent's todos: status glyph + text, under a
/// `Todos done/total` header, aligned with the transcript's reading measure.
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
            .px_4()
            .pt(px(10.))
            .pb(px(10.))
            .border_t_1()
            .border_color(palette.border)
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
                        .gap_1()
                        .child(
                            div()
                                .id("todo-header")
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(palette.muted_foreground)
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
        .child(
            div()
                .min_w_0()
                .text_color(text_color)
                .child(todo.text.clone()),
        )
        .test_support()
}

/// The state drawn as a shape rather than a font glyph: ☑ a checked disc, ◐ a
/// half disc, ☐ an empty one — all at [`GLYPH_SIZE`], so they line up and
/// carry the same weight.
fn todo_glyph(status: TodoStatus, palette: &Palette) -> AnyElement {
    let glyph = match status {
        TodoStatus::Done => disc_icon(palette),
        TodoStatus::InProgress => disc(palette, Half::Filled),
        TodoStatus::Pending => disc(palette, Half::Empty),
    };

    div()
        .size(GLYPH_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .child(glyph)
        .into_any_element()
}

fn disc_icon(palette: &Palette) -> AnyElement {
    Icon::new(IconName::CircleCheck)
        .size(GLYPH_SIZE)
        .text_color(palette.muted_foreground)
        .into_any_element()
}

/// Which half of a disc is filled.
#[derive(PartialEq)]
enum Half {
    Filled,
    Empty,
}

fn disc(palette: &Palette, half: Half) -> AnyElement {
    let color = match half {
        Half::Filled => palette.primary,
        Half::Empty => palette.muted_foreground,
    };

    let disc = div()
        .size(GLYPH_SIZE)
        .rounded_full()
        .border_1()
        .border_color(color)
        .overflow_hidden();

    match half {
        Half::Filled => disc.child(div().w((GLYPH_SIZE - px(2.)) / 2.).h_full().bg(color)),
        Half::Empty => disc,
    }
    .into_any_element()
}
