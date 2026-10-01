//! What an assistant message is parsed with beyond CommonMark and GFM.
//!
//! Two constructs a model writes must not reach the renderer as they are:
//!
//! * **an image reference.** `![alt](https://…)` becomes an `<img>` whose URI
//!   the image loader fetches over the app's HTTP client
//!   (`gpui::img::ImageAssetLoader::load`, `Resource::Uri` → `client.get`), so
//!   a message could make this app issue a request to an address a model chose.
//!   The reference is claimed here and drawn as its alt text — a transcript
//!   says what a message *refers to*, it never goes and looks;
//! * **raw HTML.** The kit interprets the tags it knows (`<b>`, `<br>`) and
//!   drops a node it cannot parse without leaving a trace, so text the reader
//!   was meant to see disappears. The node is claimed and shown as the source
//!   it is, in the mono face and muted: markup reads as markup, not as prose.
//!
//! Links are deliberately *not* claimed, so they keep the kit's own rendering:
//! a link is a run of flowing text, while anything a plugin renders is one
//! atomic box that cannot be broken across a line.

use std::sync::OnceLock;

use gpui_kit::component::text::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode,
    MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::{div, App, Div, FontWeight, IntoElement, ParentElement as _, Styled as _, Window};

use crate::style::Palette;

/// The node names of the three claims, so each renderer is found.
const IMAGE: &str = "transcript-image";
const HTML: &str = "transcript-html";
const HTML_BLOCK: &str = "transcript-html-block";

/// The extensions every assistant message is parsed with, built once.
///
/// The kit compares extensions by revision, so handing the same ones to every
/// row of every frame leaves the parsed documents alone.
pub(crate) fn extensions() -> MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS
        .get_or_init(|| {
            MarkdownExtensions::default()
                .plugin(ImageFallback)
                .plugin(RawHtml)
                .plugin(RawHtmlBlock)
                // TeX math, the two forms the kit's parser already mints nodes
                // for: `$…$` in a line, `$$…$$` of its own.
                .plugin(crate::math::Inline)
                .plugin(crate::math::Display)
        })
        .clone()
}

/// What an image reference is drawn as: its alt text, in brackets, and nothing
/// fetched.
struct ImageFallback;

impl MarkdownPlugin for ImageFallback {
    fn name(&self) -> &str {
        IMAGE
    }

    fn parse(&self, node: &Node, _cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Image(image) = node else {
            return None;
        };
        let alt = image.alt.trim();
        let label = if alt.is_empty() {
            "[image]".to_string()
        } else {
            format!("[image: {alt}]")
        };
        Some(MarkdownNode::new(IMAGE, ()).text(label))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        // The element inherits the surrounding text style; only the color is
        // its own, so a reference reads as an aside rather than as prose.
        Some(InlineElement::new(
            div()
                .text_color(Palette::from_app(cx).muted_foreground)
                .child(node.as_text().to_string()),
        ))
    }
}

/// Raw HTML inside a paragraph: the tags as they were written.
struct RawHtml;

impl MarkdownPlugin for RawHtml {
    fn name(&self) -> &str {
        HTML
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Html(html) = node else {
            return None;
        };
        let source = cx
            .node_source(node)
            .map(str::to_string)
            .unwrap_or_else(|| html.value.clone());
        Some(MarkdownNode::new(HTML, ()).text(source))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        Some(InlineElement::new(markup(node.as_text(), cx)))
    }
}

/// A block of raw HTML: the `<div>…</div>` a message carries on lines of its
/// own, which is a block node rather than a run inside a paragraph.
struct RawHtmlBlock;

impl MarkdownPlugin for RawHtmlBlock {
    fn name(&self) -> &str {
        HTML_BLOCK
    }

    fn is_block(&self) -> bool {
        true
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Html(html) = node else {
            return None;
        };
        let source = cx
            .node_source(node)
            .map(str::to_string)
            .unwrap_or_else(|| html.value.clone());
        Some(MarkdownNode::new(HTML_BLOCK, ()).text(source))
    }

    fn render(&self, node: &MarkdownNode, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        markup(node.as_text(), cx)
    }
}

/// A run of raw markup: the mono face, muted, so it reads as the markup it is
/// rather than as something the message said.
pub(crate) fn markup(source: &str, cx: &App) -> Div {
    let palette = Palette::from_app(cx);
    div()
        .font_family(palette.mono.clone())
        .font_weight(FontWeight::NORMAL)
        .text_color(palette.muted_foreground)
        .child(source.to_string())
}
