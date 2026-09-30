//! Row renderers: one function per [`RowKind`], all of them theme-tokened text.
//!
//! Every row is centred inside the shared reading measure and carries its own
//! leading space, so the list can mount rows edge to edge and the transcript
//! still reads as turns: tight inside a group of tool calls or dim lines, a
//! little apart between the parts of one turn, and separated by a hairline when
//! a new turn begins.
//!
//! Nothing here prints protocol payloads: a row is a user turn, rendered
//! markdown, a one-line tool row that opens onto its arguments and result as a
//! key/value list — containers drawn out as rows indented under their key — a
//! report block, or a dim line.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::base::{Easing, SelectableText};
use gpui_kit::component::text::{TextView, TextViewMotion};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, Icon, IconName};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, px, Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, Div, ElementId,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement, Pixels, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, WeakEntity,
};
use serde_json::Value;
use session::{DimStyle, GoalNudgeKind, Row, RowId, RowKind, ToolResult};

use crate::style::{text_style, Palette, BLOCK_GAP, GROUP_GAP, MEASURE, TIGHT_GAP, TURN_GAP};
use crate::{link, markdown, TranscriptData, TranscriptView};

/// Fade window for text appended by a streaming delta (§2.8).
const STREAM_FADE: Duration = Duration::from_millis(350);
/// A little later for each further word of one delta, so chunks overlap into
/// one gradient tail instead of blinking in.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(30);
/// Longest a tool call's arguments an open row shows, in characters — the
/// command among them, which is one of the arguments.
pub(crate) const ARGUMENTS_LIMIT: usize = 1_024;
/// Longest what a tool call returned an open row shows, in characters. A result
/// may be longer than the call that asked for it.
pub(crate) const RESULT_LIMIT: usize = 2_048;
/// The key a call's command arrives under. evo's tools name it: `bash` says
/// `command` (`read` says `path`, `eval` says `code`).
const COMMAND_KEY: &str = "command";
/// Longest a one-line value shows before it is elided; the whole text stays a
/// hover away.
pub(crate) const VALUE_LIMIT: usize = 96;
/// Height of a collapsed tool row, so a long run of them stays a list.
pub(crate) const TOOL_ROW_HEIGHT: Pixels = px(24.);
/// Width of the disclosure and status columns of a tool row.
const DISCLOSURE_WIDTH: Pixels = px(14.);
const STATUS_DOT: Pixels = px(6.);
/// The disclosure chevron: a glyph with about ten pixels of ink, centred in its
/// own column. The glyph box is larger than the ink a chevron actually draws.
const CARET_SIZE: Pixels = px(14.);
/// Width of the key column of an expanded argument list: enough for a nested
/// key like `diff.removed` without eliding it.
pub(crate) const KEY_WIDTH: Pixels = px(112.);
/// How far each level of a nested payload is indented from the level above it.
///
/// The children keep their own keys — nothing is spelled `env.sub` once it is
/// indented — so the indent is what says whose fields they are.
pub(crate) const NEST_INDENT: Pixels = px(12.);
/// How deep a payload is drawn out before what is left of it is summarised.
///
/// Four levels is as far as a reader follows a structure without losing the key
/// it belongs to; past that the summary and its tooltip carry the rest.
pub(crate) const MAX_DEPTH: usize = 4;
/// Longest array drawn item by item. A longer one is summarised: the panel is a
/// reading surface, and a hundred rows of one list is a wall rather than a
/// payload.
pub(crate) const MAX_ARRAY: usize = 20;
/// The gap between a key and its value — and so the indent of a block whose
/// content has no keys of its own.
pub(crate) const COLUMN_GAP: Pixels = px(8.);
/// The size a key is drawn at, in the UI font: a key is a label, not payload.
const KEY_SIZE: Pixels = px(12.);
/// The size a panel's caption is drawn at.
const CAPTION_SIZE: Pixels = px(11.);
/// The size a tool row's name is drawn at, and the size of the status word
/// beside it: one is the row's subject, the other a short word about it.
const NAME_SIZE: Pixels = px(13.);
const STATUS_SIZE: Pixels = px(12.);
/// The line height of payload text, as a multiple of its size.
pub(crate) const PAYLOAD_LINE_HEIGHT: f32 = 1.45;
/// Width of the label column of a report row.
const REPORT_LABEL_WIDTH: Pixels = px(66.);

/// The waiting pips that hold an assistant row's place between `message-start`
/// and the first delta: one cycle of the pulse, and how long each pip is.
const DOT_CYCLE: Duration = Duration::from_millis(1_200);
const DOT_SIZE: Pixels = px(5.);
/// Where each pip is in the cycle: a third of it apart, so the three read as one
/// pulse travelling along the row rather than three separate blinks.
pub(crate) const DOT_PHASES: [f32; 3] = [0., 1. / 3., 2. / 3.];
/// The floor of a pip's fade. A pip is always visible: the row says the message
/// is on its way, it does not blink at the reader.
pub(crate) const DOT_INK_FLOOR: f32 = 0.25;

/// The motion a streaming assistant row is rendered with.
pub(crate) fn stream_motion() -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(STREAM_FADE)
        .with_stream_fade_stagger(STREAM_FADE_STAGGER)
        .with_stream_fade_easing(Easing::EaseOut)
}

/// The group a row declares so its copy buttons are out of the way until the
/// reader is over it: an assistant message's own copy action and the one each of
/// its code blocks carries both wait for hover.
pub(crate) const COPY_GROUP: &str = "transcript-copy";

/// How long a copy button says "Copied" after it was used.
const COPIED_HOLD: Duration = Duration::from_millis(1_200);

/// The size of a copy button's icon and label: the smallest thing in a row, so
/// the affordance reads as a tool rather than as part of the transcript.
const COPY_SIZE: Pixels = px(11.);

/// Which copy button was used.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CopyTarget {
    /// An assistant message: its whole markdown source.
    Message(RowId),
    /// One of the message's code blocks, at the byte offset its fence opens at.
    Block(RowId, usize),
}

/// How often each copy button has been used.
///
/// The count is what times the acknowledgement: it is part of the "Copied"
/// element's id, so every click mounts a fresh one-shot animation instead of
/// reusing a finished one, and the button is back to rest when that animation
/// ends. The base `TextView` asks a code block's actions to be `Send + Sync`,
/// so the count is shared through an `Arc` rather than read from the view that
/// renders the buttons.
#[derive(Default)]
pub(crate) struct CopyFeedback {
    uses: Mutex<HashMap<CopyTarget, u64>>,
}

impl CopyFeedback {
    /// How many times this button has been used.
    fn uses(&self, target: CopyTarget) -> u64 {
        self.lock().get(&target).copied().unwrap_or(0)
    }

    /// Note a use, so the button renders its acknowledgement on the next frame.
    fn record(&self, target: CopyTarget) {
        *self.lock().entry(target).or_default() += 1;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CopyTarget, u64>> {
        self.uses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The row a copy button's icon and word sit in.
fn copy_face() -> Div {
    div().flex().items_center().gap(px(4.))
}

/// Fill a copy button's face: the resting one, or the acknowledgement it shows
/// while `copied`.
fn fill_copy_face<E: ParentElement>(face: E, copied: bool, with_label: bool, key: &ElementId) -> E {
    let icon = if copied {
        IconName::Check
    } else {
        IconName::Copy
    };
    let mut face = face.child(Icon::new(icon).size(COPY_SIZE));
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
///
/// Pressing it copies `text` — a message's markdown source, or a code block's
/// raw code — and the button acknowledges with "Copied" for [`COPIED_HOLD`].
/// `with_label` drops the word next to the icon, for the buttons that have to be
/// unobtrusive (a message's own action, at the top of the row).
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

    let button = div()
        .id(id.clone())
        .px(px(6.))
        .py(px(1.))
        .rounded(palette.radius)
        .bg(palette.muted)
        // A hairline, because a muted chip on a code block's own background is
        // otherwise the same tone as what it sits on.
        .border_1()
        .border_color(palette.border)
        .text_size(COPY_SIZE)
        .line_height(px(14.))
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

    // An unused button has nothing to acknowledge, so it is the plain one until
    // it has been pressed once.
    let key = id.clone();
    if uses == 0 {
        button.child(fill_copy_face(copy_face(), false, with_label, &key))
    } else {
        button.child(
            copy_face()
                // The count is in the id on purpose: a later use is a new
                // element, which starts the acknowledgement over rather than
                // inheriting the finished animation of the one before it.
                .with_animation(
                    (id, uses.to_string()),
                    Animation::new(COPIED_HOLD),
                    move |face, delta| fill_copy_face(face, delta < 1., with_label, &key),
                ),
        )
    }
}

/// How a row spaces itself against the one before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    User,
    /// The reader's own words, still queued: drawn like a turn — it is one of
    /// their messages — but with no turn boundary of its own, because it has not
    /// opened one yet.
    Pending,
    Assistant,
    Tool,
    Report,
    Dim,
    /// Injected context: a note about the session, not a part of it.
    Context,
}

impl Group {
    fn of(kind: &RowKind) -> Self {
        match kind {
            RowKind::User { .. } => Self::User,
            RowKind::PendingUser { .. } => Self::Pending,
            RowKind::Assistant { .. } => Self::Assistant,
            RowKind::Tool { .. } => Self::Tool,
            RowKind::Report { .. } => Self::Report,
            // A lane notice and a goal nudge are quiet lines like dim rows: what evo
            // and the swarm say to the agent, not what the conversation is made of,
            // and they read as one block when several land together.
            RowKind::Dim { .. }
            | RowKind::RunOutcome { .. }
            | RowKind::LaneNotice { .. }
            | RowKind::GoalNudge { .. }
            | RowKind::CommandNote { .. } => Self::Dim,
            RowKind::Context { .. } => Self::Context,
        }
    }
}

/// Space above `row`, given the row before it: nothing at the top of the
/// transcript, tight inside a group of tool or dim rows, a paragraph apart
/// between the parts of one turn, and a turn's worth before a new turn (whose
/// separator carries that space itself).
fn gap_before(previous: Option<&Row>, row: &Row) -> Pixels {
    let Some(previous) = previous else {
        return px(0.);
    };
    let (previous, current) = (Group::of(&previous.kind), Group::of(&row.kind));

    if current == Group::User {
        return px(0.);
    }
    // A queued turn draws no boundary of its own (see `Group::Pending`), so the
    // row carries the space the boundary would have.
    if current == Group::Pending {
        return BLOCK_GAP;
    }
    if previous == current {
        return match current {
            Group::Tool | Group::Dim | Group::Context => TIGHT_GAP,
            Group::Assistant | Group::Report | Group::User | Group::Pending => GROUP_GAP,
        };
    }
    BLOCK_GAP
}

/// Render the row at `index`, or an empty element when the list asks for a row
/// that is no longer there.
pub(crate) fn render_row(
    data: &mut TranscriptData,
    index: usize,
    view: &WeakEntity<TranscriptView>,
    cx: &mut Context<TranscriptData>,
) -> AnyElement {
    if data.rows.get(index).is_none() {
        return div().into_any_element();
    }

    // An assistant row's document is brought up to date here, at the frame that
    // shows the row, rather than on every delta that arrives for it: a stream
    // that lands fifty updates between two frames is one parse, not fifty.
    let waiting = match &data.rows[index].kind {
        RowKind::Assistant {
            markdown,
            streaming,
            ..
        } => *streaming && markdown.trim().is_empty(),
        _ => false,
    };
    if matches!(data.rows[index].kind, RowKind::Assistant { .. }) {
        let id = data.rows[index].id;
        data.note_rendered(id);
        if !waiting {
            data.sync_document(index, cx);
        }
    }

    let row = &data.rows[index];
    let palette = Palette::from_app(cx);
    let previous = index.checked_sub(1).and_then(|index| data.rows.get(index));

    let focus = data.focus.clone();
    let mut stack = div().flex().flex_col().w_full().min_w_0();
    // A user turn opens a new turn: say so, rather than printing a run marker.
    if matches!(row.kind, RowKind::User { .. }) && previous.is_some() {
        let turn = data.rows[..=index]
            .iter()
            .filter(|row| matches!(row.kind, RowKind::User { .. }))
            .count();
        stack = stack.child(turn_separator(turn, &palette));
    }

    stack = stack.child(match &row.kind {
        RowKind::User { text, .. } => user_row(row.id, text, &palette),
        RowKind::PendingUser { text } => pending_user_row(row.id, text, &palette),
        RowKind::Context { key, text } => context_row(
            row.id,
            key,
            text,
            data.expanded.contains(&row.id),
            view,
            &palette,
        ),
        RowKind::Assistant { .. } => assistant_row(row, data, cx, &palette),
        RowKind::Tool {
            name,
            arguments,
            result,
            ..
        } => tool_row(
            row.id,
            name,
            arguments,
            result.as_ref(),
            data.expanded.contains(&row.id),
            view,
            &palette,
        ),
        RowKind::Report { .. } => report_row(row, &palette),
        RowKind::LaneNotice { lane, text, tone } => {
            lane_notice_row(row.id, *lane, text, *tone, &palette)
        }
        RowKind::GoalNudge { .. } => {
            goal_nudge_row(row, data.expanded.contains(&row.id), view, &palette)
        }
        RowKind::CommandNote { command, text } => command_note_row(
            row.id,
            command,
            text,
            data.expanded.contains(&row.id),
            view,
            &palette,
        ),
        RowKind::Dim { style, text } => dim_row(row.id, *style, text, &palette),
        RowKind::RunOutcome { outcome, text } => run_outcome_row(row.id, outcome, text, &palette),
    });

    div()
        .id(("transcript-row", row.id))
        .w_full()
        .pt(gap_before(previous, row))
        .flex()
        .justify_center()
        .child(
            div()
                .id(("transcript-measure", row.id))
                .w_full()
                .min_w_0()
                .max_w(px(MEASURE))
                .test_support()
                .child(stack),
        )
        .test_support()
        // A drag in a row takes the focus, so ⌘C reaches the window's copy —
        // the transcript itself is not a text view with a binding of its own.
        // It is not a tab stop: the keyboard walks into the transcript, not
        // through every row of it.
        .track_focus(&focus)
        .tab_stop(false)
        .into_any_element()
}

/// The hairline between two turns, labelled with the turn it opens.
fn turn_separator(turn: usize, palette: &Palette) -> AnyElement {
    div()
        .id(("transcript-turn", turn))
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .pt(TURN_GAP)
        .pb(GROUP_GAP)
        .child(div().flex_1().h(px(1.)).bg(palette.border))
        .child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(format!("turn {turn}")),
        )
        .test_support()
        .into_any_element()
}

/// A user turn: plain text (never a markdown document) on a muted card, with
/// the accent bar that marks where the turn starts.
fn user_row(id: RowId, text: &str, palette: &Palette) -> AnyElement {
    div()
        .id(("transcript-user", id))
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_l_3()
        .border_color(palette.primary)
        .px_3()
        .py_2()
        .text_color(palette.foreground)
        .child(SelectableText::new(
            ("transcript-user-text", id),
            text.to_string(),
        ))
        .test_support()
        .into_any_element()
}

/// What a queued turn says under itself: where the reader's words are.
const QUEUED_CAPTION: &str = "queued · sent at the next step";

/// The reader's words from the moment they are sent, while evo still has them
/// queued: the same card a turn is drawn on, held back — the accent bar and the
/// text muted — with a line under it saying so.
///
/// It is deliberately not a turn yet: no `turn N` boundary is drawn (nothing has
/// opened), which is also why the row carries the space a boundary would have.
/// The `user-input`/`steering` event that carries the text replaces this row with
/// a real one, in the place evo put the turn — separator and all.
fn pending_user_row(id: RowId, text: &str, palette: &Palette) -> AnyElement {
    let caption_id = ElementId::from(("transcript-pending", id));
    div()
        .id(("transcript-user", id))
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_l_3()
        .border_color(palette.muted_foreground)
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .text_color(palette.muted_foreground)
        .child(SelectableText::new(
            ("transcript-user-text", id),
            text.to_string(),
        ))
        .child(caption(&caption_id, QUEUED_CAPTION, palette))
        .test_support()
        .into_any_element()
}

/// How much of an opened context row's text is on screen at once: the rest is a
/// scroll inside the block.
pub(crate) const CONTEXT_BLOCK_LINES: usize = 12;

/// How wide a lane notice's tooltip may grow: one line of the swarm's words, wider than
/// the reading measure they were cut to but not the full length of a task path.
const NOTICE_TOOLTIP_WIDTH: Pixels = px(520.);

/// A row that is one quiet line until it is opened: something evo, the swarm or an
/// extension steered in, named by the line and kept whole in a block under it.
///
/// `header` and `block` are the element names the row owns — a test reaches the line by
/// the first and the text by the second — and `head`/`trailing` are the line itself:
/// the part that gives way when it is longer than the measure, and a second cell that
/// never does (a budget, say, which a reader wants whether or not the objective fits).
struct QuietRow<'a> {
    id: RowId,
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
    // The whole line, cut or not, for a reader who cannot see it.
    let aria = match &trailing {
        Some(trailing) => format!("{head} {trailing}"),
        None => head.clone(),
    };

    let header = div()
        .id((name, id))
        .flex()
        .items_center()
        .gap_2()
        .h(TOOL_ROW_HEIGHT)
        .cursor_pointer()
        .aria_label(aria)
        .aria_expanded(expanded)
        .on_click(move |_, _, cx| {
            let _ = view.update(cx, |view, cx| view.toggle_expanded(id, cx));
        })
        .child(caret(expanded, palette))
        .child(
            // Content-sized, and only that: a longer line takes the room it needs from
            // nothing, so whatever follows follows it.
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .text_size(NAME_SIZE)
                .text_color(palette.muted_foreground)
                .child(head),
        )
        .children(trailing.map(|trailing| {
            div()
                .id((SharedString::from(format!("{name}-trailing")), id as usize))
                .flex_none()
                .text_size(NAME_SIZE)
                .text_color(palette.muted_foreground)
                .child(trailing)
                .test_support()
        }))
        .test_support();

    let mut row = div().w_full().min_w_0().flex().flex_col().child(header);
    if expanded {
        row = row.child(quiet_block(block, id, text, palette));
    }
    row.into_any_element()
}

/// Content an extension injected (`evo:inject-context`): one quiet line saying what it
/// is and where it came from, which opens onto the text itself.
///
/// It arrives as a user-role message but the reader never wrote it — a memory snapshot
/// is kilobytes of their own private context — so it is drawn as a note rather than as a
/// turn, and never open by default.
fn context_row(
    id: RowId,
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

/// A command the reader ran, answered with instructions for the agent: one quiet line
/// naming the command, opening onto the whole of what the extension said.
///
/// The reader typed `/global-memory <query>`, not this — what they gave the agent was
/// the query, and evo wrapped it in instructions for the agent's own use
/// (`scoped-memory-command`, `src/core-ext/memory.lisp:242`). Naming the command is what
/// tells a reader why the line is there.
fn command_note_row(
    id: RowId,
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

/// What a context row calls its key: the two the memory extension injects have
/// names a reader knows, and any other key is shown as its extension named it
/// (`recovery`, and whatever an extension adds next).
pub(crate) fn context_label(key: &str) -> String {
    match key {
        "global-memory" => "global memory".to_string(),
        "project-memory" => "project memory".to_string(),
        other => other.to_string(),
    }
}

/// A goal nudge evo steered into its own agent: one quiet line saying which nudge it is,
/// which goal it is about and what the budget stands at, which opens onto the whole
/// message.
///
/// It arrives as a user-role message because `queue-steering` puts it in the input queue
/// (`src/kernel/goal.lisp:149`, :153), but the reader did not write it and it is not part
/// of the conversation: it is evo keeping its own goal going, so it is drawn as a note —
/// closed, one line — rather than as a turn. The whole of it is a click away; the rules
/// it carries are for the agent, not for the reader.
fn goal_nudge_row(
    row: &Row,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let RowKind::GoalNudge {
        kind,
        objective,
        budget,
        text,
    } = &row.kind
    else {
        unreachable!("goal_nudge_row draws a goal nudge")
    };

    // Which nudge, and for a continuation which goal: the objective is the part that
    // gives way when the line is longer than the measure. The budget follows it and
    // never gives way — a reader wants to know what is left of it either way.
    let (head, spent) = match kind {
        GoalNudgeKind::Continue if !objective.is_empty() => (
            format!("Goal · continue — {objective}"),
            (!budget.is_empty()).then(|| format!("· {budget}")),
        ),
        GoalNudgeKind::Continue => (
            "Goal · continue".to_string(),
            (!budget.is_empty()).then(|| format!("· {budget}")),
        ),
        // A wrap-up says in its own words that the budget is what ran out.
        GoalNudgeKind::Wrapup => ("Goal · budget exhausted — wrap up".to_string(), None),
        GoalNudgeKind::Updated if !objective.is_empty() => {
            (format!("Goal · objective updated — {objective}"), None)
        }
        GoalNudgeKind::Updated => ("Goal · objective updated".to_string(), None),
    };

    quiet_row(
        QuietRow {
            id: row.id,
            header: "transcript-goal",
            head,
            trailing: spent,
            text,
            block: "transcript-goal-text",
        },
        expanded,
        view,
        palette,
    )
}

/// The text of an opened quiet row: the payload face, selectable, capped at
/// [`CONTEXT_BLOCK_LINES`] with a scroll of its own — what evo steered in is far
/// longer than a row of a transcript.
///
/// `name` is the row's own element name; the block, the text inside it and the run a
/// reader selects are `name`, `name-content` and `name-run`, which is how a test
/// reaches the text of one.
fn quiet_block(name: &'static str, id: RowId, text: &str, palette: &Palette) -> AnyElement {
    div()
        .id((name, id))
        .w_full()
        .min_w_0()
        .max_h(palette.payload_size * (PAYLOAD_LINE_HEIGHT * CONTEXT_BLOCK_LINES as f32))
        .overflow_y_scroll()
        .rounded(palette.radius)
        .border_1()
        .border_color(palette.border)
        .px_2()
        .py_1()
        // The block separates itself from whatever follows: the gap rules see the
        // row, which is one line taller with the block under it.
        .mb(GROUP_GAP)
        .font_family(palette.mono.clone())
        .text_size(palette.payload_size)
        .line_height(palette.payload_size * PAYLOAD_LINE_HEIGHT)
        .text_color(palette.foreground)
        .child(
            div()
                .id((SharedString::from(format!("{name}-content")), id as usize))
                .w_full()
                .min_w_0()
                .test_support()
                // A run of the block's own: selectable, and selected on its own, so a
                // reader can copy it without the line above it.
                .child(SelectableText::new(
                    (SharedString::from(format!("{name}-run")), id as usize),
                    text.to_string(),
                )),
        )
        .test_support()
        .into_any_element()
}

/// An assistant message: the retained markdown document, its optional thinking
/// text, and the error that ended it, if any.
///
/// A message that has started but has not sent a word yet has no document to
/// show — [`waiting_dots`] holds its place until the first delta arrives.
fn assistant_row(row: &Row, data: &TranscriptData, cx: &App, palette: &Palette) -> AnyElement {
    let RowKind::Assistant {
        markdown,
        thinking,
        streaming,
        error,
    } = &row.kind
    else {
        // Only an assistant row is drawn this way.
        return div().into_any_element();
    };
    let error = error.as_deref();
    let id = row.id;
    let mut row = div()
        .id(("transcript-assistant", id))
        .group(COPY_GROUP)
        .relative()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2();

    if *streaming && markdown.trim().is_empty() {
        row = row.child(waiting_dots(id, palette));
    } else {
        row = match data.documents.get(&id) {
            // The retained document: never recreated per delta, `set_text` extends it.
            Some(document) => {
                let feedback = data.copy_feedback.clone();
                let message = markdown.to_string();
                let code_palette = palette.clone();
                row.child(
                    TextView::new(document)
                        .style(text_style(cx))
                        .motion(stream_motion())
                        // A link opens in the browser, if it names a scheme the
                        // app opens at all (`link::openable`).
                        .on_link_click(link::on_click())
                        // Image references and raw HTML are the transcript's
                        // own business (`markdown::extensions`).
                        .markdown_extensions(markdown::extensions())
                        // A fenced block carries its own Copy: the reader who
                        // wants the code wants only the code, not the prose
                        // around it.
                        .code_block_actions(move |code_block, _, _| {
                            let block = code_block.span.map(|span| span.start).unwrap_or(0);
                            copy_button(
                                ("transcript-copy-block", id),
                                CopyTarget::Block(id, block),
                                code_block.code(),
                                &feedback,
                                true,
                                &code_palette,
                            )
                            .test_support()
                        }),
                )
                .child(
                    copy_button(
                        ("transcript-copy-message", id),
                        CopyTarget::Message(id),
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
            // Unreachable while every assistant row gets a document on the way in;
            // fall back to the source rather than dropping the message.
            None => row.child(
                div()
                    .text_color(palette.muted_foreground)
                    .child(markdown.to_string()),
            ),
        };
    }

    if data.show_thinking && !thinking.is_empty() {
        row = row.child(
            div()
                .id(("transcript-thinking", id))
                .flex()
                .flex_col()
                .gap_1()
                .pl_2()
                .border_l_2()
                .border_color(palette.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child("thinking"),
                )
                .child(
                    div()
                        .text_sm()
                        .italic()
                        .text_color(palette.muted_foreground)
                        .child(SelectableText::new(
                            ("transcript-thinking-text", id),
                            thinking.to_string(),
                        )),
                )
                .test_support(),
        );
    }

    if let Some(error) = error.filter(|error| !error.is_empty()) {
        row = row.child(
            div()
                .text_sm()
                .text_color(palette.destructive)
                .child(format!("error: {error}")),
        );
    }

    row.test_support().into_any_element()
}

/// The pips that hold an assistant row's place between a message starting and
/// its first delta.
///
/// They sit at a text line's height, so the transcript does not jump when the
/// first word lands, and they are drawn through
/// [`AnimationExt::with_animation`], which stands still under
/// [`App::reduce_motion`] rather than scheduling frames.
fn waiting_dots(id: RowId, palette: &Palette) -> AnyElement {
    let ink = palette.muted_foreground;
    h_flex()
        .id(("transcript-waiting", id))
        .items_center()
        .gap_1()
        .py(px(8.))
        .test_support()
        .with_animation(
            ("transcript-waiting-pulse", id),
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

/// How lit a pip is at `delta` through the cycle, `phase` behind the first one:
/// a triangle wave, up in the middle of its turn and dim at its ends.
pub(crate) fn dot_ink(delta: f32, phase: f32) -> f32 {
    let turn = (delta + phase).rem_euclid(1.);
    let swell = 1. - ((turn - 0.5).abs() * 2.);
    DOT_INK_FLOOR + (1. - DOT_INK_FLOOR) * swell
}

/// A tool call: one compact line — `name · ok|error|running` — that opens onto
/// the call's arguments and its result, each as a key/value list when it is
/// JSON and as capped text when it is not.
fn tool_row(
    id: RowId,
    name: &str,
    arguments: &str,
    result: Option<&ToolResult>,
    expanded: bool,
    view: &WeakEntity<TranscriptView>,
    palette: &Palette,
) -> AnyElement {
    let (status, status_color) = match result {
        None => ("running", palette.warning),
        Some(result) if result.is_error => ("error", palette.destructive),
        Some(_) => ("ok", palette.success),
    };

    let view = view.clone();
    // The header is a quiet line: the name in the mono face at its own size,
    // the status word a point smaller in the UI font, so neither shouts over
    // the other and a run of tool calls reads as one list. It is the row's
    // control — a click opens and closes it — so it is the one line of a tool
    // row that is not selectable text; what the call carried is.
    let header = div()
        .id(("transcript-tool", id))
        .flex()
        .items_center()
        .gap_2()
        .h(TOOL_ROW_HEIGHT)
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let _ = view.update(cx, |view, cx| view.toggle_expanded(id, cx));
        })
        .child(caret(expanded, palette))
        .child(
            div()
                .min_w_0()
                .font_family(palette.mono.clone())
                .font_weight(FontWeight::NORMAL)
                .text_size(NAME_SIZE)
                .text_color(palette.foreground)
                .child(name.to_string()),
        )
        .child(
            div()
                .flex_shrink_0()
                .size(STATUS_DOT)
                .rounded_full()
                .bg(status_color),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(STATUS_SIZE)
                .text_color(palette.muted_foreground)
                .child(status),
        )
        .test_support();

    let mut row = div()
        .id(("transcript-tool-row", id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(header);

    if expanded {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pt(px(2.))
                .pb(px(4.))
                .child(arguments_block(id, arguments, palette))
                .child(result_block(id, result, palette)),
        );
    }

    row.test_support().into_any_element()
}

/// The disclosure of a tool row: a chevron at [`CARET_SIZE`] in a column of its
/// own, so an opened row and a closed one keep their name on the same axis.
fn caret(expanded: bool, palette: &Palette) -> AnyElement {
    let chevron = if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    };

    div()
        .w(DISCLOSURE_WIDTH)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(chevron)
                .size(CARET_SIZE)
                .text_color(palette.muted_foreground),
        )
        .into_any_element()
}

/// The arguments of an open tool row: the call's JSON as a key/value list, or
/// the text exactly as it came when it is not a JSON object.
fn arguments_block(id: RowId, arguments: &str, palette: &Palette) -> AnyElement {
    if arguments.is_empty() {
        return div().into_any_element();
    }
    match json_fields(arguments) {
        Some(fields) if !fields.is_empty() => fields_block(
            ("transcript-tool-arguments", id),
            "arguments",
            &fields,
            ARGUMENTS_LIMIT,
            palette,
        ),
        _ => text_block(
            ("transcript-tool-arguments", id),
            "arguments",
            arguments,
            None,
            ARGUMENTS_LIMIT,
            palette,
        ),
    }
}

/// The result of an open tool row: a JSON object — or array — as the same
/// key/value list, any other text as a capped mono block.
fn result_block(id: RowId, result: Option<&ToolResult>, palette: &Palette) -> AnyElement {
    let Some(result) = result.filter(|result| !result.content.is_empty()) else {
        return div().into_any_element();
    };
    let label = if result.is_error { "error" } else { "result" };

    match json_fields(&result.content) {
        Some(fields) if !fields.is_empty() => fields_block(
            ("transcript-tool-result", id),
            label,
            &fields,
            RESULT_LIMIT,
            palette,
        ),
        _ => text_block(
            ("transcript-tool-result", id),
            label,
            &result.content,
            result.content_chars,
            RESULT_LIMIT,
            palette,
        ),
    }
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
        .text_size(CAPTION_SIZE)
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
fn caption(id: &ElementId, label: &str, palette: &Palette) -> AnyElement {
    div()
        .id((id.clone(), "caption"))
        .text_size(CAPTION_SIZE)
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
    let mut block = div()
        .id(id.clone())
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .border_1()
        .border_color(palette.border)
        .px_2()
        .py_1()
        .text_size(palette.payload_size)
        .line_height(palette.payload_size * PAYLOAD_LINE_HEIGHT)
        .child(caption(&id, label, palette));

    for (index, field) in fields.iter().enumerate() {
        block = block.child(field_row(&id, index, field, palette));
    }
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
            .tooltip(move |window, cx| Tooltip::new(full.clone()).max_w(px(520.)).build(window, cx))
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
        .text_size(KEY_SIZE)
        .text_color(palette.muted_foreground)
        .child(SelectableText::new((id.clone(), "key"), key.to_string()))
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
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
            .text_size(KEY_SIZE)
            .text_color(palette.muted_foreground)
            .child(SelectableText::new((id.clone(), "value"), summary.clone()))
            .test_support()
            .into_any_element(),
        FieldValue::Nested(_) => unreachable!("a container is drawn by field_row itself"),
    }
}

fn report_row(row: &Row, palette: &Palette) -> AnyElement {
    let RowKind::Report {
        done,
        evidence,
        next,
        blocked,
        requests,
        goal,
        lane,
    } = &row.kind
    else {
        unreachable!("report_row draws a report row")
    };
    let id = row.id;
    let heading = match lane {
        Some(lane) => format!("Lane {lane} report"),
        None => "report".to_string(),
    };

    let mut fields = vec![
        ("done", done.as_str(), palette.foreground),
        ("evidence", evidence.as_str(), palette.muted_foreground),
        ("next", next.as_str(), palette.foreground),
        ("blocked", blocked.as_str(), palette.destructive),
        ("requests", requests.as_str(), palette.primary),
    ];
    if let Some(goal) = goal {
        fields.push(("goal", goal.as_str(), palette.muted_foreground));
    }

    let mut row = div()
        .id(("transcript-report", id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .rounded(palette.radius_lg)
        .border_1()
        .border_color(palette.border)
        .px_3()
        .py_2()
        .child(
            // A report the swarm passed to the coordinator is about one of its lanes,
            // so it is headed with that lane: in the lane's own tab the heading is the
            // one it has always had.
            div()
                .id(("transcript-report-heading", id))
                .text_xs()
                .text_color(palette.muted_foreground)
                .aria_label(heading.clone())
                .child(heading)
                .test_support(),
        );

    for (label, value, color) in fields {
        if value.is_empty() {
            continue;
        }
        let value_id: ElementId = (
            SharedString::from(format!("transcript-report-{label}")),
            id as usize,
        )
            .into();
        row = row.child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .w(REPORT_LABEL_WIDTH)
                        .flex_shrink_0()
                        .text_color(palette.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_color(color)
                        .child(SelectableText::new(value_id, value.to_string())),
                ),
        );
    }

    row.test_support().into_any_element()
}

/// A line the swarm wrote to the coordinator about one of its lanes: `Lane 1 · run
/// ended (stop) — task: …`.
///
/// It is not the reader's message and not the coordinator's prose — the swarm steered it
/// in — so it keeps the swarm's own words, one line, with the lane named in front of
/// them and the whole of it a hover away. Not a card, and not a turn: this is what the
/// swarm is saying, while the reader's own words stay the only thing that opens a turn.
fn lane_notice_row(
    id: RowId,
    lane: u32,
    text: &str,
    tone: DimStyle,
    palette: &Palette,
) -> AnyElement {
    let color = match tone {
        DimStyle::Error => palette.destructive,
        _ => palette.muted_foreground,
    };
    let full: SharedString = format!("Lane {lane} · {text}").into();
    let tooltip = full.clone();
    let notice = div()
        .id(("transcript-lane-notice", id))
        .w_full()
        .min_w_0()
        .truncate()
        .text_sm()
        .line_height(px(18.))
        .text_color(color)
        // The line on screen is cut to the measure; this is the whole of what the swarm
        // said, which is what a reader using a screen reader (or a test) is told.
        .aria_label(full.clone())
        .child(full);
    // Only the lane's own line is cut: the row has no second line to fall back on, and
    // the rest of what the swarm said is what its tooltip is for. The tooltip carries
    // the lane's name too, since a cut line can cut the task that names it.
    notice
        .tooltip(move |window, cx| {
            Tooltip::new(tooltip.clone())
                .max_w(NOTICE_TOOLTIP_WIDTH)
                .build(window, cx)
        })
        .test_support()
        .into_any_element()
}

/// An `output` line or status event: one dim line, readable but out of the way.
fn dim_row(id: RowId, style: DimStyle, text: &str, palette: &Palette) -> AnyElement {
    dim_line(("transcript-dim", id), style, text, palette)
}

/// A run that ended badly, as the session wrote it: `text` is the line to show
/// and `outcome` decides the colour — a run that failed is an error, one that
/// was stopped or ran out of room is a notice.
fn run_outcome_row(id: RowId, outcome: &str, text: &str, palette: &Palette) -> AnyElement {
    dim_line(
        ("transcript-run-outcome", id),
        run_outcome_style(outcome),
        text,
        palette,
    )
}

/// The style of a run's outcome line: `error` is an error, and every other
/// outcome (`aborted`, `length`, whatever the swarm adds next) is a notice.
pub(crate) fn run_outcome_style(outcome: &str) -> DimStyle {
    if outcome == "error" {
        DimStyle::Error
    } else {
        DimStyle::Notice
    }
}

/// One dim line of the transcript.
fn dim_line(
    id: impl Into<gpui_kit::ElementId>,
    style: DimStyle,
    text: &str,
    palette: &Palette,
) -> AnyElement {
    let color = match style {
        DimStyle::Dim | DimStyle::Status => palette.muted_foreground,
        DimStyle::Notice => palette.info,
        DimStyle::Error => palette.destructive,
    };
    let id = id.into();

    div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .text_sm()
        .line_height(px(18.))
        .text_color(color)
        .child(SelectableText::new((id, "text"), text.to_string()))
        .test_support()
        .into_any_element()
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
    let mut block = div()
        .id(id.clone())
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .border_1()
        .border_color(palette.border)
        .px_2()
        .py_1()
        .child(caption(&id, label, palette))
        // A body with no keys of its own starts at the panel's own edge, under
        // its caption: a text result is the whole width of the panel, not a
        // column of it.
        .child(
            div()
                .id((id.clone(), "text"))
                .w_full()
                .min_w_0()
                .font_family(palette.mono.clone())
                .text_size(palette.payload_size)
                .line_height(palette.payload_size * PAYLOAD_LINE_HEIGHT)
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
