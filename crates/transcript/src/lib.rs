//! transcript — the center column of a tab: one agent's items (§2.8, §7.3).
//!
//! The view is fed **items** from the `session` crate, keyed by the stable id the server
//! minted (CONTRACT §4.1), and owns four pieces of retained state:
//!
//! * one [`TextViewState`] per assistant item that has been on screen, keyed by item id,
//!   extended with `set_text` so markdown is **rendered live while it streams** — never
//!   recreated per delta, and never a second parse for a delta that arrived between two
//!   frames;
//! * the reader's place in the list, which is [`pin`]'s: one agent's transcript opens at
//!   its latest item and stays there while the pane changes height, and only the reader's
//!   own scroll unpins it (the design's `Transcript.tsx`, to the pixel and the
//!   millisecond);
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

mod imgcheck;
mod link;
mod markdown;
pub mod pin;
mod rows;
mod style;
mod todo;

#[cfg(test)]
mod tests;

pub use imgcheck::decode_image;
pub use todo::TodoPanel;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TextViewState;
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName};
use gpui_kit::{
    div, linear_color_stop, linear_gradient, point, px, Animation, AnimationExt as _, AnyElement,
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _, WeakEntity, Window,
};
use session::{AgentKey, Item, ItemId, ItemKind, Todo};
use std::time::Duration;

use crate::pin::{Action, Pin};
use crate::rows::CopyFeedback;
use crate::style::Palette;
use store::design::{INSET, MEASURE};

/// How many assistant items keep their parsed document.
const KEPT_DOCUMENTS: usize = 128;

/// A handler that names the item it is about: the tool whose whole output is wanted, the
/// queued input to take back, or the oldest item the scrollback pages back from. Every one
/// is a read or an op on the server, which the owner (the tab's transport) performs.
pub type ItemHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A handler that names one image: the item it belongs to, and which of that item's
/// images. The bytes are fetched with `GET /media/<id>/<n>` and handed back through
/// [`TranscriptView::set_image`].
pub type ImageHandler = Rc<dyn Fn(&str, u32, &mut Window, &mut App)>;

/// What is known about one image an item carries.
#[derive(Clone)]
pub(crate) enum ImageState {
    /// Asked for, not here yet.
    Loading,
    /// Decoded, once.
    Ready(Arc<gpui_kit::RenderImage>),
    /// The fetch or the decode failed: the row says so calmly rather than showing nothing.
    Failed,
}

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
    pub(crate) on_fetch_image: RefCell<Option<ImageHandler>>,
    /// What is known about each image, keyed by the item that carries it and the image's
    /// own index. Decoded once, kept for the life of the view.
    pub(crate) images: HashMap<(ItemId, u32), ImageState>,
    /// The images the reader opened at full size.
    pub(crate) full_images: HashSet<(ItemId, u32)>,
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

/// One agent's transcript: a tail-following virtual list of items.
pub struct TranscriptView {
    data: Entity<TranscriptData>,
    /// The reader's place in the list: [`Pin`] decides it, and this is what it
    /// moves.
    scroll: ScrollHandle,
    pin: Pin,
    /// The step-by-step return to the latest, while one is running.
    jump: Option<Task<()>>,
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
                on_fetch_image: RefCell::new(None),
                images: HashMap::new(),
                full_images: HashSet::new(),
            }),
            scroll: ScrollHandle::new(),
            pin: Pin::new(),
            jump: None,
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

    /// Show another agent's transcript. The new one opens at its latest item, as a
    /// transcript does when it is first shown — however far up the last one's
    /// reader had scrolled.
    pub fn set_agent(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.agent = agent;
        self.pin.reset();
        self.scroll.scroll_to_bottom();
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

    /// What the owner does when an image row is on screen for the first time: one media
    /// fetch per image, and only for images somebody is looking at.
    pub fn on_fetch_image(
        &mut self,
        handler: impl Fn(&str, u32, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let handler: ImageHandler = Rc::new(handler);
        self.data.update(cx, |data, _| {
            *data.on_fetch_image.borrow_mut() = Some(handler)
        });
    }

    /// The bytes of one image, decoded. The owner fetches them (`GET /media/<id>/<n>`)
    /// and decodes off the UI thread; the view only caches what it is handed.
    pub fn set_image(
        &mut self,
        id: &str,
        n: u32,
        image: Arc<gpui_kit::RenderImage>,
        cx: &mut Context<Self>,
    ) {
        self.data.update(cx, |data, _| {
            data.images
                .insert((id.to_string(), n), ImageState::Ready(image));
        });
        cx.notify();
    }

    /// The image could not be fetched or decoded: the row says so, once.
    pub fn set_image_failed(&mut self, id: &str, n: u32, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.images.insert((id.to_string(), n), ImageState::Failed);
        });
        cx.notify();
    }

    /// Open one image at full size, or close it again. A thumbnail is a thumbnail: the
    /// reader says when they want the whole picture, rather than a row taking the column.
    pub(crate) fn toggle_image_size(&mut self, id: &str, n: u32, cx: &mut Context<Self>) {
        let key = (id.to_string(), n);
        self.data.update(cx, |data, _| {
            if !data.full_images.remove(&key) {
                data.full_images.insert(key);
            }
        });
        cx.notify();
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
        self.data.update(cx, |data, _| {
            data.items = items.into_iter().filter(is_part_of_the_record).collect();
            data.retain_documents();
        });
        cx.notify();
    }

    /// Put older items in front of what is held — the scrollback walking back through
    /// compactions. An item already held is not added twice.
    pub fn prepend(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let added = self.data.update(cx, |data, _| {
            let fresh: Vec<Item> = items
                .into_iter()
                .filter(is_part_of_the_record)
                .filter(|item| !data.items.iter().any(|held| held.id == item.id))
                .collect();
            if fresh.is_empty() {
                return 0;
            }
            let added = fresh.len();
            let mut next = fresh;
            next.append(&mut data.items);
            data.items = next;
            data.retain_documents();
            added
        });
        if added > 0 {
            cx.notify();
        }
    }

    /// Add or replace one item, by its id. Returns whether the view changed.
    ///
    /// A notice the server does not keep in the journal is not part of the
    /// conversation: it is a status line about the machine — `session ready`, at
    /// the top of every session — and it is dropped here rather than drawn at the
    /// head of every transcript.
    pub fn upsert(&mut self, item: Item, cx: &mut Context<Self>) -> bool {
        if !is_part_of_the_record(&item) {
            return false;
        }
        let changed = self
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
                    // already drawn) needs no re-render: only its own cell changed.
                    if waiting {
                        return false;
                    }
                    true
                }
                None => {
                    data.items.push(item);
                    true
                }
            });

        if !changed {
            return false;
        }
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
        if index.is_none() {
            return false;
        }
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
        self.pin.reset();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Whether the list is following its tail.
    pub fn is_following_tail(&self, _cx: &App) -> bool {
        self.pin.is_pinned()
    }

    /// Whether "↓ Jump to latest" is showing.
    pub fn is_away_from_latest(&self, _cx: &App) -> bool {
        self.pin.is_away()
    }

    /// Take the reader back to the latest item: follow the tail again, and go
    /// there — not in a jump, which is what the design's `smooth` asks for.
    pub fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.pin.jumped();
        cx.notify();
        let scroll = self.scroll.clone();
        let from = f32::from(scroll.offset().y);
        let to = f32::from(scroll.max_offset().y);
        self.jump = Some(cx.spawn(async move |_view, cx| {
            for step in 1..=JUMP_STEPS {
                cx.background_executor().timer(JUMP_STEP).await;
                let t = step as f32 / JUMP_STEPS as f32;
                let y = from + (to - from) * ease_out(t);
                scroll.set_offset(point(scroll.offset().x, px(y)));
            }
        }));
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
}

/// Whether an item belongs in the conversation.
///
/// Only one kind is ever dropped: a notice the server itself does not keep
/// (`durable: false`, §4.1) — a line about the machine saying it is ready, said
/// again at every boot. Everything else, including a durable notice, is the
/// record.
fn is_part_of_the_record(item: &Item) -> bool {
    !matches!(&item.kind, ItemKind::Notice(notice) if !notice.durable)
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
    view: &WeakEntity<TranscriptView>,
    cx: &App,
) -> Option<AnyElement> {
    let handler = data.read(cx).on_load_older.borrow().clone()?;
    let oldest = oldest?;
    let view = view.clone();
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
                    .on_click(move |_, window, cx| {
                        // The page is in flight from here: the header says so until the
                        // owner answers (which is a snapshot of the older items).
                        let _ = view.update(cx, |view, cx| view.set_history(true, true, cx));
                        handler(&oldest, window, cx)
                    })
                    .child(label)
                    .test_support(),
            )
            .into_any_element(),
    )
}

impl Render for TranscriptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let palette = Palette::from_app(cx);

        if self.data.read(cx).items.is_empty() {
            return empty_state(self.agent, &palette).into_any_element();
        }

        self.data.update(cx, |data, _| data.begin_frame());

        // The images on screen, asked for once each: a media fetch per image, and only
        // for a row somebody is looking at.
        let wanted = {
            let data = self.data.read(cx);
            let handler = data.on_fetch_image.borrow().is_some();
            if handler {
                data.items
                    .iter()
                    .flat_map(|item| match &item.kind {
                        ItemKind::User(user) => user
                            .images
                            .iter()
                            .enumerate()
                            .map(|(n, _)| (item.id.clone(), n as u32))
                            .collect::<Vec<_>>(),
                        _ => Vec::new(),
                    })
                    .filter(|key| !data.images.contains_key(key))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        if !wanted.is_empty() {
            let handler = self
                .data
                .read(cx)
                .on_fetch_image
                .borrow()
                .clone()
                .expect("checked above");
            for (id, n) in wanted {
                self.data.update(cx, |data, _| {
                    data.images.insert((id.clone(), n), ImageState::Loading)
                });
                handler(&id, n, _window, cx);
            }
        }

        let rows_held = self.data.read(cx).items.len();
        let before = {
            let data = self.data.clone();
            let view = cx.weak_entity();
            let mut rows = Vec::with_capacity(rows_held);
            for index in 0..rows_held {
                rows.push(data.update(cx, |data, cx| rows::render_row(data, index, &view, cx)));
            }
            rows
        };

        let mut measure = div()
            .id("transcript-measure")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(MEASURE))
            .px(px(INSET))
            .children(before);
        if self.has_older {
            let oldest = self.data.read(cx).items.first().map(|item| item.id.clone());
            let weak = cx.weak_entity();
            if let Some(header) =
                history_header(self.loading_older, oldest, &palette, &self.data, &weak, cx)
            {
                measure = measure.child(header);
            }
        }
        let content = div()
            .w_full()
            .flex()
            .justify_center()
            .pt(px(INSET))
            .pb(px(INSET))
            .child(measure);

        // Every frame the list is painted is a chance for the design's rule to run
        // — the same chance a scroll event gives it in a browser, and the one that
        // catches a pane that changed height under a reader who is following.
        let on_painted = {
            let weak = cx.weak_entity();
            move |_bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |view, cx| view.on_painted(cx));
            }
        };

        // The reader's own scroll: a wheel, a touch, a key or a pointer press opens
        // the design's 500ms window in which a scroll is theirs.
        let touched = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::ScrollWheelEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.pin.touched();
                cx.notify();
            },
        );
        let pressed = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::MouseDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.pin.touched();
                cx.notify();
            },
        );
        let keyed = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::KeyDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.pin.touched();
                cx.notify();
            },
        );

        let scroll = div()
            .on_children_prepainted(on_painted)
            .id("transcript-scroll")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll_wheel(touched)
            .on_mouse_down(MouseButton::Left, pressed)
            .on_key_down(keyed)
            .child(content);

        // The 20px fade the design puts over the last of the list, so a row leaving
        // the top of the dock is not cut off mid-line.
        let background: Hsla = theme.background;
        let fade = div()
            .id("transcript-fade")
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .h(px(FADE_HEIGHT))
            .bg(linear_gradient(
                180.,
                linear_color_stop(
                    Hsla {
                        a: 0.,
                        ..background
                    },
                    0.,
                ),
                linear_color_stop(background, 1.),
            ));

        div()
            .id("transcript")
            .test_support()
            .relative()
            .size_full()
            .min_h_0()
            .child(scroll)
            .child(fade)
            .children(self.jump_pill(cx))
            .into_any_element()
    }
}

/// The height of the fade over the list's last pixels.
const FADE_HEIGHT: f32 = 20.;

/// How long "↓ Jump to latest" takes to come and go, and how far it lifts.
const JUMP_FADE: Duration = Duration::from_millis(160);
const JUMP_LIFT: f32 = 6.;

/// How long "↓ Jump to latest" takes to come and go, and how far it lifts.
impl TranscriptView {
    /// The design's `↓ Jump to latest`, shown only once the reader is [`pin::JUMP_AT`]
    /// away.
    ///
    /// Its two states are one transition each way — `opacity .16s ease, transform .16s
    /// ease`, and 6px of lift — which is what the animation's two ids give it: a state
    /// change starts a fresh 160ms run from the other end.
    fn jump_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let away = self.pin.is_away();
        let palette = Palette::from_app(cx);
        let theme = cx.theme().clone();
        let jump = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::MouseDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.scroll_to_latest(cx);
            },
        );

        let pill = div()
            .id(("transcript-jump", away as usize))
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .px(px(12.))
            .rounded_full()
            .border_1()
            .border_color(palette.border)
            .bg(theme.input)
            .text_size(px(12.))
            .text_color(palette.foreground)
            .shadow_sm()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, jump)
            .child("↓ Jump to latest")
            .test_support()
            .with_animation(
                ("transcript-jump-motion", away as usize),
                Animation::new(JUMP_FADE),
                move |el, delta| {
                    // Away: in, lifted to its place. Back: out, and 6px down.
                    let (from, to) = if away { (0., 1.) } else { (1., 0.) };
                    let t = from + (to - from) * delta;
                    el.opacity(t).relative().top(px(JUMP_LIFT * (1. - t)))
                },
            );

        // Centred on the transcript's own middle, 12px above its bottom, as the design
        // places it. Hidden outright while the reader is at the latest, so it is not a
        // target for a click or a hover.
        let mut slot = div()
            .id("transcript-jump-slot")
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .bottom(px(12.))
            .flex()
            .justify_center()
            .child(pill);
        if !away {
            slot = slot.invisible();
        }
        Some(slot.into_any_element())
    }
}

impl Drop for TranscriptView {
    fn drop(&mut self) {
        // The return to the latest, if one is running, ends with the view.
        self.jump.take();
    }
}

/// What the design's rule makes of the reader's distance from the bottom.
///
/// The distance is the scroll's own: gpui keeps the offset from the top as a
/// negative number and the greatest offset as a positive one, so their sum is
/// `scrollHeight - scrollTop - clientHeight` exactly — 0 at the bottom, growing as
/// the reader goes up, and 0 for a list shorter than its pane.
impl TranscriptView {
    fn on_painted(&mut self, cx: &mut Context<Self>) {
        let gap = f32::from(self.scroll.max_offset().y + self.scroll.offset().y);
        let away = self.pin.is_away();
        match self.pin.on_scroll(gap) {
            // The layout moved under a reader who is following: pull the list back
            // rather than letting their place drift.
            Action::SnapToBottom => self.scroll.scroll_to_bottom(),
            Action::Leave => {}
        }
        if self.pin.is_away() != away {
            cx.notify();
        }
    }
}

/// The return to the latest, step by step: `smooth` in the design's `scrollTo`.
const JUMP_STEPS: u32 = 12;
const JUMP_STEP: Duration = Duration::from_millis(15);

/// A decelerating ramp for the return — `ease-out`, which is what a scroll on a
/// trackpad ends like.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t) * (1. - t)
}
