//! transcript — the center column of a tab: one agent's transcript (§2.8, §7.3).
//!
//! The view is fed rows from the `session` crate and owns three pieces of
//! retained state:
//!
//! * one [`TextViewState`] per assistant row that has been on screen, keyed by
//!   [`RowId`], extended with `set_text` so markdown is **rendered live while it
//!   streams** — never recreated per delta. A row's document is created the
//!   first time the list renders it and handed its row's text once per frame,
//!   so a stream that lands fifty deltas between two frames is one parse, and a
//!   transcript of thousands of rows holds the documents of the rows the reader
//!   is near and nothing else (see `KEPT_DOCUMENTS`);
//! * one [`MessageScrollerState`] per view, which follows the tail while the
//!   reader is at the bottom and offers the jump-to-latest button when they
//!   are not;
//! * the todos of the agent the view shows, rendered by [`TodoPanel`].
//!
//! A caller feeds a whole transcript with [`TranscriptView::replace`] on every
//! rebuild (`/transcript`, `gap`, `hello`, `settled`) and single rows with
//! [`TranscriptView::upsert`] as events arrive. Both carry the agent's revision
//! and are dropped when it is older than the last accepted one, so late results
//! from a previous revision cannot reach the UI.
//!
//! Rows are rendered as the session hands them over: a turn boundary is drawn
//! between two user turns, and a run that ended badly arrives as
//! [`RowKind::RunOutcome`] with the line to show.
//!
//! ```ignore
//! let transcript = cx.new(TranscriptView::new);
//! transcript.update(cx, |view, cx| view.replace(1, rows, cx));
//! transcript.update(cx, |view, cx| view.upsert(1, row, cx));
//! ```

mod link;
mod markdown;
mod rows;
mod style;
mod todo;

#[cfg(test)]
mod tests;

pub use todo::TodoPanel;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::base::TextViewState;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::{v_flex, ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::{
    div, px, AnyElement, App, AppContext as _, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, StyleRefinement, Styled as _,
    TestSupportExt as _, Window,
};
use session::{AgentKey, Row, RowId, RowKind, Todo};

use crate::rows::CopyFeedback;
use crate::style::Palette;

/// How many assistant rows keep their parsed document.
///
/// A window shows a few dozen rows, so this is room to scroll back and forth
/// without paying for a re-parse; what it bounds is the memory a transcript of
/// thousands of rows would otherwise hold for rows nobody is looking at.
const KEPT_DOCUMENTS: usize = 128;

/// The data the row renderer reads.
///
/// It lives in an entity of its own so the virtual list can read rows while the
/// view that owns them is mid-render, and so a row update never copies the
/// whole transcript into the renderer closure.
pub(crate) struct TranscriptData {
    pub(crate) rows: Vec<Row>,
    /// Retained markdown documents of assistant rows, keyed by row id.
    ///
    /// A row gets its document the first time it is rendered, not when it
    /// arrives: a parsed message costs tens to hundreds of kilobytes, so a
    /// transcript of thousands of rows holds the documents of the rows the
    /// reader is near and nothing else.
    pub(crate) documents: HashMap<RowId, Entity<TextViewState>>,
    /// The frame each document was last rendered in, so the ones the reader has
    /// left behind can be dropped.
    rendered: HashMap<RowId, u64>,
    /// The frame counter `rendered` is ordered by.
    frame: u64,
    /// Tool rows the reader has opened.
    pub(crate) expanded: HashSet<RowId>,
    /// Whether assistant thinking text is shown (hidden by default).
    pub(crate) show_thinking: bool,
    /// How often each of the transcript's copy buttons has been used, so one
    /// can acknowledge a click. Shared through an `Arc` because the buttons of
    /// a message's code blocks are built by a `Send + Sync` closure.
    pub(crate) copy_feedback: Arc<CopyFeedback>,
    /// The focus the rows take on a click, so the keyboard reaches the window's
    /// own copy binding: text a reader selected has to be copiable whether or
    /// not the element it lives in is a text view.
    pub(crate) focus: FocusHandle,
}

impl TranscriptData {
    fn index_of(&self, id: RowId) -> Option<usize> {
        self.rows.iter().position(|row| row.id == id)
    }

    /// Start a frame: age the documents and drop the ones the reader has left.
    fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if self.rendered.len() <= KEPT_DOCUMENTS {
            return;
        }

        let mut ages: Vec<(RowId, u64)> = self
            .rendered
            .iter()
            .map(|(id, frame)| (*id, *frame))
            .collect();
        ages.sort_by_key(|(_, frame)| *frame);
        for (id, _) in ages.drain(..ages.len() - KEPT_DOCUMENTS) {
            self.rendered.remove(&id);
            self.documents.remove(&id);
        }
    }

    /// Note that a row is on screen this frame.
    pub(crate) fn note_rendered(&mut self, id: RowId) {
        self.rendered.insert(id, self.frame);
    }

    /// Bring `rows[index]`'s document up to date, creating it the first time the
    /// row is on screen.
    ///
    /// Called from the row renderer, so a row that arrives and streams its text
    /// between two frames is handed to its document once, at the frame that
    /// shows it — never once per delta. `set_text` is a no-op when the source is
    /// unchanged, so rendering a still row costs a comparison.
    pub(crate) fn sync_document(&mut self, index: usize, cx: &mut Context<Self>) {
        let row = &self.rows[index];
        let RowKind::Assistant { markdown, .. } = &row.kind else {
            return;
        };
        let id = row.id;
        match self.documents.get(&id).cloned() {
            Some(document) => document.update(cx, |state, cx| state.set_text(markdown, cx)),
            None => {
                let document = cx
                    .new(|cx| TextViewState::markdown(markdown, cx).motion(rows::stream_motion()));
                self.documents.insert(id, document);
            }
        }
    }

    /// Keep the documents of the rows that are still here, and nothing else.
    fn retain_documents(&mut self) {
        let rows = &self.rows;
        self.documents
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
        self.rendered
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
    }
}

/// What a row update did to the list, so the scroller can be told about it.
enum ListChange {
    /// Same rows, possibly different content: remeasure without moving.
    Remeasure,
    Reset(usize),
    Append(usize),
    Remove(Range<usize>),
}

impl ListChange {
    fn between(old: &[Row], new: &[Row]) -> Self {
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

/// One agent's transcript: a tail-following virtual list of rows.
pub struct TranscriptView {
    /// Last accepted revision; older updates are dropped.
    revision: u64,
    data: Entity<TranscriptData>,
    scroller: Entity<MessageScrollerState>,
    todos: Vec<Todo>,
    /// Whose transcript this is. An empty transcript says different things to
    /// the coordinator's reader and to a lane's.
    agent: AgentKey,
}

impl TranscriptView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        // The scroller's list is not a view, so it has to tell this view when it
        // moved; every reader of `is_following_tail` then sees it.
        cx.observe(&scroller, |_, _, cx| cx.notify()).detach();

        Self {
            revision: 0,
            data: cx.new(|cx| TranscriptData {
                focus: cx.focus_handle(),
                rows: Vec::new(),
                documents: HashMap::new(),
                rendered: HashMap::new(),
                frame: 0,
                expanded: HashSet::new(),
                show_thinking: false,
                copy_feedback: Arc::new(CopyFeedback::default()),
            }),
            scroller,
            todos: Vec::new(),
            agent: AgentKey::Coordinator,
        }
    }

    /// Whose transcript this view shows.
    pub fn agent(&self) -> AgentKey {
        self.agent
    }

    /// Tell the view whose transcript it shows: what an empty one says depends
    /// on it (§7.3).
    pub fn set_agent(&mut self, agent: AgentKey, cx: &mut Context<Self>) {
        if self.agent == agent {
            return;
        }
        self.agent = agent;
        cx.notify();
    }

    /// The revision of the last accepted update.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The rows currently shown, in order.
    pub fn rows<'a>(&'a self, cx: &'a App) -> &'a [Row] {
        &self.data.read(cx).rows
    }

    /// The todos of the agent this view shows.
    pub fn todos(&self) -> &[Todo] {
        &self.todos
    }

    /// Whether assistant thinking text is shown.
    pub fn is_showing_thinking(&self, cx: &App) -> bool {
        self.data.read(cx).show_thinking
    }

    /// Whether this agent has any thinking text to reveal.
    ///
    /// The tab page's header shows its toggle only while this is true: a
    /// control that reveals nothing is noise.
    pub fn has_thinking(&self, cx: &App) -> bool {
        self.data.read(cx).rows.iter().any(|row| match &row.kind {
            RowKind::Assistant { thinking, .. } => !thinking.is_empty(),
            _ => false,
        })
    }

    /// Show or hide assistant thinking text (hidden by default).
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

    /// Replace the whole transcript, as a rebuild from `/transcript` does.
    ///
    /// Returns `false` — and changes nothing — when `revision` is older than the
    /// last accepted one.
    pub fn replace(&mut self, revision: u64, rows: Vec<Row>, cx: &mut Context<Self>) -> bool {
        if !self.accept_revision(revision) {
            return false;
        }

        let change = self.data.update(cx, |data, _| {
            let change = ListChange::between(&data.rows, &rows);
            data.rows = rows;
            data.retain_documents();
            change
        });
        self.apply_list_change(change, cx);
        cx.notify();
        true
    }

    /// Add or update one row.
    ///
    /// A row that is already there is replaced only when its `version` is
    /// newer — the version is the contract for "this row's content changed".
    /// Returns `true` when the view changed.
    pub fn upsert(&mut self, revision: u64, row: Row, cx: &mut Context<Self>) -> bool {
        if !self.accept_revision(revision) {
            return false;
        }
        // `(index, appended)`: a row that was already there keeps its place and
        // only needs remeasuring; a new one extends the list.
        let change = self.data.update(cx, |data, _| {
            let (index, appended) = match data.index_of(row.id) {
                Some(index) => {
                    if row.version <= data.rows[index].version {
                        return None;
                    }
                    data.rows[index] = row;
                    (index, false)
                }
                None => {
                    data.rows.push(row);
                    (data.rows.len() - 1, true)
                }
            };
            Some((index, appended))
        });

        match change {
            Some((_, true)) => {
                self.scroller.update(cx, |scroller, cx| {
                    scroller.append(1, cx);
                });
            }
            Some((index, false)) => {
                self.scroller.update(cx, |scroller, cx| {
                    scroller.remeasure_items(index..index + 1, cx);
                });
            }
            None => return false,
        }

        cx.notify();
        true
    }

    /// Drop one row the model no longer has — an assistant message that ended
    /// carrying nothing (a tool-only step) is removed from the model, and its
    /// row here must go too, or its waiting dots outlive it.
    /// Returns `true` when the view changed.
    pub fn remove(&mut self, revision: u64, id: RowId, cx: &mut Context<Self>) -> bool {
        if !self.accept_revision(revision) {
            return false;
        }
        let index = self.data.update(cx, |data, _| {
            let index = data.index_of(id)?;
            data.rows.remove(index);
            data.documents.remove(&id);
            data.rendered.remove(&id);
            data.expanded.remove(&id);
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

    /// Replace the todos of the agent this view shows.
    pub fn set_todos(&mut self, todos: Vec<Todo>, cx: &mut Context<Self>) {
        if self.todos == todos {
            return;
        }
        self.todos = todos;
        cx.notify();
    }

    /// Drop every row. The revision is kept: a rebuild decides it.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.data.update(cx, |data, _| {
            data.rows.clear();
            data.documents.clear();
            data.rendered.clear();
            data.expanded.clear();
        });
        self.scroller
            .update(cx, |scroller, cx| scroller.reset(0, cx));
        cx.notify();
    }

    /// Whether the reader is at the live edge of the transcript.
    pub fn is_following_tail(&self, cx: &App) -> bool {
        self.scroller.read(cx).is_following_tail()
    }

    /// Resume following the tail and jump to the latest row.
    pub fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.scroller
            .update(cx, |scroller, cx| scroller.scroll_to_end(cx));
    }

    /// Show or hide the arguments and result of one tool row, for a caller that
    /// names the row itself; a reader's click goes through the row's own header.
    pub fn set_expanded(&mut self, id: RowId, expanded: bool, cx: &mut Context<Self>) {
        let changed = self.data.update(cx, |data, _| {
            if expanded {
                data.expanded.insert(id)
            } else {
                data.expanded.remove(&id)
            }
        });
        if changed {
            cx.notify();
        }
    }

    /// A tool row the reader clicked: the one they opened, or the one they
    /// closed again.
    pub(crate) fn toggle_expanded(&mut self, id: RowId, cx: &mut Context<Self>) {
        let expanded = !self.data.read(cx).expanded.contains(&id);
        self.set_expanded(id, expanded, cx);
    }

    fn accept_revision(&mut self, revision: u64) -> bool {
        if revision < self.revision {
            return false;
        }
        self.revision = revision;
        true
    }

    fn apply_list_change(&mut self, change: ListChange, cx: &mut Context<Self>) {
        self.scroller.update(cx, |scroller, cx| match change {
            ListChange::Remeasure => scroller.remeasure(cx),
            ListChange::Reset(count) => scroller.reset(count, cx),
            ListChange::Append(count) => {
                scroller.append(count, cx);
            }
            ListChange::Remove(range) => {
                scroller.splice(range, 0, cx);
            }
        });
    }
}

/// What an empty transcript says, per agent: the coordinator's view invites a
/// request and carries the app's mark; a lane's says it has nothing to do yet.
///
/// Apart from the element so the wording is testable on its own.
fn empty_note(agent: AgentKey) -> (String, Option<&'static str>) {
    match agent {
        AgentKey::Coordinator => (
            "Ask the coordinator to get started".to_string(),
            Some("It plans the work and hands tasks to its lanes."),
        ),
        AgentKey::Lane(n) => (format!("Lane {n} hasn't been given work yet."), None),
    }
}

/// The transcript before its first row: a quiet, centred invitation, with
/// nothing else in the column (§7.3).
fn empty_state(agent: AgentKey, palette: &Palette) -> AnyElement {
    let (note, detail) = empty_note(agent);
    let mut column = v_flex()
        .size_full()
        .justify_center()
        .items_center()
        .gap_2()
        .px_6()
        .text_center();

    // The mark belongs to the coordinator's view: it is the app talking about
    // itself. A lane's note is about that lane.
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

    // The id names the state, and a lane's carries its number: the two are
    // addressable apart.
    match agent {
        AgentKey::Coordinator => column.id("transcript-empty"),
        AgentKey::Lane(n) => column.id(("transcript-empty-lane", n as u64)),
    }
    .test_support()
    .into_any_element()
}

impl Render for TranscriptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.data.clone();
        let view = cx.weak_entity();
        let theme = cx.theme().clone();

        // Nothing to scroll yet: an empty transcript is a state of its own.
        if self.data.read(cx).rows.is_empty() {
            return empty_state(self.agent, &Palette::from_app(cx));
        }

        // One frame's worth: the documents of the rows about to be rendered are
        // brought up to date by the renderer itself, and the ones the reader has
        // left behind go.
        self.data.update(cx, |data, _| data.begin_frame());

        MessageScroller::new(
            "transcript",
            self.scroller.clone(),
            move |index, _window, cx| {
                data.update(cx, |data, cx| rows::render_row(data, index, &view, cx))
            },
        )
        // Rows carry their own leading space (they know what came before them),
        // so the list adds none: only the column inset around the measure.
        .with_list_style(StyleRefinement::default().px_4().py_4())
        .with_row_style(StyleRefinement::default().pb_0())
        // A pill rather than a bare arrow: it says what it does, and carries an
        // accessible name instead of only a tooltip.
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
        .min_h_0()
        .into_any_element()
    }
}
