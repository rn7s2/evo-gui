//! Row renderers: one function per [`RowKind`], all of them theme-tokened text.
//!
//! Every row is centred inside the shared reading measure and carries its own
//! leading space, so the list can mount rows edge to edge and the transcript
//! still reads as turns: tight inside a group of tool calls or dim lines, a
//! little apart between the parts of one turn, and separated by a hairline when
//! a new turn begins.
//!
//! Nothing here prints protocol payloads: a row is a user turn, rendered
//! markdown, a one-line tool row that can open, a report block, or a dim line.

use std::time::Duration;

use gpui_kit::base::Easing;
use gpui_kit::component::text::{TextView, TextViewMotion};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, px, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled as _, WeakEntity,
};
use session::{DimStyle, Row, RowId, RowKind, ToolResult};

use crate::style::{text_style, Palette, BLOCK_GAP, GROUP_GAP, MEASURE, TIGHT_GAP, TURN_GAP};
use crate::{TranscriptData, TranscriptView};

/// Fade window for text appended by a streaming delta (§2.8).
const STREAM_FADE: Duration = Duration::from_millis(350);
/// A little later for each further word of one delta, so chunks overlap into
/// one gradient tail instead of blinking in.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(30);
/// Longest tool argument or result text an expanded row shows.
const TOOL_TEXT_LIMIT: usize = 4_000;
/// Height of a collapsed tool row, so a long run of them stays a list.
const TOOL_ROW_HEIGHT: Pixels = px(24.);
/// Width of the disclosure and status columns of a tool row.
const DISCLOSURE_WIDTH: Pixels = px(10.);
const STATUS_DOT: Pixels = px(6.);
/// Width of the label column of a report row.
const REPORT_LABEL_WIDTH: Pixels = px(66.);

/// The motion a streaming assistant row is rendered with.
pub(crate) fn stream_motion() -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(STREAM_FADE)
        .with_stream_fade_stagger(STREAM_FADE_STAGGER)
        .with_stream_fade_easing(Easing::EaseOut)
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
            RowKind::Dim { .. } => Self::Dim,
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
    data: &TranscriptData,
    index: usize,
    view: &WeakEntity<TranscriptView>,
    cx: &App,
) -> AnyElement {
    let Some(row) = data.rows.get(index) else {
        return div().into_any_element();
    };
    let palette = Palette::from_app(cx);
    let previous = index.checked_sub(1).and_then(|index| data.rows.get(index));

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
        .child(text.to_string())
        .test_support()
        .into_any_element()
}

/// An assistant message: the retained markdown document, its optional thinking
/// text, and the error that ended it, if any.
fn assistant_row(
    id: RowId,
    markdown: &str,
    thinking: &str,
    error: Option<&str>,
    data: &TranscriptData,
    cx: &App,
    palette: &Palette,
) -> AnyElement {
    let mut row = div()
        .id(("transcript-assistant", id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2();

    row = match data.documents.get(&id) {
        // The retained document: never recreated per delta, `set_text` extends it.
        Some(document) => row.child(
            TextView::new(document)
                .style(text_style(cx))
                .motion(stream_motion()),
        ),
        // Unreachable while every assistant row gets a document on the way in;
        // fall back to the source rather than dropping the message.
        None => row.child(
            div()
                .text_color(palette.muted_foreground)
                .child(markdown.to_string()),
        ),
    };

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
                        .child(thinking.to_string()),
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

/// A tool call: one compact line — `name · ok|error|running` — that opens onto
/// the truncated arguments and result.
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
        .child(
            div()
                .w(DISCLOSURE_WIDTH)
                .flex_shrink_0()
                .text_color(palette.muted_foreground)
                .child(if expanded { "▾" } else { "▸" }),
        )
        .child(
            div()
                .min_w_0()
                .font_family(palette.mono.clone())
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
                .text_xs()
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

/// The arguments of an open tool row, when the call had any.
fn arguments_block(id: RowId, arguments: &str, palette: &Palette) -> AnyElement {
    if arguments.is_empty() {
        return div().into_any_element();
    }
    text_block(
        ("transcript-tool-arguments", id),
        "arguments",
        arguments,
        None,
        palette,
    )
}

/// The result of an open tool row, when it has one and it says anything.
fn result_block(id: RowId, result: Option<&ToolResult>, palette: &Palette) -> AnyElement {
    let Some(result) = result.filter(|result| !result.content.is_empty()) else {
        return div().into_any_element();
    };
    text_block(
        ("transcript-tool-result", id),
        if result.is_error { "error" } else { "result" },
        &result.content,
        result.content_chars,
        palette,
    )
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
        .bg(palette.muted)
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
                .child(div().min_w_0().text_color(color).child(value.to_string())),
        );
    }

    row.test_support().into_any_element()
}

/// An `output` line or status event: one dim line, readable but out of the way.
fn dim_row(id: RowId, style: DimStyle, text: &str, palette: &Palette) -> AnyElement {
    let color = match style {
        DimStyle::Dim | DimStyle::Status => palette.muted_foreground,
        DimStyle::Notice => palette.info,
        DimStyle::Error => palette.destructive,
    };

    div()
        .id(("transcript-dim", id))
        .w_full()
        .min_w_0()
        .text_sm()
        .line_height(px(18.))
        .text_color(color)
        .child(text.to_string())
        .test_support()
        .into_any_element()
}

/// A labeled, bordered block of tool text, truncated to keep the transcript
/// readable.
fn text_block(
    id: impl Into<gpui_kit::ElementId>,
    label: &str,
    text: &str,
    total_chars: Option<u64>,
    palette: &Palette,
) -> AnyElement {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_1()
        .border_color(palette.border)
        .px_2()
        .py_1()
        .text_sm()
        .child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(label.to_string()),
        )
        .child(
            div()
                .min_w_0()
                .font_family(palette.mono.clone())
                .text_color(palette.foreground)
                .child(truncate(text, total_chars)),
        )
        .test_support()
        .into_any_element()
}

/// `text`, capped at [`TOOL_TEXT_LIMIT`] characters with a note saying how much
/// was left out.
fn truncate(text: &str, total_chars: Option<u64>) -> String {
    let total = total_chars
        .map(|chars| chars as usize)
        .unwrap_or_else(|| text.chars().count());
    if total <= TOOL_TEXT_LIMIT && text.chars().count() <= TOOL_TEXT_LIMIT {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(TOOL_TEXT_LIMIT).collect();
    truncated.push_str(&format!("\n… truncated ({total} characters)"));
    truncated
}
