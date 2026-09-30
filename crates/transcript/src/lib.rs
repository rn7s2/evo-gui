//! transcript — the center column of a tab: one agent's items (§2.8, §7.3).
//!
//! The view is fed **items** from the `session` crate, keyed by the stable id the server
//! minted (CONTRACT §4.1), and owns four pieces of retained state:
//!
//! * one [`TextViewState`] per assistant item that has been on screen, keyed by item id,
//!   extended with `set_text` so markdown is **rendered live while it streams** — never
//!   recreated per delta, and never a second parse for a delta that arrived between two
//!   frames;
//! * one [`MessageScrollerState`] per view, which follows the tail while the reader is at
//!   the bottom and offers the jump-to-latest button when they are not;
//! * the tools the reader opened, and the untruncated results fetched for them;
//! * the todos of the agent the view shows.
//!
//! A caller seeds the whole list with [`TranscriptView::replace`] (a snapshot, a
//! `topic.reset`), pages older items in with [`TranscriptView::prepend`] (the scrollback
//! walking back through compactions), and moves single items with [`TranscriptView::upsert`]
//! and [`TranscriptView::remove`] as ops arrive. There are no row ids of the view's own and
//! no revisions: an item's id is stable across a refetch, a reconnect and a restart, so an
//! item that changes is the same row.
//!
//! ```ignore
//! let transcript = cx.new(TranscriptView::new);
//! transcript.update(cx, |view, cx| view.replace(items, cx));
//! transcript.update(cx, |view, cx| view.upsert(item, cx));
//! ```

mod link;
mod markdown;
mod rows;
mod style;
mod todo;

#[cfg(test)]
mod tests;

pub use todo::TodoPanel;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TextViewState;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::{
    div, px, AnyElement, App, AppContext as _, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, TestSupportExt as _, Window,
};
use session::{AgentKey, Item, ItemId, ItemKind, Todo};

use crate::rows::CopyFeedback;
use crate::style::Palette;

/// How many assistant items keep their parsed document.
const KEPT_DOCUMENTS: usize = 128;

/// A handler that names the item it is about: the tool whose whole output is wanted, the
/// queued input to take back, or the oldest item the scrollback pages back from. Every one
/// is a read or an op on the server, which the owner (the tab's transport) performs.
pub type ItemHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The data the row renderer reads.
///
/// It lives in an entity of its own so the virtual list can read rows while the view that
/// owns them is mid-render, and so a row update never copies the whole transcript into the
/// renderer closure.
pub(crate) struct TranscriptData {
    pub(crate) items: Vec<Item>,
    /// Retained markdown documents of assistant items, keyed by item id.
    pub(crate) documents: HashMap<ItemId, Entity<TextViewState>>,
    rendered: HashMap<ItemId, u64>,
    frame: u64,
    /// Items the reader has opened.
    pub(crate) expanded: HashSet<ItemId>,
    /// The whole of a tool call's result, for the calls the server shortened: fetched with
    /// `GET /items/<id>` the first time the reader opens one.
    pub(crate) full_results: HashMap<ItemId, String>,
    pub(crate) show_thinking: bool,
    pub(crate) copy_feedback: Arc<CopyFeedback>,
    pub(crate) focus: FocusHandle,
    /// What the owner does for each thing a row can ask for.
    pub(crate) on_load_older: RefCell<Option<ItemHandler>>,
    pub(crate) on_fetch_item: RefCell<Option<ItemHandler>>,
    pub(crate) on_cancel_input: RefCell<Option<ItemHandler>>,
}

impl TranscriptData {
    fn index_of(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    /// Start a frame: age the documents and drop the ones the reader has left.
    fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if self.rendered.len() <= KEPT_DOCUMENTS {
            return;
        }

        let mut ages: Vec<(ItemId, u64)> = self
            .rendered
            .iter()
            .map(|(id, frame)| (id.clone(), *frame))
            .collect();
        ages.sort_by_key(|(_, frame)| *frame);
        for (id, _) in ages.drain(..ages.len() - KEPT_DOCUMENTS) {
            self.rendered.remove(&id);
            self.documents.remove(&id);
        }
    }

    pub(crate) fn note_rendered(&mut self, id: &str) {
        self.rendered.insert(id.to_string(), self.frame);
    }

    /// Bring `items[index]`'s document up to date, creating it the first time the row is on
    /// screen.
    pub(crate) fn sync_document(&mut self, index: usize, cx: &mut Context<Self>) {
        let item = &self.items[index];
        let ItemKind::Assistant(assistant) = &item.kind else {
            return;
        };
        let id = item.id.clone();
        match self.documents.get(&id).cloned() {
            Some(document) => {
                let text = assistant.text.as_str();
                document.update(cx, |state, cx| state.set_text(text, cx))
            }
            None => {
                let document = cx.new(|cx| {
                    TextViewState::markdown(&assistant.text, cx).motion(rows::stream_motion())
                });
                self.documents.insert(id, document);
            }
        }
    }

    fn retain_documents(&mut self) {
        let items = &self.items;
        self.documents
            .retain(|id, _| items.iter().any(|item| item.id == *id));
        self.rendered
            .retain(|id, _| items.iter().any(|item| item.id == *id));
        self.full_results
            .retain(|id, _| items.iter().any(|item| item.id == *id));
    }
}

/// What a list update did, so the scroller can be told about it.
enum ListChange {
    Remeasure,
    Reset(usize),
    Append(usize),
    Prepend(usize),
    Remove(Range<usize>),
}

impl ListChange {
    fn between(old: &[Item], new: &[Item]) -> Self {
        let prefix = old
            .iter()
            .zip(new)
            .take_while(|(old, new)| old.id == new.id)
            .count();

        if prefix == old.len() && prefix == new.len() {
            Self::Remeasure
        } else if prefix == old.len() {
            Self::Append(new.len() - old.len())
        } else if prefix == new.len() {
            Self::Remove(new.len()..old.len())
        } else {
            Self::Reset(new.len())
        }
    }
}

/// One agent's transcript: a tail-following virtual list of items.
pub struct TranscriptView {
    data: Entity<TranscriptData>,
    scroller: Entity<MessageScrollerState>,
    todos: Vec<Todo>,
    /// Whether the agent's topic has items older than the ones held.
    has_older: bool,
    /// Whether the page the reader asked for is still in flight.
    loading_older: bool,
    /// Whose transcript this is. An empty one says different things to the coordinator's
    /// reader and to a lane's.
    agent: AgentKey,
}

impl TranscriptView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        cx.observe(&scroller, |_, _, cx| cx.notify()).detach();

        Self {
            data: cx.new(|cx| TranscriptData {
                focus: cx.focus_handle(),
                items: Vec::new(),
                documents: HashMap::new(),
                rendered: HashMap::new(),
                frame: 0,
                expanded: HashSet::new(),
                full_results: HashMap::new(),
                show_thinking: false,
                copy_feedback: Arc::new(CopyFeedback::default()),
                on_load_older: RefCell::new(None),
                on_fetch_item: RefCell::new(None),
                on_cancel_input: RefCell::new(None),
            }),
            scroller,
            todos: Vec::new(),
            has_older: false,
            loading_older: false,
            agent: AgentKey::Coordinator,
        }
    }

    /// Whose transcript this view shows.
    pub fn agent(&self) -> AgentKey {
        self.agent
    }

    pub fn set_agent(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.agent = agent;
        cx.notify();
    }

    /// The items currently shown, in order.
    pub fn items<'a>(&'a self, cx: &'a App) -> &'a [Item] {
        &self.data.read(cx).items
    }

    pub fn todos(&self) -> &[Todo] {
        &self.todos
    }

    /// Whether there is history behind the oldest item, and whether a page is in flight.
    pub fn has_older(&self) -> bool {
        self.has_older
    }

    pub fn is_loading_older(&self) -> bool {
        self.loading_older
    }

    /// What the owner does when the reader asks for older items, for a tool's whole output,
    /// or to take a queued input back. Each is an op the owner sends; the view only asks.
    pub fn on_load_older(
        &mut self,
        handler: impl Fn(&str, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let handler: ItemHandler = Rc::new(handler);
        self.data.update(cx, |data, _| {
            *data.on_load_older.borrow_mut() = Some(handler)
        });
    }

    pub fn on_fetch_item(
        &mut self,
        handler: impl Fn(&str, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let handler: ItemHandler = Rc::new(handler);
        self.data.update(cx, |data, _| {
            *data.on_fetch_item.borrow_mut() = Some(handler)
        });
    }

    pub fn on_cancel_input(
        &mut self,
        handler: impl Fn(&str, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let handler: ItemHandler = Rc::new(handler);
        self.data.update(cx, |data, _| {
            *data.on_cancel_input.borrow_mut() = Some(handler)
        });
    }

    /// The topic has more behind what the view holds (it came with the snapshot), and
    /// whether a page is in flight.
    pub fn set_history(&mut self, has_older: bool, loading: bool, cx: &mut Context<Self>) {
        if self.has_older == has_older && self.loading_older == loading {
            return;
        }
        self.has_older = has_older;
        self.loading_older = loading;
        cx.notify();
    }

    /// Replace the whole list, as a snapshot or a `topic.reset` does.
    pub fn replace(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let change = self.data.update(cx, |data, _| {
            let change = ListChange::between(&data.items, &items);
            data.items = items;
            data.retain_documents();
            change
        });
        self.apply_list_change(change, cx);
        cx.notify();
    }

    /// Put older items in front of what is held — the scrollback walking back through
    /// compactions. An item already held is not added twice.
    pub fn prepend(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let (change, added) = self.data.update(cx, |data, _| {
            let fresh: Vec<Item> = items
                .into_iter()
                .filter(|item| !data.items.iter().any(|held| held.id == item.id))
                .collect();
            if fresh.is_empty() {
                return (ListChange::Remeasure, 0);
            }
            let added = fresh.len();
            let mut next = fresh;
            next.append(&mut data.items);
            data.items = next;
            data.retain_documents();
            let total = data.items.len();
            (ListChange::Prepend(total), added)
        });
        if added > 0 {
            self.apply_list_change(change, cx);
        }
        cx.notify();
    }

    /// Add or replace one item, by its id. Returns whether the view changed.
    pub fn upsert(&mut self, item: Item, cx: &mut Context<Self>) -> bool {
        let change = self
            .data
            .update(cx, |data, _| match data.index_of(&item.id) {
                Some(index) => {
                    let waiting = matches!(
                        (&data.items[index].kind, &item.kind),
                        (ItemKind::Assistant(old), ItemKind::Assistant(new))
                            if old.status == new.status
                                && old.text == new.text
                                && old.thinking == new.thinking
                                && old.error == new.error
                    );
                    data.items[index] = item;
                    // A row whose content is unchanged (a status flip, or an op the view has
                    // already drawn) needs no remeasure: only its own cell changed.
                    if waiting {
                        return None;
                    }
                    Some(ListChange::Remeasure)
                }
                None => {
                    data.items.push(item);
                    Some(ListChange::Append(1))
                }
            });

        let Some(change) = change else {
            return false;
        };
        self.apply_list_change(change, cx);
        cx.notify();
        true
    }

    /// Drop one item the topic no longer has.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let index = self.data.update(cx, |data, _| {
            let index = data.index_of(id)?;
            data.items.remove(index);
            data.documents.remove(id);
            data.rendered.remove(id);
            data.expanded.remove(id);
            data.full_results.remove(id);
            Some(index)
        });
        let Some(index) = index else {
            return false;
        };
        self.scroller.update(cx, |scroller, cx| {
            scroller.splice(index..index + 1, 0, cx);
        });
        cx.notify();
        true
    }

    pub fn set_todos(&mut self, todos: Vec<Todo>, cx: &mut Context<Self>) {
        if self.todos == todos {
            return;
        }
        self.todos = todos;
        cx.notify();
    }

    /// Drop every item (a session switch, before the new snapshot lands).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.items.clear();
            data.documents.clear();
            data.rendered.clear();
            data.expanded.clear();
            data.full_results.clear();
        });
        self.scroller
            .update(cx, |scroller, cx| scroller.reset(0, cx));
        cx.notify();
    }

    pub fn is_following_tail(&self, cx: &App) -> bool {
        self.scroller.read(cx).is_following_tail()
    }

    pub fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.scroller
            .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
    }

    pub fn is_showing_thinking(&self, cx: &App) -> bool {
        self.data.read(cx).show_thinking
    }

    /// Whether this agent has any thinking text to reveal.
    pub fn has_thinking(&self, cx: &App) -> bool {
        self.data
            .read(cx)
            .items
            .iter()
            .any(|item| match &item.kind {
                ItemKind::Assistant(assistant) => !assistant.thinking.is_empty(),
                _ => false,
            })
    }

    pub fn set_show_thinking(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.is_showing_thinking(cx) == show {
            return;
        }
        self.data.update(cx, |data, _| data.show_thinking = show);
        cx.notify();
    }

    pub fn toggle_thinking(&mut self, cx: &mut Context<Self>) {
        let show = !self.is_showing_thinking(cx);
        self.set_show_thinking(show, cx);
    }

    /// Show or hide one row's body. Opening a tool call whose result the server shortened
    /// asks the owner for the whole of it, once.
    pub fn set_expanded(&mut self, id: &str, expanded: bool, cx: &mut Context<Self>) {
        let changed = self.data.update(cx, |data, _| {
            if expanded {
                data.expanded.insert(id.to_string())
            } else {
                data.expanded.remove(id)
            }
        });
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn toggle_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        let expanded = !self.data.read(cx).expanded.contains(id);
        self.set_expanded(id, expanded, cx);
    }

    /// The whole of a tool call's result, as `GET /items/<id>` answered.
    pub fn set_full_result(&mut self, id: &str, text: String, cx: &mut Context<Self>) {
        self.data
            .update(cx, |data, _| data.full_results.insert(id.to_string(), text));
        cx.notify();
    }

    /// A row's own ask: the whole output of the tool call it shows, which the owner
    /// fetches with `GET /items/<id>` and hands back through
    /// [`TranscriptView::set_full_result`].
    pub(crate) fn load_full_item(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handler = self.data.read(cx).on_fetch_item.borrow().clone();
        if let Some(handler) = handler {
            handler(id, window, cx);
        }
    }

    /// A queued input's own ask: take the words back before evo has them.
    pub(crate) fn cancel_queued(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handler = self.data.read(cx).on_cancel_input.borrow().clone();
        if let Some(handler) = handler {
            handler(id, window, cx);
        }
    }

    fn apply_list_change(&mut self, change: ListChange, cx: &mut Context<Self>) {
        self.scroller.update(cx, |scroller, cx| {
            match change {
                ListChange::Remeasure => scroller.remeasure(cx),
                ListChange::Reset(count) => scroller.reset(count, cx),
                ListChange::Append(count) => {
                    scroller.append(count, cx);
                }
                // Older items went in front: the list is a different one from the
                // scroller's point of view, at a different length.
                ListChange::Prepend(total) => scroller.reset(total, cx),
                ListChange::Remove(range) => {
                    scroller.splice(range, 0, cx);
                }
            };
        });
    }
}

/// What an empty transcript says, per agent.
fn empty_note(agent: AgentKey) -> (String, Option<&'static str>) {
    match agent {
        AgentKey::Coordinator => (
            "Ask the coordinator to get started".to_string(),
            Some("It plans the work and hands tasks to its lanes."),
        ),
        AgentKey::Lane(n) => (format!("Lane {n} hasn't been given work yet."), None),
    }
}

/// The transcript before its first item: a quiet, centred invitation.
fn empty_state(agent: AgentKey, palette: &Palette) -> AnyElement {
    let (note, detail) = empty_note(agent);
    let mut column = v_flex()
        .size_full()
        .justify_center()
        .items_center()
        .gap_2()
        .px_6()
        .text_center();

    if agent == AgentKey::Coordinator {
        column = column.child(
            Icon::new(IconName::Asterisk)
                .size(px(22.))
                .text_color(palette.muted_foreground),
        );
    }

    column = column.child(
        div()
            .text_size(palette.font_size)
            .text_color(palette.foreground)
            .child(note),
    );
    if let Some(detail) = detail {
        column = column.child(
            div()
                .text_size(palette.font_size - px(2.))
                .text_color(palette.muted_foreground)
                .child(detail),
        );
    }

    match agent {
        AgentKey::Coordinator => column.id("transcript-empty"),
        AgentKey::Lane(n) => column.id(("transcript-empty-lane", n as u64)),
    }
    .test_support()
    .into_any_element()
}

/// The strip above the list: the way back into the history the tab holds only the tail of.
///
/// A topic that came with `has_more` has items older than the oldest one on screen — the
/// other side of a compaction, usually — and this is how the reader asks for them. It says
/// how far back it goes rather than how many items are missing: the server counts what it
/// cut off, which is not a number a reader has a use for.
fn history_header(
    loading: bool,
    oldest: Option<String>,
    palette: &Palette,
    data: &Entity<TranscriptData>,
    cx: &App,
) -> Option<AnyElement> {
    let handler = data.read(cx).on_load_older.borrow().clone()?;
    let oldest = oldest?;
    let label = if loading {
        "Loading earlier items…"
    } else {
        "Earlier items"
    };
    Some(
        div()
            .w_full()
            .flex()
            .justify_center()
            .pb(px(6.))
            .child(
                div()
                    .id("transcript-load-older")
                    .px_3()
                    .py(px(2.))
                    .rounded(palette.radius)
                    .border_1()
                    .border_color(palette.border)
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(palette.muted_foreground)
                    .cursor_pointer()
                    .hover(|style| style.text_color(palette.foreground))
                    .aria_label(label.to_string())
                    .on_click(move |_, window, cx| handler(&oldest, window, cx))
                    .child(label)
                    .test_support(),
            )
            .into_any_element(),
    )
}

impl Render for TranscriptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.data.clone();
        let view = cx.weak_entity();
        let theme = cx.theme().clone();

        if self.data.read(cx).items.is_empty() {
            return empty_state(self.agent, &Palette::from_app(cx)).into_any_element();
        }

        self.data.update(cx, |data, _| data.begin_frame());

        let mut column = v_flex().size_full().min_h_0();
        if self.has_older {
            let palette = Palette::from_app(cx);
            let oldest = self.data.read(cx).items.first().map(|item| item.id.clone());
            if let Some(header) =
                history_header(self.loading_older, oldest, &palette, &self.data, cx)
            {
                column = column.child(header);
            }
        }

        let view = view.clone();
        column
            .child(
                MessageScroller::new(
                    "transcript",
                    self.scroller.clone(),
                    move |index, _window, cx| {
                        data.update(cx, |data, cx| rows::render_row(data, index, &view, cx))
                    },
                )
                .with_list_style(StyleRefinement::default().px_4().py_4())
                .with_row_style(StyleRefinement::default().pb_0())
                .with_jump_button_renderer(|button| button.small().label("Jump to latest"))
                .with_jump_button_style(
                    StyleRefinement::default()
                        .bg(theme.secondary)
                        .border_color(theme.border)
                        .text_color(theme.secondary_foreground)
                        .shadow_sm(),
                )
                .with_jump_button_label("Jump to latest")
                .size_full()
                .min_h_0(),
            )
            .into_any_element()
    }
}
