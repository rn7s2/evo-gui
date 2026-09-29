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
//! key/value list, a report block, or a dim line.

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
use session::{DimStyle, Row, RowId, RowKind, ToolResult};

use crate::style::{text_style, Palette, BLOCK_GAP, GROUP_GAP, MEASURE, TIGHT_GAP, TURN_GAP};
use crate::{TranscriptData, TranscriptView};

/// Fade window for text appended by a streaming delta (§2.8).
const STREAM_FADE: Duration = Duration::from_millis(350);
/// A little later for each further word of one delta, so chunks overlap into
/// one gradient tail instead of blinking in.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(30);
/// Longest tool argument or result text an expanded row shows, in characters.
pub(crate) const TOOL_TEXT_LIMIT: usize = 4_000;
/// Lines a multi-line string — a file's contents, a command — is shown in
/// before the block is capped.
pub(crate) const BLOCK_LINES: usize = 8;
/// Longest a one-line value shows before it is elided; the whole text stays a
/// hover away.
pub(crate) const VALUE_LIMIT: usize = 96;
/// Height of a collapsed tool row, so a long run of them stays a list.
const TOOL_ROW_HEIGHT: Pixels = px(24.);
/// Width of the disclosure and status columns of a tool row.
const DISCLOSURE_WIDTH: Pixels = px(14.);
const STATUS_DOT: Pixels = px(6.);
/// The disclosure chevron: a glyph with about ten pixels of ink, centred in its
/// own column. The glyph box is larger than the ink a chevron actually draws.
const CARET_SIZE: Pixels = px(14.);
/// Width of the key column of an expanded argument list: enough for a nested
/// key like `diff.removed` without eliding it.
pub(crate) const KEY_WIDTH: Pixels = px(112.);
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
const PAYLOAD_LINE_HEIGHT: f32 = 1.45;
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
    Assistant,
    Tool,
    Report,
    Dim,
}

impl Group {
    fn of(kind: &RowKind) -> Self {
        match kind {
            RowKind::User { .. } => Self::User,
            RowKind::Assistant { .. } => Self::Assistant,
            RowKind::Tool { .. } => Self::Tool,
            RowKind::Report { .. } => Self::Report,
            RowKind::Dim { .. } | RowKind::RunOutcome { .. } => Self::Dim,
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
    if previous == current {
        return match current {
            Group::Tool => TIGHT_GAP,
            Group::Dim => TIGHT_GAP,
            Group::Assistant | Group::Report | Group::User => GROUP_GAP,
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
        RowKind::Assistant {
            markdown,
            thinking,
            error,
            ..
        } => assistant_row(
            row.id,
            markdown,
            thinking,
            waiting,
            error.as_deref(),
            data,
            cx,
            &palette,
        ),
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
        RowKind::Report {
            done,
            evidence,
            next,
            blocked,
            requests,
        } => report_row(row.id, done, evidence, next, blocked, requests, &palette),
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

/// An assistant message: the retained markdown document, its optional thinking
/// text, and the error that ended it, if any.
///
/// A message that has started but has not sent a word yet has no document to
/// show — [`waiting_dots`] holds its place until the first delta arrives.
fn assistant_row(
    id: RowId,
    markdown: &str,
    thinking: &str,
    streaming: bool,
    error: Option<&str>,
    data: &TranscriptData,
    cx: &App,
    palette: &Palette,
) -> AnyElement {
    let mut row = div()
        .id(("transcript-assistant", id))
        .group(COPY_GROUP)
        .relative()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2();

    if streaming && markdown.trim().is_empty() {
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
            palette,
        ),
        _ => text_block(
            ("transcript-tool-arguments", id),
            "arguments",
            arguments,
            None,
            palette,
        ),
    }
}

/// The result of an open tool row: a JSON object as the same key/value list, any
/// other text as a capped mono block.
fn result_block(id: RowId, result: Option<&ToolResult>, palette: &Palette) -> AnyElement {
    let Some(result) = result.filter(|result| !result.content.is_empty()) else {
        return div().into_any_element();
    };
    let label = if result.is_error { "error" } else { "result" };

    match json_fields(&result.content) {
        Some(fields) if !fields.is_empty() => {
            fields_block(("transcript-tool-result", id), label, &fields, palette)
        }
        _ => text_block(
            ("transcript-tool-result", id),
            label,
            &result.content,
            result.content_chars,
            palette,
        ),
    }
}

/// One field of a tool call's arguments, or of a JSON result.
#[derive(Debug, PartialEq)]
pub(crate) struct Field {
    /// The key the reader sees: `timeout`, or `env.RUST_LOG` when the call
    /// nested one object inside another.
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
                push_field(&mut fields, key, &value, true);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                push_field(&mut fields, index.to_string(), value, true);
            }
        }
        _ => return None,
    }
    Some(fields)
}

/// Add what `value` draws as. A container directly inside the object flattens
/// into one field per leaf (`key.sub`, `key.0`); anything nested deeper is
/// compact JSON on one line, so a structure never arrives as a wall of braces.
fn push_field(fields: &mut Vec<Field>, key: String, value: &Value, flatten: bool) {
    match (value, flatten) {
        (Value::Object(object), true) => {
            for (sub, value) in object {
                push_field(fields, format!("{key}.{sub}"), value, false);
            }
        }
        (Value::Array(items), true) => {
            for (index, value) in items.iter().enumerate() {
                push_field(fields, format!("{key}.{index}"), value, false);
            }
        }
        // A string with line breaks in it is not a line of a list: it gets a
        // block of its own, so file contents and multi-line commands stay
        // readable.
        (Value::String(text), _) if text.contains('\n') => fields.push(Field {
            key,
            value: FieldValue::Block(text.clone()),
        }),
        (Value::String(text), _) => {
            let (text, full) = elide(text);
            fields.push(Field {
                key,
                value: FieldValue::Text { text, full },
            });
        }
        (other, _) => {
            let (text, full) = elide(&other.to_string());
            fields.push(Field {
                key,
                value: FieldValue::Text { text, full },
            });
        }
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

/// Whether a value should be drawn in the mono face: a path, a command, an
/// identifier. `cargo test -p transcript` reads as code; "the provider returned
/// 429" does not.
pub(crate) fn looks_like_code(text: &str) -> bool {
    text.contains('/') || text.contains("::") || text.contains('(') || text.contains('=')
}

/// The caption of a payload panel: what the block below it holds, in the small
/// caps a label is set in — quiet enough not to read as a line of the
/// transcript. (GPUI has no letter spacing, so the caption is upper case and
/// small rather than tracked.)
fn caption(label: &str, palette: &Palette) -> AnyElement {
    div()
        .text_size(CAPTION_SIZE)
        .text_color(palette.muted_foreground)
        .child(label.to_uppercase())
        .into_any_element()
}

/// A JSON object as a compact list: one `key  value` row per field, the key
/// muted and the value plain — never the braces and quotes it arrived in.
fn fields_block(
    id: impl Into<ElementId>,
    label: &str,
    fields: &[Field],
    palette: &Palette,
) -> AnyElement {
    let id = id.into();
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
        .child(caption(label, palette));

    for (index, field) in fields.iter().enumerate() {
        block = block.child(field_row(&id, index, field, palette));
    }

    block.test_support().into_any_element()
}

/// One `key  value` row of a [`fields_block`].
fn field_row(base: &ElementId, index: usize, field: &Field, palette: &Palette) -> AnyElement {
    let elided = match &field.value {
        FieldValue::Text { full, .. } => full.clone(),
        FieldValue::Block(_) => None,
    };
    let id: ElementId = (base.clone(), index.to_string()).into();
    let row = div()
        .id(id.clone())
        .w_full()
        .min_w_0()
        .flex()
        .items_start()
        .gap(COLUMN_GAP)
        .child(
            div()
                .w(KEY_WIDTH)
                .flex_shrink_0()
                .truncate()
                .text_size(KEY_SIZE)
                .text_color(palette.muted_foreground)
                .child(SelectableText::new((id.clone(), "key"), field.key.clone())),
        )
        .child(match &field.value {
            FieldValue::Text { text, .. } => {
                // Only a value that reads as code is set in mono; prose stays in
                // the UI font, where it is easier to read.
                let value = div()
                    .flex_1()
                    .min_w_0()
                    .text_color(palette.foreground)
                    .child(SelectableText::new((id.clone(), "value"), text.clone()));
                if looks_like_code(text) {
                    value.font_family(palette.mono.clone()).into_any_element()
                } else {
                    value.into_any_element()
                }
            }
            FieldValue::Block(text) => div()
                .flex_1()
                .min_w_0()
                .pl_2()
                .border_l_2()
                .border_color(palette.border)
                .font_family(palette.mono.clone())
                .text_color(palette.foreground)
                .child(SelectableText::new(
                    (id.clone(), "value"),
                    block_text(text, None),
                ))
                .into_any_element(),
        });

    match elided {
        // Cut to fit: the whole text is one hover away.
        Some(full) => row
            .tooltip(move |window, cx| Tooltip::new(full.clone()).max_w(px(520.)).build(window, cx))
            .test_support()
            .into_any_element(),
        None => row.test_support().into_any_element(),
    }
}

/// A lane report: its own block, one labeled field per non-empty part.
fn report_row(
    id: RowId,
    done: &str,
    evidence: &str,
    next: &str,
    blocked: &str,
    requests: &str,
    palette: &Palette,
) -> AnyElement {
    let fields = [
        ("done", done, palette.foreground),
        ("evidence", evidence, palette.muted_foreground),
        ("next", next, palette.foreground),
        ("blocked", blocked, palette.destructive),
        ("requests", requests, palette.primary),
    ];

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
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child("report"),
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
    palette: &Palette,
) -> AnyElement {
    let id = id.into();
    div()
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
        .child(caption(label, palette))
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
                .child(SelectableText::new(
                    (id.clone(), "body"),
                    block_text(text, total_chars),
                ))
                .test_support(),
        )
        .test_support()
        .into_any_element()
}

/// `text` as a block: its first [`BLOCK_LINES`] lines, or [`TOOL_TEXT_LIMIT`]
/// characters, and a note saying how much was left out. `total_chars` is what
/// the swarm said the result holds, when the copy that arrived was already
/// shortened.
pub(crate) fn block_text(text: &str, total_chars: Option<u64>) -> String {
    let shown = text.chars().count();
    let total = total_chars
        .map(|chars| chars as usize)
        .unwrap_or(shown)
        .max(shown);
    let lines: Vec<&str> = text.lines().collect();

    if lines.len() > BLOCK_LINES {
        let mut capped = lines[..BLOCK_LINES].join("\n");
        capped.push_str(&format!("\n… {} more lines", lines.len() - BLOCK_LINES));
        return capped;
    }
    if shown > TOOL_TEXT_LIMIT {
        let mut capped: String = text.chars().take(TOOL_TEXT_LIMIT).collect();
        capped.push_str(&format!("\n… truncated ({total} characters)"));
        return capped;
    }
    if total > shown {
        return format!("{text}\n… truncated ({total} characters)");
    }
    text.to_string()
}
