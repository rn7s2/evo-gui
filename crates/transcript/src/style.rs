//! The theme tokens, spacing and rich-text style the transcript draws with,
//! resolved once per render.

use gpui_kit::component::text::TextViewStyle;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{px, rems, App, Hsla, Overflow, Pixels, SharedString, StyleRefinement};

/// The widest a row's content gets.
///
/// A column wider than this centres the same measure instead of letting a line
/// run the full width of a maximised window; tables and code blocks inside a
/// message share it, so the whole transcript reads as one column.
pub(crate) const MEASURE: f32 = 800.;

/// Space before a row, decided by what came before it. Consecutive rows of one
/// kind (tool calls, dim lines) are tight, the parts of one turn are a little
/// apart, and turns are further apart still.
pub(crate) const TURN_GAP: Pixels = px(26.);
pub(crate) const BLOCK_GAP: Pixels = px(10.);
pub(crate) const GROUP_GAP: Pixels = px(6.);
pub(crate) const TIGHT_GAP: Pixels = px(3.);

/// Theme colors, fonts and radii used by transcript rows and the todo panel.
///
/// Semantic tokens cover the surfaces and text roles; the status colors an ok /
/// warn / error row needs are still only on the legacy component palette, so
/// those come from there.
pub(crate) struct Palette {
    pub(crate) foreground: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) muted_foreground: Hsla,
    pub(crate) border: Hsla,
    pub(crate) primary: Hsla,
    pub(crate) destructive: Hsla,
    pub(crate) success: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) info: Hsla,
    pub(crate) mono: SharedString,
    pub(crate) radius: Pixels,
    pub(crate) radius_lg: Pixels,
}

impl Palette {
    pub(crate) fn from_app(cx: &App) -> Self {
        let theme = cx.theme();
        let colors = theme.semantic_tokens().colors;
        Self {
            foreground: colors.foreground,
            muted: colors.muted,
            muted_foreground: colors.muted_foreground,
            border: colors.border,
            primary: colors.primary,
            destructive: colors.destructive,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            mono: theme.mono_font_family.clone(),
            radius: theme.radius,
            radius_lg: theme.radius_lg,
        }
    }
}

/// The markdown style of a chat message.
///
/// A transcript is not a document: headings step up from body text instead of
/// doubling it, paragraphs sit closer together, and a table that does not fit
/// the measure scrolls inside its own block rather than spilling out of the
/// column or squeezing its cells into unreadable columns.
pub(crate) fn text_style(cx: &App) -> TextViewStyle {
    let base = cx.theme().font_size;
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);

    let mut style = TextViewStyle::default();
    style.paragraph_gap = rems(0.5);
    style.heading_base_font_size = base;
    style.heading_font_size = Some(std::sync::Arc::new(|level, base| {
        let scale = match level {
            1 => 1.25,
            2 => 1.1,
            3 => 1.05,
            _ => 1.,
        };
        px(f32::from(base) * scale)
    }));
    style.table = table;
    style
}
