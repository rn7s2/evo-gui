//! One topic's mirror: its items in order, keyed by id, plus its state (CONTRACT §4).
//!
//! A client never rebuilds a topic from a transcript: the items *are* the transcript, in
//! the order the server minted them, and every change arrives as an op on them
//! (`item.add`, `item.append`, `item.patch`, `item.remove`, `state.patch`). A snapshot
//! seeds the topic, a `topic.reset` says the mirror is not an extension of what the client
//! holds (a leaf moved, a session switched, a lane restarted) and the next snapshot
//! replaces it.
//!
//! Older items are paged in at the front (`GET /items?before=…`), which is how the
//! scrollback walks back through compactions; [`Topic::has_older`] says whether there is
//! more behind the oldest item the mirror holds.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Item, ItemChange, ItemId, Op, TopicChanges, TopicState};

/// The mirror of one topic: `session`, `swarm`, or `lane:<n>`.
#[derive(Clone, Debug, Default)]
pub struct Topic {
    name: String,
    items: Vec<Item>,
    index: HashMap<ItemId, usize>,
    state: TopicState,
    /// Whether the server has items older than the first one here.
    has_older: bool,
}

impl Topic {
    pub fn new(name: impl Into<String>) -> Topic {
        Topic {
            name: name.into(),
            ..Topic::default()
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The topic's items, oldest first.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.index.get(id).map(|index| &self.items[*index])
    }

    /// Where an item sits in the list.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn state(&self) -> &TopicState {
        &self.state
    }

    /// Whether the server holds items older than [`Topic::items`]'s first.
    pub fn has_older(&self) -> bool {
        self.has_older
    }

    /// The newest item, which is what a lane's live activity line reads.
    pub fn last_item(&self) -> Option<&Item> {
        self.items.last()
    }

    // --- seeding ----------------------------------------------------------

    /// Replace the topic with a `/snapshot` body for it:
    /// `{"state":{…},"items":[…],"has_more":bool}`. A body without items (the `swarm`
    /// topic, which carries state only) leaves an empty list.
    ///
    /// The items are a *window* ([`crate::PAGE_ITEMS`]), and a reader who paged back
    /// holds older items in front of it. When the body says there is more behind the
    /// window and the window's first item is one already held below the front, the
    /// prefix before it is the reader's own pages and is kept; the window replaces the
    /// whole suffix, so a row the server has changed or dropped is not left stale, and
    /// `has_older` stays the reader's own — the body's answer is about the window's
    /// front, not the topic's. Without that overlap — a first read, a session switch, a
    /// window that is the whole record — the window *is* the topic.
    pub fn apply_snapshot(&mut self, body: &Value) -> TopicChanges {
        self.state = TopicState::from_json(body.get("state").unwrap_or(&Value::Null));
        let items = parse_items(body.get("items"));
        let has_more = body
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let prefix = if has_more {
            items
                .first()
                .and_then(|first| self.index.get(&first.id).copied())
                .filter(|index| *index > 0)
        } else {
            None
        };
        match prefix {
            Some(prefix) => {
                let mut merged = std::mem::take(&mut self.items);
                merged.truncate(prefix);
                merged.extend(items);
                self.set_items(merged);
            }
            None => {
                self.has_older = has_more;
                self.set_items(items);
            }
        }
        TopicChanges {
            reset: true,
            state: true,
            ..TopicChanges::default()
        }
    }

    /// Prepend older items (`GET /items?before=<id>&limit=`), keeping the ones already
    /// held. An item the mirror already had is left where it is: a page that overlaps
    /// what it holds must not duplicate it.
    pub fn prepend_items(&mut self, body: &Value) -> TopicChanges {
        let older = parse_items(body.get("items"));
        let fresh: Vec<Item> = older
            .into_iter()
            .filter(|item| !self.index.contains_key(&item.id))
            .collect();
        self.has_older = body
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if fresh.is_empty() {
            return TopicChanges {
                state: false,
                ..TopicChanges::default()
            };
        }
        let count = fresh.len();
        let mut items = fresh;
        items.append(&mut self.items);
        self.set_items(items);
        TopicChanges {
            prepended: count,
            ..TopicChanges::default()
        }
    }

    // --- ops --------------------------------------------------------------

    /// Apply one op, reporting exactly what the UI has to re-set.
    pub fn apply_op(&mut self, op: &Op) -> TopicChanges {
        match op {
            Op::Hello { .. } => TopicChanges::default(),
            Op::ItemAdd { item, after } => self.add(item.as_ref().clone(), after.as_deref()),
            Op::ItemAppend { id, field, text } => {
                let Some(item) = self.item_mut(id) else {
                    // An append for an item this mirror never saw (a stream that joined
                    // mid-message): the next snapshot carries it whole.
                    return TopicChanges::default();
                };
                item.apply_append(*field, text);
                let index = self.index[id];
                TopicChanges::upsert(id.clone(), index)
            }
            Op::ItemPatch { id, patch } => {
                let Some(item) = self.item_mut(id) else {
                    return TopicChanges::default();
                };
                item.apply_patch(patch);
                let index = self.index[id];
                TopicChanges::upsert(id.clone(), index)
            }
            Op::ItemRemove { id } => self.remove(id),
            Op::StatePatch { patch } => {
                self.state.apply_patch(patch);
                TopicChanges {
                    state: true,
                    ..TopicChanges::default()
                }
            }
            Op::TopicReset { .. } | Op::StreamReset { .. } => {
                // A reset is the tab's business, not the mirror's: the items are kept (a
                // refetch that finds the same session rebuilds them identically, and the
                // reader keeps their place until it lands) and the next snapshot replaces
                // them. `TabModel` reports the reset to the UI.
                TopicChanges::default()
            }
        }
    }

    // --- internals --------------------------------------------------------

    /// Add an item, at the end or right after `after`.
    fn add(&mut self, item: Item, after: Option<&str>) -> TopicChanges {
        let id = item.id.clone();
        // A replayed add for an item already held updates it in place: ids are stable,
        // so a second copy would be a duplicate row.
        if let Some(index) = self.index.get(&id).copied() {
            self.items[index] = item;
            return TopicChanges::upsert(id, index);
        }
        let index = match after.and_then(|after| self.index.get(after).copied()) {
            // The op names the item it follows: an insert, not an append.
            Some(position) => {
                self.items.insert(position + 1, item);
                position + 1
            }
            None => {
                self.items.push(item);
                self.items.len() - 1
            }
        };
        self.reindex_from(index);
        TopicChanges::upsert(id, index)
    }

    fn remove(&mut self, id: &str) -> TopicChanges {
        let Some(index) = self.index.remove(id) else {
            return TopicChanges::default();
        };
        self.items.remove(index);
        self.reindex_from(index);
        TopicChanges {
            items: vec![ItemChange::Remove { id: id.to_string() }],
            ..TopicChanges::default()
        }
    }

    fn item_mut(&mut self, id: &str) -> Option<&mut Item> {
        let index = self.index.get(id).copied()?;
        self.items.get_mut(index)
    }

    fn set_items(&mut self, items: Vec<Item>) {
        self.items = items;
        self.index.clear();
        for (index, item) in self.items.iter().enumerate() {
            self.index.insert(item.id.clone(), index);
        }
    }

    /// Re-derive the index from `from` on: an insert or a removal shifts what follows.
    fn reindex_from(&mut self, from: usize) {
        for index in from..self.items.len() {
            self.index.insert(self.items[index].id.clone(), index);
        }
    }
}

/// The items of a body's `items` array, dropping anything without an id.
fn parse_items(value: Option<&Value>) -> Vec<Item> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Item::from_json).collect())
        .unwrap_or_default()
}
