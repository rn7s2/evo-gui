//! Reading one server the way a client does: a snapshot seeds a topic→items
//! mirror, and every op the stream carries changes it (CONTRACT §5.2/§5.3).
//!
//! The proofs assert on the mirror and on the frames, which is what makes them
//! about the contract rather than about a renderer: an op that a client could
//! not apply is a frame this module records as unapplied.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use swarm_client::{Client, Cursor, EventStream, StreamConfig, StreamFrame, StreamMsg};

/// Wait until `step` answers, or fail with `what` in the message.
pub fn wait_for<T>(deadline: Instant, what: &str, mut step: impl FnMut() -> Option<T>) -> T {
    loop {
        if let Some(value) = step() {
            return value;
        }
        assert!(Instant::now() < deadline, "no {what} before the deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A deadline `WAIT` from now.
pub fn deadline_after(wait: Duration) -> Instant {
    Instant::now() + wait
}

// --- the mirror -------------------------------------------------------------

/// One topic, as the ops have left it.
#[derive(Clone, Debug, Default)]
pub struct Topic {
    pub state: Value,
    pub items: Vec<Value>,
    pub has_more: bool,
}

impl Topic {
    pub fn item(&self, id: &str) -> Option<&Value> {
        self.items
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
    }

    /// The newest item of a kind — the one a reader would be looking at.
    pub fn last_of_kind(&self, kind: &str) -> Option<&Value> {
        self.of_kind(kind).pop()
    }

    pub fn of_kind(&self, kind: &str) -> Vec<&Value> {
        self.items
            .iter()
            .filter(|item| item.get("kind").and_then(Value::as_str) == Some(kind))
            .collect()
    }

    /// Every item's `kind`, in order, for a failure message.
    pub fn kinds(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|item| {
                item.get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_owned()
            })
            .collect()
    }
}

/// Every topic a server publishes, as the ops have left them.
#[derive(Clone, Debug, Default)]
pub struct Mirror {
    pub topics: BTreeMap<String, Topic>,
}

impl Mirror {
    pub fn topic(&self, name: &str) -> Option<&Topic> {
        self.topics.get(name)
    }

    pub fn state(&self, name: &str) -> &Value {
        self.topics
            .get(name)
            .map(|topic| &topic.state)
            .unwrap_or(&Value::Null)
    }

    pub fn items(&self, name: &str) -> &[Value] {
        self.topics
            .get(name)
            .map(|topic| topic.items.as_slice())
            .unwrap_or(&[])
    }

    pub fn item(&self, name: &str, id: &str) -> Option<&Value> {
        self.topics.get(name)?.item(id)
    }

    pub fn last_of_kind(&self, name: &str, kind: &str) -> Option<&Value> {
        self.topics.get(name)?.last_of_kind(kind)
    }

    pub fn kinds(&self, name: &str) -> Vec<String> {
        self.topics.get(name).map(Topic::kinds).unwrap_or_default()
    }

    /// Seed a topic from a `/snapshot` body (§5.2).
    pub fn apply_snapshot(&mut self, snapshot: &Value) {
        let Some(topics) = snapshot.get("topics").and_then(Value::as_object) else {
            return;
        };
        for (name, body) in topics {
            let topic = self.topics.entry(name.clone()).or_default();
            if let Some(state) = body.get("state") {
                topic.state = state.clone();
            }
            if let Some(items) = body.get("items").and_then(Value::as_array) {
                topic.items = items.clone();
            }
            topic.has_more = body
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
    }

    /// One op's `data`, as §5.3 defines it. Returns whether it changed anything
    /// (a `topic.reset` or `stream.reset` changes nothing by itself).
    pub fn apply(&mut self, data: &Value) -> bool {
        let Some(op) = data.get("op").and_then(Value::as_str) else {
            return false;
        };
        let Some(topic) = data.get("topic").and_then(Value::as_str) else {
            return false;
        };
        let target = self.topics.entry(topic.to_owned()).or_default();
        match op {
            "item.add" => {
                let Some(item) = data.get("item") else {
                    return false;
                };
                let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let after = data.get("after").and_then(Value::as_str);
                let at = match after {
                    Some(after) => target
                        .items
                        .iter()
                        .position(|item| item.get("id").and_then(Value::as_str) == Some(after))
                        .map(|index| index + 1),
                    None => None,
                };
                match at {
                    Some(at) => target.items.insert(at, item.clone()),
                    None => target.items.push(item.clone()),
                }
                target.has_more |= target.item(id).is_none();
                true
            }
            "item.remove" => {
                let Some(id) = data.get("id").and_then(Value::as_str) else {
                    return false;
                };
                let before = target.items.len();
                target
                    .items
                    .retain(|item| item.get("id").and_then(Value::as_str) != Some(id));
                target.items.len() != before
            }
            "item.append" => {
                let (Some(id), Some(field), Some(text)) = (
                    data.get("id").and_then(Value::as_str),
                    data.get("field").and_then(Value::as_str),
                    data.get("text").and_then(Value::as_str),
                ) else {
                    return false;
                };
                let Some(item) = target
                    .items
                    .iter_mut()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
                else {
                    return false;
                };
                let grown = format!(
                    "{}{}",
                    item.get(field).and_then(Value::as_str).unwrap_or_default(),
                    text
                );
                item[field] = Value::String(grown);
                true
            }
            "item.patch" => {
                let Some(id) = data.get("id").and_then(Value::as_str) else {
                    return false;
                };
                let Some(patch) = data.get("patch") else {
                    return false;
                };
                let Some(item) = target
                    .items
                    .iter_mut()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
                else {
                    return false;
                };
                merge(item, patch);
                true
            }
            "state.patch" => {
                let Some(patch) = data.get("patch") else {
                    return false;
                };
                merge(&mut target.state, patch);
                true
            }
            _ => false,
        }
    }
}

/// A JSON merge patch: objects merge key by key, `null` removes, everything else
/// (arrays included) replaces.
fn merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if value.is_null() {
                    target.remove(key);
                } else {
                    merge(target.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

// --- the stream -------------------------------------------------------------

/// One server's stream, drained into the frames it carried and the mirror they
/// leave behind.
pub struct Watcher {
    pub client: Client,
    stream: EventStream,
    pub mirror: Mirror,
    pub frames: Vec<StreamFrame>,
    pub cursor: Option<Cursor>,
    pub reconnects: usize,
    /// `topic.reset` reasons, in order.
    pub topic_resets: Vec<String>,
    /// `stream.reset` reasons, in order.
    pub stream_resets: Vec<String>,
}

impl Watcher {
    /// Follow `topics` from `since` (the snapshot's cursor, so nothing between
    /// the snapshot and the stream is lost).
    pub fn start(client: &Client, topics: &[&str], since: Option<Cursor>) -> Watcher {
        let config = match since {
            Some(cursor) => StreamConfig::new(topics.to_vec()).from(cursor),
            None => StreamConfig::new(topics.to_vec()),
        };
        Watcher {
            client: client.clone(),
            stream: EventStream::start(client.clone(), config),
            mirror: Mirror::default(),
            frames: Vec::new(),
            cursor: None,
            reconnects: 0,
            topic_resets: Vec::new(),
            stream_resets: Vec::new(),
        }
    }

    /// Take everything that has arrived, folding each frame into the mirror.
    pub fn pump(&mut self) -> usize {
        let mut seen = 0;
        while let Some(message) = self.stream.try_recv() {
            seen += 1;
            self.absorb(message);
        }
        seen
    }

    fn absorb(&mut self, message: StreamMsg) {
        match message {
            StreamMsg::Connected { cursor } => {
                self.cursor = Some(cursor);
            }
            StreamMsg::Frame(frame) => {
                if frame.is_hello() {
                    self.cursor = frame.cursor.clone();
                } else {
                    self.mirror.apply(&frame.data);
                    if let Some(reason) = frame.topic_reset() {
                        self.topic_resets.push(reason.as_str().to_owned());
                    }
                    if let Some(reason) = frame.stream_reset() {
                        self.stream_resets.push(reason.as_str().to_owned());
                    }
                }
                if let Some(cursor) = &frame.cursor {
                    self.cursor = Some(cursor.clone());
                }
                self.frames.push(frame);
            }
            StreamMsg::Reset { .. } => {}
            StreamMsg::Reconnecting { .. } => self.reconnects += 1,
            StreamMsg::Stopped => {}
        }
    }

    /// Fold `frame` as if it had arrived — for a re-snapshot the client takes on
    /// its own after a reset.
    pub fn reseed(&mut self, snapshot: &Value) {
        self.mirror.apply_snapshot(snapshot);
    }

    /// The frame the caller is waiting for.
    pub fn wait_frame(
        &mut self,
        deadline: Instant,
        what: &str,
        mut pred: impl FnMut(&StreamFrame) -> bool,
    ) -> StreamFrame {
        loop {
            self.pump();
            if let Some(frame) = self.frames.iter().find(|frame| pred(frame)) {
                return frame.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no {what} before the deadline; saw {:?}",
                self.frames
                    .iter()
                    .map(|frame| frame.op.clone())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait for the mirror itself to say something — the moment a client would
    /// have drawn it.
    pub fn wait_mirror(
        &mut self,
        deadline: Instant,
        what: &str,
        mut pred: impl FnMut(&Mirror) -> bool,
    ) {
        loop {
            self.pump();
            if pred(&self.mirror) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never became {what}; topics: {:?}",
                self.mirror
                    .topics
                    .iter()
                    .map(|(name, topic)| (name.clone(), topic.kinds()))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn saw_op(&self, op: &str) -> bool {
        self.frames.iter().any(|frame| frame.op == op)
    }
}

// --- the client's own verbs -------------------------------------------------

/// One topic read, as §5.2 answers it.
pub fn snapshot(client: &Client, topics: &[&str], items: u32) -> Value {
    let topics: Vec<String> = topics.iter().map(|topic| (*topic).to_owned()).collect();
    let body = client.snapshot(&topics, Some(items)).expect("a snapshot");
    serde_json::to_value(body).expect("the snapshot as JSON")
}

/// One op, and its `result` — a failure is a proof failure.
pub fn op(client: &Client, name: &str, args: Value) -> Value {
    let reply = client.op(name, args).expect("the op was answered");
    let reply = serde_json::to_value(reply).expect("the reply as JSON");
    assert_eq!(
        reply.get("ok").and_then(Value::as_bool),
        Some(true),
        "{name} failed: {reply}"
    );
    reply.get("result").cloned().unwrap_or(Value::Null)
}

/// One op, the whole reply — for the ops a proof expects to refuse.
pub fn try_op(client: &Client, name: &str, args: Value) -> Value {
    let reply = client.op(name, args).expect("the op was answered");
    serde_json::to_value(reply).expect("the reply as JSON")
}

/// `input.send` with no images, from now.
pub fn send(client: &Client, text: &str) -> Value {
    op(
        client,
        "input.send",
        json!({ "text": text, "queue": "now" }),
    )
}
