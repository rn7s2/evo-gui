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
//! * the **record** itself — every item the topic holds — drawn by a virtual list
//!   ([`gpui::ListState`]): the rows the pane can reach are the rows that are built, so a
//!   journal of ten thousand items costs a pane of rows a frame rather than a parse and a
//!   layout per item;
//! * the tools the reader opened, and the untruncated results fetched for them.
//!
//! A caller seeds the whole list with [`TranscriptView::replace`] (a snapshot, a
//! `topic.reset`), pages older items in with [`TranscriptView::prepend`] (the scrollback
//! walking back through compactions), and moves single items with [`TranscriptView::upsert`]
//! and [`TranscriptView::remove`] as ops arrive. Older pages are asked for by the view
//! itself, one in flight at a time, for as long as the owner says the topic has more
//! behind what is held ([`TranscriptView::set_history`]): the reader never presses for the
//! scrollback, and a page that lands is spliced in front of them without moving what they
//! are reading. There are no row ids of the view's own and no revisions: an item's id is
//! stable across a refetch, a reconnect and a restart, so an item that changes is the same
//! row.
//!
//! ```ignore
//! let transcript = cx.new(TranscriptView::new);
//! transcript.update(cx, |view, cx| view.replace(items, cx));
//! transcript.update(cx, |view, cx| view.upsert(item, cx));
//! ```

mod imgcheck;
mod link;
mod linkify;
mod markdown;
mod math;
pub mod normalize_math;
pub mod pin;
mod rows;
mod style;

#[cfg(test)]
mod tests;

pub use imgcheck::decode_image;
/// The reader's font zoom for every transcript (§7.2): what the View menu sets.
pub use style::TranscriptZoom;

/// What this client tells a session about how it renders: the note a launch
/// passes as `evo-agent serve --prompt-note <path>`, so the agent writes math
/// this renderer draws rather than math it has to explain (the maths module is
/// where what can be drawn is decided). Markdown, and short — it rides in every
/// system prompt of the session. It lives beside the renderer that decides what
/// it can say.
pub const RATEX_NOTE: &str = include_str!("ratex_note.md");

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TextViewState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, linear_color_stop, linear_gradient, list, px, Animation, AnimationExt as _, AnyElement,
    App, AppContext as _, Bounds, BoxShadow, Context, Entity, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement as _, Pixels, Render, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window,
};
use session::{AgentKey, Item, ItemId, ItemKind};
use std::time::Duration;
use widgets::effort::cubic_bezier;
use widgets::paint;

use crate::pin::{Action, Pin};
use crate::rows::CopyFeedback;
use crate::style::Palette;
use store::design::{palette as design_palette, INSET};

/// How much of the record the virtual list keeps built around the pane, above and below
/// it: enough that a wheel's next screenful is already measured, and small enough that a
/// frame builds a pane of rows rather than a page of them.
const LIST_OVERDRAW: Pixels = px(800.);

/// How many rows a frame built, and how many frames the view has rendered — the two
/// numbers the tests hold the virtual list and the cache to (`cargo test -p
/// transcript`, and the crates that embed this view in their own tests). Off unless
/// `test-support` is on, so a shipping build carries no bookkeeping.
#[cfg(any(test, feature = "test-support"))]
pub mod counted {
    use std::cell::Cell;

    thread_local! {
        static ROWS: Cell<u64> = const { Cell::new(0) };
    }

    /// One row of the record was built into an element.
    pub(super) fn row() {
        ROWS.with(|rows| rows.set(rows.get() + 1));
    }

    /// How many rows have been built since [`reset`].
    pub fn rows() -> u64 {
        ROWS.with(Cell::get)
    }

    /// Start counting rows again.
    pub fn reset() {
        ROWS.with(|rows| rows.set(0));
    }
}

/// A handler that names the item it is about: the tool whose whole output is wanted, the
/// queued input to take back, or the oldest item the scrollback pages back from. Every one
/// is a read or an op on the server, which the owner (the tab's transport) performs.
pub type ItemHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A handler that names one image: the item it belongs to, and which of that item's
/// images. The bytes are fetched with `GET /media/<id>/<n>` and handed back through
/// [`TranscriptView::set_image`].
pub type ImageHandler = Rc<dyn Fn(&str, u32, &mut Window, &mut App)>;

/// What the app does when the reader presses a link: the address or the path the link
/// named. Shared with the rows (`Send + Sync`, since a text view keeps its handler for
/// the life of a document), and replaceable so that a test can hold what was pressed
/// without opening anything.
pub type LinkHandler = Arc<dyn Fn(&str, &mut Window, &mut App) + Send + Sync>;

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
    /// Whether this transcript is a swarm's or one agent's (§7.2): an empty one invites
    /// the reader to the coordinator, which has lanes to hand work to, or to the one
    /// agent, which has none. The tab says which when it makes the view.
    pub(crate) swarm: bool,
    pub(crate) copy_feedback: Arc<CopyFeedback>,
    pub(crate) focus: FocusHandle,
    /// What the owner does for each thing a row can ask for.
    pub(crate) on_load_older: RefCell<Option<ItemHandler>>,
    pub(crate) on_fetch_item: RefCell<Option<ItemHandler>>,
    pub(crate) on_cancel_input: RefCell<Option<ItemHandler>>,
    pub(crate) on_fetch_image: RefCell<Option<ImageHandler>>,
    /// Where a path in a row is resolved from, and what the file system said the last
    /// time it was asked: a row's text is read every frame it is on screen, and a `stat`
    /// per token per frame would put the disk in the middle of every scroll.
    pub(crate) paths: Rc<linkify::Paths>,
    /// What the app does when a link is pressed. The owner may replace it — a test says
    /// what was pressed without opening anything — and without one the platform opens
    /// the address or the path.
    pub(crate) on_open_link: RefCell<Option<LinkHandler>>,
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
        // A path in the message that is really there is a link the reader can press.
        let text = self.paths.prose(&text);
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

    /// Bring one piece of a row's own plain text up to date — a user's words, a notice,
    /// the body of a quiet row — creating its document the first time the row is on
    /// screen.
    ///
    /// The text is read as Markdown that draws exactly the characters it holds
    /// ([`linkify::literal`]), so that an address or a path in it is a link the reader
    /// can press without the row reading as anything but what was said.
    pub(crate) fn sync_plain_documents(&mut self, index: usize, cx: &mut Context<Self>) {
        let expanded = self.expanded.contains(&self.items[index].id);
        for (key, text) in rows::plain_texts(&self.items[index], expanded) {
            self.sync_plain_document(index, key, &text, cx);
        }
    }

    /// The same, for one named piece of a row's text.
    pub(crate) fn sync_plain_document(
        &mut self,
        index: usize,
        key: &'static str,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let paths = self.paths.clone();
        let id = self.items[index].id.clone();
        let source = paths.literal(text);
        let key = (id, key);
        match self.field_documents.get(&key).cloned() {
            Some(document) => document.update(cx, |state, cx| state.set_text(&source, cx)),
            None => {
                let document = cx.new(|cx| TextViewState::markdown(&source, cx));
                self.field_documents.insert(key, document);
            }
        }
    }

    /// Who opens a link in a row: the owner's handler, or none — which is the platform.
    pub(crate) fn open_link(&self) -> Option<LinkHandler> {
        self.on_open_link.borrow().clone()
    }

    /// The document one piece of a row's own plain text was given, for the row to draw.
    pub(crate) fn plain_document(
        &self,
        id: &ItemId,
        key: &'static str,
    ) -> Option<Entity<TextViewState>> {
        self.field_documents.get(&(id.clone(), key)).cloned()
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
        let text = self.paths.prose(&text);
        match self.field_documents.get(&key).cloned() {
            Some(document) => document.update(cx, |state, cx| state.set_text(&text, cx)),
            None => {
                let document = cx.new(|cx| TextViewState::markdown(&text, cx));
                self.field_documents.insert(key, document);
            }
        }
    }
}

/// The list's own shape: what the virtual list is holding, in its own indices — the
/// "Loading earlier items…" line at the head while a page is in flight, then every item of
/// the record, then the working pips at the foot. A record row's list index is
/// [`Slots::at`] of its place in `items`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Slots {
    /// The quiet line at the head: a page of scrollback is on its way.
    head: bool,
    /// How many of the record's items the list holds — all of them.
    rows: usize,
    /// The working pips are the list's foot.
    tail: bool,
}

impl Slots {
    /// How many items the list holds.
    fn len(&self) -> usize {
        usize::from(self.head) + self.rows + usize::from(self.tail)
    }

    /// Where the record's row `index` sits in the list.
    fn at(&self, index: usize) -> usize {
        usize::from(self.head) + index
    }

    /// Which row of the record a list index draws, if it is a row at all.
    fn record(&self, index: usize) -> Option<usize> {
        let start = usize::from(self.head);
        index.checked_sub(start).filter(|row| *row < self.rows)
    }
}

/// One agent's transcript: the whole record, drawn by a tail-following virtual list.
pub struct TranscriptView {
    data: Entity<TranscriptData>,
    /// The virtual list's own bookkeeping: how many items it holds, where the reader is
    /// in them, and how tall each one was when it was last laid out. Rows are built and
    /// measured only as the pane reaches them.
    list: ListState,
    /// What the list was last told to hold. Every op that moves the record splices the
    /// list with it, so a height already measured stays measured.
    slots: Slots,
    /// The reader's place in the list: [`Pin`] decides it, and this is what it
    /// moves.
    pin: Pin,
    /// The step-by-step return to the latest, while one is running.
    jump: Option<Task<()>>,
    /// Whether the agent's topic has items older than the ones held.
    has_older: bool,
    /// Whether a page of scrollback is in flight.
    loading_older: bool,
    /// How many items the record held when that page was asked for: an answer that did
    /// not lengthen the record is an answer that gave nothing back, and the walk stops
    /// there rather than asking the same question again.
    asking: Option<usize>,
    /// The scrollback gave nothing back: no more pages are asked for until the record
    /// changes shape under us.
    barren: bool,
    /// How many times the reader has scrolled for themselves, which is how a paint tells
    /// their own scroll from the layout's.
    scrolls: u64,
    /// Whether the reader's own scroll came to rest at the foot of what the list had
    /// measured. gpui clamps a scroll at the end of what it has laid out, and a reader
    /// who asked for more than there was was asking for the bottom — the browser's
    /// `scrollTop` clamps to its maximum the same way. Written where the scroll itself
    /// happens (the wheel handler, which the list has already answered), read by the
    /// paint that decides the pin.
    reaches_the_foot: Rc<Cell<bool>>,
    /// What the last paint left behind: the reader's own scroll count, where the list
    /// was, and how tall it was.
    painted: Painted,
    /// Whose transcript this is. An empty one says different things to the coordinator's
    /// reader and to a lane's.
    agent: AgentKey,
    /// How many times this view has rendered. Only the tests read it, and only when
    /// they are compiled in.
    #[cfg(any(test, feature = "test-support"))]
    renders: u64,
}

impl TranscriptView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let reaches_the_foot = Rc::new(Cell::new(true));
        let list = ListState::new(0, ListAlignment::Top, LIST_OVERDRAW);
        // What the list's own scroll answered: which rows the pane reaches. The list
        // holds its state borrowed while it runs this, so the flag is all it may write.
        list.set_scroll_handler({
            let reaches_the_foot = reaches_the_foot.clone();
            move |event, _window, _cx| {
                reaches_the_foot.set(event.visible_range.end >= event.count);
            }
        });
        let view = Self {
            data: cx.new(|cx| TranscriptData {
                focus: cx.focus_handle(),
                items: Vec::new(),
                index: HashMap::new(),
                turns: Vec::new(),
                thinking: 0,
                documents: HashMap::new(),
                field_documents: HashMap::new(),
                expanded: HashSet::new(),
                full_results: HashMap::new(),
                show_thinking: false,
                running: false,
                // A view nobody has told is a swarm's: that is the program the
                // transcript's own records are about, and the app sets the real one
                // when it makes the view (`TranscriptView::set_swarm`).
                swarm: true,
                copy_feedback: Arc::new(CopyFeedback::default()),
                on_load_older: RefCell::new(None),
                on_fetch_item: RefCell::new(None),
                on_cancel_input: RefCell::new(None),
                on_fetch_image: RefCell::new(None),
                paths: Rc::new(linkify::Paths::new()),
                on_open_link: RefCell::new(None),
                images: HashMap::new(),
                full_images: HashSet::new(),
            }),
            list,
            slots: Slots::default(),
            pin: Pin::new(),
            jump: None,
            has_older: false,
            loading_older: false,
            scrolls: 0,
            reaches_the_foot: reaches_the_foot.clone(),
            painted: Painted::default(),
            asking: None,
            barren: false,
            agent: AgentKey::Coordinator,
            #[cfg(any(test, feature = "test-support"))]
            renders: 0,
        };
        // A zoom the reader chose redraws every transcript on screen at once
        // (§7.2): the rows are measured again at the new size on the next frame.
        cx.observe_global::<TranscriptZoom>(|view, cx| view.remeasure_all(cx))
            .detach();
        view
    }

    /// Whose transcript this view shows.
    pub fn agent(&self) -> AgentKey {
        self.agent
    }

    /// How many times this view has rendered, since it was made.
    ///
    /// A cached view renders only when something it shows is notified; this is how a
    /// test tells a frame that reused the transcript from one that built it again.
    #[cfg(any(test, feature = "test-support"))]
    pub fn renders(&self) -> u64 {
        self.renders
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
        self.reaches_the_foot.set(true);
        self.list.scroll_to_end();
        cx.notify();
    }

    /// Whether this transcript is a swarm's coordinator's or one agent's (§7.2).
    ///
    /// The tab says which when it makes the view, and again whenever the program is
    /// known: an empty transcript's copy is the coordinator's or the one agent's, and a
    /// `set_swarm` that does not tell the view would leave a cached subtree drawing the
    /// other program's invitation.
    pub fn set_swarm(&mut self, swarm: bool, cx: &mut Context<Self>) {
        if self.data.read(cx).swarm == swarm {
            return;
        }
        self.data.update(cx, |data, _| data.swarm = swarm);
        cx.notify();
    }

    /// Whether this transcript is a swarm's.
    pub fn swarm(&self, cx: &App) -> bool {
        self.data.read(cx).swarm
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

    /// The folder the tab runs the swarm in: what a relative path in a row's text is
    /// measured from (`crates/transcript/src/lib.rs` in the project it names).
    ///
    /// A folder that moves re-reads every row: the paths that were links are measured
    /// from somewhere else now, and some that were not are.
    pub fn set_folder(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        if !self.data.read(cx).paths.set_folder(folder) {
            return;
        }
        // The rows' own documents are derived from the text and the file system: the
        // next frame reads them again, for the rows it builds.
        self.data.update(cx, |data, _| {
            data.documents.clear();
            data.field_documents.clear();
        });
        cx.notify();
    }

    /// What the app does when the reader presses a link, in place of the platform.
    /// A test says what was pressed without opening anything.
    pub fn on_open_link(
        &mut self,
        handler: impl Fn(&str, &mut Window, &mut App) + Send + Sync + 'static,
        cx: &mut Context<Self>,
    ) {
        let handler: LinkHandler = Arc::new(handler);
        self.data.update(cx, |data, _| {
            *data.on_open_link.borrow_mut() = Some(handler)
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
        // The picture is drawn where the placeholder was: the row's height is measured
        // again.
        self.remeasure_row(id, cx);
        cx.notify();
    }

    /// The image could not be fetched or decoded: the row says so, once.
    pub fn set_image_failed(&mut self, id: &str, n: u32, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.images.insert((id.to_string(), n), ImageState::Failed);
        });
        // The row says so calmly instead of showing a picture: its height changed.
        self.remeasure_row(id, cx);
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
        // Thumbnail or whole picture: a row of a different height.
        self.remeasure_row(id, cx);
        cx.notify();
    }

    /// What the owner says about the history behind the record: whether the topic has
    /// more than what is held, and whether a page is in flight.
    ///
    /// Older pages are not the reader's to ask for: for as long as this says there is
    /// more, the view asks for the next page itself, one at a time, until the answer says
    /// there is nothing behind ([`TranscriptView::ask_for_older`]). A page that is no
    /// longer in flight has settled — answered, empty, or given up on — and if it did not
    /// lengthen the record it gave nothing back: the walk stops there rather than asking
    /// the same question again and again.
    pub fn set_history(&mut self, has_older: bool, loading: bool, cx: &mut Context<Self>) {
        if self.loading_older && !loading {
            if let Some(asked) = self.asking.take() {
                let held = self.data.read(cx).items.len();
                self.barren = held == asked;
            }
        }
        if !has_older {
            // Nothing behind what is held: whatever an earlier walk ended at is over.
            self.barren = false;
        } else if !self.has_older {
            // The topic has scrollback the last snapshot did not: walk it again.
            self.barren = false;
        }
        if self.has_older == has_older && self.loading_older == loading {
            return;
        }
        self.has_older = has_older;
        self.loading_older = loading;
        cx.notify();
    }

    /// Replace the whole record, as a snapshot or a `topic.reset` does: every item is
    /// held and drawn, and the list starts again at its latest item.
    pub fn replace(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.reaches_the_foot.set(true);
        // A record replaced outright is not the one an earlier walk gave up on.
        self.barren = false;
        self.data.update(cx, |data, _| {
            data.items = items.into_iter().filter(is_part_of_the_record).collect();
            data.reindex();
            data.retain_documents();
        });
        self.asking = None;
        let slots = self.slots_now(cx);
        self.list.reset(slots.len());
        self.slots = slots;
        if self.pin.is_pinned() {
            self.list.scroll_to_end();
        }
        cx.notify();
    }

    /// Put older items in front of what is held — the scrollback walking back through
    /// compactions. An item already held is not added twice.
    ///
    /// They are spliced in above the reader: the rows on screen do not move, and a reader
    /// who is following the tail stays at the tail, which is where the newest rows are.
    pub fn prepend(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let held = self.asking.is_some();
        let added = self.data.update(cx, |data, _| {
            let fresh: Vec<Item> = items
                .into_iter()
                .filter(is_part_of_the_record)
                .filter(|item| data.index_of(&item.id).is_none())
                .collect();
            if fresh.is_empty() {
                return 0;
            }
            let added = fresh.len();
            let mut next = fresh;
            next.append(&mut data.items);
            data.items = next;
            data.reindex();
            added
        });
        if added == 0 {
            // An answer that brought no row is an answer that gave nothing back: do not
            // ask the same question again.
            if held {
                self.barren = true;
            }
            return;
        }
        let at = self.slots.head as usize;
        self.list.splice(at..at, added);
        self.slots.rows += added;
        self.barren = false;
        // The rows above the reader were added by the view, not by them: whatever scroll
        // that lays out is not their own.
        self.pin.settled();
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
        let (changed, at, fresh) = self
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
                    (!waiting, index, false)
                }
                None => {
                    data.items.push(item);
                    data.index_last();
                    (true, data.items.len() - 1, true)
                }
            });

        if !changed {
            return false;
        }
        // The list is told what happened to the row itself: a row that arrived is spliced
        // in where it belongs — the record's own order, at the end — and one whose text
        // changed is measured again, since its height may have. Everything else keeps
        // the height it was already measured at.
        let at = self.slots.at(at);
        if fresh {
            self.list.splice(at..at, 1);
            self.slots.rows += 1;
        } else {
            self.list.remeasure_items(at..at + 1);
        }
        cx.notify();
        true
    }

    /// Drop one item the topic no longer has.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let removed = self.data.update(cx, |data, _| {
            let index = data.index_of(id)?;
            data.items.remove(index);
            data.reindex();
            data.documents.remove(id);
            data.field_documents.retain(|(held, _), _| held != id);
            data.expanded.remove(id);
            data.full_results.remove(id);
            Some(index)
        });
        let Some(index) = removed else {
            return false;
        };
        // The row leaves the list where it stood: the rows in front of the reader stay
        // exactly where they were.
        let at = self.slots.at(index);
        self.list.splice(at..at + 1, 0);
        self.slots.rows = self.slots.rows.saturating_sub(1);
        self.pin.settled();
        cx.notify();
        true
    }

    /// Drop every item (a session switch, before the new snapshot lands).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.items.clear();
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
        self.asking = None;
        self.barren = false;
        self.reaches_the_foot.set(true);
        self.jump.take();
        self.list.reset(0);
        self.slots = Slots::default();
        cx.notify();
    }

    /// Whether the list is following its tail.
    pub fn is_following_tail(&self, _cx: &App) -> bool {
        self.pin.is_pinned()
    }

    /// Whether "↓ Jump to latest" is showing: the reader is away from the bottom of the
    /// record.
    pub fn is_away_from_latest(&self, _cx: &App) -> bool {
        self.pin.is_away()
    }

    /// Measure every row again, at the size the rows are drawn at now.
    ///
    /// A zoom the reader chose changes the text size the rows are laid out at, and the
    /// heights the list cached were measured at the old one: the list is told to measure
    /// them again at the new size, keeping the reader's place proportionally.
    pub fn remeasure_all(&mut self, cx: &mut Context<Self>) {
        self.list.remeasure();
        cx.notify();
    }

    /// Take the reader back to the latest item: follow the tail again, and go
    /// there — not in a jump, which is what the design's `smooth` asks for.
    pub fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.pin.jumped();
        self.reaches_the_foot.set(true);
        cx.notify();
        let from = self.gap();
        if from <= 0. {
            self.jump.take();
            self.list.scroll_to_end();
            return;
        }
        self.jump = Some(cx.spawn(async move |view, cx| {
            for step in 1..=JUMP_STEPS {
                cx.background_executor().timer(JUMP_STEP).await;
                let t = step as f32 / JUMP_STEPS as f32;
                // How far from the bottom the list should be by now: the eased return,
                // measured afresh each step, since the rows below may still be being
                // measured as they come into view.
                let want = from * (1. - ease_out(t));
                let _ = view.update(cx, |view, cx| {
                    let now = view.gap();
                    if now.is_finite() {
                        view.list.scroll_by(px(now - want));
                    } else {
                        // The tail is not laid out at all: it is further away than any
                        // ease, so land on it in one step.
                        view.list.scroll_to_end();
                    }
                    cx.notify();
                });
            }
            // Land exactly on the tail, even if it grew while we travelled.
            let _ = view.update(cx, |view, cx| {
                view.list.scroll_to_end();
                cx.notify();
            });
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
        // Every row that carries thinking changed shape at once.
        self.list.remeasure();
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
            // A body that opened or closed is a row of a different height.
            self.remeasure_row(id, cx);
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
        // The row's body is longer than the shortened one it was measured with.
        self.remeasure_row(id, cx);
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

/// The list: where the reader is in it, and how the record keeps it in step.
impl TranscriptView {
    /// The reader did something that scrolls: a wheel, a touch, a key, a pointer press.
    /// The design's 500ms window opens, and the view remembers that they have moved.
    fn touched(&mut self) {
        self.scrolls += 1;
        self.pin.touched();
        // gpui's offset runs negative as the list goes down, so the reader is at the
        // foot when it equals the greatest offset there is.
        self.reaches_the_foot
            .set(-self.offset() >= f32::from(self.list.max_offset_for_scrollbar().y) - 0.5);
    }

    /// How far the reader is from the bottom of the record, as the pin's rule measures
    /// it: the design's `scrollHeight - scrollTop - clientHeight`, over the heights the
    /// list has measured.
    ///
    /// The heights of rows the reader has never reached are not known — they are
    /// measured as the pane arrives at them — so the distance is exact for everything
    /// the pane can reach, and `f32::INFINITY` when the foot of the record is not laid
    /// out at all, which is as far from the latest as a reader can be.
    fn gap(&self) -> f32 {
        let count = self.list.item_count();
        let pane = self.list.viewport_bounds();
        if count == 0 || pane.size.height <= px(0.) {
            return 0.;
        }
        // The list is anchored at or past its last item: it is on the foot of the
        // record, which is where a reader following the tail is.
        if self.list.logical_scroll_top().item_ix >= count {
            return 0.;
        }
        // Nothing to scroll — the whole record is on screen — is the tail too.
        if self.list.max_offset_for_scrollbar().y <= px(0.) {
            return 0.;
        }
        match self.list.bounds_for_item(count - 1) {
            Some(last) => f32::from(last.bottom() - pane.bottom()).max(0.),
            None => f32::INFINITY,
        }
    }

    /// Where the list sits, and how tall it is: the offset is for telling one paint from
    /// the next, and the two heights are for telling a scroll from a resize.
    fn sizes(&self) -> (f32, f32) {
        let pane = f32::from(self.list.viewport_bounds().size.height);
        (
            pane + f32::from(self.list.max_offset_for_scrollbar().y),
            pane,
        )
    }

    /// How far down the list has been scrolled, in the list's own pixels — negative as
    /// it goes down, as gpui's own offsets are.
    fn offset(&self) -> f32 {
        f32::from(self.list.scroll_px_offset_for_scrollbar().y)
    }

    /// The row of `id` changed shape — a body opened, an image landed, text streamed in:
    /// the list measures it again, since a list that keeps a stale height for a row it
    /// draws puts everything below it in the wrong place.
    fn remeasure_row(&self, id: &str, cx: &App) {
        if let Some(index) = self.data.read(cx).index_of(id) {
            let at = self.slots.at(index);
            self.list.remeasure_items(at..at + 1);
        }
    }

    /// What the list holds at this moment: the loading line while a page is in flight,
    /// every item of the record, and the pips while the agent is working with nothing on
    /// screen saying so.
    fn slots_now(&self, cx: &App) -> Slots {
        let data = self.data.read(cx);
        Slots {
            head: self.loading_older,
            rows: data.items.len(),
            tail: rows::shows_work(data),
        }
    }

    /// Bring the list in step with the record before it is laid out.
    ///
    /// Every op that moves the record splices the list itself, so a row that was
    /// measured stays measured; this is the last word before the frame, and covers the
    /// two things an op cannot: the loading line coming and going at the head, the pips
    /// at the foot, and any change to the record the list was never told about — which
    /// starts the list again at the record's new shape.
    fn sync_list(&mut self, cx: &App) {
        let now = self.slots_now(cx);
        if now == self.slots {
            return;
        }
        if now.rows != self.slots.rows {
            self.list.reset(now.len());
            self.slots = now;
            if self.pin.is_pinned() {
                // The list starts at the head: a reader who is following ends up back at
                // the tail, which is where they were.
                self.list.scroll_to_end();
            }
            return;
        }
        if now.head != self.slots.head {
            if now.head {
                self.list.splice(0..0, 1);
            } else {
                self.list.splice(0..1, 0);
            }
        }
        if now.tail != self.slots.tail {
            if now.tail {
                let at = self.list.item_count();
                self.list.splice(at..at, 1);
            } else {
                let at = self.list.item_count().saturating_sub(1);
                self.list.splice(at..at + 1, 0);
            }
        }
        self.slots = now;
    }

    /// Ask for the page of scrollback in front of the oldest item held, if the topic
    /// says there is one and none is in flight.
    ///
    /// The reader never presses for the past: a transcript walks its own scrollback back
    /// — one page at a time, each asked for once the one before it has landed — until
    /// the topic says there is nothing behind. A page that came back empty, or that
    /// failed, ends the walk where it is; the next snapshot, or `has_older` turning true
    /// again, starts it over.
    fn ask_for_older(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_older || !self.has_older || self.barren {
            return;
        }
        let (oldest, handler, held) = {
            let data = self.data.read(cx);
            let Some(oldest) = data.items.first().map(|item| item.id.clone()) else {
                return;
            };
            (
                oldest,
                data.on_load_older.borrow().clone(),
                data.items.len(),
            )
        };
        let Some(handler) = handler else {
            return;
        };
        self.loading_older = true;
        self.asking = Some(held);
        cx.notify();
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
/// as the record changes, so the control that reveals them costs no walk of the record.
fn carries_thinking(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Assistant(assistant) if !assistant.thinking.is_empty())
}

/// The images the row at `index` carries that nobody has asked for yet: marked as on
/// their way, and handed back for the owner to fetch (`GET /media/<id>/<n>`). Each image
/// is asked for once, and only for a row the list actually built.
fn images_wanted(data: &mut TranscriptData, index: usize) -> Vec<(ItemId, u32)> {
    let Some((id, count)) = data.items.get(index).and_then(|item| match &item.kind {
        ItemKind::User(user) => Some((item.id.clone(), user.images.len() as u32)),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut wanted = Vec::new();
    for n in 0..count {
        let key = (id.clone(), n);
        if !data.images.contains_key(&key) {
            data.images.insert(key.clone(), ImageState::Loading);
            wanted.push(key);
        }
    }
    wanted
}

/// What an empty transcript says, per program and agent.
///
/// A swarm's coordinator plans the work and hands it to its lanes; one agent has nobody
/// to coordinate and does the work itself, which is the whole of the difference the tab
/// tells this view about (§7.2). A lane's line is its own either way.
fn empty_note(agent: AgentKey, swarm: bool) -> (String, Option<&'static str>) {
    match agent {
        AgentKey::Coordinator if swarm => (
            "Ask the coordinator to get started".to_string(),
            Some("It plans the work and hands tasks to its lanes."),
        ),
        AgentKey::Coordinator => (
            "Ask the agent to get started".to_string(),
            Some("It does the work itself — there are no lanes to hand it to."),
        ),
        AgentKey::Lane(n) => (format!("Lane {n} hasn't been given work yet."), None),
    }
}

/// The transcript before its first item: a quiet, centred invitation.
fn empty_state(agent: AgentKey, swarm: bool, palette: &Palette) -> AnyElement {
    let (note, detail) = empty_note(agent, swarm);
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
            .id("transcript-empty-note")
            .aria_label(note.clone())
            .text_size(palette.font_size)
            .text_color(palette.foreground)
            .child(note)
            .test_support(),
    );
    if let Some(detail) = detail {
        column = column.child(
            div()
                .id("transcript-empty-detail")
                .aria_label(detail.to_string())
                .text_size(palette.font_size - px(2.))
                .text_color(palette.muted_foreground)
                .child(detail)
                .test_support(),
        );
    }

    match agent {
        AgentKey::Coordinator => column.id("transcript-empty"),
        AgentKey::Lane(n) => column.id(("transcript-empty-lane", n as u64)),
    }
    .test_support()
    .into_any_element()
}

/// The quiet line at the head of the list while a page of scrollback is on its way.
///
/// It is not a control: the view asks for older items itself, for as long as the topic
/// says there is more, so this only says that something is happening back there while
/// the reader catches up with what has arrived.
fn loading_line(palette: &Palette) -> AnyElement {
    div()
        .id("transcript-loading-older")
        .w_full()
        .flex()
        .justify_center()
        .pb(px(6.))
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(palette.muted_foreground)
        .child("Loading earlier items…")
        .test_support()
        .into_any_element()
}

impl Render for TranscriptView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(any(test, feature = "test-support"))]
        {
            self.renders += 1;
        }
        let theme = cx.theme().clone();
        let palette = Palette::from_app(cx);

        if self.data.read(cx).items.is_empty() {
            let swarm = self.data.read(cx).swarm;
            return empty_state(self.agent, swarm, &palette).into_any_element();
        }

        // The scrollback walks itself back: the topic says there is more behind the
        // oldest item held, so the next page is asked for here, and the answer brings
        // one more.
        self.ask_for_older(window, cx);
        // The list is told what the record looks like now before it is laid out, so a
        // row that arrived is spliced rather than the whole list rebuilt.
        self.sync_list(cx);

        // Only the rows the pane reaches are built: a wheel over a journal of ten
        // thousand items costs a pane of rows a frame, not a parse of the whole record.
        // The reading measure and its inset are the row wrapper's, since the list lays
        // every row out at the pane's own width.
        let rows = {
            let data = self.data.clone();
            let view = cx.weak_entity();
            let slots = self.slots;
            let palette = palette.clone();
            let count = slots.len();
            // The reading measure, its inset, and the scroll's own padding: the list
            // lays every row out at the pane's own width and offsets them by nothing, so
            // the column and the air above the first row and below the last are the
            // rows' own boxes.
            //
            // The measure is the one the reader's zoom asks for (§7.2): a `max_w`, never
            // a width, so a pane narrower than it fills the pane as it always has.
            let measure = palette.measure();
            let within_measure = move |index: usize, element: AnyElement| -> AnyElement {
                let column = div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .when(index == 0, |box_| box_.pt(px(INSET)))
                    .when(index + 1 == count, |box_| box_.pb(px(INSET)))
                    .child(
                        div()
                            .id(("transcript-column", index))
                            .w_full()
                            .max_w(measure)
                            .px(px(INSET))
                            .test_support()
                            .child(element),
                    );
                column.into_any_element()
            };
            list(self.list.clone(), move |index, window, cx| {
                let Some(record) = slots.record(index) else {
                    return if index == 0 {
                        within_measure(index, loading_line(&palette))
                    } else {
                        data.update(cx, |data, cx| {
                            rows::pending_row(data, cx)
                                .map(|pending| within_measure(index, pending))
                                .unwrap_or_else(|| div().into_any_element())
                        })
                    };
                };
                #[cfg(any(test, feature = "test-support"))]
                counted::row();
                let view = view.clone();
                let handler = data.read(cx).on_fetch_image.borrow().clone();
                let (element, wanted) = data.update(cx, |data, cx| {
                    let element = rows::render_row(data, record, &view, cx);
                    let wanted = match handler {
                        // An image row's bytes are fetched when the row is first built,
                        // and only then: a row nobody has scrolled to asks for nothing.
                        Some(_) => images_wanted(data, record),
                        None => Vec::new(),
                    };
                    (element, wanted)
                });
                if let Some(handler) = handler {
                    for (id, n) in wanted {
                        handler(&id, n, window, cx);
                    }
                }
                within_measure(index, element)
            })
            .size_full()
            .min_h_0()
        };

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

        // Every frame the list is painted is a chance for the design's rule to run
        // — the same chance a scroll event gives it in a browser, and the one that
        // catches a pane that changed height under a reader who is following.
        let on_painted = {
            let weak = cx.weak_entity();
            move |_bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |view, cx| view.on_painted(cx));
            }
        };

        let scroller = div()
            .on_children_prepainted(on_painted)
            .id("transcript-scroll")
            .test_support()
            .size_full()
            .min_h_0()
            .on_scroll_wheel(touched)
            .on_mouse_down(MouseButton::Left, pressed)
            .on_key_down(keyed)
            .child(rows);

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
        // with the rows. It is driven by the list's own state either way, and
        // the app's scrollbar mode (`app::theme`) shows it only while scrolling.
        div()
            .id("transcript")
            .test_support()
            .relative()
            .size_full()
            .min_h_0()
            .child(scroller)
            .child(fade)
            .children(self.jump_pill(cx))
            .vertical_scrollbar(&self.list)
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
/// The distance is the list's own: `scrollHeight - scrollTop - clientHeight` over the
/// heights it has measured — 0 at the bottom, growing as the reader goes up, and 0 for a
/// list shorter than its pane.
impl TranscriptView {
    fn on_painted(&mut self, cx: &mut Context<Self>) {
        // Whose scroll this frame is — if it is a scroll at all. Only a frame in which
        // the reader's own wheel, key or press arrived, the offset moved, or the list or
        // its pane changed height is one the design's rule has anything to say about;
        // between those the reader's flags stand.
        let now = Painted {
            scrolls: self.scrolls,
            offset: self.offset(),
            size: self.sizes(),
        };
        let before = std::mem::replace(&mut self.painted, now);
        let by_input = now.scrolls != before.scrolls;
        let scrolled = (now.offset - before.offset).abs() > 0.5;
        let resized = moved_size(now.size, before.size);

        let away = self.pin.is_away();
        if by_input || scrolled || resized {
            // The reader's own scroll that came to rest at the foot of what the list had
            // measured asked for the bottom of the record: the rows below them are
            // measured as the layout arrives at them, so the list is anchored at the
            // record's own foot rather than left a row short of it. That is the browser's
            // clamped `scrollTop`, read as the design's 0.
            let at_the_foot = by_input && self.reaches_the_foot.get();
            if at_the_foot {
                self.list.scroll_to_end();
                cx.notify();
            }
            let gap = if at_the_foot { 0. } else { self.gap() };
            // A scroll the view saw no input for — a drag of the scroll bar — is still
            // the reader's if the content did not move under them; everything else that
            // is not their own wheel is the layout, and the layout never puts the list
            // back on its tail: the list does not come back to the latest because a row
            // left the record or a pane grew.
            let action = if by_input || (scrolled && !resized) {
                self.pin.on_scroll(gap)
            } else {
                self.pin.on_layout(gap)
            };
            match action {
                // The layout moved under a reader who is following: pull the list back
                // rather than letting their place drift.
                Action::SnapToBottom => {
                    self.list.scroll_to_end();
                    cx.notify();
                }
                Action::Leave => {}
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
