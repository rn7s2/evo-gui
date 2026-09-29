//! Todos, from `/state.todos` and from `todo-changed` events.
//!
//! `core-ext/todo.lisp` replaces the whole checklist per call; the journal's `:custom`
//! `todo` entry is the source of truth, `/state.todos` is that fold, and the
//! `todo-changed` event carries the same vector. Statuses cross the wire as
//! `"pending"`, `"in-progress"` (a keyword value lower-cased) or `"done"`;
//! `parse-status` also accepts `"in_progress"` from a model that writes it that way.

use serde_json::Value;

use crate::{Todo, TodoStatus};

/// One status string as the panel's status. An unknown status reads as pending, the
/// glyph `format-todos` falls back to, so an item is never dropped for its wording.
pub fn todo_status_from_str(status: &str) -> TodoStatus {
    match status {
        "done" => TodoStatus::Done,
        "in-progress" | "in_progress" => TodoStatus::InProgress,
        _ => TodoStatus::Pending,
    }
}

/// The checklist from a `/state.todos` array or a `todo-changed` event's `todos`.
/// A null, a missing array or an empty one is an empty checklist (the panel hides
/// when the selected agent has none). Items without text are dropped.
pub fn todos_from_json(value: &Value) -> Vec<Todo> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item.get("text").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(todo_status_from_str)
                .unwrap_or(TodoStatus::Pending);
            Some(Todo { text: text.to_string(), status })
        })
        .collect()
}
