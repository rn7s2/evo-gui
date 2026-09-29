//! One agent's transcript, assembled from `/transcript` and driven by its event stream.
//!
//! The rule the frontend follows (§9.1): **the transcript is truth, events are
//! liveness**. On tab open (and on `settled`, `gap`, `hello` or a reconnect) the UI
//! fetches `GET /transcript` and calls [`AgentModel::rebuild_from_transcript`]; in
//! between, every SSE event goes through [`AgentModel::apply_event`].
//!
//! Both paths produce the same rows for the same session: a message's markdown is
//! accumulated from `text-delta`s on the event path and read whole from `/transcript` on
//! the rebuild path, the tool calls pair with their results by call id on both.
//!
//! Row ids are the model's own, handed out in order and never reused, so a restarted
//! server — a new event log whose event ids start again at 1, announced by `hello` —
//! cannot duplicate a row. A rebuild starts a fresh id space and bumps
//! [`AgentModel::revision`], which is what tells the UI that every row view it holds is
//! stale.

use std::collections::HashMap;

use serde_json::Value;

use crate::readout::{string_field, u64_field, CacheTotals, Readout};
use crate::{todos_from_json, Activity, DimStyle, Effect, Row, RowId, RowKind, Todo, ToolResult};

/// One agent's view model: its transcript rows, its checklist, its activity and the
/// §7.3 readout. One instance per agent (the coordinator, and one per watched lane).
#[derive(Clone, Debug)]
pub struct AgentModel {
    revision: u64,
    rows: Vec<Row>,
    next_id: RowId,
    /// The assistant row a `text-delta`/`thinking-delta` appends to; set by
    /// `message-start` (or by a rebuild whose last message is an assistant message, so a
    /// resync in the middle of a stream keeps streaming into that row).
    open_assistant: Option<RowId>,
    /// Tool call id → its row, for `tool-result` pairing.
    tool_rows: HashMap<String, RowId>,
    todos: Vec<Todo>,
    activity: Activity,
    readout: Readout,
    /// The last event id seen. Ids are per server process and restart at 1 with `hello`;
    /// the model never uses them as row ids and never drops an event for going backwards.
    last_event_id: Option<u64>,
}

impl Default for AgentModel {
    fn default() -> Self {
        AgentModel::new()
    }
}

impl AgentModel {
    pub fn new() -> AgentModel {
        AgentModel {
            revision: 0,
            rows: Vec::new(),
            next_id: 1,
            open_assistant: None,
            tool_rows: HashMap::new(),
            todos: Vec::new(),
            activity: Activity::Idle,
            readout: Readout::new(),
            last_event_id: None,
        }
    }

    // --- reading ---------------------------------------------------------

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn row(&self, id: RowId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    pub fn todos(&self) -> &[Todo] {
        &self.todos
    }

    pub fn activity(&self) -> Activity {
        self.activity
    }

    pub fn readout(&self) -> &Readout {
        &self.readout
    }

    /// Mutable access to the readout, for the seed steps the UI does once per tab
    /// (`apply_registry`, `set_cache_totals`) and for `apply_state`'s state fields.
    pub fn readout_mut(&mut self) -> &mut Readout {
        &mut self.readout
    }

    /// How many times this model has been re-seeded from `/transcript`. Rebuilding is
    /// the only thing that bumps it, so a UI request stamped with the revision it
    /// started at can drop its own late answer — the view was rebuilt underneath it
    /// (§9.1) — without every streamed delta invalidating it. Row-level changes are
    /// reported by [`Effect::ROWS`] and carried by each row's own [`version`].
    ///
    /// [`version`]: crate::Row::version
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn last_event_id(&self) -> Option<u64> {
        self.last_event_id
    }

    /// The id of the assistant row currently streaming, if any.
    pub fn streaming_row(&self) -> Option<RowId> {
        self.open_assistant
    }

    // --- seeding from the API --------------------------------------------

    /// Replace the rows with the ones `/transcript` describes: user turns, assistant
    /// messages (their markdown source whole, thinking blocks, an error when the message
    /// carries one) and tool calls paired with their results by call id. A rebuild starts
    /// a fresh row-id space and bumps the revision.
    pub fn rebuild_from_transcript(&mut self, transcript: &Value) -> Effect {
        self.rows.clear();
        self.tool_rows.clear();
        self.open_assistant = None;
        self.next_id = 1;
        self.revision += 1;

        let messages = transcript
            .get("messages")
            .and_then(Value::as_array)
            .or_else(|| transcript.as_array());
        let Some(messages) = messages else {
            return Effect::REBUILT;
        };
        // Only the *last* message can still be growing: a transcript that ends on a tool
        // call is finished, and its deltas must not be appended to the message before it.
        let mut open_assistant = None;
        for message in messages {
            let mut message_open: Option<RowId> = None;
            match message.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let text = join_blocks(message.get("content"), "text", "text");
                    if !text.is_empty() {
                        self.push_row(RowKind::User { text });
                    }
                }
                Some("assistant") => {
                    let markdown = join_blocks(message.get("content"), "text", "text");
                    let thinking = join_blocks(message.get("content"), "thinking", "thinking");
                    let error = string_field(message, "error_message").filter(|e| !e.is_empty());
                    if !markdown.is_empty() || !thinking.is_empty() || error.is_some() {
                        let id = self.push_row(RowKind::Assistant {
                            markdown,
                            thinking,
                            streaming: false,
                            error,
                        });
                        message_open = Some(id);
                    }
                    for block in content_blocks(message.get("content")) {
                        if block.get("type").and_then(Value::as_str) == Some("tool-call") {
                            self.push_tool_call(block);
                        }
                    }
                }
                Some("tool-result") => {
                    let call_id = string_field(message, "tool_call_id").unwrap_or_default();
                    let content = join_blocks(message.get("content"), "text", "text");
                    let result = ToolResult {
                        is_error: message.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        content_chars: Some(content.chars().count() as u64),
                        content,
                    };
                    self.complete_tool_call(
                        &call_id,
                        string_field(message, "tool_name").unwrap_or_default(),
                        result,
                    );
                }
                _ => {}
            }
            open_assistant = message_open;
        }
        // A resync in the middle of a stream keeps appending to the message it caught
        // mid-flight instead of starting a second row for the same message.
        self.open_assistant = open_assistant;
        Effect::REBUILT
    }

    /// Seed the state-derived parts from `GET /state`: the readout, the checklist, the
    /// activity and the goal. The transcript comes from `/transcript`, so this does not
    /// touch the rows.
    pub fn apply_state(&mut self, state: &Value) -> Effect {
        let mut effect = Effect::NONE;
        self.readout.apply_state(state);
        effect |= Effect::READOUT;
        if let Some(status) = string_field(state, "status") {
            effect |= self.set_activity(match status.as_str() {
                "running" => Activity::Running,
                "compacting" => Activity::Compacting,
                _ => Activity::Idle,
            });
        }
        effect |= self.set_todos(todos_from_json(&state["todos"]));
        effect
    }

    /// Fold one SSE event into the model and report what the UI has to do.
    ///
    /// `id` is the event's own id; it is recorded and otherwise unused (ids restart at 1
    /// when a crashed coordinator is restarted — `hello` says so, and the row ids stay
    /// this model's own). `kind` is the SSE event name, `data` its JSON payload.
    pub fn apply_event(&mut self, id: u64, kind: &str, data: &Value) -> Effect {
        self.last_event_id = Some(id);
        match kind {
            "message-start" => {
                self.open_assistant();
                Effect::ROWS
            }
            "text-delta" => {
                let text = string_field(data, "text").unwrap_or_default();
                if text.is_empty() {
                    return Effect::NONE;
                }
                self.append_markdown(&text);
                Effect::ROWS
            }
            "thinking-delta" => {
                let text = string_field(data, "text").unwrap_or_default();
                if text.is_empty() {
                    return Effect::NONE;
                }
                self.append_thinking(&text);
                Effect::ROWS
            }
            "tool-call-start" => {
                self.push_tool_call(data);
                Effect::ROWS
            }
            "tool-result" => {
                let result = ToolResult {
                    is_error: data.get("is-error").and_then(Value::as_bool).unwrap_or(false),
                    content: string_field(data, "content").unwrap_or_default(),
                    content_chars: data.get("content-chars").and_then(Value::as_u64),
                };
                let call_id = string_field(data, "id").unwrap_or_default();
                let name = string_field(data, "name").unwrap_or_default();
                self.complete_tool_call(&call_id, name, result);
                Effect::ROWS
            }
            "message-end" => self.close_assistant(data),
            "user-input" | "steering" => {
                let text = string_field(data, "text").unwrap_or_default();
                if text.is_empty() {
                    return Effect::NONE;
                }
                self.push_row(RowKind::User { text });
                Effect::ROWS
            }
            "report" => {
                self.push_row(RowKind::Report {
                    done: string_field(data, "done").unwrap_or_default(),
                    evidence: string_field(data, "evidence").unwrap_or_default(),
                    next: string_field(data, "next").unwrap_or_default(),
                    blocked: string_field(data, "blocked").unwrap_or_default(),
                    requests: string_field(data, "requests").unwrap_or_default(),
                });
                Effect::ROWS
            }
            "output" => {
                let text = string_field(data, "text").unwrap_or_default();
                if text.is_empty() {
                    return Effect::NONE;
                }
                let style = match string_field(data, "style").as_deref() {
                    Some("notice") | Some("success") => DimStyle::Notice,
                    Some("error") => DimStyle::Error,
                    _ => DimStyle::Dim,
                };
                self.push_row(RowKind::Dim { style, text });
                Effect::ROWS
            }
            // Compaction and provider retries are the TUI's activity line; here they are
            // dim status rows (§5), so a run that stalls or re-sends says so in place.
            "compaction-start" => {
                self.push_row(RowKind::Dim { style: DimStyle::Status, text: "compacting...".into() });
                Effect::ROWS
            }
            "compaction-end" => {
                self.push_row(RowKind::Dim {
                    style: DimStyle::Status,
                    text: "compaction finished".into(),
                });
                Effect::ROWS
            }
            "provider-retry" => {
                let attempt = u64_field(data, "attempt");
                let max = u64_field(data, "max");
                let reason = string_field(data, "reason").unwrap_or_default();
                let reason: String = reason.lines().next().unwrap_or("").chars().take(100).collect();
                self.push_row(RowKind::Dim {
                    style: DimStyle::Notice,
                    text: format!("⟲ retry {}/{} · {}", attempt, max, reason),
                });
                Effect::ROWS
            }
            "todo-changed" => self.set_todos(todos_from_json(&data["todos"])),
            // Activity: a task is a run or a compaction, started and reaped by the
            // session thread; `settled` is when nothing else started.
            "task-start" => {
                let kind = string_field(data, "kind").unwrap_or_default();
                self.set_activity(if kind == "compact" {
                    Activity::Compacting
                } else {
                    Activity::Running
                })
            }
            "task-end" | "run-end" => self.set_activity(Activity::Idle),
            "run-start" => {
                if self.activity == Activity::Idle {
                    self.set_activity(Activity::Running)
                } else {
                    Effect::NONE
                }
            }
            // `settled` means the session is idle again *and* that the run is over, so the
            // transcript is the truth once more: refetch and rebuild (§5, §9.1).
            "settled" => self.set_activity(Activity::Idle) | Effect::RESYNC,
            // `hello` (a restarted coordinator, whose event ids start again at 1), `gap`
            // (events this stream missed) and `session-switched` (`/new`, `/fork`,
            // `/resume`) all mean the view has to be refetched.
            "hello" | "gap" | "session-switched" => Effect::RESYNC,
            // A `lane-state` event is the left column's, not this agent's; the tab feeds
            // it to its LaneList.
            "lane-state" => Effect::LANES,
            // serve says so in band when an event carries a value the mapping cannot
            // carry: log it, show nothing.
            "unprintable-event" => Effect::NONE,
            // Lifecycle and anything a later evo version adds: nothing to render.
            _ => Effect::NONE,
        }
    }

    /// Fold an SSE capture (id, event name, payload) in order.
    pub fn apply_events<'a, I>(&mut self, events: I) -> Effect
    where
        I: IntoIterator<Item = (u64, &'a str, &'a Value)>,
    {
        let mut effect = Effect::NONE;
        for (id, kind, data) in events {
            effect |= self.apply_event(id, kind, data);
        }
        effect
    }

    // --- rows -------------------------------------------------------------

    fn push_row(&mut self, kind: RowKind) -> RowId {
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(Row { id, version: 1, kind });
        id
    }

    fn row_mut(&mut self, id: RowId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.id == id)
    }

    fn touch(&mut self, id: RowId) {
        if let Some(row) = self.row_mut(id) {
            row.version += 1;
        }
    }

    /// A new assistant message begins. Any row still open (a `message-end` the stream
    /// never delivered, say) is closed first, so only one row is ever streaming.
    fn open_assistant(&mut self) {
        if let Some(id) = self.open_assistant.take() {
            if let Some(row) = self.row_mut(id) {
                if let RowKind::Assistant { streaming, .. } = &mut row.kind {
                    *streaming = false;
                }
            }
            self.drop_if_empty(id);
        }
        let id = self.push_row(RowKind::Assistant {
            markdown: String::new(),
            thinking: String::new(),
            streaming: true,
            error: None,
        });
        self.open_assistant = Some(id);
    }

    /// Append streamed markdown to the open assistant row, creating one if the stream
    /// joined late (a resync mid-message, or a `message-start` this stream missed).
    fn append_markdown(&mut self, text: &str) {
        let id = match self.open_assistant {
            Some(id) => id,
            None => {
                let id = self.push_row(RowKind::Assistant {
                    markdown: String::new(),
                    thinking: String::new(),
                    streaming: true,
                    error: None,
                });
                self.open_assistant = Some(id);
                id
            }
        };
        if let Some(RowKind::Assistant { markdown, streaming, .. }) = self.row_mut(id).map(|row| &mut row.kind) {
            markdown.push_str(text);
            *streaming = true;
        }
        self.touch(id);
    }

    /// Append streamed thinking to the open assistant row (the UI shows it only when the
    /// per-tab toggle is on — the model keeps it either way, as the transcript does).
    fn append_thinking(&mut self, text: &str) {
        let id = match self.open_assistant {
            Some(id) => id,
            None => {
                let id = self.push_row(RowKind::Assistant {
                    markdown: String::new(),
                    thinking: String::new(),
                    streaming: true,
                    error: None,
                });
                self.open_assistant = Some(id);
                id
            }
        };
        if let Some(RowKind::Assistant { thinking, .. }) = self.row_mut(id).map(|row| &mut row.kind) {
            thinking.push_str(text);
        }
        self.touch(id);
    }

    /// A tool call from `tool-call-start` (or from a transcript message's `tool-call`
    /// block). The row is added to the transcript; the result completes it by call id.
    fn push_tool_call(&mut self, data: &Value) {
        let call_id = string_field(data, "id").unwrap_or_default();
        let name = string_field(data, "name").unwrap_or_default();
        let arguments = match string_field(data, "arguments_json") {
            Some(json) => json,
            None => data
                .get("arguments")
                .map(render_arguments)
                .unwrap_or_default(),
        };
        if call_id.is_empty() {
            self.push_row(RowKind::Tool { call_id, name, arguments, result: None });
            return;
        }
        if let Some(existing) = self.tool_rows.get(&call_id).copied() {
            // A repeat (a replayed stream): update that row instead of stacking a copy.
            if let Some(RowKind::Tool { name: row_name, arguments: row_args, .. }) =
                self.row_mut(existing).map(|row| &mut row.kind)
            {
                *row_name = name;
                *row_args = arguments;
            }
            self.touch(existing);
            return;
        }
        let id = self.push_row(RowKind::Tool { call_id: call_id.clone(), name, arguments, result: None });
        self.tool_rows.insert(call_id, id);
    }

    /// A tool result: complete the call's row. A result whose call this stream never saw
    /// (a late join, or a `?since=` that started past the `tool-call-start`) still gets a
    /// row, so the output is not lost.
    fn complete_tool_call(&mut self, call_id: &str, name: String, result: ToolResult) {
        match self.tool_rows.get(call_id).copied() {
            Some(id) => {
                if let Some(RowKind::Tool { result: slot, name: row_name, .. }) =
                    self.row_mut(id).map(|row| &mut row.kind)
                {
                    if row_name.is_empty() {
                        *row_name = name;
                    }
                    *slot = Some(result);
                }
                self.touch(id);
            }
            None => {
                let id = self.push_row(RowKind::Tool {
                    call_id: call_id.to_string(),
                    name,
                    arguments: String::new(),
                    result: Some(result),
                });
                if !call_id.is_empty() {
                    self.tool_rows.insert(call_id.to_string(), id);
                }
            }
        }
    }

    /// `message-end`: close the assistant row, carrying the provider's error when the
    /// message failed. A message that produced neither text nor thinking nor an error is
    /// dropped — the transcript fold does not carry an empty assistant message either, so
    /// both paths agree on a tool-only turn. Usage re-anchors the readout.
    fn close_assistant(&mut self, data: &Value) -> Effect {
        let error = data
            .get("error")
            .and_then(Value::as_str)
            .filter(|error| !error.is_empty())
            .map(str::to_string);
        let before_readout = self.readout.text();
        self.readout.fold_message_end(data);
        let readout_changed = before_readout != self.readout.text();

        let id = match self.open_assistant {
            Some(id) => Some(id),
            // No `message-start` seen on this stream: a failed message still has to be
            // visible, so it gets a row here.
            None if error.is_some() => Some(self.push_row(RowKind::Assistant {
                markdown: String::new(),
                thinking: String::new(),
                streaming: false,
                error: error.clone(),
            })),
            None => None,
        };
        let mut effect = if readout_changed { Effect::READOUT } else { Effect::NONE };
        if let Some(id) = id {
            if let Some(RowKind::Assistant { streaming, error: slot, .. }) =
                self.row_mut(id).map(|row| &mut row.kind)
            {
                *streaming = false;
                if error.is_some() {
                    *slot = error;
                }
            }
            self.open_assistant = None;
            self.touch(id);
            effect |= Effect::ROWS;
            self.drop_if_empty(id);
        }
        effect
    }

    /// Drop an assistant row that carries nothing: no markdown, no thinking, no error.
    fn drop_if_empty(&mut self, id: RowId) {
        let empty = matches!(
            self.row(id).map(|row| &row.kind),
            Some(RowKind::Assistant { markdown, thinking, error, .. })
                if markdown.is_empty() && thinking.is_empty() && error.is_none()
        );
        if empty {
            self.rows.retain(|row| row.id != id);
            if self.open_assistant == Some(id) {
                self.open_assistant = None;
            }
        }
    }

    // --- todos and activity ----------------------------------------------

    fn set_todos(&mut self, todos: Vec<Todo>) -> Effect {
        if self.todos == todos {
            Effect::NONE
        } else {
            self.todos = todos;
            Effect::TODOS
        }
    }

    fn set_activity(&mut self, activity: Activity) -> Effect {
        if self.activity == activity {
            Effect::NONE
        } else {
            self.activity = activity;
            Effect::ACTIVITY
        }
    }

    /// The cache totals the readout shows, for the UI's bookkeeping.
    pub fn cache_totals(&self) -> CacheTotals {
        self.readout.cache_totals()
    }
}

/// A message's `content` as an array of blocks. `null`, a missing content and a content
/// that is a plain string (the fold can carry one) all read as no blocks — a string's
/// text is picked up by [`join_blocks`] instead.
fn content_blocks(content: Option<&Value>) -> &[Value] {
    match content.and_then(Value::as_array) {
        Some(blocks) => blocks,
        None => &[],
    }
}

/// The text of every `block_type` block, joined with a newline: how a message's markdown
/// is put back together from its blocks. A content that is a plain string is that text.
///
/// `key` names the field the block keeps its text in (`text`, or `thinking` for a
/// thinking block).
fn join_blocks(content: Option<&Value>, block_type: &str, key: &str) -> String {
    if let Some(text) = content.and_then(Value::as_str) {
        return text.to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    for block in content_blocks(content) {
        if block.get("type").and_then(Value::as_str) != Some(block_type) {
            continue;
        }
        if let Some(text) = block.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    parts.join("\n")
}

/// `tool-call-start`'s `arguments` as display text, for a call that carried no
/// `arguments-json`: compact JSON, so the row shows the same one-line shape either way.
fn render_arguments(arguments: &Value) -> String {
    match arguments {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
