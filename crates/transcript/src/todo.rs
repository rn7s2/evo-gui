//! The todo panel of the selected agent (§7.3, center bottom).

use gpui_kit::{
    div, px, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use session::{Todo, TodoStatus};

use crate::style::Palette;

/// How tall the list may grow before it scrolls on its own, so a long todo
/// list never pushes the transcript away.
const MAX_LIST_HEIGHT: gpui_kit::Pixels = px(132.);
/// Width of the glyph column: one glyph column whatever the glyph is.
const GLYPH_COLUMN: gpui_kit::Pixels = px(18.);
/// One glyph size for ☑ / ◐ / ☐, so the three read as one column.
const GLYPH_SIZE: gpui_kit::Pixels = px(12.);

/// A compact list of the selected agent's todos: status glyph + text, under a
/// `Todos done/total` header.
///
/// The panel renders nothing while the agent has no todos, so a caller can
/// place it unconditionally; [`TodoPanel::is_empty`] exposes the same fact.
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
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

        div()
            .id("todo-panel")
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(palette.border)
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
                div()
                    .id("todo-list")
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(MAX_LIST_HEIGHT)
                    .overflow_y_scroll()
                    .children(
                        self.todos
                            .iter()
                            .enumerate()
                            .map(|(index, todo)| todo_item(index, todo, &palette)),
                    ),
            )
            .test_support()
            .into_any_element()
    }
}

/// One todo: a fixed-width, vertically centred glyph column and its text.
fn todo_item(index: usize, todo: &Todo, palette: &Palette) -> impl IntoElement {
    // Only the work in hand is coloured; everything else stays quiet.
    let (glyph_color, text_color) = match todo.status {
        TodoStatus::Done => (palette.muted_foreground, palette.muted_foreground),
        TodoStatus::InProgress => (palette.primary, palette.foreground),
        TodoStatus::Pending => (palette.muted_foreground, palette.muted_foreground),
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
                .text_size(GLYPH_SIZE)
                .text_color(glyph_color)
                .child(todo.status.glyph().to_string())
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
