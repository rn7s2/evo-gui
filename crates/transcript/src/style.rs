//! The theme tokens, spacing and rich-text style the transcript draws with,
//! resolved once per render.

use gpui_kit::base::TextViewStyle;
use gpui_kit::component::ActiveTheme as _;

use gpui_kit::{
    px, rems, App, FontWeight, Hsla, Overflow, Pixels, SharedString, StyleRefinement, Styled as _,
};
use store::design;

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
#[derive(Clone)]
pub(crate) struct Palette {
    pub(crate) background: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) muted_foreground: Hsla,
    pub(crate) border: Hsla,
    pub(crate) primary: Hsla,
    pub(crate) destructive: Hsla,
    pub(crate) success: Hsla,
    /// The chrome surface a card's head is drawn on.
    pub(crate) sidebar: Hsla,
    /// The lightest surface: a card's body, an input.
    pub(crate) input: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) info: Hsla,
    pub(crate) mono: SharedString,
    /// The theme's body size: what a row measures itself against.
    pub(crate) font_size: Pixels,
    /// The size a tool's payload — its arguments, its result — is drawn at, and
    /// their line height: `Rows.css` sets both, `.tc-kv` and `.tc-result` alike
    /// (`font-size:13px; line-height:19px`).
    pub(crate) payload_size: Pixels,
    pub(crate) payload_line: Pixels,
    pub(crate) radius: Pixels,
}

/// `color-mix(in srgb, a pct%, b)`, the design's mixing rule, on two theme
/// colours. sRGB, channel by channel, with `pct` percent taken from `a`.
pub(crate) fn mix(a: Hsla, pct: f32, b: Hsla) -> Hsla {
    let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
    let t = (pct / 100.).clamp(0., 1.);
    gpui_kit::Rgba {
        r: a.r * t + b.r * (1. - t),
        g: a.g * t + b.g * (1. - t),
        b: a.b * t + b.b * (1. - t),
        a: a.a * t + b.a * (1. - t),
    }
    .into()
}

impl Palette {
    /// A card's hairline: `color-mix(in srgb, var(--fg) 17%, var(--bg))` — the
    /// rule a table's frame is drawn with, strong enough to read on the warm
    /// surface.
    pub(crate) fn rule(&self) -> Hsla {
        mix(self.foreground, 17., self.background)
    }

    /// The softer one inside a card: `color-mix(in srgb, var(--fg) 10%, var(--bg))`.
    pub(crate) fn rule_soft(&self) -> Hsla {
        mix(self.foreground, 10., self.background)
    }

    pub(crate) fn from_app(cx: &App) -> Self {
        let theme = cx.theme();
        let colors = theme.semantic_tokens().colors;
        let color = |token: store::design::Rgb| {
            let rgba = gpui_kit::Rgba {
                r: f32::from(token.r) / 255.,
                g: f32::from(token.g) / 255.,
                b: f32::from(token.b) / 255.,
                a: 1.,
            };
            Hsla::from(rgba)
        };
        Self {
            background: colors.background,
            foreground: colors.foreground,
            // The two surfaces the design's cards are drawn on. They come from
            // the design's own palette rather than the theme's, because the theme
            // carries no `input.background` token: a card's body is the lightest
            // surface in the design (`--input`), and a card's head the chrome one
            // (`--sidebar`).
            sidebar: color(design::palette(theme.is_dark()).sidebar),
            input: color(design::palette(theme.is_dark()).input),
            muted: colors.muted,
            muted_foreground: colors.muted_foreground,
            border: colors.border,
            primary: colors.primary,
            destructive: colors.destructive,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            mono: theme.mono_font_family.clone(),
            font_size: theme.font_size,
            // `Rows.css`: `.tc-kv{font-size:13px;line-height:19px}`, which is the
            // design's mono size (`--mono`, 13) and the line box it sets under it.
            payload_size: px(design::FONT_MONO),
            payload_line: px(19.),
            radius: theme.radius,
        }
    }
}

/// The markdown style of a chat message.
///
/// A transcript is not a document: headings step up from body text instead of
/// doubling it, paragraphs sit closer together, and a table that does not fit
/// the measure scrolls inside its own block rather than spilling out of the
/// column or squeezing its cells into unreadable columns.
///
/// Base exposes the node border used for table row rules; the component wrapper
/// does not. Preserve its theme colours explicitly, replacing only that border
/// with `--rule` (`Rows.css`).
pub(crate) fn text_style(cx: &App) -> TextViewStyle {
    let theme = cx.theme();
    let base = theme.font_size;
    let palette = Palette::from_app(cx);
    let rule = palette.rule();
    let rule_soft = palette.rule_soft();

    // `Rows.css`'s `.measure table`: a rounded frame on the lightest surface, a
    // tinted header row, and rules strong enough to read against the warm editor.
    // Horizontal scroll rather than squeezing: a table that does not fit reads in
    // its own frame instead of in unreadable columns.
    // Put body type on the table: the kit nests every cell inside its row, so
    // declaring it on `table_cell` would override the header's size and weight.
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);
    let table = table
        .border_1()
        .border_color(rule)
        .rounded(px(8.))
        .bg(palette.input)
        .text_size(px(13.5))
        .font_weight(FontWeight::NORMAL);

    // The header row: the chrome surface, the second voice, and a touch smaller
    // than the cells under it. `Rows.css`'s `.measure th{…font-weight:500}` — the
    // design's medium, which this stack draws with `widgets::text::MEDIUM`.
    let table_head = StyleRefinement::default()
        .bg(palette.sidebar)
        .text_color(palette.muted_foreground)
        .text_size(px(12.5))
        .font_weight(widgets::text::MEDIUM);

    // Leave border widths to the kit: it omits the last column's right rule and
    // the last row's bottom rule, as Rows.css requires. Only override the cell's
    // rule colour; a width here would add a second line inside the outer frame.
    // The table frame and row bottoms retain their separate `--rule` colour.
    //
    // The padding is what gpui-base measures a column's floor with (16px plus the
    // border), so a wider padding than its assumption lets a column shrink under
    // its longest word and break it mid-word; 12px a side keeps a half-cell of
    // slack, and a cell still has air around it.
    let table_cell = StyleRefinement::default()
        .px(px(12.))
        .py(px(7.))
        .border_color(rule_soft);

    // A fenced block: the muted surface, 10px 12px of padding, and the design's
    // 10px above and below.
    let code_block = StyleRefinement::default()
        .my(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(palette.radius)
        .bg(palette.muted);

    // Inline code: the mono face at the mono size, on the muted surface.
    let inline_code = gpui_kit::HighlightStyle {
        background_color: Some(palette.muted),
        ..Default::default()
    };

    // The colours the component fold used to take from the theme, so the only
    // thing this seam changes is the rule ink: `theme.foreground` and the rest are
    // the same tokens (`Theme: Deref<Target = ThemeColor>`), passed explicitly
    // because `TextViewStyle::default()` is the neutral light palette and would
    // otherwise win.
    TextViewStyle::default()
        .with_foreground(theme.foreground)
        .with_muted_foreground(theme.muted_foreground)
        .with_link(theme.link)
        .with_selection(theme.selection)
        // In a fenced block's surface (`--muted`).
        .with_code_background(theme.muted)
        // The one this seam exists for: `--rule`, not the widget border the
        // theme hands the base style (`node.rs` draws the row rules, the frame,
        // a blockquote's rule and `hr` in it).
        .with_border(rule)
        // `p { margin: 10px 0 }`
        .with_paragraph_gap(rems(0.625))
        // Headings step up from the body size rather than doubling it.
        .with_heading(move |level| {
            let scale = match level {
                1 => 1.25,
                2 => 1.1,
                3 => 1.05,
                _ => 1.,
            };
            StyleRefinement::default().text_size(px(f32::from(base) * scale))
        })
        .with_table(table)
        .with_table_head(table_head)
        .with_table_cell(table_cell)
        .with_code_block(code_block)
        .with_inline_code(inline_code)
        // Dark-mode assets (a task list's tick) follow the theme's appearance.
        .with_dark(theme.is_dark())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::base::{TextView, TextViewState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::App;
    use gpui_kit::{
        div, point, size, AppContext as _, Bounds, Context, Entity, IntoElement,
        ParentElement as _, Quad, Render, TestAppContext, Window, WindowBounds, WindowOptions,
    };

    /// Every cell is inside its row; a cell's type would shadow the header's.
    #[gpui_kit::test]
    fn the_head_wears_the_designs_medium_and_nothing_below_it_shadows_that(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let style = cx.update(|cx: &mut App| text_style(cx));

        // The body: `Rows.css`'s `.measure table{font-size:13.5px}`, at the plain weight.
        assert_eq!(style.table().text.font_size, Some(px(13.5).into()));
        assert_eq!(style.table().text.font_weight, Some(FontWeight::NORMAL));

        // The head: `.measure th{font-weight:500;font-size:12.5px}`, the medium being
        // the one face this font stack can draw for the 500 ([`widgets::text::MEDIUM`]).
        assert_eq!(style.table_head().text.font_size, Some(px(12.5).into()));
        assert_eq!(
            style.table_head().text.font_weight,
            Some(widgets::text::MEDIUM),
            "`Rows.css`: `.measure th{{font-weight:500}}`"
        );

        // The cell: neither channel, so neither can shadow the row above it.
        assert_eq!(
            style.table_cell().text.font_size,
            None,
            "the body's size is the table's; a cell that repeats it wins over the head"
        );
        assert_eq!(
            style.table_cell().text.font_weight,
            None,
            "the body's weight is the table's; a cell that repeats it wins over the head"
        );

        // Model the kit's inherited text style: the nearest declaration wins.
        let head_size = style
            .table_cell()
            .text
            .font_size
            .or(style.table_head().text.font_size)
            .or(style.table().text.font_size);
        assert_eq!(
            head_size,
            Some(px(12.5).into()),
            "the head's own size reaches its text"
        );
        let head_weight = style
            .table_cell()
            .text
            .font_weight
            .or(style.table_head().text.font_weight)
            .or(style.table().text.font_weight);
        assert_eq!(
            head_weight,
            Some(widgets::text::MEDIUM),
            "and so does its medium"
        );

        // A body cell's text, the same way: the table's type, nothing under it.
        let body_size = style
            .table_cell()
            .text
            .font_size
            .or(style.table().text.font_size);
        assert_eq!(body_size, Some(px(13.5).into()));
        let body_weight = style
            .table_cell()
            .text
            .font_weight
            .or(style.table().text.font_weight);
        assert_eq!(body_weight, Some(FontWeight::NORMAL));
    }
    /// The table the scene test draws: a header and two body rows, two columns.
    ///
    /// Two columns is what makes the count readable: each row's first cell is one the
    /// kit gives a right rule, and its second is the last column, which it leaves bare.
    const TABLE: &str = "| column | what it holds |\n\
                         | --- | --- |\n\
                         | first | a row of the table |\n\
                         | second | another one |\n";

    /// One markdown document, styled the way the transcript styles a message.
    struct TableHost {
        document: Entity<TextViewState>,
    }

    impl Render for TableHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(760.)).child(
                TextView::new(&self.document)
                    .style(text_style(cx))
                    .markdown_extensions(crate::markdown::extensions()),
            )
        }
    }

    /// Cell refinements must preserve the kit's positional border widths.
    #[gpui_kit::test]
    fn the_soft_right_rule_stays_off_the_last_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (style, rule, soft) = cx.update(|cx: &mut App| {
            let palette = Palette::from_app(cx);
            (text_style(cx), palette.rule(), palette.rule_soft())
        });

        // What the seam declares: the interior rule's ink, and no width of its own.
        assert_eq!(
            style.table_cell().border_color,
            Some(soft),
            "the interior rule is `--rule-soft` (`Rows.css`: `th,td{{border-right:1px solid var(--rule-soft)}}`)"
        );
        assert_eq!(
            style.table_cell().border_widths.right,
            None,
            "the width is the kit's positional one; a width here is on the last column too"
        );

        // What that paints: the scene's own border quads.
        let document = cx.update(|cx| cx.new(|cx| TextViewState::markdown(TABLE, cx)));
        let (window, _host) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(760.), px(420.)),
                })),
                ..Default::default()
            };
            let document = document.clone();
            gpui_kit::open_window(options, cx, |_window, cx| {
                cx.new(|_cx| TableHost {
                    document: document.clone(),
                })
            })
            .expect("the table window")
        });
        let quads = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.painted_quads()
            })
            .expect("the table window");

        let has_right_rule = |quad: &Quad| quad.border_widths.right.as_f32() > 0.;
        // A window's `painted_quads` carries every paint it has done, so the same
        // rule arrives several times at the same bounds: the count below is of the
        // rules themselves, not of the paints.
        let mut painted: Vec<(f32, f32, f32, f32)> = Vec::new();
        let mut soft_rules: Vec<&Quad> = Vec::new();
        for quad in quads
            .iter()
            .filter(|quad| has_right_rule(quad) && quad.border_color == soft)
        {
            let key = (
                quad.bounds.origin.x.as_f32(),
                quad.bounds.origin.y.as_f32(),
                quad.bounds.size.width.as_f32(),
                quad.bounds.size.height.as_f32(),
            );
            if painted.contains(&key) {
                continue;
            }
            painted.push(key);
            soft_rules.push(quad);
        }
        assert_eq!(
            soft_rules.len(),
            3,
            "one interior rule per row — a header and two body rows; six means the \
             last column was given one too"
        );

        let right = |quad: &Quad| quad.bounds.origin.x.as_f32() + quad.bounds.size.width.as_f32();
        let frame = quads
            .iter()
            .filter(|quad| has_right_rule(quad) && quad.border_color == rule)
            .max_by(|a, b| right(a).total_cmp(&right(b)))
            .expect("the table's frame, in `--rule`");
        let frame_inner = right(frame) - frame.border_widths.right.as_f32();
        assert!(
            soft_rules
                .iter()
                .all(|interior| right(interior) <= frame_inner - 1.),
            "every interior rule stops short of the frame's inner edge; a rule ending \
             on it is the last column's, drawn inside the frame"
        );
    }
}
