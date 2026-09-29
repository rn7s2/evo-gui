//! The todo panel of the selected agent (§7.3, center bottom).

use gpui_kit::{
    div, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _,
    TestSupportExt as _, Window,
};
use session::{Todo, TodoStatus};

use crate::style::Palette;

/// A compact list of the selected agent's todos: status glyph + text.
///
/// The panel renders nothing while the agent has no todos, so a caller can
/// place it unconditionally; [`TodoPanel::is_empty`] exposes the same fact. It
/// has no header of its own — the surface that mounts it owns that.
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
            .children(self.todos.iter().enumerate().map(|(index, todo)| {
                let (glyph, glyph_color) = match todo.status {
                    TodoStatus::Done => ("☑", palette.success),
                    TodoStatus::InProgress => ("◐", palette.primary),
                    TodoStatus::Pending => ("☐", palette.muted_foreground),
                };
                let text_color = match todo.status {
                    TodoStatus::Done => palette.muted_foreground,
                    TodoStatus::InProgress | TodoStatus::Pending => palette.foreground,
                };

                div()
                    .id(("todo-item", index))
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_sm()
                    .child(div().flex_shrink_0().text_color(glyph_color).child(glyph))
                    .child(
                        div()
                            .min_w_0()
                            .text_color(text_color)
                            .child(todo.text.clone()),
                    )
                    .test_support()
            }))
            .test_support()
            .into_any_element()
    }
}
