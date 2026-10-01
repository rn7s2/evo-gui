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
//! * the **window** over the record: the whole of the topic is held, so a later op can
//!   move a row that is off screen, but only [`WINDOW_ITEMS`] of its newest rows are
//!   built, and "Earlier items" opens the rows behind them a page at a time;
//! * the tools the reader opened, and the untruncated results fetched for them.
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
mod math;
pub mod normalize_math;
pub mod pin;
mod rows;
mod style;

#[cfg(test)]
mod tests;

pub use imgcheck::decode_image;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TextViewState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName};
use gpui_kit::{
    div, linear_color_stop, linear_gradient, point, px, Animation, AnimationExt as _, AnyElement,
    App, AppContext as _, Bounds, BoxShadow, Context, Entity, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _,
    WeakEntity, Window,
};
use session::{AgentKey, Item, ItemId, ItemKind, PAGE_ITEMS};
use std::time::Duration;
use widgets::effort::cubic_bezier;
use widgets::paint;

use crate::pin::{Action, Pin};
use crate::rows::CopyFeedback;
use crate::style::Palette;
use store::design::{palette as design_palette, INSET, MEASURE};

/// How many items a live transcript draws: the newest whole page of the record.
///
/// The record itself is kept whole — every op works on all of it — but only this many
/// of its newest items are built into rows, and each "Earlier items" the reader asks
/// for opens another page behind them. The number is the session's own page size, so a
/// window and a fetched page are the same handful of rows.
const WINDOW_ITEMS: usize = PAGE_ITEMS as usize;

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
    /// The whole record, exactly as the topic holds it: every op lands here, whether or
    /// not the row it moves is on screen.
    pub(crate) items: Vec<Item>,
    /// The rows the list draws: `items[shown]`. Everything in front of it is held, not
    /// built — older items the reader has not asked for — and everything behind it is a
    /// row that arrived while they were reading.
    pub(crate) shown: Range<usize>,
    /// Where each item is, so an op finds its row without walking the record:
    /// `items` is the whole topic, and a delta must not cost a scan of it.
    index: HashMap<ItemId, usize>,
    /// How many turns have opened by `items[i]`. A turn rule counts from the head of
    /// the record, so its number does not move when the window does — and does not
    /// cost a walk of the hidden rows in front of it either.
    turns: Vec<usize>,
    /// How many items of the record carry thinking text. Counted as the record changes,
    /// because the control that reveals it is drawn from the header of every frame and
    /// the record behind the window is not walked for it.
    thinking: usize,
    /// Retained markdown documents of assistant items, keyed by item id.
    pub(crate) documents: HashMap<ItemId, Entity<TextViewState>>,
    /// Retained markdown documents of a lane report's own fields, keyed by the
    /// item and the field's label: `.rp-row`'s body is markdown in the design.
    pub(crate) field_documents: HashMap<(ItemId, &'static str), Entity<TextViewState>>,
    /// Items the reader has opened.
    pub(crate) expanded: HashSet<ItemId>,
    /// The whole of a tool call's result, for the calls the server shortened: fetched with
    /// `GET /items/<id>` the first time the reader opens one.
    pub(crate) full_results: HashMap<ItemId, String>,
    pub(crate) show_thinking: bool,
    /// Whether the agent is running (`status: running`): the list's foot shows the
    /// working pips until a row of the turn shows the work itself.
    pub(crate) running: bool,
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
        self.index.get(id).copied()
    }

    /// How many turns have opened by `items[index]` — the number the rule above that
    /// row is labelled with, counted from the head of the record.
    pub(crate) fn turn_number(&self, index: usize) -> usize {
        self.turns.get(index).copied().unwrap_or(0)
    }

    /// How many items are held in front of the window.
    pub(crate) fn hidden_before(&self) -> usize {
        self.shown.start
    }

    /// Restate where every item is and how many turns have opened, after the record
    /// itself has changed shape (a snapshot, a page, a row dropped, a turn opened).
    fn reindex(&mut self) {
        self.index.clear();
        self.turns.clear();
        self.index.reserve(self.items.len());
        self.turns.reserve(self.items.len());
        let mut turns = 0;
        let mut thinking = 0;
        for (index, item) in self.items.iter().enumerate() {
            self.index.insert(item.id.clone(), index);
            if rows::opens_a_turn(&item.kind) {
                turns += 1;
            }
            if carries_thinking(&item.kind) {
                thinking += 1;
            }
            self.turns.push(turns);
        }
        self.thinking = thinking;
    }

    /// The cheap half of [`TranscriptData::reindex`]: one item appended at the end,
    /// which is the only thing a live transcript's record usually does.
    fn index_last(&mut self) {
        let index = self.items.len() - 1;
        let item = &self.items[index];
        self.index.insert(item.id.clone(), index);
        let turns =
            self.turns.last().copied().unwrap_or(0) + usize::from(rows::opens_a_turn(&item.kind));
        self.turns.push(turns);
        self.thinking += usize::from(carries_thinking(&item.kind));
    }

    /// Draw `items[start..end]`, and nothing else.
    ///
    /// A row the window has left gives up what belongs to it — its parsed document and
    /// its decoded images: both are made again if the reader comes back to it, which is
    /// cheaper than holding one of each for every item of a record that can be tens of
    /// thousands of rows long.
    ///
    /// Returns whether the window moved.
    fn set_window(&mut self, start: usize, end: usize) -> bool {
        let end = end.min(self.items.len());
        let start = start.min(end);
        if self.shown.start == start && self.shown.end == end {
            return false;
        }
        self.shown = start..end;
        let ids: HashSet<&str> = self.items[start..end]
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        self.documents.retain(|id, _| ids.contains(id.as_str()));
        self.field_documents
            .retain(|(id, _), _| ids.contains(id.as_str()));
        self.images.retain(|(id, _), _| ids.contains(id.as_str()));
        self.full_images.retain(|(id, _)| ids.contains(id.as_str()));
        true
    }

    /// The window a reader who is following the tail sees: the newest whole page.
    fn window_to_tail(&mut self, page: usize) -> bool {
        let end = self.items.len();
        self.set_window(end.saturating_sub(page), end)
    }

    /// A window is never empty while the record has rows: every op that could leave one
    /// empty — a row removed, a page put in front of an empty window — opens the newest
    /// page instead. Returns whether it moved.
    fn repair_window(&mut self) -> bool {
        if self.shown.is_empty() && !self.items.is_empty() {
            return self.window_to_tail(WINDOW_ITEMS);
        }
        false
    }

    /// Bring `items[index]`'s document up to date, creating it the first time the row is on
    /// screen.
    pub(crate) fn sync_document(&mut self, index: usize, cx: &mut Context<Self>) {
        let item = &self.items[index];
        let ItemKind::Assistant(assistant) = &item.kind else {
            return;
        };
        let id = item.id.clone();
        // TeX's `\(…\)` and `\[…\]` are made visible to the Markdown parser
        // here, before the text becomes a document: the pass is a pure function of
        // the message, so a streamed one settles on the same document it would
        // have had all at once.
        let text = normalize_math::normalize(&assistant.text);
        match self.documents.get(&id).cloned() {
            Some(document) => document.update(cx, |state, cx| state.set_text(&text, cx)),
            None => {
                let document =
                    cx.new(|cx| TextViewState::markdown(&text, cx).motion(rows::stream_motion()));
                self.documents.insert(id, document);
            }
        }
    }

    /// Drop what belongs to items the record no longer holds: a snapshot can leave the
    /// whole of the last one behind, and a row's own documents, its images and the
    /// whole output the reader had fetched for it go with the row.
    fn retain_documents(&mut self) {
        let held: HashSet<&str> = self.items.iter().map(|item| item.id.as_str()).collect();
        self.documents.retain(|id, _| held.contains(id.as_str()));
        self.field_documents
            .retain(|(id, _), _| held.contains(id.as_str()));
        self.images.retain(|(id, _), _| held.contains(id.as_str()));
        self.full_images
            .retain(|(id, _)| held.contains(id.as_str()));
        self.full_results.retain(|id, _| held.contains(id.as_str()));
        self.expanded.retain(|id| held.contains(id.as_str()));
    }

    /// Bring one of a report's fields up to date, creating its document the first
    /// time the row is on screen: the design draws `.rp-row`'s body as markdown
    /// (an evidence line is a list, a path is a code span), not as plain text.
    pub(crate) fn sync_field_document(
        &mut self,
        index: usize,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let item = &self.items[index];
        let ItemKind::LaneReport(report) = &item.kind else {
            return;
        };
        let Some((_, text)) = rows::report_fields(report)
            .into_iter()
            .find(|(name, _)| *name == label)
        else {
            return;
        };
        let key = (item.id.clone(), label);
        let text = normalize_math::normalize(text);
        match self.field_documents.get(&key).cloned() {
            Some(document) => document.update(cx, |state, cx| state.set_text(&text, cx)),
            None => {
                let document = cx.new(|cx| TextViewState::markdown(&text, cx));
                self.field_documents.insert(key, document);
            }
        }
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
    /// Whether the agent's topic has items older than the ones held.
    has_older: bool,
    /// Whether the page the reader asked for is still in flight.
    loading_older: bool,
    /// How many times the reader has scrolled for themselves. A page that lands after
    /// they have gone on reading is a page they have left behind.
    scrolls: u64,
    /// What the last paint left behind: the reader's own scroll count, where the list
    /// was, and how tall it was.
    painted: Painted,
    /// The [`TranscriptView::scrolls`] count when the page in flight was asked for:
    /// the answer belongs to the reader who asked, and only if they are still there.
    awaiting: Option<u64>,
    /// The reader has just been placed at the head of a block they asked for: the next
    /// paint decides their pin from the layout that block makes.
    at_head: bool,
    /// Where the reader's place has to be put back, after rows were opened in front of
    /// them: the distance from the bottom they had.
    anchor: Option<f32>,
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
                shown: 0..0,
                index: HashMap::new(),
                turns: Vec::new(),
                thinking: 0,
                documents: HashMap::new(),
                field_documents: HashMap::new(),
                expanded: HashSet::new(),
                full_results: HashMap::new(),
                show_thinking: false,
                running: false,
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
            has_older: false,
            loading_older: false,
            scrolls: 0,
            painted: Painted::default(),
            awaiting: None,
            at_head: false,
            anchor: None,
            agent: AgentKey::Coordinator,
        }
    }

    /// Whose transcript this view shows.
    pub fn agent(&self) -> AgentKey {
        self.agent
    }

    /// Show another agent's transcript. The new one opens at its latest item, as a
    /// transcript does when it is first shown — however far up the last one's
    /// reader had scrolled, and however many pages they had opened in front of it.
    pub fn set_agent(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.agent = agent;
        self.pin.reset();
        self.awaiting = None;
        self.at_head = false;
        self.anchor = None;
        self.data
            .update(cx, |data, _| data.window_to_tail(WINDOW_ITEMS));
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The items currently shown, in order.
    pub fn items<'a>(&'a self, cx: &'a App) -> &'a [Item] {
        &self.data.read(cx).items
    }

    /// Whether the agent is running. While it is, the transcript shows its working
    /// pips from the moment the request starts — before the first delta, through
    /// reasoning the provider does not stream, and between a tool's result and the
    /// next request — not only once a message is streaming.
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        let changed = self.data.update(cx, |data, _| {
            std::mem::replace(&mut data.running, running) != running
        });
        if changed {
            cx.notify();
        }
    }

    pub fn is_running(&self, cx: &App) -> bool {
        self.data.read(cx).running
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
    ///
    /// A page that is no longer in flight is a page that has settled — answered, empty,
    /// or given up on — and the ask it belonged to goes with it: the next prepend is
    /// somebody else's, and is not the answer to an old press.
    pub fn set_history(&mut self, has_older: bool, loading: bool, cx: &mut Context<Self>) {
        if !loading {
            self.awaiting = None;
        }
        if self.has_older == has_older && self.loading_older == loading {
            return;
        }
        self.has_older = has_older;
        self.loading_older = loading;
        cx.notify();
    }

    /// Replace the whole list, as a snapshot or a `topic.reset` does. The window opens
    /// on the newest page again, as it does on a transcript just shown.
    pub fn replace(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.items = items.into_iter().filter(is_part_of_the_record).collect();
            data.reindex();
            data.retain_documents();
            data.window_to_tail(WINDOW_ITEMS);
        });
        self.awaiting = None;
        self.at_head = false;
        self.anchor = None;
        cx.notify();
    }

    /// Put older items in front of what is held — the scrollback walking back through
    /// compactions. An item already held is not added twice.
    ///
    /// A page the reader asked for and is still waiting for opens in front of them,
    /// with its own first row at the head of the list. Anything else — a page that
    /// arrived after they went back to the latest, or after they had read on — is held
    /// in front of the window without being shown, and their place does not move.
    pub fn prepend(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let asked_for = self.awaiting.take().is_some_and(|at| at == self.scrolls);
        let following = self.pin.is_pinned();
        let moved = self.data.update(cx, |data, _| {
            let fresh: Vec<Item> = items
                .into_iter()
                .filter(is_part_of_the_record)
                .filter(|item| data.index_of(&item.id).is_none())
                .collect();
            if fresh.is_empty() {
                return None;
            }
            let added = fresh.len();
            let mut next = fresh;
            next.append(&mut data.items);
            data.items = next;
            data.reindex();
            if asked_for {
                // The page they asked for: it is opened in front of them.
                let end = data.shown.end + added;
                Some(data.set_window(0, end))
            } else {
                // Someone else's page: held in front of the window, not opened under a
                // reader who is reading.
                let (start, end) = (data.shown.start, data.shown.end);
                let moved = data.set_window(start + added, end + added);
                Some(moved || data.repair_window())
            }
        });
        let Some(moved) = moved else {
            return;
        };
        if asked_for {
            self.open_at_head(cx);
        } else if moved && !following {
            self.hold_anchor();
            self.pin.settled();
        }
        cx.notify();
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
        // Whether the reader is following the tail decides what a new item does to the
        // window: it slides under a follower, and is held behind a reader who is
        // somewhere else in the record.
        let following = self.pin.is_pinned();
        let (changed, moved) = self
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
                    let opened = rows::opens_a_turn(&data.items[index].kind);
                    let thought = carries_thinking(&data.items[index].kind);
                    data.items[index] = item;
                    // A queued input evo has taken opens a turn, and thinking arriving on
                    // a message is one the reader can reveal: the counts behind it are
                    // restated.
                    if rows::opens_a_turn(&data.items[index].kind) != opened
                        || carries_thinking(&data.items[index].kind) != thought
                    {
                        data.reindex();
                    }
                    // A row whose content is unchanged (a status flip, or an op the view has
                    // already drawn) needs no re-render: only its own cell changed.
                    (!waiting, false)
                }
                None => {
                    data.items.push(item);
                    data.index_last();
                    // Following the tail, the window slides: the newest row is drawn
                    // and the oldest one it was holding is let go. A reader who has
                    // opened pages of their own keeps them, and one who is reading
                    // somewhere else is not moved at all.
                    let moved = if data.shown.is_empty()
                        || (following && data.shown.len() <= WINDOW_ITEMS)
                    {
                        data.window_to_tail(WINDOW_ITEMS)
                    } else {
                        false
                    };
                    (true, moved)
                }
            });

        if moved {
            // Hiding the oldest row of the window is the view's own doing: the scroll
            // it causes must not be read as the reader's, whatever they last touched.
            self.pin.settled();
        }
        if !changed {
            return false;
        }
        cx.notify();
        true
    }

    /// Drop one item the topic no longer has.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let removed = self.data.update(cx, |data, _| {
            let index = data.index_of(id)?;
            // A row the reader cannot see at all — in front of every row the window
            // draws — takes its own height off the content above them: their place has
            // to be put back where it was once the shorter layout is there.
            let in_front = index < data.shown.start;
            data.items.remove(index);
            data.reindex();
            // The window keeps its place around the hole: rows in front of it are one
            // nearer the head, and a row inside it leaves one fewer.
            let (start, end) = (data.shown.start, data.shown.end);
            let (start, end) = if index < start {
                (start - 1, end - 1)
            } else if index < end {
                (start, end - 1)
            } else {
                (start, end)
            };
            data.set_window(start, end);
            data.repair_window();
            data.documents.remove(id);
            data.field_documents.retain(|(held, _), _| held != id);
            data.expanded.remove(id);
            data.full_results.remove(id);
            Some(in_front)
        });
        let Some(in_front) = removed else {
            return false;
        };
        if in_front && !self.pin.is_pinned() {
            self.hold_anchor();
            self.pin.settled();
        }
        cx.notify();
        true
    }

    /// Drop every item (a session switch, before the new snapshot lands).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.items.clear();
            data.shown = 0..0;
            data.index.clear();
            data.turns.clear();
            data.thinking = 0;
            data.documents.clear();
            data.field_documents.clear();
            data.expanded.clear();
            data.full_results.clear();
            data.images.clear();
            data.full_images.clear();
        });
        self.pin.reset();
        self.awaiting = None;
        self.at_head = false;
        self.anchor = None;
        self.jump.take();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Whether the list is following its tail.
    pub fn is_following_tail(&self, _cx: &App) -> bool {
        self.pin.is_pinned()
    }

    /// Whether "↓ Jump to latest" is showing: the reader is away from the bottom, or
    /// the window is holding newer rows back — output that arrived while they were
    /// reading, which is below everything on screen.
    pub fn is_away_from_latest(&self, cx: &App) -> bool {
        let data = self.data.read(cx);
        self.pin.is_away() || data.shown.end < data.items.len()
    }

    /// Take the reader back to the latest item: follow the tail again, and go
    /// there — not in a jump, which is what the design's `smooth` asks for.
    ///
    /// The window collapses to the newest page as it does, and a page the reader had
    /// asked for is forgotten: an answer that arrives after this is held in front of
    /// them, not opened under them.
    pub fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.pin.jumped();
        cx.notify();
        if self.collapse(cx) {
            // The window changed: the rows the offset was measured against are gone,
            // so the list lands on the tail in one step rather than easing through a
            // layout that is no longer there.
            self.jump.take();
            self.scroll.scroll_to_bottom();
            return;
        }
        let scroll = self.scroll.clone();
        // gpui's offset grows *negative* as the list scrolls down (`set_offset`: "as
        // you scroll further down the offset becomes more negative"), so the tail is
        // at `-max_offset`. Aiming at `+max_offset` clamped to 0 — the head.
        let from = f32::from(scroll.offset().y);
        let to = -f32::from(scroll.max_offset().y);
        self.jump = Some(cx.spawn(async move |view, cx| {
            for step in 1..=JUMP_STEPS {
                cx.background_executor().timer(JUMP_STEP).await;
                let t = step as f32 / JUMP_STEPS as f32;
                let y = from + (to - from) * ease_out(t);
                scroll.set_offset(point(scroll.offset().x, px(y)));
                let _ = view.update(cx, |_, cx| cx.notify());
            }
            // Land exactly on the tail, even if it grew while we travelled.
            scroll.scroll_to_bottom();
            let _ = view.update(cx, |_, cx| cx.notify());
        }));
    }

    pub fn is_showing_thinking(&self, cx: &App) -> bool {
        self.data.read(cx).show_thinking
    }

    /// Whether this agent has any thinking text to reveal.
    pub fn has_thinking(&self, cx: &App) -> bool {
        self.data.read(cx).thinking > 0
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

/// The window: how much of the record is built, and how the reader asks for more of it.
impl TranscriptView {
    /// The reader did something that scrolls: a wheel, a touch, a key, a pointer press.
    /// The design's 500ms window opens, and the view remembers that they have moved —
    /// a page that arrives afterwards is a page they have left behind.
    fn touched(&mut self) {
        self.scrolls += 1;
        self.pin.touched();
    }

    /// How far the reader is from the bottom, as the pin's rule measures it.
    fn gap(&self) -> f32 {
        f32::from(self.scroll.max_offset().y + self.scroll.offset().y)
    }

    /// The height of everything the list draws, and of the pane it is drawn in.
    fn sizes(&self) -> (f32, f32) {
        let pane = f32::from(self.scroll.bounds().size.height);
        (f32::from(self.scroll.max_offset().y) + pane, pane)
    }

    /// Put a block of rows the reader asked for at the head of the list: the block's
    /// own first row is the first row drawn, so the top of the content is its start.
    /// The pin is re-decided on the next paint, when there is a real distance to
    /// decide from — the layout at this moment is still the old one.
    fn open_at_head(&mut self, cx: &mut Context<Self>) {
        self.anchor = None;
        self.at_head = true;
        self.scroll
            .set_offset(point(self.scroll.offset().x, px(0.)));
        cx.notify();
    }

    /// Remember the reader's place, before a page nobody asked for is held in front of
    /// it: gpui measures the offset from the *head* of the content, so rows added above
    /// a reader move what they are looking at unless the distance from the bottom is
    /// put back. A reader already at the bottom has no place to hold.
    fn hold_anchor(&mut self) {
        let gap = self.gap();
        if gap > 0. {
            self.anchor = Some(gap);
        }
    }

    /// Back at the latest: the window is the newest page again, and a page the reader
    /// had asked for is forgotten — an answer that arrives after this is held in front
    /// of them rather than opened under them. Returns whether the window moved.
    fn collapse(&mut self, cx: &mut Context<Self>) -> bool {
        self.awaiting = None;
        self.anchor = None;
        self.at_head = false;
        self.data
            .update(cx, |data, _| data.window_to_tail(WINDOW_ITEMS))
    }

    /// The reader asked for more of the record.
    ///
    /// The rows the window is holding back come first: they are already in hand, so
    /// they open at once, and the topic's own scrollback is not asked for until the
    /// reader has seen all of them. That ask goes out once, however many times the
    /// header is pressed while a page is in flight.
    fn load_older(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.data.read(cx).hidden_before() > 0 {
            let moved = self.data.update(cx, |data, _| {
                let start = data.shown.start.saturating_sub(WINDOW_ITEMS);
                data.set_window(start, data.shown.end)
            });
            if moved {
                self.open_at_head(cx);
            }
            return;
        }
        if self.loading_older {
            // A page is on its way: asking again would only duplicate it.
            return;
        }
        let handler = self.data.read(cx).on_load_older.borrow().clone();
        let Some(handler) = handler else {
            return;
        };
        let oldest = self.data.read(cx).items.first().map(|item| item.id.clone());
        let Some(oldest) = oldest else {
            return;
        };
        self.awaiting = Some(self.scrolls);
        self.set_history(true, true, cx);
        handler(&oldest, window, cx);
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

/// Whether an item carries thinking text a reader could reveal. The view counts these
/// as the record changes, so the control that reveals them costs no walk of the record
/// — and a row behind the window still counts, since what it reveals is the transcript,
/// not the window.
fn carries_thinking(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Assistant(assistant) if !assistant.thinking.is_empty())
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
/// It stands for two things at once, in this order: the rows the window is holding back,
/// which open at once, and — once those are all on screen — the items the *topic* still
/// has behind them, which are a page the owner fetches. It says how far back it goes
/// rather than how many items are missing: the server counts what it cut off, which is
/// not a number a reader has a use for.
fn history_header(
    loading: bool,
    palette: &Palette,
    view: &WeakEntity<TranscriptView>,
) -> AnyElement {
    let view = view.clone();
    let label = if loading {
        "Loading earlier items…"
    } else {
        "Earlier items"
    };
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
                    let _ = view.update(cx, |view, cx| view.load_older(window, cx));
                })
                .child(label)
                .test_support(),
        )
        .into_any_element()
}

impl Render for TranscriptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let palette = Palette::from_app(cx);

        if self.data.read(cx).items.is_empty() {
            return empty_state(self.agent, &palette).into_any_element();
        }

        // The images of the rows the window draws, asked for once each: a media fetch
        // per image, and only for an image somebody is looking at.
        let wanted = {
            let data = self.data.read(cx);
            if data.on_fetch_image.borrow().is_none() {
                Vec::new()
            } else {
                data.items[data.shown.clone()]
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

        // Only the window is built into rows: the record behind it is held for the ops
        // that move it, and building the whole of it — as the list did before there was
        // a window — costs a parse and a layout per item every frame.
        let shown = self.data.read(cx).shown.clone();
        let before = {
            let data = self.data.clone();
            let view = cx.weak_entity();
            let mut rows = Vec::with_capacity(shown.len());
            for index in shown {
                rows.push(data.update(cx, |data, cx| rows::render_row(data, index, &view, cx)));
            }
            rows
        };

        // The way back into the record heads the list: the rows the window is holding
        // back, and then the topic's own scrollback behind them.
        let header = if self.data.read(cx).hidden_before() > 0 || self.has_older {
            let weak = cx.weak_entity();
            Some(history_header(self.loading_older, &palette, &weak))
        } else {
            None
        };
        let measure = div()
            .id("transcript-measure")
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(MEASURE))
            .px(px(INSET))
            .children(header)
            .children(before)
            .children(rows::pending_row(self.data.read(cx), cx));
        // Every frame the list is painted is a chance for the design's rule to run
        // — the same chance a scroll event gives it in a browser, and the one that
        // catches a pane that changed height under a reader who is following.
        let on_painted = {
            let weak = cx.weak_entity();
            move |_bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |view, cx| view.on_painted(cx));
            }
        };

        let content = div()
            .w_full()
            .flex()
            .justify_center()
            .pt(px(INSET))
            .pb(px(INSET))
            .child(measure);

        // The reader's own scroll: a wheel, a touch, a key or a pointer press opens
        // the design's 500ms window in which a scroll is theirs.
        let touched = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::ScrollWheelEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.touched();
                cx.notify();
            },
        );
        let pressed = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::MouseDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.touched();
                cx.notify();
            },
        );
        let keyed = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::KeyDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.touched();
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

        // The scrollbar's overlay goes on the *host* — the box that does not
        // scroll — rather than on the scroller: the kit draws the bar as a child
        // of the element that carries it, so on the scroller it would be translated
        // with the rows. It is driven by the scroller's own handle either way, and
        // the app's scrollbar mode (`app::theme`) shows it only while scrolling.
        div()
            .id("transcript")
            .test_support()
            .relative()
            .size_full()
            .min_h_0()
            .child(scroll)
            .child(fade)
            .children(self.jump_pill(cx))
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }
}

/// The height of the fade over the list's last pixels.
const FADE_HEIGHT: f32 = 20.;

/// How long "↓ Jump to latest" takes to come and go, and how far it lifts.
const JUMP_FADE: Duration = Duration::from_millis(160);
const JUMP_LIFT: f32 = 6.;

/// The ink the pill's shadow is cast in: `.ws-main .jump`'s
/// `rgba(60,40,10,.12)`, the warm ink the design's other shadows use.
const JUMP_SHADOW_INK: paint::Rgb = paint::Rgb::new(0x3C, 0x28, 0x0A);

/// How long "↓ Jump to latest" takes to come and go, and how far it lifts.
impl TranscriptView {
    /// The design's `↓ Jump to latest`, shown only once the reader is [`pin::JUMP_AT`]
    /// away.
    ///
    /// Its two states are one transition each way — `opacity .16s ease, transform .16s
    /// ease`, and 6px of lift — which is what the animation's two ids give it: a state
    /// change starts a fresh 160ms run from the other end.
    fn jump_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let away = self.is_away_from_latest(cx);
        let palette = Palette::from_app(cx);
        let jump = cx.listener(
            |view: &mut Self,
             _: &gpui_kit::MouseDownEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                view.scroll_to_latest(cx);
            },
        );

        let pill = div()
            .id("transcript-jump")
            .h(px(28.))
            .flex()
            .items_center()
            .gap_1()
            .px(px(12.))
            .rounded_full()
            .border_1()
            .border_color(palette.border)
            // `.ws-main .jump`: `background:var(--input)`, the design's lightest
            // surface — not the kit theme's own input token, which is the
            // widget border in this app's theme.
            .bg(palette.input)
            .text_size(px(12.))
            .text_color(palette.foreground)
            // `.ws-main .jump`: `box-shadow: 0 2px 8px rgba(60,40,10,.12)`.
            .shadow(vec![BoxShadow::new(
                px(0.),
                px(2.),
                paint::wash(JUMP_SHADOW_INK, 12.),
            )
            .blur_radius(px(8.))])
            .cursor_default()
            .hover(|style| {
                style.bg(paint::color(
                    design_palette(cx.theme().mode.is_dark()).sidebar,
                ))
            })
            .on_mouse_down(MouseButton::Left, jump)
            .child("↓ Jump to latest")
            .test_support()
            .with_animation(
                ("transcript-jump-motion", away as usize),
                // `transition: opacity .16s ease, transform .16s ease`.
                Animation::new(JUMP_FADE).with_easing(cubic_bezier(0.25, 0.1, 0.25, 1.)),
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
        // Rows were opened in front of the reader: put the offset back on the distance
        // from the bottom they had, or the page above them slides what they are reading
        // down the pane.
        if let Some(gap) = self.anchor.take() {
            let y = (gap - f32::from(self.scroll.max_offset().y)).min(0.);
            self.scroll.set_offset(point(self.scroll.offset().x, px(y)));
            self.pin.settled();
        }

        // Whose scroll this frame is — if it is a scroll at all. Only a frame in which
        // the reader's own wheel, key or press arrived, the offset moved, or the list or
        // its pane changed height is one the design's rule has anything to say about;
        // between those the reader's flags stand.
        let now = Painted {
            scrolls: self.scrolls,
            offset: f32::from(self.scroll.offset().y),
            size: self.sizes(),
        };
        let before = std::mem::replace(&mut self.painted, now);
        let by_input = now.scrolls != before.scrolls;
        let scrolled = (now.offset - before.offset).abs() > 0.5;
        let resized = moved_size(now.size, before.size);

        let away = self.pin.is_away();
        if std::mem::take(&mut self.at_head) {
            // The reader has just been placed at the head of a block they asked for.
            // Where that leaves them is the pin's business — and only now, with the
            // block laid out, is there a distance to decide it from.
            self.pin.placed(self.gap());
        } else if by_input || scrolled || resized {
            // A scroll the view saw no input for — a drag of the scroll bar — is still
            // the reader's if the content did not move under them; everything else that
            // is not their own wheel is the layout, and the layout never puts the list
            // back on its tail: the list does not come back to the latest because a row
            // left the record or a pane grew.
            let gap = self.gap();
            let was_pinned = self.pin.is_pinned();
            let action = if by_input || (scrolled && !resized) {
                self.pin.on_scroll(gap)
            } else {
                self.pin.on_layout(gap)
            };
            match action {
                // The layout moved under a reader who is following: pull the list back
                // rather than letting their place drift.
                Action::SnapToBottom => self.scroll.scroll_to_bottom(),
                Action::Leave => {}
            }
            // Back at the latest: the window is the newest page once more, and the list
            // lands against the shorter layout that leaves.
            if self.pin.is_pinned() && !was_pinned && self.collapse(cx) {
                self.scroll.scroll_to_bottom();
            }
        }
        if self.is_away_from_latest(cx) != away {
            cx.notify();
        }
    }
}

/// Where the list was, and how tall it was, at the last paint — and how many of the
/// reader's own scrolls had happened by then.
#[derive(Clone, Copy, Default)]
struct Painted {
    scrolls: u64,
    offset: f32,
    /// The height of what the list draws, and of the pane it is drawn in.
    size: (f32, f32),
}

/// Whether two views of the list's height are the same picture: half a pixel of drift
/// between frames is layout arithmetic, not a row arriving or a pane growing.
fn moved_size(now: (f32, f32), before: (f32, f32)) -> bool {
    (now.0 - before.0).abs() > 0.5 || (now.1 - before.1).abs() > 0.5
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
