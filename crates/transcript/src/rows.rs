//! Row renderers: one function per item kind, all of them theme-tokened text.
//!
//! Every row is centred inside the shared reading measure and carries its own leading
//! space, so the list can mount rows edge to edge and the transcript still reads as turns:
//! tight inside a group of tool calls or quiet lines, a little apart between the parts of
//! one turn, and separated by a hairline when a new turn begins.
//!
//! Nothing here sniffs at text: the item's `kind` says what it is (CONTRACT §4.1) and how
//! loudly it is said (a notice's `severity`, a lane event's `severity`), so a row is a user
//! turn, rendered markdown, a one-line tool row that opens onto its arguments and result
//! as a key/value list, a structured lane report, a goal transition, a compaction divider,
//! or a quiet line about something evo, the swarm or a person did.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::base::{Easing, SelectableText, TextView, TextViewMotion};
use gpui_kit::component::{h_flex, Icon, IconName};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, linear_color_stop, linear_gradient, point, px, Animation, AnimationExt as _, AnyElement,
    App, ClipboardItem, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, Pixels, Point, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, WeakEntity,
};
use serde_json::Value;
use session::{
    AssistantItem, Compaction, GoalEventKind, Item, ItemId, ItemKind, LaneEvent, LaneReport,
    Notice, NoticeSeverity, QueuePosition, RunOutcome, ToolItem, UserItem, UserStatus,
};

use crate::imgcheck::picture;
use crate::ImageState;

use crate::style::{mix, text_style, Palette, BLOCK_GAP, GROUP_GAP, MEASURE, TIGHT_GAP, TURN_GAP};
use crate::{link, markdown, TranscriptData, TranscriptView};
use widgets::glyph;

/// Fade window for text appended by a streaming delta (§2.8).
const STREAM_FADE: Duration = Duration::from_millis(350);
/// A little later for each further word of one delta, so chunks overlap into
/// one gradient tail instead of blinking in.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(30);
/// Longest a tool call's arguments an open row shows, in characters — the
/// command among them, which is one of the arguments.
pub(crate) const ARGUMENTS_LIMIT: usize = 1_024;
/// Longest what a tool call returned an open row shows, in characters. A result
/// may be longer than the call that asked for it; the whole of it is one click away
/// (`GET /items/<id>`).
pub(crate) const RESULT_LIMIT: usize = 2_048;
/// The key a call's command arrives under. evo's tools name it: `bash` says
/// `command` (`read` says `path`, `eval` says `code`).
const COMMAND_KEY: &str = "command";
/// Longest a one-line value shows before it is elided; the whole text stays a
/// hover away.
pub(crate) const VALUE_LIMIT: usize = 96;
/// Height of a collapsed tool row, so a long run of them stays a list. It holds
/// one line of the row's own text, so it follows the reader's zoom (§7.2).
pub(crate) const TOOL_ROW_HEIGHT: f32 = 24.;
/// The caret's column in a tool row's head: `Rows.css`'s `.tc-caret` is 16px
/// square, holding the design's 12px chevron. A plain number so the head's own
/// geometry can be added up in a `const`: [`TC_BODY_INDENT`] is derived from it.
const DISCLOSURE_WIDTH: f32 = 16.;
/// The disclosure chevron: a glyph with about ten pixels of ink, centred in its
/// own column. The glyph box is larger than the ink a chevron actually draws.
/// Width of the key column of an expanded argument list: enough for a nested
/// key like `diff.removed` without eliding it.
/// The key column of a tool call's arguments: `Rows.css`'s
/// `.tc-kv{grid-template-columns: 72px 1fr}`.
pub(crate) const KEY_WIDTH: Pixels = px(72.);
/// How far each level of a nested payload is indented from the level above it.
pub(crate) const NEST_INDENT: Pixels = px(12.);
/// How deep a payload is drawn out before what is left of it is summarised.
pub(crate) const MAX_DEPTH: usize = 4;
/// Longest array drawn item by item.
pub(crate) const MAX_ARRAY: usize = 20;
/// The gap between a key and its value — `Rows.css`'s `.tc-kv{gap:3px 12px}` —
/// and so the indent of a block whose content has no keys of its own.
pub(crate) const COLUMN_GAP: Pixels = px(12.);
/// The size a key is drawn at, in the UI font: a key is a label, not payload. The
/// text sizes and line heights here are the design's own numbers at a zoom of 1.0;
/// every one is drawn through [`Palette::scaled`] (§7.2).
const KEY_SIZE: f32 = 12.;
/// The size a panel's caption is drawn at: `Rows.css`'s `.tc-caption`, 11.5px.
const CAPTION_SIZE: f32 = 11.5;
/// The size a tool row's name is drawn at, and the size of the status word
/// beside it.
const NAME_SIZE: f32 = 13.;
const STATUS_SIZE: f32 = 12.;
const STATUS_LINE: f32 = 18.;
/// The line height of payload text, as a multiple of its size.
pub(crate) const PAYLOAD_LINE_HEIGHT: f32 = 1.45;
/// Width of the label column of a report row.
const REPORT_LABEL_WIDTH: Pixels = px(80.);
/// `.rp`: its head's height and type, and the type of one field's row — the label
/// and the value under it share the design's 20px line box.
const REPORT_HEAD: f32 = 36.;
const REPORT_SIZE: f32 = 13.;
const REPORT_LABEL_SIZE: f32 = 12.;
const REPORT_VALUE_SIZE: f32 = 13.5;
const REPORT_VALUE_LINE: f32 = 20.;

/// The waiting pips that hold an assistant row's place between `message-start`
/// and the first delta: one cycle of the pulse, and how long each pip is.
const DOT_CYCLE: Duration = Duration::from_millis(1_200);
const DOT_SIZE: Pixels = px(5.);
/// Where each pip is in the cycle.
pub(crate) const DOT_PHASES: [f32; 3] = [0., 1. / 3., 2. / 3.];
/// The floor of a pip's fade.
pub(crate) const DOT_INK_FLOOR: f32 = 0.25;

/// The motion a streaming assistant row is rendered with.
pub(crate) fn stream_motion() -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(STREAM_FADE)
        .with_stream_fade_stagger(STREAM_FADE_STAGGER)
        .with_stream_fade_easing(Easing::EaseOut)
}

/// The group a row declares so its copy buttons are out of the way until the
/// reader is over it.
pub(crate) const COPY_GROUP: &str = "transcript-copy";

/// How long a copy button says "Copied" after it was used.
const COPIED_HOLD: Duration = Duration::from_millis(1_200);

/// The size of a copy button's icon and label, and the line box under it.
const COPY_SIZE: f32 = 11.;
const COPY_LINE: f32 = 14.;

/// Which copy button was used.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CopyTarget {
    /// An assistant message: its whole markdown source.
    Message(usize),
    /// One of the message's code blocks, at the byte offset its fence opens at.
    Block(usize, usize),
}

/// How often each copy button has been used.
#[derive(Default)]
pub(crate) struct CopyFeedback {
    uses: Mutex<HashMap<CopyTarget, u64>>,
}

impl CopyFeedback {
    fn uses(&self, target: CopyTarget) -> u64 {
        self.lock().get(&target).copied().unwrap_or(0)
    }

    fn record(&self, target: CopyTarget) {
        *self.lock().entry(target).or_default() += 1;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CopyTarget, u64>> {
        self.uses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The element id of a cell of one row: the cell's name, and the item it belongs to. An
/// item's id is stable, so a cell keeps its identity across a patch, a prepend and a
/// scroll.
pub(crate) fn row_id(name: impl Into<SharedString>, id: &str) -> ElementId {
    (ElementId::from(name.into()), id.to_string()).into()
}

/// The row a copy button's icon and word sit in.
fn copy_face() -> Div {
    div().flex().items_center().gap(px(4.))
}

/// Fill a copy button's face: the resting one, or the acknowledgement it shows
/// while `copied`.
fn fill_copy_face<E: ParentElement>(
    face: E,
    copied: bool,
    with_label: bool,
    key: &ElementId,
    size: Pixels,
) -> E {
    let icon = if copied {
        IconName::Check
    } else {
        IconName::Copy
    };
    let mut face = face.child(Icon::new(icon).size(size));
    if copied {
        face = face.child(
            div()
                .id((key.clone(), "copied"))
                .child("Copied")
                .test_support(),
        );
    } else if with_label {
        face = face.child("Copy");
    }
    face
}

/// A copy button: quiet, and out of the way until the reader hovers the row it
/// belongs to.
fn copy_button(
    id: impl Into<ElementId>,
    target: CopyTarget,
    text: SharedString,
    feedback: &Arc<CopyFeedback>,
    with_label: bool,
    palette: &Palette,
) -> Stateful<Div> {
    let id = id.into();
    let feedback = feedback.clone();
    let uses = feedback.uses(target);
    let size = palette.scaled(COPY_SIZE);

    let button = div()
        .id(id.clone())
        .px(px(6.))
        .py(px(1.))
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_1()
        .border_color(palette.border)
        .text_size(size)
        .line_height(palette.scaled(COPY_LINE))
        .text_color(palette.muted_foreground)
        .cursor_pointer()
        .hover(|style| style.text_color(palette.foreground))
        .invisible()
        .group_hover(COPY_GROUP, |style| style.visible())
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
            feedback.record(target);
            window.refresh();
        });

    let key = id.clone();
    if uses == 0 {
        button.child(fill_copy_face(copy_face(), false, with_label, &key, size))
    } else {
        button.child(copy_face().with_animation(
            (id, uses.to_string()),
            Animation::new(COPIED_HOLD),
            move |face, delta| fill_copy_face(face, delta < 1., with_label, &key, size),
        ))
    }
}

/// How a row spaces itself against the one before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    User,
    Assistant,
    Tool,
    Report,
    /// A quiet line: a notice, a lane event, a goal transition, a command note, a run's
    /// outcome. They read as one block when several land together.
    Quiet,
    /// Injected context: a note about the session, not a part of it.
    Context,
    /// A compaction marker: a divider, which carries its own space.
    Divider,
}

impl Group {
    fn of(kind: &ItemKind) -> Self {
        match kind {
            ItemKind::User(_) => Self::User,
            ItemKind::Assistant(_) => Self::Assistant,
            ItemKind::Tool(_) => Self::Tool,
            ItemKind::LaneReport(_) => Self::Report,
            ItemKind::Compaction(_) => Self::Divider,
            ItemKind::Context(_) => Self::Context,
            ItemKind::LaneEvent(_)
            | ItemKind::Goal(_)
            | ItemKind::CommandNote(_)
            | ItemKind::HumanAction(_)
            | ItemKind::Notice(_)
            | ItemKind::RunOutcome(_)
            | ItemKind::Recovery(_)
            | ItemKind::ProviderRetry(_)
            | ItemKind::Unknown { .. } => Self::Quiet,
        }
    }

    /// The air the design puts above and below this kind of row, as a block
    /// margin: `.tc{margin:10px 0}`, `.rp{margin:14px 0}`, `p{margin:10px 0}`,
    /// `.thinking{margin:10px 0}`.
    ///
    /// The quiet lines and the context rows have no counterpart in the design
    /// (the app draws a lane event, a goal transition, a notice and an injected
    /// context note itself), so they keep the app's own quieter rhythm.
    fn margin(self) -> Pixels {
        match self {
            // A user row's own margin depends on whether it opens a turn: see
            // `row_margin`.
            Self::User => px(0.),
            Self::Assistant | Self::Tool => px(10.),
            Self::Report => px(14.),
            Self::Quiet | Self::Context => BLOCK_GAP,
            Self::Divider => BLOCK_GAP,
        }
    }
}

/// The air a row asks for above and below itself.
///
/// A user row that opens a turn has none of its own — the turn rule above it
/// carries that space — but one that does not (queued, or cancelled before evo
/// ever took it) is an ordinary block in the flow, so it keeps the ordinary
/// block air. Without that a queued row butts straight up against the row above
/// it, since it gets neither the rule nor a margin.
fn row_margin(item: &Item) -> Pixels {
    match Group::of(&item.kind) {
        Group::User if !opens_a_turn(&item.kind) => BLOCK_GAP,
        group => group.margin(),
    }
}

/// Space above `row`, given the row before it.
///
/// The design's rows carry their own vertical margins and a block flow collapses
/// the two it finds side by side, so the space between one row and the next is
/// the larger of the two margins — 10px between two tool calls, 14px wherever a
/// report is involved. A run of quiet lines is the app's own and stays tight,
/// and a turn opens with its own separator, which carries the space itself.
fn gap_before(previous: Option<&Item>, row: &Item) -> Pixels {
    let Some(previous) = previous else {
        return px(0.);
    };
    let group = Group::of(&previous.kind);
    let current = Group::of(&row.kind);
    match current {
        // A sent turn opens with its own rule, which carries the space itself;
        // a queued or cancelled one opens none and is spaced like any block.
        Group::User if opens_a_turn(&row.kind) => px(0.),
        Group::Divider => BLOCK_GAP,
        Group::Quiet | Group::Context if group == current => TIGHT_GAP,
        _ => row_margin(previous).max(row_margin(row)),
    }
}

/// Whether a user row is a turn of the reader's that evo has taken: only those open a
/// turn boundary. A queued row is not a turn yet, and a cancelled one never was.
pub(crate) fn opens_a_turn(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::User(user) if user.status == UserStatus::Sent)
}

/// Render the row at `index`, or an empty element when the list asks for a row
/// that is no longer there.
pub(crate) fn render_row(
    data: &mut TranscriptData,
    index: usize,
    view: &WeakEntity<TranscriptView>,
    cx: &mut Context<TranscriptData>,
) -> AnyElement {
    if data.items.get(index).is_none() {
        return div().into_any_element();
    }

    // An assistant row's document is brought up to date here, at the frame that
    // shows the row, rather than on every delta that arrives for it.
    let waiting = match &data.items[index].kind {
        ItemKind::Assistant(assistant) => {
            assistant.is_streaming() && assistant.text.trim().is_empty()
        }
        _ => false,
    };
    if matches!(data.items[index].kind, ItemKind::Assistant(_)) && !waiting {
        data.sync_document(index, cx);
    }
    // A report's own fields are markdown too: each document is brought up to date
    // here, at the frame that shows the row.
    if let ItemKind::LaneReport(report) = &data.items[index].kind {
        let fields: Vec<&'static str> = report_fields(report)
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(label, _)| label)
            .collect();
        for label in fields {
            data.sync_field_document(index, label, cx);
        }
    }

    let item = &data.items[index];
    let palette = Palette::from_app(cx);
    let previous = index.checked_sub(1).and_then(|index| data.items.get(index));

    let focus = data.focus.clone();
    let mut stack = div().flex().flex_col().w_full().min_w_0();
    // A user turn opens a new turn: say so, rather than printing a run marker. The
    // number counts from the head of the record, not from the window: a turn hidden
    // above the rows on screen still opened, and the label a reader sees does not
    // move when the window does.
    if opens_a_turn(&item.kind) && previous.is_some() {
        stack = stack.child(turn_separator(data.turn_number(index), &palette));
    }

    stack = stack.child(match &item.kind {
        ItemKind::User(user) => user_row(item.id.clone(), user, data, view, &palette),
        ItemKind::Context(context) => context_row(
            item.id.clone(),
            &context.key,
            &context.text,
            data.expanded.contains(&item.id),
            view,
            &palette,
        ),
        ItemKind::Assistant(assistant) => assistant_row(item, assistant, data, cx, &palette),
        ItemKind::Tool(tool) => tool_row(item, tool, data, view, &palette),
        ItemKind::LaneReport(report) => report_row(item.id.clone(), report, data, cx, &palette),
        ItemKind::LaneEvent(event) => lane_event_row(item.id.clone(), event, &palette),
        ItemKind::Goal(goal) => goal_row(
            item.id.clone(),
            goal,
            data.expanded.contains(&item.id),
            view,
            &palette,
        ),
        ItemKind::CommandNote(note) => command_note_row(
            item.id.clone(),
            &note.command,
            &note.text,
            data.expanded.contains(&item.id),
            view,
            &palette,
        ),
        ItemKind::HumanAction(action) => quiet_row(
            QuietRow {
                id: item.id.clone(),
                header: "transcript-action",
                head: match action.lanes_label() {
                    Some(lanes) => format!("Stopped {lanes}"),
                    None => "Stopped the run".to_string(),
                },
                trailing: None,
                text: &action.action,
                block: "transcript-action-text",
            },
            data.expanded.contains(&item.id),
            view,
            &palette,
        ),
        ItemKind::Notice(notice) => notice_row(item.id.clone(), notice, &palette),
        ItemKind::RunOutcome(outcome) => run_outcome_row(item.id.clone(), outcome, &palette),
        ItemKind::Compaction(compaction) => compaction_row(item.id.clone(), compaction, &palette),
        ItemKind::Recovery(recovery) => quiet_row(
            QuietRow {
                id: item.id.clone(),
                header: "transcript-recovery",
                head: match recovery.reason.as_deref() {
                    Some(reason) => format!("Recovered · {reason}"),
                    None => format!("Recovered · {}", recovery.status),
                },
                trailing: recovery.code.clone(),
                text: &recovery.status,
                block: "transcript-recovery-text",
            },
            data.expanded.contains(&item.id),
            view,
            &palette,
        ),
        ItemKind::ProviderRetry(retry) => {
            let text = match retry.reason.as_deref() {
                Some(reason) => format!(
                    "Retrying provider ({}/{}) in {} ms — {reason}",
                    retry.attempt, retry.max, retry.delay_ms
                ),
                None => format!(
                    "Retrying provider ({}/{}) in {} ms",
                    retry.attempt, retry.max, retry.delay_ms
                ),
            };
            quiet_line(
                item.id.clone(),
                "transcript-retry",
                &text,
                palette.info,
                &palette,
            )
        }
        ItemKind::Unknown { kind, text } => quiet_line(
            item.id.clone(),
            "transcript-unknown",
            &format!("{kind} · {text}"),
            palette.muted_foreground,
            &palette,
        ),
    });

    div()
        .id(row_id("transcript-row", &item.id))
        .w_full()
        .pt(gap_before(previous, item))
        .flex()
        .justify_center()
        .child(
            div()
                .id(row_id("transcript-measure", &item.id))
                .w_full()
                .min_w_0()
                .max_w(px(MEASURE))
                // The body size of the transcript, at the reader's zoom (§7.2):
                // what a row does not set a size of its own inherits this — the
                // message's markdown among them, which reads it off the ambient
                // text style when its document is laid out.
                .text_size(palette.font_size)
                .test_support()
                .child(stack),
        )
        .test_support()
        .track_focus(&focus)
        .tab_stop(false)
        .into_any_element()
}

/// The hairline between two turns, labelled with the turn it opens.
/// The rule between two turns: a hairline 26px above the turn it opens, with
/// `turn N` sitting on it at the right, in the page's own colour so it reads as a
/// label on the line rather than a break in it (`.turn-rule`).
fn turn_separator(turn: usize, palette: &Palette) -> AnyElement {
    // The design's `.turn-rule`: a 26px band whose floor is the hairline,
    // `margin-top: 26px`, and a 12px label at the right whose box straddles the
    // line by 7px (`bottom: -7px`), on the page's own colour so it reads as a
    // label *on* the rule rather than a break in it.
    //
    // The 7px the label hangs below the line is reserved by the band here rather
    // than overhung: gpui paints in tree order and has no `z-index`, so a label
    // that hung into the next row's box would be covered by that row's own
    // surface. The geometry the design cares about is exactly kept — the label's
    // centre is on the line, the row after it starts below the line — and what
    // the reserve costs is 7px of page colour under the line.
    //
    // The band, the label's own line box and the 7px it hangs all follow the
    // reader's zoom (the hairline does not: a rule is a rule); the 26px of air
    // above it is space, and stays as the design drew it.
    let rule = palette.scaled(TURN_RULE);
    let overhang = palette.scaled(TURN_LABEL_OVERHANG);
    div()
        .id(("transcript-turn", turn))
        .relative()
        .w_full()
        // 26px of band, the 1px hairline on its floor, and the 7px the label hangs
        // below the line reserved under it.
        .h(rule + px(TURN_LINE) + overhang)
        .mt(TURN_GAP)
        .text_size(palette.scaled(TURN_LABEL_SIZE))
        .child(
            div()
                .id(("transcript-turn-line", turn))
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .top(rule)
                .h(px(TURN_LINE))
                .bg(palette.border)
                .test_support(),
        )
        .child(
            div()
                .id(("transcript-turn-label", turn))
                .absolute()
                .right(px(0.))
                .bottom(px(0.))
                .pl(px(8.))
                // The design's own line box for a 12px label: `line-height: 1.5`,
                // which is what puts its centre a hair above the line.
                .line_height(palette.scaled(TURN_LABEL_LINE))
                .bg(palette.background)
                .text_color(palette.muted_foreground)
                .child(format!("turn {turn}"))
                .test_support(),
        )
        .test_support()
        .into_any_element()
}

/// The hairline itself: `border-bottom: 1px` in the design.
pub(crate) const TURN_LINE: f32 = 1.;

/// How far the design's label hangs below the rule's line (`bottom: -7px`,
/// measured from the line's own bottom edge), and so how much of the band is
/// reserved under it.
pub(crate) const TURN_LABEL_OVERHANG: f32 = 7.;

/// The turn rule's own height: `.turn-rule { height: 26px }`, and the label's own
/// type — 12px on the page's `line-height: 1.5`.
const TURN_RULE: f32 = 26.;
const TURN_LABEL_SIZE: f32 = 12.;
const TURN_LABEL_LINE: f32 = 18.;

/// A user turn: plain text on a muted card, with the accent bar that marks where the turn
/// starts. A queued turn is held back (muted, said so, and cancellable); a cancelled one is
/// drawn as what it is rather than removed, so the reader sees what became of their words.
///
/// `.user-row{font-size:14px;line-height:1.5}` — the two numbers, at the design's
/// own zoom.
const USER_SIZE: f32 = 14.;
const USER_LINE: f32 = 21.;

fn user_row(
    id: ItemId,
    user: &UserItem,
    data: &TranscriptData,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let queued = user.status == UserStatus::Queued;
    let cancelled = user.status == UserStatus::Cancelled;
    let accent = if queued {
        palette.muted_foreground
    } else if cancelled {
        palette.border
    } else {
        palette.primary
    };
    // Words evo has not taken (or never will) are held back, not shouted.
    let text_color = if cancelled || queued {
        palette.muted_foreground
    } else {
        palette.foreground
    };

    let mut card = div()
        .id(row_id("transcript-user", &id))
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_l_3()
        .border_color(accent)
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .text_size(palette.scaled(USER_SIZE))
        // `.user-row{font-size:14px}` under the page's `line-height: 1.5`.
        .line_height(palette.scaled(USER_LINE))
        .text_color(text_color)
        .child(SelectableText::new(
            row_id("transcript-user-text", &id),
            user.text.clone(),
        ));

    if !user.images.is_empty() {
        card = card.child(image_row(&id, user, data, view, palette));
    }

    if cancelled {
        card = card.child(caption(
            &row_id("transcript-user-cancelled", &id),
            "cancelled",
            palette,
        ));
    } else if queued {
        card = card.child(queued_footer(id, user, palette, view));
    }

    card.test_support().into_any_element()
}

/// The pictures a turn carried: a small thumbnail each, fetched only while the row is on
/// screen, with the reader's click opening one at full size.
///
/// Nothing is inlined in a transcript, so nothing is drawn until the bytes arrive: a
/// thumbnail that is still loading says so, and one that could not be read says that
/// instead of leaving a hole.
fn image_row(
    id: &ItemId,
    user: &UserItem,
    data: &TranscriptData,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let slots = user.images.iter().enumerate().map(|(n, image)| {
        let n = n as u32;
        let key = (id.clone(), n);
        match data.images.get(&key) {
            Some(ImageState::Ready(frame)) => {
                let full = data.full_images.contains(&key);
                let view = view.clone();
                let click_id = id.clone();
                let name = image.name.clone();
                div()
                    .id(row_id(format!("transcript-image-{n}"), id))
                    .cursor_pointer()
                    .rounded(palette.radius)
                    .border_1()
                    .border_color(palette.border)
                    .overflow_hidden()
                    .on_click(move |_, _, cx| {
                        let _ =
                            view.update(cx, |view, cx| view.toggle_image_size(&click_id, n, cx));
                    })
                    .aria_label(format!(
                        "{name} — click to {}",
                        if full { "shrink" } else { "open" }
                    ))
                    .child(picture(
                        frame.clone(),
                        if full { FULL_IMAGE } else { THUMBNAIL },
                    ))
                    .test_support()
                    .into_any_element()
            }
            Some(ImageState::Failed) => placeholder(
                row_id(format!("transcript-image-note-{n}"), id),
                &format!("{} — could not be shown", image.name),
                palette,
            ),
            _ => placeholder(
                row_id(format!("transcript-image-note-{n}"), id),
                &format!("{} — loading…", image.name),
                palette,
            ),
        }
    });
    h_flex()
        .id(row_id("transcript-user-images", id))
        .flex_wrap()
        .gap_2()
        .pt_1()
        .children(slots)
        .test_support()
        .into_any_element()
}

/// How tall a thumbnail is, and how tall the same picture is when it is opened: a row is a
/// row until the reader asks for the whole picture.
const THUMBNAIL: Pixels = px(120.);
const FULL_IMAGE: Pixels = px(340.);

/// One image the transcript is not showing: a muted line saying what it is and where it
/// got to, in place of a hole.
fn placeholder(id: ElementId, text: &str, palette: &Palette) -> AnyElement {
    div()
        .id(id)
        .aria_label(text.to_string())
        .px_2()
        .py_1()
        .rounded(palette.radius)
        .bg(palette.muted)
        .text_size(palette.scaled(CAPTION_SIZE))
        .text_color(palette.muted_foreground)
        .child(text.to_string())
        .test_support()
        .into_any_element()
}

/// What a queued turn says under itself, and the one thing that can be done about it: the
/// status line, and the cancel button that takes the words back before evo has them.
fn queued_footer(
    id: ItemId,
    user: &UserItem,
    palette: &Palette,
    view: &WeakEntity<TranscriptView>,
) -> AnyElement {
    // `now` is drained at the run's next step; `after_run` waits for the run to end
    // (a stop the swarm queued, an input from another client).
    let caption = match user.queue {
        Some(QueuePosition::AfterRun) => "queued · sent when the run ends",
        _ => "queued · sent at the next step",
    };
    let caption_id = row_id("transcript-queued", &id);
    let view = view.clone();
    let button_id = id.clone();
    h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(caption_id)
                .text_size(palette.scaled(CAPTION_SIZE))
                .text_color(palette.muted_foreground)
                .aria_label(caption)
                .child(caption)
                .test_support(),
        )
        .child(
            div()
                .id(row_id("transcript-cancel", &id))
                .px(px(6.))
                .py(px(1.))
                .rounded(palette.radius)
                .border_1()
                .border_color(palette.border)
                .text_size(palette.scaled(CAPTION_SIZE))
                .line_height(palette.scaled(COPY_LINE))
                .text_color(palette.muted_foreground)
                .cursor_pointer()
                .hover(|style| style.text_color(palette.destructive))
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |view, cx| view.cancel_queued(&button_id, window, cx));
                })
                .child("Cancel")
                .test_support(),
        )
        .into_any_element()
}

/// How much of an opened context row's text is on screen at once.
pub(crate) const CONTEXT_BLOCK_LINES: usize = 12;

/// How wide a quiet line's tooltip may grow.
const NOTICE_TOOLTIP_WIDTH: Pixels = px(520.);

/// A row that is one quiet line until it is opened.
struct QuietRow<'a> {
    id: ItemId,
    header: &'static str,
    head: String,
    trailing: Option<String>,
    text: &'a str,
    block: &'static str,
}

/// Draw a quiet row: one line, muted, closed until the reader asks for it.
fn quiet_row(
    row: QuietRow<'_>,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let QuietRow {
        id,
        header: name,
        head,
        trailing,
        text,
        block,
    } = row;
    let view = view.clone();
    let aria = match &trailing {
        Some(trailing) => format!("{head} {trailing}"),
        None => head.clone(),
    };

    let click_id = id.clone();
    let header = div()
        .id(row_id(name, &id))
        .flex()
        .items_center()
        .gap_2()
        .h(palette.scaled(TOOL_ROW_HEIGHT))
        .cursor_pointer()
        .aria_label(aria)
        .aria_expanded(expanded)
        .on_click(move |_, _, cx| {
            let _ = view.update(cx, |view, cx| view.toggle_expanded(&click_id, cx));
        })
        .child(caret(expanded, palette))
        // The head and its trailing figure read as one line — `Goal · updated · 240k/50k`
        // — so they sit together with a word space between them, not the row's gap: the
        // head may truncate, and the figure after it never does.
        .child(
            div()
                .flex()
                .items_center()
                .min_w_0()
                .flex_shrink(1.)
                .text_size(palette.scaled(NAME_SIZE))
                .text_color(palette.muted_foreground)
                .child(div().min_w_0().flex_shrink(1.).truncate().child(head))
                .children(trailing.map(|trailing| {
                    div()
                        .id(row_id(format!("{name}-trailing"), &id))
                        .flex_none()
                        .whitespace_nowrap()
                        // A no-break space: an ordinary one at the start of a run is
                        // the kind a layout may drop.
                        .child(format!("\u{a0}{trailing}"))
                        .test_support()
                })),
        )
        .test_support();

    let mut row = div().w_full().min_w_0().flex().flex_col().child(header);
    if expanded {
        row = row.child(quiet_block(block, id, text, palette));
    }
    row.into_any_element()
}

/// Content an extension injected (`evo:inject-context`): one quiet line saying what it
/// is and where it came from, which opens onto the text itself.
fn context_row(
    id: ItemId,
    key: &str,
    text: &str,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    quiet_row(
        QuietRow {
            id,
            header: "transcript-context",
            head: format!("Context · {}", context_label(key)),
            trailing: None,
            text,
            block: "transcript-context-text",
        },
        expanded,
        view,
        palette,
    )
}

/// A command the reader ran, answered with instructions for the agent.
fn command_note_row(
    id: ItemId,
    command: &str,
    text: &str,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    quiet_row(
        QuietRow {
            id,
            header: "transcript-command",
            head: format!("Command · {command}"),
            trailing: None,
            text,
            block: "transcript-command-text",
        },
        expanded,
        view,
        palette,
    )
}

/// What a context row calls its key.
pub(crate) fn context_label(key: &str) -> String {
    match key {
        "global-memory" => "global memory".to_string(),
        "project-memory" => "project memory".to_string(),
        other => other.to_string(),
    }
}

/// A goal transition: one quiet line naming what happened to the goal, which opens onto
/// the objective itself.
///
/// The goal's **id** is evo's own handle on it (`g-b7ba`), not something a reader reads:
/// the line names the event and the objective, and neither the header nor the block
/// spells the id out.
fn goal_row(
    id: ItemId,
    goal: &session::GoalItem,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let head = match goal.event {
        GoalEventKind::Created | GoalEventKind::ObjectiveUpdated => {
            match goal.objective.as_deref() {
                Some(objective) => format!("Goal · {} — {objective}", goal.event.label()),
                None => format!("Goal · {}", goal.event.label()),
            }
        }
        _ => format!("Goal · {}", goal.event.label()),
    };
    let trailing = match (goal.budget, goal.tokens) {
        (Some(budget), Some(tokens)) => Some(format!(
            "· {}/{}",
            session::k_tokens(tokens),
            session::k_tokens(budget)
        )),
        (None, Some(tokens)) => Some(format!("· {}", session::k_tokens(tokens))),
        _ => None,
    };
    let text = goal_text(goal);
    quiet_row(
        QuietRow {
            id,
            header: "transcript-goal",
            head,
            trailing,
            text: &text,
            block: "transcript-goal-text",
        },
        expanded,
        view,
        palette,
    )
}

/// What a goal transition's block holds: the objective itself, or — with none on the
/// item — the event's own words. The goal's id is not part of it: it is evo's handle on
/// the goal (`b1`), not something a reader reads.
pub(crate) fn goal_text(goal: &session::GoalItem) -> String {
    match &goal.objective {
        Some(objective) => objective.clone(),
        None => goal.event.label().to_string(),
    }
}

/// A lane's transition, as the swarm published it: one line — `Lane 2 · crashed — …` —
/// in the colour its own `severity` asks for.
fn lane_event_row(id: ItemId, event: &LaneEvent, palette: &Palette) -> AnyElement {
    let mut text = event.event.label().to_string();
    if let Some(outcome) = event.outcome.as_deref().filter(|o| *o != "stop") {
        text.push_str(&format!(" ({outcome})"));
    }
    if let Some(goal) = event.goal_status.as_deref() {
        text.push_str(&format!(" · goal {goal}"));
    }
    if let Some(detail) = event.detail.as_deref() {
        text.push_str(&format!(" — {detail}"));
    }
    quiet_source_line(
        id,
        "transcript-lane-event",
        Some(&format!("Lane {}", event.lane)),
        palette.muted_foreground,
        &text,
        severity_color(event.severity, palette),
        palette,
    )
}

/// A notice, as the server said it: severity decides how loud the line is, and the source
/// says who is talking.
fn notice_row(id: ItemId, notice: &Notice, palette: &Palette) -> AnyElement {
    quiet_source_line(
        id,
        "transcript-notice",
        notice.source.label(),
        palette.muted_foreground,
        &notice.text,
        severity_color(notice.severity, palette),
        palette,
    )
}

/// A run that ended as something other than `stop`, said in the run's own vocabulary.
fn run_outcome_row(id: ItemId, outcome: &RunOutcome, palette: &Palette) -> AnyElement {
    let color = match outcome.outcome.as_str() {
        "error" => palette.destructive,
        _ => palette.info,
    };
    quiet_line(
        id,
        "transcript-run-outcome",
        &outcome.text(),
        color,
        palette,
    )
}

/// The point the scrollback pages across: a divider naming what was compacted away.
fn compaction_row(id: ItemId, compaction: &Compaction, palette: &Palette) -> AnyElement {
    let label = if compaction.manual {
        "compacted (manual)"
    } else {
        "compacted"
    };
    let tokens = format!(
        "{} → {}",
        session::k_tokens(compaction.tokens_before),
        session::k_tokens(compaction.tokens_after)
    );
    let mut column = div()
        .id(row_id("transcript-compaction", &id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .pt(GROUP_GAP)
        .pb(GROUP_GAP)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().h(px(1.)).bg(palette.border))
                .child(
                    div()
                        .id(row_id("transcript-compaction-label", &id))
                        .flex_none()
                        .text_size(palette.scaled(CAPTION_SIZE))
                        .text_color(palette.muted_foreground)
                        .aria_label(format!("context {label}, {tokens}"))
                        .child(format!("context {label} · {tokens}"))
                        .test_support(),
                )
                .child(div().flex_1().h(px(1.)).bg(palette.border)),
        );
    if !compaction.summary.trim().is_empty() {
        column = column.child(
            div()
                .id(row_id("transcript-compaction-summary", &id))
                .w_full()
                .min_w_0()
                .px_3()
                .text_size(palette.scaled(STATUS_SIZE))
                .line_height(palette.scaled(STATUS_LINE))
                .text_color(palette.muted_foreground)
                .child(SelectableText::new(
                    "transcript-compaction-text",
                    compaction.summary.clone(),
                ))
                .test_support(),
        );
    }
    column.test_support().into_any_element()
}

/// How loud a severity is: an error is the destructive colour, a warning the warning one,
/// and anything else the quiet info colour.
/// What a system line is drawn in, by how loud it is.
///
/// An info line is chrome — the muted ink, no accent at all — because a line the
/// server says in place is not a link and not a warning. Warn and error are the
/// design's own two colours; nothing here is the primary accent.
pub(crate) fn severity_color(severity: NoticeSeverity, palette: &Palette) -> gpui_kit::Hsla {
    match severity {
        NoticeSeverity::Error => palette.destructive,
        NoticeSeverity::Warn => palette.warning,
        NoticeSeverity::Info => palette.muted_foreground,
    }
}

/// The text of an opened quiet row: the payload face, selectable, capped at
/// [`CONTEXT_BLOCK_LINES`] with a scroll of its own.
fn quiet_block(name: &'static str, id: ItemId, text: &str, palette: &Palette) -> AnyElement {
    let content: ElementId = row_id(format!("{name}-content"), &id);
    let run: ElementId = row_id(format!("{name}-run"), &id);
    div()
        .id(row_id(name, &id))
        .w_full()
        .min_w_0()
        .max_h(palette.payload_size * (PAYLOAD_LINE_HEIGHT * CONTEXT_BLOCK_LINES as f32))
        .overflow_y_scroll()
        .rounded(palette.radius)
        .border_1()
        .border_color(palette.border)
        .px_2()
        .py_1()
        .mb(GROUP_GAP)
        .font_family(palette.mono.clone())
        .text_size(palette.payload_size)
        .line_height(palette.payload_size * PAYLOAD_LINE_HEIGHT)
        .text_color(palette.foreground)
        .child(
            div()
                .id(content)
                .w_full()
                .min_w_0()
                .test_support()
                .child(SelectableText::new(run, text.to_string())),
        )
        .test_support()
        .into_any_element()
}

/// `.thinking-label`'s own type and the line box under it, and `.thinking-text`'s
/// — the design's numbers, scaled by the reader's zoom where they are drawn.
const THINKING_LABEL_SIZE: f32 = 12.;
const THINKING_LABEL_LINE: f32 = 18.;
const THINKING_SIZE: f32 = 14.;
const THINKING_LINE: f32 = 21.;

/// An assistant message: the retained markdown document, its optional thinking
/// text, and the error that ended it, if any.
///
/// The thinking comes *first*: it is what the model said to itself before it
/// wrote the message or reached for a tool, and the design draws it above the
/// message it belongs to (`Transcript.tsx`'s `Thinking`, before its `Markdown`).
/// The air between the two is the design's own — `.thinking{margin:10px 0}` and
/// `p{margin:10px 0}` collapse to 10px.
fn assistant_row(
    item: &Item,
    assistant: &AssistantItem,
    data: &TranscriptData,
    cx: &App,
    palette: &Palette,
) -> AnyElement {
    let id = item.id.clone();
    let mut row = div()
        .id(row_id("transcript-assistant", &id))
        .group(COPY_GROUP)
        .relative()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(BLOCK_GAP);

    if data.show_thinking && !assistant.thinking.is_empty() {
        row = row.child(
            div()
                .id(row_id("transcript-thinking", &id))
                .flex()
                .flex_col()
                .pl_2()
                .border_l_2()
                .border_color(palette.border)
                .text_color(palette.muted_foreground)
                .child(
                    // `.thinking-label`: 12px, on the muted ink, with the design's
                    // 1.5 line box under it.
                    div()
                        .text_size(palette.scaled(THINKING_LABEL_SIZE))
                        .line_height(palette.scaled(THINKING_LABEL_LINE))
                        .child("thinking"),
                )
                .child(
                    // `.thinking-text`: 14px, italic, the muted ink.
                    div()
                        .id(row_id("transcript-thinking-text", &id))
                        .test_support()
                        .w_full()
                        .min_w_0()
                        .text_size(palette.scaled(THINKING_SIZE))
                        .line_height(palette.scaled(THINKING_LINE))
                        .italic()
                        .child(SelectableText::new(
                            row_id("transcript-thinking-run", &id),
                            assistant.thinking.clone(),
                        )),
                )
                .test_support(),
        );
    }

    if assistant.is_streaming() && assistant.text.trim().is_empty() {
        row = row.child(waiting_line(&id, assistant, data.show_thinking, palette));
    } else {
        row = match data.documents.get(&id) {
            Some(document) => {
                let feedback = data.copy_feedback.clone();
                let message = assistant.text.clone();
                let code_palette = palette.clone();
                let message_id = id.clone();
                row.child(
                    div()
                        .id(row_id("transcript-message", &id))
                        .test_support()
                        .w_full()
                        .min_w_0()
                        .child(
                            TextView::new(document)
                                .style(text_style(cx))
                                .motion(stream_motion())
                                .on_link_click(link::on_click())
                                .markdown_extensions(markdown::extensions())
                                .code_block_actions(move |code_block, _, _| {
                                    let block = code_block.span.map(|span| span.start).unwrap_or(0);
                                    copy_button(
                                        (
                                            ElementId::from("transcript-copy-block"),
                                            message_id.clone(),
                                        ),
                                        CopyTarget::Block(copy_offset(&message_id), block),
                                        code_block.code(),
                                        &feedback,
                                        true,
                                        &code_palette,
                                    )
                                    .test_support()
                                }),
                        ),
                )
                .child(
                    copy_button(
                        row_id("transcript-copy-message", &id),
                        CopyTarget::Message(copy_offset(&id)),
                        message.into(),
                        &data.copy_feedback,
                        false,
                        palette,
                    )
                    .absolute()
                    .top_0()
                    .right_0()
                    .test_support(),
                )
            }
            None => row.child(
                div()
                    .text_color(palette.muted_foreground)
                    .child(assistant.text.clone()),
            ),
        };
    }

    if let Some(error) = assistant.error.as_deref().filter(|error| !error.is_empty()) {
        row = row.child(
            div()
                .text_sm()
                .text_color(palette.destructive)
                .child(format!("error: {error}")),
        );
    }

    row.test_support().into_any_element()
}

/// A stable number for an item's element ids, which are taken as `usize`s by the copy
/// buttons' feedback map.
fn copy_offset(id: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish() as usize
}

/// The pips that hold an assistant row's place between the message starting and its first
/// delta.
fn waiting_dots(id: &ItemId, palette: &Palette) -> AnyElement {
    pips(
        row_id("transcript-waiting", id),
        row_id("transcript-waiting-pulse", id),
        palette,
    )
}

/// How much of the model's thinking the ticker holds. It is one line, so only the
/// tail of it can ever be seen: the rest is not worth laying out every frame. The
/// TUI cuts its tail to the columns its terminal gives it (`tui.lisp`); this is the
/// same idea with the app's own bound.
pub(crate) const THINKING_TAIL_CHARS: usize = 400;
/// The ticker's own size: a size under the message's, so a line running past stays
/// quiet beside the pips.
const TICKER_SIZE: f32 = 13.;
/// The line box the ticker's one line sits on.
const TICKER_LINE: f32 = 18.;
/// How far the fade over the ticker's left edge reaches — where the words that
/// have run past are cut.
const TICKER_FADE: Pixels = px(24.);

/// The line an assistant row holds its place with between the message starting and
/// its first delta: the pips, and — while the thinking is hidden and there is
/// thinking to show — the model's own last words running past beside them.
///
/// It reads as the TUI's activity line (`tui.lisp`'s `thinking · <tail>`): the
/// newest text is at the right end and the older text slides off the left as the
/// deltas come in, the cut end fading into the page rather than stopping flat.
/// With the thinking shown, or nothing thought yet, the line is the pips alone.
fn waiting_line(
    id: &ItemId,
    assistant: &AssistantItem,
    show_thinking: bool,
    palette: &Palette,
) -> AnyElement {
    let pips = waiting_dots(id, palette);
    let tail = if show_thinking {
        None
    } else {
        thinking_tail(&assistant.thinking)
    };
    let Some(tail) = tail else {
        return pips;
    };

    let background = palette.background;
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .child(pips)
        .child(
            div()
                .id(row_id("transcript-thinking-ticker", id))
                .relative()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .aria_label(tail.clone())
                .test_support()
                .child(
                    // `justify_end` on a line wider than its box puts the newest
                    // words at the right edge and pushes the rest out of the left
                    // one, which is where the box clips it.
                    h_flex().w_full().min_w_0().justify_end().child(
                        div()
                            .id(row_id("transcript-thinking-ticker-text", id))
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_size(palette.scaled(TICKER_SIZE))
                            .line_height(palette.scaled(TICKER_LINE))
                            .italic()
                            .text_color(palette.muted_foreground)
                            .test_support()
                            .child(tail),
                    ),
                )
                .child(
                    // The page's own colour over the cut end, thinning to nothing
                    // to the right: the words that have run past fade out instead
                    // of being chopped off.
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(TICKER_FADE)
                        .bg(linear_gradient(
                            90.,
                            linear_color_stop(background, 0.),
                            linear_color_stop(background.alpha(0.), 1.),
                        )),
                ),
        )
        .into_any_element()
}

/// The last of what the model is saying to itself, as one line: every newline, tab
/// and run of spaces folds into a single space, and only a bounded tail is kept, so
/// the newest words are at the right end and the older ones slide off the left as
/// the deltas arrive. `None` when there is nothing but whitespace to show.
pub(crate) fn thinking_tail(thinking: &str) -> Option<SharedString> {
    let mut line = String::new();
    for ch in thinking.chars() {
        if ch.is_whitespace() {
            if !line.is_empty() && !line.ends_with(' ') {
                line.push(' ');
            }
        } else {
            line.push(ch);
        }
    }
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    // The byte the tail starts at: the character `THINKING_TAIL_CHARS` from the
    // end, so a cut never lands inside one.
    let start = line
        .char_indices()
        .rev()
        .nth(THINKING_TAIL_CHARS - 1)
        .map(|(index, _)| index)
        .unwrap_or(0);
    Some(SharedString::from(line[start..].to_owned()))
}

/// Whether the agent's current turn already shows that it is working: an assistant
/// message still streaming (its own pips, its thinking or its words) or a tool call
/// still running (the card's pips). The turn is everything after the last user input
/// evo took — of the rows given, so the caller decides how far back that looks.
fn turn_shows_work(items: &[Item]) -> bool {
    items
        .iter()
        .rev()
        .take_while(|item| !opens_a_turn(&item.kind))
        .any(|item| match &item.kind {
            ItemKind::Assistant(assistant) => assistant.is_streaming(),
            ItemKind::Tool(tool) => tool.status.is_running(),
            _ => false,
        })
}

/// Whether the foot of the record is the pips: the agent is running and no row of its
/// last turn says so itself.
pub(crate) fn shows_work(data: &TranscriptData) -> bool {
    data.running && !turn_shows_work(&data.items)
}

/// The pips at the foot of the list while the agent is running and nothing in its turn
/// says so yet: the request is in flight, the model is reasoning without streaming it,
/// or the next request after a tool is on its way. `None` once a row of the turn shows
/// the work itself, or while the agent is not running.
///
/// They mark the foot of the record, and the record is what the list draws: the list's
/// own foot is the record's, since every item is in it.
pub(crate) fn pending_row(data: &TranscriptData, cx: &App) -> Option<AnyElement> {
    if !shows_work(data) {
        return None;
    }
    let palette = Palette::from_app(cx);
    let gap = data
        .items
        .last()
        .map(|previous| {
            Group::of(&previous.kind)
                .margin()
                .max(Group::Assistant.margin())
        })
        .unwrap_or(px(0.));
    Some(
        div()
            .id("transcript-pending-row")
            .w_full()
            .pt(gap)
            .child(pips(
                ElementId::from("transcript-pending"),
                ElementId::from("transcript-pending-pulse"),
                &palette,
            ))
            .test_support()
            .into_any_element(),
    )
}

/// Three pips swelling in turn: the transcript's "working" mark.
fn pips(id: ElementId, pulse: ElementId, palette: &Palette) -> AnyElement {
    let ink = palette.muted_foreground;
    h_flex()
        .id(id)
        .items_center()
        .gap_1()
        .py(px(8.))
        .test_support()
        .with_animation(
            pulse,
            Animation::new(DOT_CYCLE).repeat(),
            move |pips, delta| {
                pips.children(DOT_PHASES.map(|phase| {
                    div()
                        .size(DOT_SIZE)
                        .rounded_full()
                        .bg(ink.alpha(dot_ink(delta, phase)))
                }))
            },
        )
        .into_any_element()
}

/// How lit a pip is at `delta` through the cycle.
pub(crate) fn dot_ink(delta: f32, phase: f32) -> f32 {
    let turn = (delta + phase).rem_euclid(1.);
    let swell = 1. - ((turn - 0.5).abs() * 2.);
    DOT_INK_FLOOR + (1. - DOT_INK_FLOOR) * swell
}

/// A tool call: one quiet card, as `Rows.css` draws it.
///
/// The head reads as a sentence — the tool, what it was called on, and a small
/// status — and opens onto its arguments as a key/value grid and its result. A
/// result the server truncated says so, and offers the whole of it
/// (`GET /items/<id>`) in place rather than pretending the tail is not there.
///
/// What the sentence says comes from the call's own arguments: [`tool_sentence`]
/// picks the one the tool was aimed at and the one that describes the work. A
/// call with nothing to say in either stays a name and a status — no invented
/// subject.
fn tool_row(
    item: &Item,
    tool: &ToolItem,
    data: &TranscriptData,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let id = item.id.clone();
    let expanded = data.expanded.contains(&id);
    let (status, pill) = match tool.status {
        session::ToolStatus::Running => ("running", palette.warning),
        session::ToolStatus::Error => ("error", palette.destructive),
        session::ToolStatus::Blocked => ("blocked", palette.destructive),
        session::ToolStatus::Ok => ("ok", palette.success),
    };
    let (target, summary) = tool_sentence(&tool.args);
    let (target, summary) = (one_line(&target), one_line(&summary));

    let click_view = view.clone();
    let click_id = id.clone();
    let mut head = div()
        .id(row_id("transcript-tool", &id))
        .h(palette.scaled(TC_HEAD))
        .w_full()
        .flex()
        .items_center()
        .gap(px(TC_HEAD_GAP))
        .pl(px(TC_HEAD_PAD))
        .pr(px(10.))
        .text_size(palette.scaled(TC_HEAD_SIZE))
        .cursor_default()
        .hover({
            let sidebar = palette.sidebar;
            move |style| style.bg(mix(palette.foreground, 4., sidebar))
        })
        .on_click(move |_, _, cx| {
            let _ = click_view.update(cx, |view, cx| view.toggle_expanded(&click_id, cx));
        })
        .child(caret(expanded, palette))
        .child(
            div()
                .id(row_id("transcript-tool-name", &id))
                .flex_shrink_0()
                .font_family(palette.mono.clone())
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(palette.scaled(TC_NAME_SIZE))
                .test_support()
                .child(tool.name.clone()),
        );
    if !target.is_empty() {
        head = head
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(palette.muted_foreground)
                    .child("→"),
            )
            .child(
                div()
                    // `.tc-target{flex:0 0 auto;font-weight:500}` — but it gives way
                    // (with an ellipsis) rather than push the status out of the card
                    // when it is longer than the head.
                    .flex_shrink(1.)
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(widgets::text::MEDIUM)
                    .child(target),
            );
    }
    head = head.child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_color(palette.muted_foreground)
            .child(summary),
    );
    head = head.child(
        div()
            .id(row_id("transcript-tool-status", &id))
            .flex_shrink_0()
            .test_support()
            .child(status_pill(
                status,
                pill,
                palette,
                tool.status == session::ToolStatus::Ok,
            )),
    );

    let mut card = div()
        .id(row_id("transcript-tool-row", &id))
        .w_full()
        .min_w_0()
        .rounded(px(10.))
        .border_1()
        .border_color(palette.rule_soft())
        .bg(palette.sidebar)
        .overflow_hidden()
        .child(head.test_support());

    if expanded {
        let full = data.full_results.get(&id);
        let can_fetch = data.on_fetch_item.borrow().is_some();
        card = card.child(
            div()
                .id(row_id("transcript-tool-body", &id))
                .w_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .border_t_1()
                .border_color(palette.rule_soft())
                .bg(palette.input)
                .pt(px(10.))
                .pr(px(12.))
                .pb(px(12.))
                .pl(px(TC_BODY_INDENT))
                .child(arguments_block(id.clone(), &tool.args, palette))
                .child(result_block(
                    id.clone(),
                    tool,
                    full,
                    can_fetch,
                    view,
                    palette,
                )),
        );
    }

    card.test_support().into_any_element()
}

/// A tool head's text as the one line the head has room for: the first line that
/// says something, with `…` after it when more lines follow. A width the line
/// overruns is the head's own ellipsis.
pub(crate) fn one_line(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

/// The tool head's own numbers: its height (`.tc-head`), the leading padding before
/// the caret and the gap between the head's children, and the column the head's text
/// starts in — that padding, the caret's own column and the gap after it.
/// [`TC_BODY_INDENT`] is the same column, so a call's arguments and its result begin
/// under the tool's name.
const TC_HEAD: f32 = 34.;
const TC_HEAD_PAD: f32 = 8.;
const TC_HEAD_GAP: f32 = 8.;
pub(crate) const TC_BODY_INDENT: f32 = TC_HEAD_PAD + DISCLOSURE_WIDTH + TC_HEAD_GAP;
/// The head's text — the design's `.tc-head{font-size:13px}` — and the tool's own
/// name, which is the one run in the row set in the mono face.
const TC_HEAD_SIZE: f32 = 13.;
const TC_NAME_SIZE: f32 = 12.5;

/// What the call was aimed at, and what it was asked to do — the two halves of the
/// design's sentence, read out of the call's own arguments.
///
/// The first is the argument that says *where* the tool went: a lane, a path, a
/// command, a pattern. The second is the longest other string, which is what a
/// task, a prompt or a note looks like. Neither is invented: a call that names
/// neither is drawn as its name alone.
///
/// A `delegate` to lane 5 with a task reads `→ lane 5` and the task; a `bash` with
/// a command reads `→ make test` and nothing else, because that is all it was.
pub(crate) fn tool_sentence(args: &Value) -> (String, String) {
    const WHERE: [&str; 9] = [
        "lane", "target", "path", "file", "command", "cmd", "url", "pattern", "name",
    ];
    let Some(object) = args.as_object() else {
        return (String::new(), String::new());
    };
    let text = |value: &Value| match value {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    };
    let mut target = String::new();
    for key in WHERE {
        if let Some(value) = object.get(key) {
            let value = text(value);
            if !value.is_empty() {
                // A lane is named as the design names it; everything else is what
                // the argument says.
                target = if key == "lane" {
                    format!("lane {value}")
                } else {
                    value
                };
                break;
            }
        }
    }
    // The work, rather than the place: the longest string argument that is not the
    // target itself.
    let mut summary = String::new();
    for (key, value) in object {
        if WHERE.contains(&key.as_str()) {
            continue;
        }
        let value = text(value);
        if value.len() > summary.len() {
            summary = value;
        }
    }
    // Counted in characters: a byte cut can land inside a multi-byte one (an
    // argument that already carries `…` did, and took the app down).
    if summary.chars().count() > TC_SUMMARY_LIMIT {
        let head: String = summary.chars().take(TC_SUMMARY_LIMIT).collect();
        summary = format!("{head}…");
    }
    (target, summary)
}

/// How much of a call's summary the head shows before the ellipsis. The design
/// lets CSS cut it to the row; this keeps a head from measuring a novel.
const TC_SUMMARY_LIMIT: usize = 120;

/// The status pill a card's head carries: the status colour most of the way to
/// the ink, on a ground of the same colour 12% over the surface, at a fixed 20px
/// — the design's rule for its green, applied to whatever state the card is in.
///
/// The design's pill carries a small tick; a state that is not a success has no
/// tick to show, so one is drawn for the states that are done.
fn status_pill(status: &str, colour: Hsla, palette: &Palette, tick: bool) -> AnyElement {
    let mut pill = div()
        .flex_shrink_0()
        .h(palette.scaled(PILL_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .pl(px(5.))
        .pr(px(7.))
        .rounded_full()
        .text_size(palette.scaled(PILL_SIZE))
        .text_color(mix(colour, 85., palette.foreground))
        .bg(mix(colour, 12., palette.sidebar));
    if tick {
        // `<Tick/>` in the design: `m5 12 5 5L20 7` of a 24-unit box, 3 units of
        // stroke, round caps — the same painter as the strip's glyphs.
        pill = pill.child(glyph::stroked(
            TICK_GLYPH,
            TICK_STROKE,
            &tick_lines(),
            pill_ink(colour, palette),
        ));
    }
    pill.child(status.to_string()).into_any_element()
}

/// The tick's own size, from the design's 11px glyph at a 3-in-24 stroke.
const TICK_GLYPH: f32 = 11.;
const TICK_STROKE: f32 = 1.4;

/// The pill's own box and type: the design's fixed 20px height at 11.5px.
const PILL_HEIGHT: f32 = 20.;
const PILL_SIZE: f32 = 11.5;

/// The tick `m5 12 5 5L20 7`, scaled to [`TICK_GLYPH`].
fn tick_lines() -> Vec<(Point<Pixels>, Point<Pixels>)> {
    let size = TICK_GLYPH;
    let at = |x: f32, y: f32| point(px(x / 24. * size), px(y / 24. * size));
    vec![(at(5., 12.), at(10., 17.)), (at(10., 17.), at(20., 7.))]
}

/// The pill's ink: the status colour most of the way to the foreground.
fn pill_ink(colour: Hsla, palette: &Palette) -> Hsla {
    mix(colour, 85., palette.foreground)
}

/// The disclosure of a tool row: the design's chevron, turning a quarter in
/// 120ms as the card opens (`transition: transform .12s ease`).
///
/// gpui has no `transform` on an element — no rotation, no scale — so the chevron
/// is *painted* rather than transformed: its three points are turned about the
/// caret's own centre on every frame of the animation, and stroked. `ease` is
/// `cubic-bezier(.25,.1,.25,1)`, the curve the design names.
fn caret(expanded: bool, palette: &Palette) -> AnyElement {
    let colour = palette.muted_foreground;
    // The two ends are the two states, so a fresh animation on a toggle runs from
    // the angle the caret is at to the angle it is going to — and the ids make the
    // toggle a fresh animation.
    let (from, to) = if expanded { (0., 1.) } else { (1., 0.) };
    let easing = widgets::effort::cubic_bezier(0.25, 0.1, 0.25, 1.0);

    div()
        .w(px(DISCLOSURE_WIDTH))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id(ElementId::from((
                    "transcript-tool-caret",
                    expanded as usize,
                )))
                .flex_none()
                .size(px(CARET_GLYPH))
                .with_animation(
                    ElementId::from(("transcript-tool-caret-motion", expanded as usize)),
                    Animation::new(CARET_TURN).with_easing(easing),
                    move |el, delta| {
                        let angle = (from + (to - from) * delta) * std::f32::consts::FRAC_PI_2;
                        el.child(glyph::stroked(
                            CARET_GLYPH,
                            CARET_STROKE,
                            &chevron_lines(angle),
                            colour,
                        ))
                    },
                ),
        )
        .into_any_element()
}

/// The chevron `m9 6 6 6-6 6` of the design's 24-unit box, at `size`, turned by
/// `angle` about its own centre — two strokes through its three points.
fn chevron_lines(angle: f32) -> Vec<(Point<Pixels>, Point<Pixels>)> {
    let size = CARET_GLYPH;
    let (sin, cos) = angle.sin_cos();
    let turn = |x: f32, y: f32| {
        let (x, y) = (x * size, y * size);
        let (dx, dy) = (x - size / 2., y - size / 2.);
        point(
            px(size / 2. + dx * cos - dy * sin),
            px(size / 2. + dx * sin + dy * cos),
        )
    };
    // 9,6 → 15,12 → 9,18 of 24.
    let (a, b, c) = (turn(0.375, 0.25), turn(0.625, 0.5), turn(0.375, 0.75));
    vec![(a, b), (b, c)]
}

/// The caret's own numbers: the design's 12px glyph with a 2.2px stroke drawn at
/// half scale, turning in 120ms.
const CARET_GLYPH: f32 = 12.;
const CARET_STROKE: f32 = 1.1;
const CARET_TURN: Duration = Duration::from_millis(120);

/// The arguments of an open tool row: the call's own object as a key/value list, or the
/// text exactly as it came when it is not one.
fn arguments_block(id: ItemId, args: &Value, palette: &Palette) -> AnyElement {
    match args {
        Value::Null => div().into_any_element(),
        Value::Object(object) if object.is_empty() => div().into_any_element(),
        value => {
            let text = value.to_string();
            match json_fields(&text) {
                Some(fields) if !fields.is_empty() => fields_block(
                    row_id("transcript-tool-arguments", &id),
                    "arguments",
                    &fields,
                    ARGUMENTS_LIMIT,
                    palette,
                ),
                _ => text_block(
                    row_id("transcript-tool-arguments", &id),
                    "arguments",
                    &text,
                    None,
                    ARGUMENTS_LIMIT,
                    palette,
                ),
            }
        }
    }
}

/// The result of an open tool row, with the way to the whole of it when the server cut it.
fn result_block(
    id: ItemId,
    tool: &ToolItem,
    full: Option<&String>,
    can_fetch: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let Some(result) = tool.result.as_ref() else {
        return div().into_any_element();
    };
    if result.text.is_empty() {
        return div().into_any_element();
    }
    let label = if tool.status.is_error() {
        "error"
    } else {
        "result"
    };

    // The wire copy of a result is shortened at 4 KiB (CONTRACT §4.1); the row shows
    // less still. Either way the reader is offered the whole of it rather than the part
    // that fits.
    let needs_full = result.truncated || result.chars as usize > RESULT_LIMIT;
    let mut block = div()
        .id(row_id("transcript-tool-result-box", &id))
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0();
    match full {
        Some(full) => {
            block = block.child(text_block(
                row_id("transcript-tool-result", &id),
                label,
                full,
                None,
                usize::MAX,
                palette,
            ));
        }
        None => {
            block = block.child(match json_fields(&result.text) {
                Some(fields) if !fields.is_empty() => fields_block(
                    row_id("transcript-tool-result", &id),
                    label,
                    &fields,
                    RESULT_LIMIT,
                    palette,
                ),
                _ => text_block(
                    row_id("transcript-tool-result", &id),
                    label,
                    &result.text,
                    Some(result.chars),
                    RESULT_LIMIT,
                    palette,
                ),
            });
            // The offer only exists when there is a way to fetch: a tab whose owner
            // cannot read `/items/<id>` says what the panel left out instead of
            // offering a button that would do nothing.
            if needs_full && can_fetch {
                block = block.child(load_more_row(id, view, palette));
            } else if needs_full {
                block = block.child(cap_note(
                    row_id("transcript-tool-result-note", &id),
                    (result.chars as usize).saturating_sub(result.text.chars().count()),
                    palette,
                ));
            }
        }
    }
    block.into_any_element()
}

/// What an opened row whose output the server shortened offers: the whole of it, one click
/// away (`GET /items/<id>`).
fn load_more_row(id: ItemId, view: &WeakEntity<TranscriptView>, palette: &Palette) -> AnyElement {
    let view = view.clone();
    div()
        .id(row_id("transcript-load-more", &id))
        .px(px(6.))
        .py(px(1.))
        .rounded(palette.radius)
        .border_1()
        .border_color(palette.border)
        .text_size(palette.scaled(CAPTION_SIZE))
        .line_height(palette.scaled(COPY_LINE))
        .text_color(palette.muted_foreground)
        .cursor_pointer()
        .hover(|style| style.text_color(palette.foreground))
        .on_click(move |_, window, cx| {
            let id = id.clone();
            let _ = view.update(cx, |view, cx| view.load_full_item(&id, window, cx));
        })
        .child("Load the whole output")
        .test_support()
        .into_any_element()
}

/// One quiet line of the transcript, in a colour the caller chose.
fn quiet_line(
    id: ItemId,
    name: &'static str,
    text: &str,
    color: gpui_kit::Hsla,
    palette: &Palette,
) -> AnyElement {
    quiet_source_line(id, name, None, color, text, color, palette)
}

/// The same, with who said it in front: the source is chrome and stays in the
/// muted ink, while what it says takes the line's own colour — a system line that
/// shouts its source in the accent reads as a link, which is not what it is.
///
/// Its own type: the design's 13px on its 1.5 line box (`Rows.css`'s
/// `.notice{font-size:13px}`), at the reader's zoom.
const QUIET_SIZE: f32 = 13.;
const QUIET_LINE: f32 = 18.;

fn quiet_source_line(
    id: ItemId,
    name: &'static str,
    source: Option<&str>,
    source_ink: gpui_kit::Hsla,
    text: &str,
    color: gpui_kit::Hsla,
    palette: &Palette,
) -> AnyElement {
    let full: SharedString = match source {
        Some(source) => format!("{source} · {text}").into(),
        None => text.to_string().into(),
    };
    let tooltip = full.clone();
    let mut line = div()
        .id(row_id(name, &id))
        .w_full()
        .min_w_0()
        .truncate()
        .text_size(palette.scaled(QUIET_SIZE))
        .line_height(palette.scaled(QUIET_LINE))
        .text_color(color)
        .aria_label(full.clone());
    if let Some(source) = source {
        line = line
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .text_color(source_ink)
                    .child(format!("{source} ·")),
            )
            .child(div().min_w_0().truncate().child(text.to_string()));
    } else {
        line = line.child(full.clone());
    }
    line.tooltip(move |window, cx| {
        widgets::tooltip::text(
            "notice-tooltip",
            tooltip.clone(),
            NOTICE_TOOLTIP_WIDTH,
            window,
            cx,
        )
    })
    .test_support()
    .into_any_element()
}

/// A lane report's own fields, in the order the card draws them: the label
/// `Rows.css`'s `.rp-label` shows and the text under it.
pub(crate) fn report_fields(report: &LaneReport) -> [(&'static str, &str); 6] {
    [
        ("done", report.done.as_str()),
        ("evidence", report.evidence.as_str()),
        ("next", report.next.as_str()),
        ("blocked", report.blocked.as_str()),
        ("requests", report.requests.as_str()),
        ("goal", report.goal.as_deref().unwrap_or_default()),
    ]
}

/// What a report's field is drawn in: the design's `.rp-row` is one voice, and the
/// two fields that need an eye — a block, an ask — take the two colours that mean
/// something.
fn report_field_ink(label: &str, palette: &Palette) -> Hsla {
    match label {
        "blocked" => palette.destructive,
        "requests" => palette.primary,
        "evidence" | "goal" => palette.muted_foreground,
        _ => palette.foreground,
    }
}

/// The markdown style of a report's row: the message style with the design's own
/// `.rp-row p{margin:0}` — a paragraph inside a row adds no air of its own.
fn report_text_style(cx: &App) -> gpui_kit::base::TextViewStyle {
    text_style(cx).with_paragraph_gap(gpui_kit::rems(0.))
}

/// One lane's report: what it did, in the fields the item carries — never re-parsed out of
/// the prose the swarm used to send.
///
/// A field's body is markdown, as the design draws it (`Rows.css`'s `.rp-row`, whose
/// rules are `p{margin:0}`, `ul{margin:0;padding-left:18px}`, `code{font-size:12px}`):
/// an evidence line is a list, a path is a code span.
fn report_row(
    id: ItemId,
    report: &LaneReport,
    data: &TranscriptData,
    cx: &App,
    palette: &Palette,
) -> AnyElement {
    // `Rows.css`'s `.rp`: a labelled card — a head with who sent it and what state
    // it is in, then one row per section, so the answer is scannable rather than a
    // wall.
    let mut row = div()
        .id(row_id("transcript-report", &id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(palette.rule_soft())
        .bg(palette.input)
        .overflow_hidden()
        .child(
            div()
                .id(row_id("transcript-report-heading", &id))
                .h(palette.scaled(REPORT_HEAD))
                .flex()
                .items_center()
                .gap_2()
                .px(px(12.))
                .border_b_1()
                .border_color(palette.rule_soft())
                .bg(palette.sidebar)
                .text_size(palette.scaled(REPORT_SIZE))
                .aria_label(format!("Lane {} report", report.lane))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("lane {}", report.lane)),
                )
                .child(div().text_color(palette.muted_foreground).child("report"))
                .child(
                    div()
                        .ml_auto()
                        .child(status_pill("done", palette.success, palette, true)),
                )
                .test_support(),
        );

    for (label, value) in report_fields(report) {
        if value.is_empty() {
            continue;
        }
        let line_id = row_id(format!("transcript-report-{label}"), &id);
        // `.rp-row`'s body is markdown — `Rows.css` gives it `p{margin:0}`,
        // `ul{margin:0;padding-left:18px}`, `code{font-size:12px}`. The document is
        // brought up to date by `render_row`, before the row is drawn.
        let body = match data.field_documents.get(&(id.clone(), label)) {
            Some(document) => TextView::new(document)
                .style(report_text_style(cx))
                .selectable(true)
                .markdown_extensions(markdown::extensions())
                .into_any_element(),
            None => div().child(value.to_string()).into_any_element(),
        };
        row = row.child(
            div()
                .id(line_id.clone())
                .test_support()
                .flex()
                .items_start()
                .gap_3()
                .px(px(12.))
                .py(px(9.))
                .border_t_1()
                .border_color(palette.rule_soft())
                .text_size(palette.scaled(REPORT_VALUE_SIZE))
                .line_height(palette.scaled(REPORT_VALUE_LINE))
                .child(
                    div()
                        .id((line_id.clone(), "label"))
                        .test_support()
                        .w(REPORT_LABEL_WIDTH)
                        .flex_shrink_0()
                        .text_size(palette.scaled(REPORT_LABEL_SIZE))
                        .line_height(palette.scaled(REPORT_VALUE_LINE))
                        .text_color(palette.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .id((line_id, "value"))
                        .test_support()
                        .min_w_0()
                        .flex_1()
                        .text_color(report_field_ink(label, palette))
                        .child(body),
                ),
        );
    }

    row.test_support().into_any_element()
}

/// One field — or one container — of a tool call's arguments, or of a JSON
/// result.
#[derive(Debug, PartialEq)]
pub(crate) struct Field {
    /// The key the reader sees: `timeout`, `env.RUST_LOG` for a container the
    /// top-level object holds directly, or the bare `env` when its own fields
    /// are drawn under it.
    pub(crate) key: String,
    pub(crate) value: FieldValue,
}

/// How a field's value is drawn.
#[derive(Debug, PartialEq)]
pub(crate) enum FieldValue {
    /// One line of text. `full` is the whole string when the line had to be
    /// elided to fit.
    Text { text: String, full: Option<String> },
    /// A string with line breaks in it: a small mono block, capped.
    Block(String),
    /// A container drawn as its own fields, indented under this one.
    Nested(Vec<Field>),
    /// A container the panel will not draw out — deeper than [`MAX_DEPTH`], or
    /// an array longer than [`MAX_ARRAY`]: what it holds, in a muted summary,
    /// with the whole of it on hover.
    Collapsed { summary: String, full: String },
}

/// `text` as the fields of a JSON object — or of an array, keyed by index — in
/// the order the call wrote them, or `None` when it is neither, so a caller can
/// show the text as it came instead of guessing.
pub(crate) fn json_fields(text: &str) -> Option<Vec<Field>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let mut fields = Vec::new();

    match value {
        Value::Object(object) => {
            for (key, value) in object {
                push_field(&mut fields, key, &value, 0, true);
            }
        }
        Value::Array(items) => {
            if items.len() > MAX_ARRAY {
                fields.push(collapsed(String::new(), &Value::Array(items)));
                return Some(fields);
            }
            for (index, value) in items.iter().enumerate() {
                push_field(&mut fields, index.to_string(), value, 0, true);
            }
        }
        _ => return None,
    }
    Some(fields)
}

/// Add what `value` draws as.
///
/// A container the top-level object holds directly flattens into one field per
/// leaf (`env.RUST_LOG`, `args.0`): one dense row per leaf reads better than a
/// row for the container and its children under it. What is nested inside that
/// keeps its own keys and is drawn as rows indented under it, up to
/// [`MAX_DEPTH`]; past that — or for an array longer than [`MAX_ARRAY`] — the
/// container is summarised, so a structure never arrives as a wall of braces.
fn push_field(fields: &mut Vec<Field>, key: String, value: &Value, depth: usize, flatten: bool) {
    if flatten {
        match value {
            Value::Object(object) => {
                for (sub, value) in object {
                    push_field(fields, format!("{key}.{sub}"), value, depth, false);
                }
                return;
            }
            Value::Array(items) if items.len() <= MAX_ARRAY => {
                for (index, value) in items.iter().enumerate() {
                    push_field(fields, format!("{key}.{index}"), value, depth, false);
                }
                return;
            }
            _ => {}
        }
    }

    match value {
        Value::Object(object) if object.is_empty() => fields.push(Field {
            key,
            value: FieldValue::Text {
                text: "{}".to_string(),
                full: None,
            },
        }),
        Value::Array(items) if items.is_empty() => fields.push(Field {
            key,
            value: FieldValue::Text {
                text: "[]".to_string(),
                full: None,
            },
        }),
        Value::Object(object) if depth < MAX_DEPTH => {
            let mut children = Vec::new();
            for (sub, value) in object {
                push_field(&mut children, sub.clone(), value, depth + 1, false);
            }
            fields.push(Field {
                key,
                value: FieldValue::Nested(children),
            });
        }
        Value::Array(items) if depth < MAX_DEPTH && items.len() <= MAX_ARRAY => {
            let mut children = Vec::new();
            for (index, value) in items.iter().enumerate() {
                push_field(&mut children, index.to_string(), value, depth + 1, false);
            }
            fields.push(Field {
                key,
                value: FieldValue::Nested(children),
            });
        }
        Value::Object(_) | Value::Array(_) => fields.push(collapsed(key, value)),
        // A string with line breaks in it is not a line of a list: it gets a
        // block of its own, so file contents and multi-line commands stay
        // readable. A command is a block whether or not it breaks lines: it is
        // what the call does, and the panel's budget is what caps it, not the
        // one-line elision.
        Value::String(text) if text.contains('\n') || key == COMMAND_KEY => fields.push(Field {
            key,
            value: FieldValue::Block(text.clone()),
        }),
        Value::String(text) => {
            let (text, full) = elide(text);
            fields.push(Field {
                key,
                value: FieldValue::Text { text, full },
            });
        }
        other => {
            let (text, full) = elide(&other.to_string());
            fields.push(Field {
                key,
                value: FieldValue::Text { text, full },
            });
        }
    }
}

/// A container the panel will not draw out: how much it holds, and the whole of
/// it as one line for the tooltip.
fn collapsed(key: String, value: &Value) -> Field {
    let summary = match value {
        Value::Object(object) if object.len() == 1 => "{…1 key}".to_string(),
        Value::Object(object) => format!("{{…{} keys}}", object.len()),
        Value::Array(items) if items.len() == 1 => "[…1 item]".to_string(),
        Value::Array(items) => format!("[…{} items]", items.len()),
        // Only a container is ever collapsed.
        _ => String::new(),
    };
    Field {
        key,
        value: FieldValue::Collapsed {
            summary,
            full: value.to_string(),
        },
    }
}

/// A one-line string, cut at [`VALUE_LIMIT`] with an ellipsis. The whole string
/// comes back too, for the tooltip of a value that had to be cut.
fn elide(text: &str) -> (String, Option<String>) {
    if text.chars().count() <= VALUE_LIMIT {
        return (text.to_string(), None);
    }
    let mut shown: String = text.chars().take(VALUE_LIMIT).collect();
    shown.push('…');
    (shown, Some(text.to_string()))
}

/// A panel's budget as it is fitted to its limit: how many characters it may
/// still show, and how many the limit has made it cut.
pub(crate) struct Cap {
    left: usize,
    hidden: usize,
}

impl Cap {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            left: limit,
            hidden: 0,
        }
    }

    /// Takes the first `chars` of a value the panel would otherwise draw whole:
    /// as many as the budget has left, and the rest — the part of it the limit
    /// cuts — is what the note counts.
    fn take(&mut self, chars: usize) -> usize {
        let shown = chars.min(self.left);
        self.left -= shown;
        self.hidden += chars - shown;
        shown
    }

    /// What the panel did not show, for the note under it.
    pub(crate) fn hidden(&self) -> usize {
        self.hidden
    }
}

/// The first `chars` characters of `text`. A cut lands between characters, so a
/// payload written in emoji or CJK is shortened, never torn.
pub(crate) fn take_chars(text: &str, chars: usize) -> String {
    if chars >= text.chars().count() {
        return text.to_string();
    }
    text.chars().take(chars).collect()
}

/// The fields of a panel, fitted to the budget: values the budget does not reach
/// are dropped and values it can only partly reach are cut. The limit is counted
/// over the text the panel draws — a value's own characters, not the key it is
/// labeled with, and not the text a value keeps to itself (the ellipsis of an
/// elided line, the count of a collapsed container) — because what the note
/// under the panel counts is what the limit cut, not what the panel never draws.
pub(crate) fn cap_fields(fields: &[Field], cap: &mut Cap) -> Vec<Field> {
    let mut capped = Vec::new();
    for field in fields {
        if let Some(value) = cap_value(&field.value, cap) {
            capped.push(Field {
                key: field.key.clone(),
                value,
            });
        }
    }
    capped
}

/// One value as the budget can show it, or `None` when it can show none of it.
fn cap_value(value: &FieldValue, cap: &mut Cap) -> Option<FieldValue> {
    if cap.left == 0 {
        // Nothing left to draw this with: the whole of it is left out.
        cap.hidden += drawn_chars_of(value);
        return None;
    }

    match value {
        FieldValue::Nested(children) => {
            // A container whose own fields the limit could not reach is no row
            // at all: a key with nothing under it is worse than no key.
            let fitted = cap_fields(children, cap);
            (!fitted.is_empty()).then_some(FieldValue::Nested(fitted))
        }
        FieldValue::Text { text, full } => {
            let shown = cap.take(text.chars().count());
            (shown > 0).then(|| FieldValue::Text {
                text: take_chars(text, shown),
                full: full.clone(),
            })
        }
        FieldValue::Block(text) => {
            let shown = cap.take(text.chars().count());
            (shown > 0).then(|| FieldValue::Block(take_chars(text, shown)))
        }
        FieldValue::Collapsed { summary, full } => {
            let shown = cap.take(summary.chars().count());
            (shown > 0).then(|| FieldValue::Collapsed {
                summary: take_chars(summary, shown),
                full: full.clone(),
            })
        }
    }
}

/// Characters of text a value draws: what the panel shows of it when the limit
/// does not cut it first.
fn drawn_chars_of(value: &FieldValue) -> usize {
    let count = |text: &str| text.chars().count();
    match value {
        FieldValue::Text { text, .. } => count(text),
        FieldValue::Block(text) => count(text),
        FieldValue::Collapsed { summary, .. } => count(summary),
        FieldValue::Nested(children) => children
            .iter()
            .map(|field| drawn_chars_of(&field.value))
            .sum(),
    }
}

/// What the note under a capped panel says: how many characters the panel's
/// limit leaves out.
pub(crate) fn cap_note_text(hidden: usize) -> String {
    if hidden == 1 {
        "… (1 more char)".to_string()
    } else {
        format!("… ({hidden} more chars)")
    }
}

/// The muted line under a panel the limit cut: how many characters of the call
/// are not on screen. The full text stays in the row — this says where the view
/// stops, not where the call did.
fn cap_note(id: impl Into<ElementId>, hidden: usize, palette: &Palette) -> AnyElement {
    let note = cap_note_text(hidden);
    let id = id.into();
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .text_size(palette.scaled(CAPTION_SIZE))
        .text_color(palette.muted_foreground)
        .aria_label(note.clone())
        .child(note)
        .test_support()
        .into_any_element()
}

/// Whether a value should be drawn in the mono face: a path, a command, an
/// identifier. `cargo test -p transcript` reads as code; "the provider returned
/// 429" does not.
pub(crate) fn looks_like_code(text: &str) -> bool {
    text.contains('/') || text.contains("::") || text.contains('(') || text.contains('=')
}

/// The caption of a payload panel: what the block below it holds, small and
/// muted — quiet enough not to read as a line of the transcript. (GPUI has no
/// letter spacing, so a caption is set small rather than tracked; it is not
/// upper-cased, which shouts.)
///
/// `Rows.css`'s `.tc-caption`: 11.5px on a 1.5 line box, the muted ink, and 4px
/// below it.
fn caption(id: &ElementId, label: &str, palette: &Palette) -> AnyElement {
    div()
        .id((id.clone(), "caption"))
        .mb(px(4.))
        .text_size(palette.scaled(CAPTION_SIZE))
        // `.tc-caption` inherits the page's `line-height: 1.5`: 17.25px under an
        // 11.5px caption.
        .line_height(palette.scaled(17.25))
        .text_color(palette.muted_foreground)
        .aria_label(label.to_string())
        .child(label.to_string())
        .test_support()
        .into_any_element()
}

/// A JSON object as a compact list: one `key  value` row per field, the key
/// muted and the value plain — never the braces and quotes it arrived in.
///
/// A field whose value is a container carries the rows of its own fields
/// indented under it, [`NEST_INDENT`] per level, so a payload's shape is read
/// from the indentation rather than from a row of compact JSON.
fn fields_block(
    id: impl Into<ElementId>,
    label: &str,
    fields: &[Field],
    limit: usize,
    palette: &Palette,
) -> AnyElement {
    let id = id.into();
    let mut cap = Cap::new(limit);
    let fields = cap_fields(fields, &mut cap);
    // `Rows.css`'s `.tc-body`: a caption, then the payload as a grid of
    // `72px 1fr` k/v pairs. The card around it is the tool's own.
    //
    // The grid's own gaps: `.tc-kv{gap:3px 12px}` is 3px between one field and
    // the next and 12px between a key and its value (the latter is the row's own
    // [`COLUMN_GAP`]), and the caption's `.tc-caption{margin-bottom:4px}` is the
    // 4px between it and the first field.
    let mut block = div()
        .id(id.clone())
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .text_size(palette.payload_size)
        .line_height(palette.payload_line)
        .child(caption(&id, label, palette));

    let mut rows = div()
        .id((id.clone(), "fields"))
        .flex()
        .flex_col()
        .gap(px(3.))
        .w_full()
        .min_w_0();
    for (index, field) in fields.iter().enumerate() {
        rows = rows.child(field_row(&id, index, field, palette));
    }
    block = block.child(rows);
    if cap.hidden() > 0 {
        block = block.child(cap_note((id.clone(), "note"), cap.hidden(), palette));
    }

    block.test_support().into_any_element()
}

/// One field of a [`fields_block`]: a `key  value` row, or — for a container
/// the panel draws out — that row with the rows of its own fields under it.
fn field_row(base: &ElementId, index: usize, field: &Field, palette: &Palette) -> AnyElement {
    let id: ElementId = (base.clone(), index.to_string()).into();

    // A value that was cut to fit, or a container that was summarised: the
    // whole of it is one hover away.
    let elided = match &field.value {
        FieldValue::Text { full, .. } => full.clone(),
        FieldValue::Collapsed { full, .. } => Some(full.clone()),
        FieldValue::Block(_) | FieldValue::Nested(_) => None,
    };

    let row = match &field.value {
        FieldValue::Nested(children) => div()
            .id(id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(key_cell(&id, &field.key, palette))
            .child(
                // The children keep their own keys, [`NEST_INDENT`] further in
                // per level: the indentation is what says whose fields they are.
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w_full()
                    .min_w_0()
                    .pl(NEST_INDENT)
                    .children(
                        children
                            .iter()
                            .enumerate()
                            .map(|(index, field)| field_row(&id, index, field, palette)),
                    ),
            ),
        value => div()
            .id(id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .gap(COLUMN_GAP)
            .child(key_cell(&id, &field.key, palette))
            .child(value_cell(&id, value, palette)),
    };

    match elided {
        Some(full) => row
            .tooltip(move |window, cx| {
                widgets::tooltip::text("row-tooltip", full.clone(), px(520.), window, cx)
            })
            .test_support()
            .into_any_element(),
        None => row.test_support().into_any_element(),
    }
}

/// The key column of a field: as wide in every row, so the values of one level
/// share a left edge, and a key too long for it is one hover away — a
/// `python_hash_seed` the reader cannot read is a key they cannot look up. The
/// cells are addressable on their own, which is what a test measures an indent
/// with.
fn key_cell(id: &ElementId, key: &str, palette: &Palette) -> AnyElement {
    let full = key.to_string();
    div()
        .id((id.clone(), "key"))
        .w(KEY_WIDTH)
        .flex_shrink_0()
        .truncate()
        // `.tc-kv dt`: a key is payload syntax, so it is set in the mono face at
        // 12px — the values beside it stay in the UI font.
        .font_family(palette.mono.clone())
        .text_size(palette.scaled(KEY_SIZE))
        .text_color(palette.muted_foreground)
        .child(SelectableText::new((id.clone(), "key"), key.to_string()))
        .tooltip(move |window, cx| {
            widgets::tooltip::text("path-tooltip", full.clone(), px(520.), window, cx)
        })
        .test_support()
        .into_any_element()
}

/// What a field's value draws as, beside its key.
fn value_cell(id: &ElementId, value: &FieldValue, palette: &Palette) -> AnyElement {
    match value {
        FieldValue::Text { text, .. } => {
            // Only a value that reads as code is set in mono; prose stays in the
            // UI font, where it is easier to read.
            let cell = div()
                .id((id.clone(), "value"))
                .flex_1()
                .min_w_0()
                .text_color(palette.foreground)
                .child(SelectableText::new((id.clone(), "value"), text.clone()));
            if looks_like_code(text) {
                cell.font_family(palette.mono.clone())
                    .test_support()
                    .into_any_element()
            } else {
                cell.test_support().into_any_element()
            }
        }
        FieldValue::Block(text) => div()
            .id((id.clone(), "value"))
            .flex_1()
            .min_w_0()
            .pl_2()
            .border_l_2()
            .border_color(palette.border)
            .font_family(palette.mono.clone())
            .text_color(palette.foreground)
            // The panel's budget has already cut it: nothing more is left out
            // here.
            .child(SelectableText::new((id.clone(), "value"), text.clone()))
            .test_support()
            .into_any_element(),
        // A container the panel does not draw out: what it holds, quiet enough
        // that it reads as a count rather than as a value.
        FieldValue::Collapsed { summary, .. } => div()
            .id((id.clone(), "value"))
            .flex_1()
            .min_w_0()
            .text_size(palette.scaled(KEY_SIZE))
            .text_color(palette.muted_foreground)
            .child(SelectableText::new((id.clone(), "value"), summary.clone()))
            .test_support()
            .into_any_element(),
        FieldValue::Nested(_) => unreachable!("a container is drawn by field_row itself"),
    }
}

/// A labeled panel of tool text, capped so one result cannot take the whole
/// transcript. The text starts in the value column of a keyed panel, so the
/// panels of one tool call share a left edge.
fn text_block(
    id: impl Into<gpui_kit::ElementId>,
    label: &str,
    text: &str,
    total_chars: Option<u64>,
    limit: usize,
    palette: &Palette,
) -> AnyElement {
    let id = id.into();
    let (body, hidden) = cap_text(text, total_chars, limit);
    // `Rows.css`'s `.tc-result`: the payload as plain text under its caption, on
    // the card's own body — the frame is the card's, not a box of its own.
    let mut block = div()
        .id(id.clone())
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .child(caption(&id, label, palette))
        // A body with no keys of its own starts at the panel's own edge, under
        // its caption: a text result is the whole width of the panel, not a
        // column of it.
        .child(
            div()
                .id((id.clone(), "text"))
                .w_full()
                .min_w_0()
                // `.tc-result`: the UI font at 13/19, the design's own payload
                // size. The design sets mono only for a call's arguments' keys
                // and the tool's own name.
                .text_size(palette.payload_size)
                .line_height(palette.payload_line)
                .text_color(palette.foreground)
                .child(SelectableText::new((id.clone(), "body"), body))
                .test_support(),
        );

    if hidden > 0 {
        block = block.child(cap_note((id.clone(), "note"), hidden, palette));
    }

    block.test_support().into_any_element()
}

/// `text` as a panel body: its first `limit` characters, and how many
/// characters of the whole that leaves out. `total_chars` is what the swarm
/// said the result holds, when the copy that arrived was already shortened —
/// the note counts from there, so a reader is told what the tool really sent.
pub(crate) fn cap_text(text: &str, total_chars: Option<u64>, limit: usize) -> (String, usize) {
    let content = text.chars().count();
    // The note counts from what the tool sent, when the swarm said the copy that
    // arrived was already shortened: the panel is not showing that either.
    let total = total_chars
        .map(|chars| chars as usize)
        .unwrap_or(content)
        .max(content);
    let shown = content.min(limit);

    (take_chars(text, shown), total - shown)
}
