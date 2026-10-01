//! The stream's ops, and the ops a UI sends back (CONTRACT §5.3, §5.5).
//!
//! Inbound: every frame of `GET /stream` is one [`Op`], parsed from the event name and its
//! JSON payload. Outbound: a UI action becomes one [`OpRequest`] — the `{rid, op, args}`
//! body a transport POSTs to `/ops` — which it hands to an [`OpSink`]. Nothing in this
//! crate holds a connection, and nothing composes an op out of prose.

use serde_json::{json, Value};

use crate::{AppendField, Item, ItemId};

/// One frame of the op stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// The stream's first frame: where it starts, and which process it belongs to.
    Hello { epoch: String, seq: u64 },
    /// A new item, positioned after `after` (or at the end when `None`). The item is
    /// boxed because it is far larger than any other frame, and the enum is built and
    /// dropped once per frame.
    ItemAdd {
        item: Box<Item>,
        after: Option<ItemId>,
    },
    /// Streamed text appended to an item's field.
    ItemAppend {
        id: ItemId,
        field: AppendField,
        text: String,
    },
    /// A JSON merge patch on one item.
    ItemPatch { id: ItemId, patch: Value },
    /// An item the topic no longer has.
    ItemRemove { id: ItemId },
    /// A JSON merge patch on the topic's state.
    StatePatch { patch: Value },
    /// The topic's items are not an extension of what the client holds: re-snapshot it.
    TopicReset { reason: String },
    /// Everything is stale: re-snapshot every topic and reconnect with the new cursor.
    StreamReset { reason: String },
}

impl Op {
    /// Parse one stream frame. `kind` is the SSE event name (or the frame's `op` field);
    /// `data` its payload. Unknown frames are `None` — a later evo version's op is not
    /// this build's to draw.
    pub fn from_json(kind: &str, data: &Value) -> Option<Op> {
        let kind = if kind.is_empty() {
            data.get("op").and_then(Value::as_str).unwrap_or("")
        } else {
            kind
        };
        Some(match kind {
            "hello" => Op::Hello {
                epoch: string(data, "epoch"),
                seq: u64_field(data, "seq"),
            },
            "item.add" => Op::ItemAdd {
                item: Box::new(Item::from_json(data.get("item")?)?),
                after: data
                    .get("after")
                    .and_then(Value::as_str)
                    .filter(|after| !after.is_empty())
                    .map(str::to_string),
            },
            "item.append" => Op::ItemAppend {
                id: string(data, "id"),
                field: match data.get("field").and_then(Value::as_str) {
                    Some("thinking") => AppendField::Thinking,
                    _ => AppendField::Text,
                },
                text: string(data, "text"),
            },
            "item.patch" => Op::ItemPatch {
                id: string(data, "id"),
                patch: data.get("patch").cloned().unwrap_or(Value::Null),
            },
            "item.remove" => Op::ItemRemove {
                id: string(data, "id"),
            },
            "state.patch" => Op::StatePatch {
                patch: data.get("patch").cloned().unwrap_or(Value::Null),
            },
            "topic.reset" => Op::TopicReset {
                reason: string(data, "reason"),
            },
            "stream.reset" => Op::StreamReset {
                reason: string(data, "reason"),
            },
            _ => return None,
        })
    }
}

/// The topic a frame is about: `session`, `swarm`, or `lane:<n>`.
pub fn topic_of(data: &Value) -> Option<String> {
    data.get("topic")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One request for `POST /ops`: the op's name and its arguments.
///
/// `rid` is the transport's (a client uuid, so a retried request is answered from the
/// server's reply cache and does nothing twice); this crate builds the op, not the
/// envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct OpRequest {
    pub op: String,
    pub args: Value,
}

impl OpRequest {
    fn new(op: &str, args: Value) -> OpRequest {
        OpRequest {
            op: op.to_string(),
            args,
        }
    }

    /// `input.send`: the reader's words. `queue` is `now` or `after_run`; `images`
    /// entries are `{name, media_type, data}` with the data base64.
    pub fn input_send(
        text: &str,
        images: Vec<Value>,
        queue: Queue,
        topic: Option<&str>,
    ) -> OpRequest {
        let mut args = json!({
            "text": text,
            "images": images,
            "queue": queue.name(),
        });
        if let Some(topic) = topic {
            args["topic"] = Value::String(topic.to_string());
        }
        OpRequest::new("input.send", args)
    }

    /// `input.cancel`: take back an input still queued (error `already_sent` when evo
    /// has taken it).
    pub fn input_cancel(item_id: &str) -> OpRequest {
        OpRequest::new("input.cancel", json!({ "item_id": item_id }))
    }

    /// `run.interrupt`: stop the session, the whole swarm, or one lane.
    pub fn run_interrupt(scope: Scope, lane: Option<u32>) -> OpRequest {
        let mut args = json!({ "scope": scope.name() });
        if let Some(lane) = lane {
            args["lane"] = json!(lane);
        }
        OpRequest::new("run.interrupt", args)
    }

    /// `goal.set`.
    pub fn goal_set(objective: &str, budget: Option<u64>) -> OpRequest {
        let mut args = json!({ "objective": objective });
        if let Some(budget) = budget {
            args["budget"] = json!(budget);
        }
        OpRequest::new("goal.set", args)
    }

    pub fn goal_pause() -> OpRequest {
        OpRequest::new("goal.pause", json!({}))
    }

    pub fn goal_resume() -> OpRequest {
        OpRequest::new("goal.resume", json!({}))
    }

    pub fn goal_clear() -> OpRequest {
        OpRequest::new("goal.clear", json!({}))
    }

    /// `model.set`.
    pub fn model_set(id: &str, provider: Option<&str>) -> OpRequest {
        let mut args = json!({ "id": id });
        if let Some(provider) = provider {
            args["provider"] = Value::String(provider.to_string());
        }
        OpRequest::new("model.set", args)
    }

    /// `thinking.set`.
    pub fn thinking_set(level: &str) -> OpRequest {
        OpRequest::new("thinking.set", json!({ "level": level }))
    }

    /// `language.set`.
    pub fn language_set(code: &str) -> OpRequest {
        OpRequest::new("language.set", json!({ "code": code }))
    }

    /// `session.new`.
    pub fn session_new() -> OpRequest {
        OpRequest::new("session.new", json!({}))
    }

    /// `session.fork`.
    pub fn session_fork(entry_id: &str) -> OpRequest {
        OpRequest::new("session.fork", json!({ "entry_id": entry_id }))
    }

    /// `session.resume`, by id or by path.
    pub fn session_resume(session_id: Option<&str>, path: Option<&str>) -> OpRequest {
        let mut args = json!({});
        if let Some(id) = session_id {
            args["session_id"] = Value::String(id.to_string());
        }
        if let Some(path) = path {
            args["path"] = Value::String(path.to_string());
        }
        OpRequest::new("session.resume", args)
    }

    /// `session.rewind` to a point in the tree.
    pub fn session_rewind(session_id: &str) -> OpRequest {
        OpRequest::new("session.rewind", json!({ "entry_id": session_id }))
    }

    /// `session.move`: show another point of the tree.
    pub fn session_move(entry_id: &str) -> OpRequest {
        OpRequest::new("session.move", json!({ "entry_id": entry_id }))
    }

    /// `context.compact`, optionally with a hint for the summarizer.
    pub fn context_compact(hint: Option<&str>) -> OpRequest {
        let mut args = json!({});
        if let Some(hint) = hint {
            args["hint"] = Value::String(hint.to_string());
        }
        OpRequest::new("context.compact", args)
    }

    /// `lore.add`.
    pub fn lore_add(scope: LoreScope, text: &str) -> OpRequest {
        OpRequest::new("lore.add", json!({ "scope": scope.name(), "text": text }))
    }

    /// `memory.request`: the reader's query, for the memory extension to answer.
    pub fn memory_request(text: &str) -> OpRequest {
        OpRequest::new("memory.request", json!({ "text": text }))
    }

    /// `command.run`: any slash command, builtin or extension, with free-text arguments.
    pub fn command_run(name: &str, args: &str) -> OpRequest {
        OpRequest::new("command.run", json!({ "name": name, "args": args }))
    }

    /// `extension.load`.
    pub fn extension_load(path: &str) -> OpRequest {
        OpRequest::new("extension.load", json!({ "path": path }))
    }

    /// `complete`: what the caret in an input box is on, and what could fill it. A
    /// read — nothing idle, and not gated by `--no-http-eval`. `cursor` is the
    /// protocol's own count: characters, not bytes
    /// ([`crate::completion::complete_request`] converts).
    pub fn complete(text: &str, cursor: usize) -> OpRequest {
        OpRequest::new("complete", json!({ "text": text, "cursor": cursor }))
    }

    /// `eval` (RCE behind the token, disabled by `--no-http-eval`).
    pub fn eval(code: &str) -> OpRequest {
        OpRequest::new("eval", json!({ "code": code }))
    }

    /// `server.shutdown`.
    pub fn server_shutdown() -> OpRequest {
        OpRequest::new("server.shutdown", json!({}))
    }
}

/// When a sent input runs: now, or after the run in flight finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    Now,
    AfterRun,
}

impl Queue {
    pub fn name(self) -> &'static str {
        match self {
            Queue::Now => "now",
            Queue::AfterRun => "after_run",
        }
    }
}

/// What a `run.interrupt` stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Session,
    Swarm,
    Lane,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Session => "session",
            Scope::Swarm => "swarm",
            Scope::Lane => "lane",
        }
    }
}

/// Where durable guidance goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoreScope {
    Project,
    Global,
}

impl LoreScope {
    pub fn name(self) -> &'static str {
        match self {
            LoreScope::Project => "project",
            LoreScope::Global => "global",
        }
    }
}

/// Where a UI hands its requests: the transport (tab_engine) implements this and does the
/// POST. The model never talks to a server itself.
pub trait OpSink {
    fn send(&self, request: OpRequest);
}

fn string(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn u64_field(data: &Value, key: &str) -> u64 {
    data.get(key).and_then(Value::as_u64).unwrap_or(0)
}
