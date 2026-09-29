//! Row renderers: one function per [`RowKind`], all of them theme-tokened text.
//!
//! Nothing here prints protocol payloads: a row is a user turn, rendered
//! markdown, a one-line tool row that can open, a report block, or a dim line.

use gpui_kit::base::{Easing, TextView, TextViewMotion};
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, WeakEntity,
};
use session::{DimStyle, RowId, RowKind, ToolResult};

use crate::style::Palette;
use crate::{TranscriptData, TranscriptView};

/// Fade window for text appended by a streaming delta (§2.8).
const STREAM_FADE: std::time::Duration = std::time::Duration::from_millis(350);
/// A little later for each further word of one delta, so chunks overlap into
/// one gradient tail instead of blinking in.
const STREAM_FADE_STAGGER: std::time::Duration = std::time::Duration::from_millis(30);
/// Longest tool argument or result text an expanded row shows.
const TOOL_TEXT_LIMIT: usize = 4_000;

/// The motion a streaming assistant row is rendered with.
pub(crate) fn stream_motion() -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(STREAM_FADE)
        .with_stream_fade_stagger(STREAM_FADE_STAGGER)
        .with_stream_fade_easing(Easing::EaseOut)
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

    match &row.kind {
        RowKind::User { text, .. } => user_row(row.id, text, &palette),
        RowKind::Assistant {
            markdown,
            thinking,
            error,
            ..
        } => assistant_row(row.id, markdown, thinking, error.as_deref(), data, &palette),
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
    }
}

/// A user turn: plain text (never a markdown document) on a muted surface.
fn user_row(id: RowId, text: &str, palette: &Palette) -> AnyElement {
    div()
        .id(("transcript-row", id))
        .w_full()
        .min_w_0()
        .rounded(palette.radius)
        .bg(palette.muted)
        .border_1()
        .border_color(palette.border)
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
    palette: &Palette,
) -> AnyElement {
    let mut row = div()
        .id(("transcript-row", id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2();

    row = match data.documents.get(&id) {
        // The retained document: never recreated per delta, `set_text` extends it.
        Some(document) => row.child(TextView::new(document).motion(stream_motion())),
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

/// A tool call: one line — `name — ok|error|running` — that opens onto the
/// truncated arguments and result.
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
        None => ("running".to_string(), palette.warning),
        Some(result) if result.is_error => ("error".to_string(), palette.destructive),
        Some(_) => ("ok".to_string(), palette.success),
    };

    let view = view.clone();
    let header = div()
        .id(("transcript-tool", id))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            let _ = view.update(cx, |view, cx| view.toggle_expanded(id, cx));
        })
        .child(
            div()
                .text_color(palette.muted_foreground)
                .child(if expanded { "▾" } else { "▸" }),
        )
        .child(div().text_color(palette.foreground).child(name.to_string()))
        .child(div().text_color(palette.muted_foreground).child("—"))
        .child(div().text_color(status_color).child(status))
        .test_support();

    let mut row = div()
        .id(("transcript-row", id))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(header);

    if expanded {
        if !arguments.is_empty() {
            row = row.child(text_block(
                ("transcript-tool-arguments", id),
                "arguments",
                arguments,
                None,
                palette,
            ));
        }
        if let Some(result) = result.filter(|result| !result.content.is_empty()) {
            row = row.child(text_block(
                ("transcript-tool-result", id),
                if result.is_error { "error" } else { "result" },
                &result.content,
                result.content_chars,
                palette,
            ));
        }
    }

    row.test_support().into_any_element()
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
        .id(("transcript-row", id))
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
                        .w_24()
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
        .id(("transcript-row", id))
        .w_full()
        .min_w_0()
        .text_sm()
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
