//! session — the per-tab view model: a mirror of the server's VIEW topics (no gpui, no I/O).
//!
//! CONTRACT (shared with the `transcript`, `composer`, `agent_list` and `workspace`
//! crates): the public types below are what the UI crates render. Additive changes are
//! fine; renames or removals must be agreed with the coordinator first.
//!
//! Inputs are raw JSON (`serde_json::Value`) exactly as the server sends them, so this
//! crate does not depend on `swarm_client`.
//!
//! # The shape of it
//!
//! * [`Item`] — one thing a transcript draws, keyed by the stable id the server minted
//!   (CONTRACT §4.1). The UI renders items; it never parses prose to find out what one
//!   means.
//! * [`Topic`] — one topic's mirror: its items in order plus its [`TopicState`]. Ops
//!   ([`Op`]) change it, a snapshot seeds it, `GET /items?before=` pages older items in at
//!   the front.
//! * [`TabModel`] — the topic `session` (the coordinator), topic `swarm` (the lane list)
//!   and one `lane:<n>` per lane the tab watches, plus the selection. Every entry point
//!   returns the [`Changes`] the UI has to apply.
//! * [`OpRequest`] — one UI action turned into a `POST /ops` body, handed to an
//!   [`OpSink`] (the transport) rather than sent from here.
//!
//! # What the UI does with it
//!
//! ```text
//! tab.on_snapshot("session", &body)     one topic's state + newest items
//! tab.on_op("lane:2", &op)              one stream frame
//! tab.on_items_before("session", &body) older items, paged in at the front
//! tab.select(AgentKey::Lane(2))         the center column shows another agent
//! ```

mod completion;
mod format;
mod item;
mod launcher;
mod op;
mod state;
mod swarm;
mod tab;
mod topic;

pub use completion::{
    byte_offset, char_offset, complete_request, completion, Completion, CompletionItem,
    CompletionKind,
};
pub use format::{
    clip, k_tokens, lane_task_label, merge_patch, plural, round_div_half_even, short_duration,
};
pub use item::{
    tool_status_word, AppendField, AssistantItem, AssistantStatus, CommandNote, Compaction,
    ContextItem, GoalEventKind, GoalItem, HumanAction, Image, Item, ItemId, ItemKind, LaneEvent,
    LaneEventKind, LaneReport, Notice, NoticeSeverity, NoticeSource, ProviderRetry, QueuePosition,
    Recovery, RunOutcome, ToolItem, ToolResult, ToolStatus, Usage, UserItem, UserStatus,
};
pub use launcher::{
    command_options, history_rows, home_short, model_options, relative_time, thinking_levels,
    CommandOption, HistoryEntry, HistoryRow, HistorySource, LaunchPlan, Launcher, ModelOption,
    Role, DEFAULT_WORKERS, WORKERS_MAX, WORKERS_MIN,
};
pub use op::{topic_of, LoreScope, Op, OpRequest, OpSink, Queue, Scope};
pub use state::{
    ordered_segments, todo_status_from_str, todos_from_json, ContextInfo, GoalInfo, Job, ModelInfo,
    Segment, SessionInfo, Side, Status, SwarmLane, SwarmState, Todo, TodoStatus, TopicState,
};
pub use swarm::{lane_list, LaneList, LaneRow, LaneStatus, SwarmInfo};
pub use tab::{AgentKey, Changes, ItemChange, StreamStatus, TabModel, TopicChanges};
pub use topic::Topic;

/// How a topic is doing, in the vocabulary the status line and the rows use. `Waiting` is
/// a coordinator that settled but is held while its lanes work.
pub type Activity = state::Status;

/// How many items a snapshot carries and one page of scrollback asks for
/// (`GET /snapshot?items=`, `GET /items?before=&limit=`, §5.2/§5.4): a live transcript
/// holds a whole window, and the way back into older history is another whole window.
pub const PAGE_ITEMS: u32 = 256;
