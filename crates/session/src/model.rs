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
//!
//! Every field is read in the spelling the wire actually uses: serve maps a plist key with
//! `keyword->json-key` (`src/serve/json.lisp`), which lower-cases it and turns each hyphen
//! into an underscore — `:is-error` arrives as `is_error`, `:content-chars` as
//! `content_chars`, `:arguments-json` as `arguments_json`.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::readout::{string_field, u64_field, CacheTotals, Readout};
use crate::{
    todos_from_json, Activity, DimStyle, Effect, Row, RowChanges, RowId, RowKind, Todo, ToolResult,
};

/// When the agent's current step began: one turn of its loop, or one compaction, which the
/// TUI counts as a step of its own (`src/tui/tui.lisp`'s `begin-step` — called at a turn
/// opening and at both ends of a compaction, so the interrupted turn "gets a fresh clock
/// back").
///
/// The model never reads a clock: it records the event and its turn, plus the arrival time
/// when the caller stamps one ([`AgentModel::apply_event_at`]). A frontend that did not stamp
/// can still count from its own clock, starting it when [`Effect::STEP`] says the step
/// changed.
///
/// [`Effect::STEP`]: crate::Effect::STEP
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepClock {
    /// The turn the step belongs to, as `run-start`/`turn-start`/`compaction-*` report it.
    pub turn: u64,
    /// The event that began the step.
    pub event_id: u64,
    /// The epoch milliseconds the tab's I/O layer saw that event at, when it passed them;
    /// `None` when the step was applied without a stamp.
    pub started_at_millis: Option<u64>,
}

impl StepClock {
    /// How long the step has been running as of `now_millis`, or `None` when the caller
    /// never stamped its arrival ([`AgentModel::apply_event`]). A clock that ran backwards
    /// (`now_millis` before the stamp — two clocks disagreeing) reads as zero rather than
    /// panicking: a frontend showing `0s` is better than a crash.
    pub fn elapsed(&self, now_millis: u64) -> Option<Duration> {
        Some(Duration::from_millis(
            now_millis.saturating_sub(self.started_at_millis?),
        ))
    }

    /// The clock as the swarm writes one — `45s`, `3m`, `1h2m`, the same
    /// [`short_duration`](crate::short_duration) a lane row's `step_age` is formatted with,
    /// so both columns of the left list count in the same words. `None` without a stamp.
    pub fn clock_label(&self, now_millis: u64) -> Option<String> {
        Some(crate::short_duration(self.elapsed(now_millis)?.as_secs()))
    }
}

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
    /// Rows touched since the UI last looked, and whether the whole row set was rebuilt —
    /// drained by [`AgentModel::take_row_changes`] so the UI re-sets only what changed.
    dirty_rows: Vec<RowId>,
    rebuilt: bool,
    /// The step in flight, cleared when the run ends ([`AgentModel::step_started`]).
    step: Option<StepClock>,
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
            dirty_rows: Vec::new(),
            rebuilt: false,
            step: None,
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

    /// The step in flight, or `None` while the agent is between runs. The elapsed time is
    /// the frontend's arithmetic: `now_millis - started_at_millis`.
    pub fn step_started(&self) -> Option<StepClock> {
        self.step
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
        self.rebuilt = true;
        self.dirty_rows.clear();

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
                        // An extension's injected message is a user-role message
                        // with a `meta.key`; the reader did not write it, so it is
                        // context rather than a turn of theirs.
                        match context_key(message) {
                            Some(key) => {
                                self.push_row(RowKind::Context { key, text });
                            }
                            None => {
                                self.push_row(RowKind::User { text });
                            }
                        }
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
                        is_error: message
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
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
        self.apply_event_with(id, kind, data, None)
    }

    /// [`AgentModel::apply_event`] with the time the tab's I/O layer saw the event, which is
    /// what a step clock counts from.
    pub fn apply_event_at(&mut self, id: u64, kind: &str, data: &Value, now_millis: u64) -> Effect {
        self.apply_event_with(id, kind, data, Some(now_millis))
    }

    /// The one body both entry points share; the tab calls it with the arrival time its
    /// caller passed.
    pub(crate) fn apply_event_with(
        &mut self,
        id: u64,
        kind: &str,
        data: &Value,
        arrival: Option<u64>,
    ) -> Effect {
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
                    is_error: data
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    content: string_field(data, "content").unwrap_or_default(),
                    content_chars: data.get("content_chars").and_then(Value::as_u64),
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
            // dim status rows (§5), so a run that stalls or re-sends says so in place. Both
            // ends of a compaction are step boundaries, as the TUI has them. The words are
            // the reader's: no event name, no field name, no count the event does not carry.
            "compaction-start" => {
                self.begin_step(id, data, arrival);
                self.push_row(RowKind::Dim {
                    style: DimStyle::Status,
                    text: "Compacting context…".into(),
                });
                Effect::ROWS | Effect::STEP
            }
            "compaction-end" => {
                self.begin_step(id, data, arrival);
                self.push_row(RowKind::Dim {
                    style: DimStyle::Status,
                    text: "Context compacted".into(),
                });
                Effect::ROWS | Effect::STEP
            }
            // `src/provider/core.lisp`'s retry emit: attempt, max, delay (seconds — a whole
            // `Retry-After`, or `2^attempt` plus a fraction) and the reason the attempt died.
            "provider-retry" => {
                let attempt = u64_field(data, "attempt");
                let max = u64_field(data, "max");
                let reason = string_field(data, "reason").unwrap_or_default();
                let reason: String = reason
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(100)
                    .collect();
                let mut text = format!("Retrying provider ({attempt}/{max})");
                if let Some(delay) = data.get("delay").and_then(Value::as_f64) {
                    text.push_str(" in ");
                    text.push_str(&retry_delay_words(delay));
                }
                if !reason.is_empty() {
                    text.push_str(" — ");
                    text.push_str(&reason);
                }
                self.push_row(RowKind::Dim {
                    style: DimStyle::Notice,
                    text,
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
            "task-end" => {
                self.step = None;
                self.set_activity(Activity::Idle) | Effect::STEP
            }
            // A run that ended as asked says nothing the turn boundary has not already
            // drawn; one that ended any other way is worth a line, in the run's own
            // vocabulary (`run-end`'s `outcome`: `stop`, `length`, `error`, `aborted` —
            // `src/kernel/loop.lisp`). `run-start` never emits a row.
            "run-end" => {
                self.step = None;
                let mut effect = self.set_activity(Activity::Idle) | Effect::STEP;
                let outcome = string_field(data, "outcome").unwrap_or_default();
                if let Some(text) = self.run_outcome_text(&outcome) {
                    self.push_run_outcome(outcome, text);
                    effect |= Effect::ROWS;
                }
                effect
            }
            "run-start" | "turn-start" => {
                self.begin_step(id, data, arrival);
                let mut effect = Effect::STEP;
                if self.activity == Activity::Idle {
                    // A turn is opening: a session we joined mid-run is running.
                    effect |= self.set_activity(Activity::Running);
                }
                effect
            }
            // `settled` means the session is idle again *and* that the run is over, so the
            // transcript is the truth once more: refetch and rebuild (§5, §9.1).
            "settled" => {
                self.step = None;
                self.set_activity(Activity::Idle) | Effect::RESYNC | Effect::STEP
            }
            // `hello` (a restarted coordinator, whose event ids start again at 1), `gap`
            // (events this stream missed) and `session-switched` (`/new`, `/fork`,
            // `/resume`) all mean the view has to be refetched — and after a restart or a
            // session switch the step that was running belongs to the old one.
            "hello" | "session-switched" => {
                self.step = None;
                Effect::RESYNC | Effect::STEP
            }
            "gap" => {
                self.push_row(RowKind::Dim {
                    style: DimStyle::Notice,
                    text: "Reconnected — some events may be missing".into(),
                });
                Effect::ROWS | Effect::RESYNC
            }
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
        self.rows.push(Row {
            id,
            version: 1,
            kind,
        });
        self.dirty_rows.push(id);
        id
    }

    fn row_mut(&mut self, id: RowId) -> Option<&mut Row> {
        self.rows.iter_mut().find(|row| row.id == id)
    }

    fn touch(&mut self, id: RowId) {
        if let Some(row) = self.row_mut(id) {
            row.version += 1;
            self.dirty_rows.push(id);
        }
    }

    /// A step begins: the turn is opening, or a compaction took over or handed back (the
    /// TUI's `begin-step` fires at all three).
    fn begin_step(&mut self, event_id: u64, data: &Value, arrival: Option<u64>) {
        self.step = Some(StepClock {
            turn: u64_field(data, "turn"),
            event_id,
            started_at_millis: arrival,
        });
    }

    /// The words a run that ended badly gets, or `None` for the ones that need no line.
    /// `stop` finishes a run as asked — and a `run-end` that carries no outcome at all, or
    /// the older `ok` spelling, is the same thing — so the turn boundary already says it.
    /// An `error` is told with the error itself, which is the failing assistant message's
    /// ([`AgentModel::last_error`]).
    /// The row a run's outcome gets — the outcome line, unless the row right before
    /// it already says the same thing.
    ///
    /// A run that fails is published twice: the kernel's own `output` line ("error: …",
    /// error-styled) and then the run's outcome ("Run failed: …"), and a reader does
    /// not need the sentence twice — `docs/screens/09-bad-run-light.png` caught the tab
    /// saying it twice. The output row is the one that goes, and the outcome is what is
    /// left: it says the same thing *and* says what it was (a run that ended badly), so
    /// the row becomes the outcome row rather than gaining a second one beside it.
    fn push_run_outcome(&mut self, outcome: String, text: String) {
        if let Some(row) = self.rows.last_mut() {
            if let RowKind::Dim {
                style: DimStyle::Error,
                text: previous,
            } = &row.kind
            {
                if Self::repeats_failure(previous, &text) {
                    let id = row.id;
                    row.kind = RowKind::RunOutcome { outcome, text };
                    self.touch(id);
                    return;
                }
            }
        }
        self.push_row(RowKind::RunOutcome { outcome, text });
    }

    fn run_outcome_text(&self, outcome: &str) -> Option<String> {
        match outcome {
            "" | "ok" | "stop" => None,
            "aborted" => Some("Run aborted".to_string()),
            "error" => Some(match self.last_error() {
                Some(error) => format!("Run failed: {error}"),
                None => "Run failed".to_string(),
            }),
            "length" => Some("Run stopped at the length limit".to_string()),
            other => Some(format!("Run ended: {other}")),
        }
    }

    /// Whether a run's outcome line is the same failure as the error-styled output row
    /// just before it: `Run failed: M` against `error: M`, the two prefixes the kernel
    /// writes. Exact, after those prefixes, so an unrelated error line is left alone.
    fn repeats_failure(output: &str, outcome: &str) -> bool {
        let message = outcome.strip_prefix("Run failed: ").unwrap_or(outcome);
        let shown = output.strip_prefix("error: ").unwrap_or(output);
        !message.is_empty() && message == shown
    }

    /// The error the most recent assistant message carries — what a run that ended `:error`
    /// failed with (`src/kernel/loop.lisp`: the message's `stop-reason` *is* the run's
    /// outcome, and its `error-message` rides along).
    fn last_error(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match &row.kind {
            RowKind::Assistant {
                error: Some(error), ..
            } if !error.is_empty() => Some(error.as_str()),
            _ => None,
        })
    }

    /// The rows touched since the last call, drained: the UI re-sets only those, and a
    /// rebuild says every row view it holds is stale. See [`RowChanges`].
    pub fn take_row_changes(&mut self) -> RowChanges {
        if self.rebuilt {
            self.rebuilt = false;
            self.dirty_rows.clear();
            return RowChanges::Rebuilt;
        }
        if self.dirty_rows.is_empty() {
            return RowChanges::Unchanged;
        }
        let mut ids = std::mem::take(&mut self.dirty_rows);
        ids.sort_unstable();
        ids.dedup();
        RowChanges::Changed(ids)
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
        if let Some(RowKind::Assistant {
            markdown,
            streaming,
            ..
        }) = self.row_mut(id).map(|row| &mut row.kind)
        {
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
        if let Some(RowKind::Assistant { thinking, .. }) = self.row_mut(id).map(|row| &mut row.kind)
        {
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
            self.push_row(RowKind::Tool {
                call_id,
                name,
                arguments,
                result: None,
            });
            return;
        }
        if let Some(existing) = self.tool_rows.get(&call_id).copied() {
            // A repeat (a replayed stream): update that row instead of stacking a copy.
            if let Some(RowKind::Tool {
                name: row_name,
                arguments: row_args,
                ..
            }) = self.row_mut(existing).map(|row| &mut row.kind)
            {
                *row_name = name;
                *row_args = arguments;
            }
            self.touch(existing);
            return;
        }
        let id = self.push_row(RowKind::Tool {
            call_id: call_id.clone(),
            name,
            arguments,
            result: None,
        });
        self.tool_rows.insert(call_id, id);
    }

    /// A tool result: complete the call's row. A result whose call this stream never saw
    /// (a late join, or a `?since=` that started past the `tool-call-start`) still gets a
    /// row, so the output is not lost.
    fn complete_tool_call(&mut self, call_id: &str, name: String, result: ToolResult) {
        match self.tool_rows.get(call_id).copied() {
            Some(id) => {
                if let Some(RowKind::Tool {
                    result: slot,
                    name: row_name,
                    ..
                }) = self.row_mut(id).map(|row| &mut row.kind)
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
        let mut effect = if readout_changed {
            Effect::READOUT
        } else {
            Effect::NONE
        };
        if let Some(id) = id {
            if let Some(RowKind::Assistant {
                streaming,
                error: slot,
                ..
            }) = self.row_mut(id).map(|row| &mut row.kind)
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
            // A removed row is a `RowId` the UI must forget: it is in the change list and
            // no longer in `rows`.
            self.dirty_rows.push(id);
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

/// A message's `meta.key`: the tag an extension gave the content it injected with
/// `evo:inject-context`, or `None` for everything a person typed.
///
/// The key crosses the wire through serve's `keyword->json-key`, so `:meta (:key ...)`
/// arrives as `{"meta": {"key": "<key>"}}`.
fn context_key(message: &Value) -> Option<String> {
    string_field(message.get("meta")?, "key").filter(|key| !key.is_empty())
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

/// `provider-retry`'s `delay` in seconds (`src/provider/core.lisp`'s `retry-delay`) as the
/// words a reader wants: `500 ms` under a second, `3 s` on the whole second, `1.5 s`
/// otherwise. The wire's number is fractional even when nobody asked for fractions — the
/// backoff is `2^attempt` plus a random one, and a `Retry-After` is whole seconds — so the
/// row rounds to the millisecond and then drops what says nothing.
fn retry_delay_words(delay: f64) -> String {
    let millis = (delay * 1000.0).round().max(0.0);
    if millis < 1000.0 {
        return format!("{} ms", millis as u64);
    }
    let seconds = millis / 1000.0;
    if seconds.fract() == 0.0 {
        format!("{} s", seconds as u64)
    } else {
        format!("{seconds:.1} s")
    }
}
