//! TeX math in a transcript: `$…$` and `$$…$$` drawn as pictures.
//!
//! gpui's Markdown parser already mints a math node for both forms — the two
//! plugins below only have to draw them. RaTeX reads the formula, lays it out and
//! emits a self-contained SVG whose glyphs are KaTeX outlines, so nothing is
//! fetched and no system font is needed; the transcript draws it as an image.
//!
//! A node is **claimed** whatever its formula is — the only span left to the kit
//! is one that is not `$…$` the way TeX means it (a space against a delimiter),
//! which is a sentence mentioning a dollar amount rather than a formula. And a
//! claimed node carries the source the model wrote, delimiters and all: it is
//! what the kit draws when no picture can be made (so a formula this engine
//! cannot read — or one holding characters outside the embedded faces, which
//! would send RaTeX looking for a system font — reads exactly as it was
//! written), what a copy yields, and what the kit would otherwise have turned a
//! `$$…$$` into a code block without its delimiters.
//! Nothing here ever draws a blank row or panics: a failure is a `None`, and a
//! `None` is the source.
//!
//! What a `None` covers is *decided before the work happens* ([`draw`]): a formula
//! past [`MAX_BODY_BYTES`], one wider than its own rendering is willing to draw
//! ([`INLINE_MAX_EM`], [`DISPLAY_MAX_EM`]), one naming a character the embedded
//! faces need not carry (`\char`), and one whose picture came back with anything
//! but outlines in it. A picture nobody would draw is then never laid out, never
//! serialized and never cached — the reader gets the source at the cost of a
//! length check.
//!
//! Pictures are cached process-wide, bounded and keyed by the formula, the style,
//! the type size and the ink — a theme or a resize is another key, never a leak.
//! **Failures are cached too**: a formula that cannot be drawn is not parsed again
//! on every frame of a stream that keeps re-parsing its message.
//!
//! Laying a formula out is CPU work, and it never runs on the thread that draws.
//! The frame that first meets a formula claims it, asks the app's background
//! executor for the picture and draws the source meanwhile; the picture lands in
//! that same bounded cache under that same key, and the windows are asked to draw
//! again. A frame that comes in between finds the claim pending and asks for
//! nothing more — a formula is laid out once, however much the message that holds
//! it is re-parsed — and a task that outlives its window just fills a cache entry
//! nobody reads. The bound is kept by never letting a *claim* go: a full cache of
//! formulas still being laid out is full, and a reader waiting on one of them is
//! drawn the source until one lands (and the paint that landing asks for) rather
//! than laying the same formula out twice.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::component::text::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin,
};
use gpui_kit::{
    div, img, px, AnyElement, App, AsyncApp, ElementId, Hsla, Image, ImageFormat, Img,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};

use ratex_layout::{layout, to_display_list, LayoutOptions};
use ratex_parser::parse as parse_tex;
use ratex_svg::{render_to_svg, SvgOptions};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;
use store::design::{INSET, MEASURE};

use crate::style::Palette;

/// The node names the two plugins claim.
const INLINE: &str = "transcript-math";
const DISPLAY: &str = "transcript-math-block";

/// Padding around a picture, as a fraction of the type size: enough that ink
/// outside the layout box — a radical's hook, an accent — is not clipped, and
/// small enough not to open a gap in a line of prose.
const PAD_EM: f64 = 0.08;

/// Stroke width for the rules RaTeX draws (fraction bars, radicals), in em.
const STROKE_EM: f64 = 0.04;

/// The widest formula an **inline** row draws before it shows its source
/// instead. An inline picture is one atomic object that cannot scroll, so one
/// wider than this would simply be clipped by the column. Applied in [`draw`],
/// before the picture is made, so a formula past it costs a length check.
///
/// The number is the *narrowest* column the app has: the conversation pane never
/// goes below `store::app_state::CENTER_MIN` (420px), which leaves about 388px of
/// measure, and the type size the transcript draws at is 16px — so 24em is 384px,
/// and anything an inline line is allowed to draw fits the pane it is in.
const INLINE_MAX_EM: f64 = 24.;

/// The same bound for a **display** formula, which does scroll — past this a
/// picture is not worth rasterizing, and the source is easier to read anyway.
/// Applied in [`draw`], before the picture is made.
const DISPLAY_MAX_EM: f64 = 200.;

/// The most source [`draw`] will read, in bytes. A real formula is tens of bytes;
/// this is the line between one and a pasted kilobyte of TeX, whose layout — and
/// whose SVG — grows far faster than the text does. Past it the reader keeps the
/// source and nothing is parsed.
const MAX_BODY_BYTES: usize = 8192;

/// How many pictures and failures are kept. Small on purpose: a reader looks at
/// a handful of formulas, and the ones they scroll past are cheap to draw again.
const CACHED: usize = 128;

/// What one claimed node is: the formula, and whether it is written as a display
/// formula.
///
/// `$$…$$` is a display formula wherever it is written — markdown-rs mints an
/// *inline* math node for the one-line spelling and a flow node for the
/// fence-on-its-own-line one — so the carried flag is what tells the two
/// renderings apart, not which parser saw it.
#[derive(Clone, PartialEq, Eq)]
struct Formula {
    body: String,
    display: bool,
}

/// One formula, drawn: the image the transcript draws and the geometry it needs to
/// place it — all in logical pixels.
///
/// The image is built once, on the background executor, and shared by every frame
/// that draws the picture: gpui knows an image by the content of its bytes, so the
/// same `Image` is the same entry in its own cache — and no frame pays for a copy
/// of an SVG it has already been drawn from.
struct Picture {
    image: Arc<Image>,
    width: f32,
    height: f32,
    /// Distance from the picture's top edge to the math baseline.
    baseline: f32,
}

/// What drawing a formula came to. A failure is kept as plainly as a picture, and
/// a claim that is still being drawn is kept too: between them they say that a
/// formula has been asked for and must not be asked for again.
#[derive(Clone)]
enum Entry {
    Picture(Arc<Picture>),
    Failed,
    Pending,
}

/// What a lookup says to do about a formula.
enum Lookup {
    /// Already drawn.
    Ready(Arc<Picture>),
    /// Claimed already — on its way, or impossible. Either way the reader has the
    /// source until it lands, and no second job is started.
    Wait,
    /// Nothing is known about it: claim it, then ask for it.
    Draw,
}

/// What a picture is a picture *of*: the formula, the style, the type size it was
/// drawn at (in quarter pixels) and the ink.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    body: String,
    display: bool,
    size: u32,
    ink: u32,
}

/// The bounded, first-in-first-out cache of drawn formulas.
#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    order: VecDeque<Key>,
}

impl Cache {
    fn get(&mut self, key: &Key) -> Option<Entry> {
        self.entries.get(key).cloned()
    }

    /// Make room for one more entry, if there is any to make.
    ///
    /// The oldest entry is let go — but only a *settled* one. A claim whose job is
    /// still running is the answer to a formula somebody is looking at, and letting
    /// it go would only start that formula over; a cache that is nothing but claims
    /// is therefore full, and says so.
    fn make_room(&mut self) -> bool {
        if self.entries.len() < CACHED {
            return true;
        }
        let settled = self
            .order
            .iter()
            .position(|key| !matches!(self.entries.get(key), Some(Entry::Pending)));
        let Some(at) = settled else {
            return false;
        };
        if let Some(key) = self.order.remove(at) {
            self.entries.remove(&key);
        }
        true
    }

    /// Claim `key` as one being drawn: only ever called with room made for it, so
    /// the bound holds whatever becomes of the job.
    fn claim(&mut self, key: Key) {
        self.order.push_back(key.clone());
        self.entries.insert(key, Entry::Pending);
    }
}

static PICTURES: OnceLock<Mutex<Cache>> = OnceLock::new();

fn cache() -> &'static Mutex<Cache> {
    PICTURES.get_or_init(|| Mutex::new(Cache::default()))
}

/// A formula's picture at this type size and ink: the one already drawn, or —
/// the first time a formula is met — a request to the app's background executor,
/// while this frame draws the source. `None` means "not a picture yet", never
/// "nothing to draw": the callers turn it into the source the model wrote.
///
/// The claim is taken before the job is started and under the one lock, so a
/// frame arriving while the first is still running finds [`Lookup::Wait`] and
/// starts nothing: a formula is laid out once per key, however many frames look
/// at it, and a message that is re-parsed as it streams does not lay it out
/// again either.
fn picture(body: &str, display: bool, size: f32, ink: u32, cx: &mut App) -> Option<Arc<Picture>> {
    let key = Key {
        body: body.to_string(),
        display,
        size: (size * 4.).round().max(1.) as u32,
        ink,
    };
    // The lock is let go before anything is spawned: the job takes it back to
    // settle, and a render is no place to hold it.
    let decision = {
        let mut cache = cache().lock().ok()?;
        lookup(&mut cache, &key)
    };
    match decision {
        Lookup::Ready(picture) => Some(picture),
        Lookup::Wait => None,
        Lookup::Draw => {
            let body = body.to_string();
            let size = f64::from(size);
            cx.spawn(async move |cx: &mut AsyncApp| {
                // Not on this thread: the foreground future parks while the app's
                // executor lays the formula out.
                let drawn = cx
                    .background_executor()
                    .spawn(async move { draw(&body, display, size, ink) })
                    .await;
                if let Ok(mut cache) = cache().lock() {
                    settle(&mut cache, &key, drawn);
                }
                cx.refresh();
            })
            .detach();
            None
        }
    }
}

/// The one decision the cache makes: a picture, a wait, or a claim to make.
///
/// A claim is only made where there is room for one ([`Cache::make_room`]): a full
/// cache of formulas still being laid out asks for nothing new, and the reader is
/// drawn the source until one of them lands and the paint that landing asks for
/// comes round again. So no formula is ever laid out twice, and the bound holds
/// however many formulas a message holds.
fn lookup(cache: &mut Cache, key: &Key) -> Lookup {
    match cache.get(key) {
        Some(Entry::Picture(picture)) => Lookup::Ready(picture),
        Some(Entry::Failed) | Some(Entry::Pending) => Lookup::Wait,
        None if cache.make_room() => {
            cache.claim(key.clone());
            Lookup::Draw
        }
        None => Lookup::Wait,
    }
}

/// A drawing came back — a picture or a failure — and every window is asked to
/// draw again. The entry keeps its place in the bounded cache: a formula that
/// cannot be drawn is not attempted on the next frame, nor on the next re-parse.
///
/// The answer belongs to the *claim* it was asked for: a key that is no longer
/// pending — let go, or claimed again by a job of its own — is left alone rather
/// than put back into a bounded cache it was taken out of.
fn settle(cache: &mut Cache, key: &Key, drawn: Option<Picture>) {
    if !matches!(cache.entries.get(key), Some(Entry::Pending)) {
        return;
    }
    cache.entries.insert(
        key.clone(),
        match drawn {
            Some(picture) => Entry::Picture(Arc::new(picture)),
            None => Entry::Failed,
        },
    );
}

/// Read, lay out and serialize one formula. `None` whenever this engine cannot
/// make a picture of it — an unreadable formula, an empty one, a size that makes
/// no sense, a number that is not finite — and never a panic.
///
/// Everything a picture could be refused for is decided **here, before the work**:
/// the length of the source, the characters it names, and how wide the layout came
/// out. A formula that would be drawn only to be thrown away — one wider than its
/// own rendering draws, or one whose picture would not be the same picture on
/// another machine — is refused while its cost is a comparison, and the reader gets
/// the source.
fn draw(body: &str, display: bool, size: f64, ink: u32) -> Option<Picture> {
    if body.trim().is_empty()
        || body.len() > MAX_BODY_BYTES
        || !body.is_ascii()
        || !(0.5..=1024.).contains(&size)
        // A formula holding characters outside the embedded KaTeX faces is refused
        // too: drawing one would send the library looking for a system font, which
        // prints to the app's own stderr and makes the picture depend on the machine.
        // Main math — the ASCII TeX a model writes — is what is drawn.
        || asks_for_a_character_by_number(body)
    {
        return None;
    }
    let nodes = parse_tex(body).ok()?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let options = LayoutOptions::default()
        .with_style(style)
        .with_color(color_of(ink));
    let list = to_display_list(&layout(&nodes, &options));
    if !(list.width.is_finite() && list.height.is_finite() && list.depth.is_finite()) {
        return None;
    }
    let pad = size * PAD_EM;
    let width = list.width * size + 2. * pad;
    let height = (list.height + list.depth) * size + 2. * pad;
    // The cap the renderer would have applied after the fact, applied here: past it
    // the picture is not worth rasterizing, and serializing it is what would be
    // wasted — an SVG grows far faster than the formula that made it.
    let cap = if display {
        DISPLAY_MAX_EM
    } else {
        INLINE_MAX_EM
    } * size;
    if width > cap {
        return None;
    }
    let svg = render_to_svg(
        &list,
        &SvgOptions {
            font_size: size,
            padding: pad,
            stroke_width: (size * STROKE_EM).max(0.8),
            // Glyph outlines, not `<text>`: the picture carries its own fonts.
            embed_glyphs: true,
            font_dir: String::new(),
        },
    );
    // Belt and braces for the faces: a picture with `<text>` in it would lean on a
    // font this machine has to have, and one with `<image>` in it carries a raster
    // of a system font — neither is the same picture everywhere.
    if !is_glyph_outlines(&svg) {
        return None;
    }
    Some(Picture {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width: width.max(1.) as f32,
        height: height.max(1.) as f32,
        baseline: (list.height * size + pad) as f32,
    })
}

/// Whether a picture is glyph outlines and nothing else: the shape a formula has to
/// take to be the same picture on every machine, and the reason no font has to be
/// installed for one to be drawn.
fn is_glyph_outlines(svg: &str) -> bool {
    !svg.contains("<text") && !svg.contains("<image")
}

/// Whether the source names a character by number, with TeX's `\char` — the way a
/// formula written entirely in ASCII still asks for a glyph the embedded faces need
/// not carry, which is what sends the engine to a font on this machine instead.
///
/// A TeX command is a backslash and the letters that follow it, so this reads the
/// command's own name rather than looking for the five characters: `\\char` (a
/// literal backslash, then the word) and `\charset` are other things, and a formula
/// that merely spells `char` is only refused when it spells it as the command.
fn asks_for_a_character_by_number(source: &str) -> bool {
    let mut rest = 0;
    while let Some(offset) = source[rest..].find('\\') {
        let name = rest + offset + 1;
        let Some(after) = source.get(name..) else {
            break;
        };
        let Some(first) = after.chars().next() else {
            break;
        };
        if first.is_ascii_alphabetic() {
            // A control word: the letters that follow the backslash, and no more.
            let end = after
                .find(|ch: char| !ch.is_ascii_alphabetic())
                .unwrap_or(after.len());
            if &after[..end] == "char" {
                return true;
            }
            rest = name + end;
        } else {
            // A control symbol — `\\` is the line break, `\,` a thin space: the
            // backslash and the one character it names, so the letters after them
            // are the text's own and not a command.
            rest = name + first.len_utf8();
        }
    }
    false
}

/// The image element for a picture. Sized in logical pixels rather than by what
/// the SVG says its size is, so the transcript's own type size decides.
fn picture_image(picture: &Picture) -> Img {
    img(picture.image.clone())
        .w(px(picture.width))
        .h(px(picture.height))
        .flex_none()
}

/// `$…$`: one atomic picture inside the line, sitting on the text's baseline.
pub(crate) struct Inline;

impl MarkdownPlugin for Inline {
    fn name(&self) -> &str {
        INLINE
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else {
            return None;
        };
        // An alias — `\(…\)` or `\[…\]` — is handed over inside the dollars the
        // source pass wrapped it in: the body is the LaTeX, and the alias itself is
        // what the model wrote.
        let alias = alias(&math.value);
        // The TeX rule for inline math: no space against either delimiter. The kit's
        // own construct is looser — `spent $5 and $10` arrives here as the span
        // `$5 and $` — and claiming that would draw a sentence as a formula, so a
        // span that is not tightly delimited is left to the kit, which shows it as
        // the prose it is. An alias is its own construct: its delimiters are TeX's.
        if alias.is_none() && !delimited_tightly(&math.value) {
            return None;
        }
        // Otherwise it is claimed whatever it holds: a formula this engine cannot
        // draw is drawn by the fallback as the source the model wrote, which is only
        // possible if the node is ours to draw, delimiters and all.
        let source = copy_source(&math.value, cx.node_source(node), "$");
        let display = alias.is_some_and(|(_, display)| display)
            || cx
                .node_source(node)
                .is_some_and(|source| source.trim_start().starts_with("$$"));
        let body = alias.map_or(math.value.as_str(), |(body, _)| body);
        Some(math_node(INLINE, body, source, display))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        if formula.display {
            // `$$…$$` written on one line is a display formula all the same, and gets
            // the display rendering: its own scroller, the width of the column.
            return Some(InlineElement::new(self.displayed(node, &formula.body, cx)));
        }
        let size = f32::from(context.font_size());
        let ink = ink_of(Palette::from_app(cx).foreground);
        match picture(&formula.body, false, size, ink, cx) {
            // The width rule lives in `draw`, which refuses a formula too wide to
            // sit in a line, so a picture here is one that fits: the alternative is
            // the source, which is what the fallback has always been.
            Some(picture) => Some(
                InlineElement::new(picture_image(&picture)).with_baseline(px(picture.baseline)),
            ),
            None => Some(InlineElement::new(crate::markdown::markup(
                node.as_markdown(),
                cx,
            ))),
        }
    }
}

impl Inline {
    /// The display rendering, for the `$$…$$` spelling that arrives as an inline
    /// node: the picture in its own sideways scroller.
    fn displayed(&self, node: &MarkdownNode, body: &str, cx: &mut App) -> AnyElement {
        displayed(node, body, cx)
            .unwrap_or_else(|| crate::markdown::markup(node.as_markdown(), cx).into_any_element())
    }
}

/// `$$…$$`: the formula on its own line, whole, scrolling sideways when it is
/// wider than the column.
pub(crate) struct Display;

impl MarkdownPlugin for Display {
    fn name(&self) -> &str {
        DISPLAY
    }

    fn is_block(&self) -> bool {
        true
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Math(math) = node else {
            return None;
        };
        // A display alias, `\[…\]`, arrives the same way: wrapped for the parser,
        // read by its body, copied as the alias itself. So does a `$$…$$` the
        // source pass fenced to make it a block: its content is the model's own
        // line, and the body is what that line holds.
        let alias = alias(&math.value);
        let body = alias
            .map(|(body, _)| body)
            .or_else(|| dollar_marker(&math.value))
            .unwrap_or(math.value.as_str());
        Some(math_node(
            DISPLAY,
            body,
            copy_source(&math.value, cx.node_source(node), "$$"),
            true,
        ))
    }

    fn render(&self, node: &MarkdownNode, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(formula) = node.data::<Formula>() else {
            return crate::markdown::markup(node.as_markdown(), cx).into_any_element();
        };
        // Unreadable, empty, or wider than it is worth drawing: the source, as the
        // model wrote it — the delimiters stay, which is what the kit's own fallback
        // for a flow math node would have thrown away.
        displayed(node, &formula.body, cx)
            .unwrap_or_else(|| crate::markdown::markup(node.as_markdown(), cx).into_any_element())
    }
}

/// A display formula's element: the picture in its own sideways scroller, the
/// width of the column, so one wider than the pane is read by scrolling instead
/// of being clipped. `None` when no picture can be made of it — and that is now
/// every refusal there is: the width rule lives in [`draw`], with the rest.
///
/// The scroller carries the formula's own accessible name: a block plugin's element
/// is drawn as it is (the kit's label travels only with an inline object), and a
/// picture has no text for a screen reader to find.
fn displayed(node: &MarkdownNode, body: &str, cx: &mut App) -> Option<AnyElement> {
    let palette = Palette::from_app(cx);
    let size = f32::from(palette.font_size);
    let picture = picture(body, true, size, ink_of(palette.foreground), cx)?;
    Some(
        div()
            .id(block_id(node))
            .test_support()
            .aria_label(format!("math: {}", body.trim()))
            // The reading column's own width, and never more: a display formula the
            // width of the box it is in, and one wider than the box scrolling inside
            // it rather than widening the pane or widening the page. `w_full` and
            // `min_w_0` are what would have it follow a narrow parent instead of
            // taking its own maximum; the kit lays a custom block out at its content's
            // width, so `max_w` below is the bound that holds today.
            .w_full()
            .min_w_0()
            .max_w(px(MEASURE - 2. * INSET))
            .overflow_x_scroll()
            // `mx_auto` centres a picture that fits and leaves a wider one against
            // the left edge, where the scroller can reach it.
            .child(picture_image(&picture).mx_auto())
            .into_any_element(),
    )
}

/// The custom node both plugins hand the kit.
///
/// Its **text** and its **markdown** are both the original source, delimiters
/// included: a copy, plain or source, gives back exactly the LaTeX the model
/// wrote, and if a picture cannot be made the kit draws that same source. Its
/// accessible name says what the run is, since a formula drawn as a picture has
/// no text of its own for a screen reader to find.
fn math_node(name: &'static str, body: &str, source: String, display: bool) -> MarkdownNode {
    let label = format!("math: {}", body.trim());
    MarkdownNode::new(
        name,
        Formula {
            body: body.to_string(),
            display,
        },
    )
    .text(source.clone())
    .markdown(source)
    .accessibility_label(label)
}

/// Whether an inline math span is delimited the way TeX means it to be: no space
/// against either `$`. A span that fails this is prose that merely mentions a
/// dollar amount, and the kit's own path keeps it that way.
fn delimited_tightly(value: &str) -> bool {
    !(value.starts_with(char::is_whitespace) || value.ends_with(char::is_whitespace))
}

/// The TeX alias the source pass wraps for the parser: `\(…\)` inline, `\[…\]`
/// display. What comes back is the LaTeX inside it and whether it is display math.
///
/// The alias is not stripped from the *source*: an alias is exactly what the model
/// wrote, so it is what a copy gives back — the dollars the pass added for the
/// parser are nobody's text.
fn alias(value: &str) -> Option<(&str, bool)> {
    if let Some(body) = value
        .strip_prefix("\\(")
        .and_then(|rest| rest.strip_suffix("\\)"))
    {
        return Some((body, false));
    }
    let body = value
        .strip_prefix("\\[")
        .and_then(|rest| rest.strip_suffix("\\]"))?;
    Some((body, true))
}

/// The formula inside a `$$…$$` marker: the line the source pass put inside the
/// fence it wrote to make a standalone `$$…$$` a block of the column's width.
/// `None` for anything else, a flow formula's own body included.
fn dollar_marker(value: &str) -> Option<&str> {
    let value = value.trim();
    let body = value.strip_prefix("$$")?.strip_suffix("$$")?;
    (!body.is_empty() && !body.contains('$')).then_some(body)
}

/// The text a copy of this span gives back: an alias exactly as it was written, or
/// the delimited source the kit's own `$…$` form came with.
fn copy_source(value: &str, node_source: Option<&str>, delimiter: &str) -> String {
    if alias(value).is_some() {
        return value.to_string();
    }
    if dollar_marker(value).is_some() {
        return value.trim().to_string();
    }
    source_of(node_source, value, delimiter)
}

/// A node's own source in the document, delimiters included; the delimiters are
/// added back when the parser kept no positions.
fn source_of(source: Option<&str>, body: &str, delimiter: &str) -> String {
    source
        .map(str::to_string)
        .unwrap_or_else(|| format!("{delimiter}{body}{delimiter}"))
}

/// A stable name for one display formula's scroller: where it sits in the
/// document, or — with no positions to go by — a fingerprint of the source.
fn block_id(node: &MarkdownNode) -> ElementId {
    let name = match node.source_range() {
        Some(range) => format!("transcript-math-block-{}-{}", range.start, range.end),
        None => format!(
            "transcript-math-block-{:016x}",
            fingerprint(node.as_markdown())
        ),
    };
    ElementId::Name(SharedString::from(name))
}

fn fingerprint(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The ink an `Hsla` is, packed as bytes: the cache key for a theme's colour.
fn ink_of(color: Hsla) -> u32 {
    let rgba = gpui_kit::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    (channel(rgba.r) << 24) | (channel(rgba.g) << 16) | (channel(rgba.b) << 8) | channel(rgba.a)
}

/// The same colour, as RaTeX draws with it.
fn color_of(ink: u32) -> Color {
    let channel = |shift: u32| ((ink >> shift) & 0xff) as f32 / 255.;
    Color::new(channel(24), channel(16), channel(8), channel(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One formula, laid out here and now: the work itself, without a window or a
    /// background executor in the way.
    fn drawn(body: &str, display: bool) -> Picture {
        draw(body, display, 16., ink_of(Hsla::black())).expect("a picture")
    }

    fn svg(body: &str, display: bool) -> String {
        svg_of(&drawn(body, display))
    }

    /// The bytes of a picture's image, as text: what the transcript hands gpui.
    fn svg_of(picture: &Picture) -> String {
        String::from_utf8(picture.image.bytes.clone()).expect("utf-8")
    }

    /// A formula is glyph outlines, not `<text>`: the picture carries its own
    /// fonts and needs nothing from the machine it is drawn on.
    #[test]
    fn a_fraction_is_drawn_with_embedded_outlines() {
        let svg = svg("\\frac{a}{b}", false);
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(svg.contains("<path"), "outlines, not text: {svg}");
        assert!(!svg.contains("<text"), "{svg}");
    }

    #[test]
    fn roots_sums_and_matrices_are_drawn() {
        for body in [
            "\\sqrt{x^2 + y^2}",
            "\\sum_{i=1}^{n} i^2",
            "\\begin{matrix} a & b \\\\ c & d \\end{matrix}",
        ] {
            let picture = drawn(body, body.contains("matrix"));
            assert!(picture.width > 0. && picture.height > 0., "{body}");
            assert!(
                picture.baseline > 0. && picture.baseline < picture.height,
                "{body}: baseline {} of {}",
                picture.baseline,
                picture.height
            );
        }
    }

    /// The baseline is the layout's own `height`, so an inline picture sits on
    /// the line the way a glyph does rather than floating in a box.
    #[test]
    fn the_baseline_is_the_layout_height() {
        let picture = drawn("x", false);
        let pad = 16. * PAD_EM;
        assert!(picture.baseline < picture.height);
        assert!(picture.baseline > 0.);
        assert!(f64::from(picture.height) >= pad * 2.);
    }

    /// A formula this engine cannot read has no picture — and the callers turn
    /// that into the original source, never into a blank row.
    #[test]
    fn an_unreadable_formula_has_no_picture() {
        assert!(draw("\\frac{a}{", false, 16., 0).is_none());
        assert!(draw("\\begin{matrix} a", true, 16., 0).is_none());
        // An empty formula is nothing to draw, and a size that makes no sense is
        // refused rather than drawn.
        assert!(draw("", false, 16., 0).is_none());
        assert!(draw("x", false, 0., 0).is_none());
        // Nothing above is a panic, either: every failure is a `None`.
    }

    /// A formula is claimed once and asked for once: every frame after the first
    /// waits, and a failure waits like a drawing — a message that keeps being
    /// re-parsed while it streams never queues a second job for the same formula.
    #[test]
    fn a_formula_is_claimed_once() {
        let mut cache = Cache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        };
        let key = Key {
            body: "\\frac{a}{b}".to_string(),
            display: false,
            size: 64,
            ink: 0,
        };
        assert!(
            matches!(lookup(&mut cache, &key), Lookup::Draw),
            "the first frame claims it and asks"
        );
        for _ in 0..4 {
            assert!(
                matches!(lookup(&mut cache, &key), Lookup::Wait),
                "and every frame after that waits, job or no job"
            );
        }

        settle(
            &mut cache,
            &key,
            Some(Picture {
                image: Arc::new(Image::from_bytes(ImageFormat::Svg, b"<svg/>".to_vec())),
                width: 1.,
                height: 1.,
                baseline: 0.5,
            }),
        );
        assert!(matches!(lookup(&mut cache, &key), Lookup::Ready(_)));

        // A failure is remembered the same way, with exactly one attempt spent.
        let broken = Key {
            body: "\\frac{a}{".to_string(),
            ..key
        };
        assert!(matches!(lookup(&mut cache, &broken), Lookup::Draw));
        settle(&mut cache, &broken, None);
        assert!(matches!(lookup(&mut cache, &broken), Lookup::Wait));
        assert!(draw(&broken.body, false, 16., broken.ink).is_none());
    }

    /// Light and dark are different keys and different pictures: the ink is baked
    /// into the SVG, so a theme change is a redraw, not a wrong-coloured formula.
    #[test]
    fn the_ink_is_the_themes() {
        let light = ink_of(Hsla::black());
        let dark = ink_of(Hsla::white());
        assert_ne!(light, dark);
        let light_svg = svg_of(&draw("x^2", false, 16., light).unwrap());
        let dark_svg = svg_of(&draw("x^2", false, 16., dark).unwrap());
        assert_ne!(light_svg, dark_svg, "the ink is part of the picture");
    }

    /// The cache is bounded: a transcript can hold many formulas, a reader can
    /// change the theme and the size as often as they like, and a formula that
    /// cannot be drawn is remembered like any other entry.
    #[test]
    fn the_cache_stays_bounded() {
        let mut cache = Cache::default();
        for n in 0..(CACHED + 64) {
            let key = key_at(n);
            assert!(
                matches!(lookup(&mut cache, &key), Lookup::Draw),
                "room is made for every claim: the oldest picture is let go, never a claim"
            );
            settle(
                &mut cache,
                &key,
                Some(Picture {
                    image: Arc::new(Image::from_bytes(ImageFormat::Svg, b"<svg/>".to_vec())),
                    width: 1.,
                    height: 1.,
                    baseline: 0.5,
                }),
            );
            assert!(matches!(lookup(&mut cache, &key), Lookup::Ready(_)));
            // A second look at a settled formula claims nothing: one entry, one job.
            assert!(matches!(lookup(&mut cache, &key), Lookup::Ready(_)));
        }
        assert!(cache.entries.len() <= CACHED);
        assert!(cache.order.len() <= CACHED);
    }

    /// A cache full of formulas *still being drawn* is full: a new formula waits for
    /// one of them rather than a claim being let go — a claim is what keeps the same
    /// formula from being laid out twice — and nothing goes over the bound.
    #[test]
    fn a_cache_of_claims_in_flight_waits_instead_of_drawing() {
        let mut cache = Cache::default();
        for n in 0..CACHED {
            assert!(matches!(lookup(&mut cache, &key_at(n)), Lookup::Draw));
        }
        assert_eq!(cache.entries.len(), CACHED);
        assert!(
            matches!(lookup(&mut cache, &key_at(CACHED)), Lookup::Wait),
            "no settled entry to let go, so nothing new is claimed"
        );
        assert_eq!(cache.entries.len(), CACHED, "and the bound is kept");

        // One lands: now there is a settled entry, and the waiting formula is claimed
        // on the frame that settlement asks for — still inside the bound.
        settle(&mut cache, &key_at(0), None);
        assert!(matches!(lookup(&mut cache, &key_at(CACHED)), Lookup::Draw));
        assert_eq!(cache.entries.len(), CACHED);
        assert!(
            !cache.entries.contains_key(&key_at(0)),
            "the settled entry is the one that was let go"
        );
    }

    /// An answer for a claim that is gone is dropped: the entry is not put back into
    /// a cache that has already let it go, and a stale job cannot grow the cache.
    #[test]
    fn an_answer_for_a_claim_that_is_gone_is_dropped() {
        let mut cache = Cache::default();
        let key = key_at(0);
        assert!(matches!(lookup(&mut cache, &key), Lookup::Draw));
        // As if the claim had been let go (nothing evicts a claim today — this is the
        // guard that keeps it that way).
        cache.entries.remove(&key);
        cache.order.retain(|held| held != &key);
        settle(&mut cache, &key, None);
        assert!(cache.entries.is_empty(), "the late answer is dropped");
        assert!(cache.order.is_empty());
    }

    /// A picture holds the image it is drawn from, built once where the picture was
    /// built: two pictures of the same formula are the same image, which is what gpui
    /// caches a decoded picture by — no frame copies or re-hashes an SVG.
    #[test]
    fn a_picture_carries_its_image() {
        let first = drawn("x^2", false);
        let again = drawn("x^2", false);
        assert_eq!(
            first.image.id, again.image.id,
            "the same bytes are the same image"
        );
        assert_eq!(first.image.format, ImageFormat::Svg);
    }

    fn key_at(n: usize) -> Key {
        Key {
            body: format!("x_{{{n}}}"),
            display: false,
            size: 64,
            ink: 0,
        }
    }

    /// A formula outside the embedded faces is refused, so nothing ever goes
    /// looking for a system font — and the reader sees the source.
    #[test]
    fn a_formula_outside_the_embedded_fonts_is_not_drawn() {
        assert!(
            !drawable_for_test("α + β"),
            "no system font for a literal alpha"
        );
        assert!(!drawable_for_test("\\frac{a}{"));
        assert!(drawable_for_test("\\frac{a}{b}"));
    }

    fn drawable_for_test(body: &str) -> bool {
        draw(body, false, 16., ink_of(Hsla::black())).is_some()
    }

    /// A formula past the source bound is not read at all — the reader gets the
    /// source, and refusing it costs a length check rather than a layout, an SVG and
    /// a cache entry. A formula that is merely long-winded about its spaces, which
    /// would draw a narrow picture, is refused by the bound just the same.
    #[test]
    fn a_formula_past_the_source_bound_is_not_read() {
        let padded = format!("x{}y", " ".repeat(MAX_BODY_BYTES));
        assert!(padded.len() > MAX_BODY_BYTES);
        assert!(
            drawable_for_test("x y"),
            "the fixture is a formula when short"
        );
        assert!(
            draw(&padded, false, 16., ink_of(Hsla::black())).is_none(),
            "past the bound, nothing is parsed and nothing is drawn"
        );
    }

    /// The inline cap is the narrowest pane the app has, so an inline picture — one
    /// atomic object that cannot scroll — is never wider than the column it is drawn
    /// in, at the type size the transcript draws at.
    #[test]
    fn the_inline_cap_fits_the_narrowest_pane() {
        let body = 16.;
        let narrowest_measure = f64::from(store::app_state::CENTER_MIN) - 2. * f64::from(INSET);
        assert!(
            INLINE_MAX_EM * body <= narrowest_measure,
            "{}em at {body}px is {}px, and the narrowest pane holds {narrowest_measure}px",
            INLINE_MAX_EM,
            INLINE_MAX_EM * body
        );
    }

    /// The width rule is the renderer's own, applied before the SVG exists: a formula
    /// too wide for a line draws no picture — and the same formula as a display one,
    /// which may be 200em wide and scrolls, still draws.
    #[test]
    fn a_formula_too_wide_for_its_rendering_is_refused() {
        let wide = (0..40)
            .map(|i| format!("sigma_{{{i}}}"))
            .collect::<Vec<_>>()
            .join(" + ");
        let ink = ink_of(Hsla::black());
        assert!(
            draw(&wide, false, 16., ink).is_none(),
            "30-odd em is more than an inline line takes"
        );
        let display = draw(&wide, true, 16., ink).expect("a display formula may be wider");
        assert!(f64::from(display.width) <= DISPLAY_MAX_EM * 16.);
    }

    /// A refusal is remembered like a drawing: the frames after it wait, so a message
    /// that keeps being re-parsed does not try an impossible formula again — and the
    /// reader keeps the source either way.
    #[test]
    fn a_refused_formula_is_cached_as_refused() {
        let mut cache = Cache::default();
        let key = Key {
            body: format!("x{}", " ".repeat(MAX_BODY_BYTES)),
            display: false,
            size: 64,
            ink: 0,
        };
        assert!(matches!(lookup(&mut cache, &key), Lookup::Draw));
        settle(&mut cache, &key, draw(&key.body, false, 16., key.ink));
        assert!(
            matches!(cache.entries.get(&key), Some(Entry::Failed)),
            "refused, and remembered as refused"
        );
        assert!(matches!(lookup(&mut cache, &key), Lookup::Wait));

        // The same for one refused on width, which is found out after the layout.
        let wide = Key {
            body: (0..40)
                .map(|i| format!("sigma_{{{i}}}"))
                .collect::<Vec<_>>()
                .join(" + "),
            ..key
        };
        assert!(matches!(lookup(&mut cache, &wide), Lookup::Draw));
        settle(&mut cache, &wide, draw(&wide.body, false, 16., wide.ink));
        assert!(matches!(cache.entries.get(&wide), Some(Entry::Failed)));
        assert!(matches!(lookup(&mut cache, &wide), Lookup::Wait));
    }

    /// A formula that names a character by number leaves the embedded faces — it is
    /// what sends the engine looking for a font on this machine — so it is refused
    /// before the engine ever sees it, and the reader keeps the source.
    #[test]
    fn a_formula_that_names_a_character_is_refused() {
        let ink = ink_of(Hsla::black());
        for body in [
            "\\char\"2603",
            "\\text{\\char\"2603}",
            "\\char\"1F600",
            "x + \\char\"41",
        ] {
            assert!(
                draw(body, false, 16., ink).is_none(),
                "a named character is not drawn: {body}"
            );
        }
    }

    /// The command is read as a command, not as five characters: `\\char` is a line
    /// break and then the word, and `\\charset` is another command entirely.
    #[test]
    fn the_character_command_is_read_as_a_token() {
        assert!(asks_for_a_character_by_number("\\char"));
        assert!(asks_for_a_character_by_number("\\char\"41"));
        assert!(asks_for_a_character_by_number("\\char'101"));
        assert!(asks_for_a_character_by_number("\\char 65"));
        assert!(asks_for_a_character_by_number("a + \\text{\\char\"2603}"));
        assert!(!asks_for_a_character_by_number("\\charset{x}"));
        assert!(!asks_for_a_character_by_number("\\character"));
        assert!(!asks_for_a_character_by_number("char"));
        assert!(
            !asks_for_a_character_by_number("a \\\\char b"),
            "a literal backslash, then the word"
        );
    }

    /// The belt and braces after the render: a picture that is not glyph outlines —
    /// `<text>` would lean on a font this machine has to have, `<image>` carries a
    /// raster from one — is not a picture, so a formula can only ever be the same
    /// picture everywhere.
    #[test]
    fn a_picture_that_is_not_glyph_outlines_is_refused() {
        assert!(is_glyph_outlines("<svg><path d=\"M0 0\"/></svg>"));
        assert!(!is_glyph_outlines(
            "<svg><text font-family=\"KaTeX_Main\">x</text></svg>"
        ));
        assert!(!is_glyph_outlines(
            "<svg><image href=\"data:image/png;base64,AA\"/></svg>"
        ));
    }

    /// A sentence that merely mentions two dollar amounts is not a formula: the
    /// span markdown-rs mints for it is not tightly delimited, so the kit keeps it
    /// as the prose it was.
    #[test]
    fn prose_with_dollar_signs_is_not_taken_for_a_formula() {
        assert!(!delimited_tightly("5 and "), "`spent $5 and $10` is prose");
        assert!(!delimited_tightly(" 5"));
        assert!(delimited_tightly("x^2"));
        assert!(delimited_tightly(""));
    }

    /// What a copied formula is, and what a reader on a screen reader hears: the
    /// source exactly as it was written — delimiters and all, for a plain copy as
    /// much as a source one — and a label that says what the run is.
    #[test]
    fn a_node_copies_its_source_and_names_itself() {
        let node = math_node(INLINE, "x^2", "$x^2$".to_string(), false);
        assert_eq!(node.as_text(), "$x^2$");
        assert_eq!(node.as_markdown(), "$x^2$");
        assert_eq!(node.accessibility_name(), "math: x^2");

        let display = math_node(
            DISPLAY,
            "\\frac{a}{b}",
            "$$\\frac{a}{b}$$".to_string(),
            true,
        );
        assert_eq!(display.as_text(), "$$\\frac{a}{b}$$");
        assert_eq!(display.as_markdown(), "$$\\frac{a}{b}$$");
        assert_eq!(display.accessibility_name(), "math: \\frac{a}{b}");

        // The unreadable one is the same node, with the same source to fall back to.
        let broken = math_node(DISPLAY, "\\frac{a}{", "$$\\frac{a}{$$".to_string(), true);
        assert_eq!(broken.as_markdown(), "$$\\frac{a}{$$");
        assert!(draw("\\frac{a}{", true, 16., ink_of(Hsla::black())).is_none());
    }

    /// An alias is read for its LaTeX and copied as itself: the dollars the source
    /// pass wrapped it in never reach a reader, and the delimiters the model wrote
    /// do — in the text a copy gives, in the fallback, and in the name a screen
    /// reader is told.
    #[test]
    fn an_alias_reads_its_body_and_copies_the_alias_itself() {
        assert_eq!(alias("x^2"), None, "the kit's own form is not an alias");
        assert_eq!(alias("\\(x^2"), None, "an alias that never closed");
        assert_eq!(alias("\\(x^2\\)"), Some(("x^2", false)));
        assert_eq!(alias("\\[\\frac{a}{b}\\]"), Some(("\\frac{a}{b}", true)));

        let inline = copy_source("\\(x^2\\)", Some("$\\(x^2\\)$"), "$");
        assert_eq!(
            inline, "\\(x^2\\)",
            "the dollars are the parser's, not ours"
        );
        assert_eq!(
            copy_source("\\[y^2\\]", Some("$$\\[y^2\\]$$"), "$$"),
            "\\[y^2\\]"
        );
        assert_eq!(
            copy_source("x^2", Some("$x^2$"), "$"),
            "$x^2$",
            "the kit's own form copies as it was written"
        );

        let node = math_node(
            INLINE,
            alias("\\(x^2\\)").expect("an alias").0,
            copy_source("\\(x^2\\)", Some("$\\(x^2\\)$"), "$"),
            false,
        );
        assert_eq!(node.as_text(), "\\(x^2\\)");
        assert_eq!(node.as_markdown(), "\\(x^2\\)");
        assert_eq!(node.accessibility_name(), "math: x^2");
        assert_eq!(node.data::<Formula>().expect("a formula").body, "x^2");
    }
}

/// The whole path, in a window: the kit's parser, the two plugins, the picture,
/// and the image the transcript draws. What the unit tests above cannot say is
/// that the pieces fit.
#[cfg(test)]
mod drawn_in_a_window {
    use super::*;
    use gpui_kit::base::{TextView, TextViewState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, Context, Entity, Render, TestAppContext};

    struct Host {
        document: Entity<TextViewState>,
        /// The width the page is given: the pane a formula has to live inside.
        width: f32,
    }

    impl Host {
        fn new(document: Entity<TextViewState>, width: f32) -> Host {
            Host { document, width }
        }
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("narrow-host")
                .test_support()
                .w(px(self.width))
                .child(
                    TextView::new(&self.document)
                        .style(crate::style::text_style(cx))
                        .markdown_extensions(crate::markdown::extensions()),
                )
        }
    }

    /// Laying a formula out is the app's work, not the frame's: the call that
    /// first meets one is given no picture and starts the job, the calls after it
    /// while the job is in flight are given no picture either and start nothing,
    /// and the picture is there once the app has run the task.
    #[gpui_kit::test]
    fn a_formula_is_laid_out_off_the_frame(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let ink = ink_of(Hsla::black());
        let body = "\\sqrt{2 + \\frac{p}{q}}";

        assert!(
            cx.update(|cx| picture(body, false, 16., ink, cx)).is_none(),
            "the frame that meets a formula draws the source, not a picture"
        );
        assert!(
            cx.update(|cx| picture(body, false, 16., ink, cx)).is_none(),
            "and the next frame waits for it instead of laying it out again"
        );

        cx.run_until_parked();

        let drawn = cx
            .update(|cx| picture(body, false, 16., ink, cx))
            .expect("the picture the executor drew");
        assert!(drawn.width > 0. && drawn.height > 0. && drawn.baseline > 0.);
        let svg = String::from_utf8(drawn.image.bytes.clone()).expect("utf-8");
        assert!(svg.starts_with("<svg"));

        // The picture is the image it is drawn from: every frame after this one hands
        // gpui the very same `Image`, which is what its cache is keyed by.
        let again = cx
            .update(|cx| picture(body, false, 16., ink, cx))
            .expect("the cached picture");
        assert_eq!(again.image.id, drawn.image.id, "no second SVG per frame");
    }

    /// A line of prose with an inline formula, a display formula of its own, an
    /// unreadable formula and an unclosed `$$` all render together — the display
    /// one through its own scroller, the other two as the source the model wrote.
    #[gpui_kit::test]
    fn a_document_with_math_draws_its_pictures_and_shows_what_it_cannot_draw(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let text = "spent $5 and $10 today\n\nbefore $x^2$ after\n\n$$\n\\frac{a}{b}\n$$\n\nbad $\\frac{a}{$ tail\n\n$$\n\\frac{a}{\n$$\n\nunclosed $$x";
        let document = cx.update(|cx| cx.new(|cx| TextViewState::markdown(text, cx)));
        let (_host, cx) = cx.add_window_view(|_, _cx| Host::new(document, 600.));

        // Where the display formula sits in the document, which is what names its
        // scroller: `MarkdownNode::source_range` is a byte range of that source.
        let start = text.find("$$").expect("a display formula");
        let end = text[start + 2..].find("$$").expect("its closing run") + start + 4;

        cx.update(|window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .try_find(SharedString::from(format!(
                        "transcript-math-block-{start}-{end}"
                    )))
                    .is_some(),
                "a display formula is drawn in its own horizontal scroller"
            );
            // The scroller leans on the kit's own label for an inline object, which a
            // block does not get: the formula's name is on the scroller itself, or a
            // picture has nothing for a screen reader to read.
            assert_eq!(
                window
                    .find(SharedString::from(format!(
                        "transcript-math-block-{start}-{end}"
                    )))
                    .label(),
                Some("math: \\frac{a}{b}"),
                "a drawn formula names itself"
            );
            // The display formula that cannot be read is not a scroller either: it is
            // drawn by the fallback, as the source with its `$$` — never as a `$$`‐less
            // code block, which is what the kit does with a flow math node nobody
            // claimed.
            let broken_start = text[end..].find("$$").expect("a second formula") + end;
            let broken_end = text[broken_start + 2..]
                .find("$$")
                .expect("its closing run")
                + broken_start
                + 4;
            assert_eq!(
                &text[broken_start..broken_end],
                "$$\n\\frac{a}{\n$$",
                "the fallback is given the source, delimiters and all"
            );
            assert!(
                window
                    .try_find(SharedString::from(format!(
                        "transcript-math-block-{broken_start}-{broken_end}"
                    )))
                    .is_none(),
                "an unreadable formula is not drawn in a scroller"
            );
            // A second frame draws the same thing again, out of the picture cache,
            // with nothing else to re-read.
            window.render_frame(cx);
        });
    }

    /// A narrow pane (`store::app_state::CENTER_MIN`, 420px): a display formula is
    /// drawn in a box of its own, one spelling of `$$…$$` or the other, and that box
    /// is never wider than the reading measure — the widest a formula is ever given,
    /// whatever the pane. (Following a *narrower* parent is the kit's to give: it lays
    /// a custom block out at its content's width, so a block plugin's own `max_w` is
    /// the one bound it can set.)
    #[gpui_kit::test]
    fn a_display_formula_in_a_narrow_pane_stays_within_the_measure(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // Wider than the pane (about 30em at 16px) and inside both caps, so the only
        // thing that can keep it in the pane is the box it is drawn in.
        // Wider than the pane (about 45em at 16px) and inside both caps, so what
        // keeps it in hand is the box it is drawn in — one spelling or the other.
        let formula = (0..20)
            .map(|i| format!("x_{{{i}}}"))
            .collect::<Vec<_>>()
            .join(" + ");
        let text = format!("before\n\n$${formula}$$\n\nthe same one inline $${formula}$$ after");
        let document = cx.update(|cx| cx.new(|cx| TextViewState::markdown(&text, cx)));
        let (_host, cx) = cx.add_window_view(|_, _cx| Host::new(document, 420.));
        let blocks = blocks(&text);
        assert_eq!(
            blocks.len(),
            2,
            "one on its own line, one inside a sentence"
        );

        cx.update(|window, cx| {
            window.render_frame(cx);
            for (start, end) in &blocks {
                let scroller = SharedString::from(format!("transcript-math-block-{start}-{end}"));
                let drawn = window.find(scroller).bounds();
                assert!(
                    drawn.size.width <= px(MEASURE - 2. * INSET),
                    "a display formula is never wider than the reading measure: {drawn:?}"
                );
                assert!(drawn.size.height > px(0.), "and is still drawn: {drawn:?}");
            }
        });
    }

    /// The byte ranges of the `$$…$$` blocks in a document: what the math module names
    /// each of their scrollers after.
    fn blocks(text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(open) = text[at..].find("$$") {
            let start = at + open;
            let Some(close) = text[start + 2..].find("$$") else {
                break;
            };
            let end = start + 2 + close + 2;
            out.push((start, end));
            at = end;
        }
        out
    }

    /// A formula past the source bound, one too wide to be worth drawing, and one that
    /// names a character the embedded faces need not carry: all three are the source
    /// the model wrote, drawn in the row like any other text — no scroller, no picture,
    /// and nothing laid out for them.
    #[gpui_kit::test]
    fn a_formula_past_the_bounds_is_drawn_as_its_source(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let long = format!("x{}", " + y".repeat(3000));
        let wide = (0..120)
            .map(|i| format!("sigma_{{{i}}}"))
            .collect::<Vec<_>>()
            .join(" + ");
        assert!(long.len() > MAX_BODY_BYTES, "the fixture is over the bound");
        let text =
            format!("before\n\n$${long}$$\n\n$$\n{wide}\n$$\n\n$$\n\\frac{{p}}{{q}}\n$$\n\nafter");
        let document = cx.update(|cx| cx.new(|cx| TextViewState::markdown(&text, cx)));
        let (_host, cx) = cx.add_window_view(|_, _cx| Host::new(document, 600.));
        let blocks = blocks(&text);
        assert_eq!(blocks.len(), 3, "a formula its own size sits between them");

        let scroller = |start: usize, end: usize| {
            SharedString::from(format!("transcript-math-block-{start}-{end}"))
        };
        cx.update(|window, cx| {
            window.render_frame(cx);
            for (start, end) in blocks.iter().take(2) {
                assert!(
                    window.try_find(scroller(*start, *end)).is_none(),
                    "a formula past the bounds is not drawn as a picture"
                );
            }
            // The document is drawn, and the refusals are about those two formulas:
            // the one between them is a picture like any other.
            let (start, end) = blocks[2];
            assert!(
                window.try_find(scroller(start, end)).is_some(),
                "a formula within the bounds is drawn as a picture"
            );
            window.render_frame(cx);
        });
    }
}
