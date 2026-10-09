//! What an assistant message is parsed with beyond CommonMark and GFM.
//!
//! Three constructs a model writes must not reach the renderer as they are:
//!
//! * **an image reference.** `![alt](https://…)` becomes an `<img>` whose URI
//!   the image loader fetches over the app's HTTP client
//!   (`gpui::img::ImageAssetLoader::load`, `Resource::Uri` → `client.get`), so
//!   a message could make this app issue a request to an address a model chose.
//!   The reference is claimed here and drawn as its alt text — a transcript
//!   says what a message *refers to*, it never goes and looks. The one exception
//!   is a reference to a file on this machine, which is drawn as the picture it
//!   names: see [`ImageFallback`] and [`crate::images`];
//! * **raw HTML.** The kit interprets the tags it knows (`<b>`, `<br>`) and
//!   drops a node it cannot parse without leaving a trace, so text the reader
//!   was meant to see disappears. The node is claimed and shown as the source
//!   it is, in the mono face and muted: markup reads as markup, not as prose.
//!
//! Links are deliberately *not* claimed, so they keep the kit's own rendering:
//! a link is a run of flowing text, while anything a plugin renders is one
//! atomic box that cannot be broken across a line.

use std::sync::Arc;

use gpui_kit::component::text::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode,
    MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::{
    div, App, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, TestSupportExt as _, Window,
};

use crate::images::LocalImages;
use crate::imgcheck;
use crate::style::Palette;

/// The node names of the three claims, so each renderer is found.
const IMAGE: &str = "transcript-image";
const HTML: &str = "transcript-html";
const HTML_BLOCK: &str = "transcript-html-block";

/// The element id a drawn picture wears, so a test or a probe can find it: one name for
/// every picture, told apart by the reference it came from.
pub const IMAGE_ID: &str = "transcript-markdown-image";

/// The extensions every assistant message is parsed with, built once per transcript —
/// the image plugin holds that transcript's own cache of local pictures, so one is
/// enough for the whole view and no two views share a picture.
///
/// The kit compares extensions by revision, and this instance is handed to every row of
/// every frame, so the parsed documents are left alone.
pub(crate) fn extensions(images: &Arc<LocalImages>) -> MarkdownExtensions {
    MarkdownExtensions::default()
        .plugin(ImageFallback {
            images: images.clone(),
        })
        .plugin(RawHtml)
        .plugin(RawHtmlBlock)
        // TeX math, the two forms the kit's parser already mints nodes for:
        // `$…$` in a line, `$$…$$` of its own.
        .plugin(crate::math::Inline)
        .plugin(crate::math::Display)
}

/// The extensions a document is parsed with outside a view — a test, a demo — where
/// there are no pictures of a record to hold: everything is claimed the same way, and
/// every reference is drawn as its alt text.
#[cfg(test)]
pub(crate) fn test_extensions() -> MarkdownExtensions {
    extensions(&Arc::new(LocalImages::new()))
}

/// One image reference: what it says, and what it names.
struct Reference {
    url: String,
    alt: String,
}

/// What an image reference is drawn as.
///
/// A reference to a file on this machine is drawn as the picture: the file is read on a
/// worker when the row is on screen ([`crate::images`]), and until it arrives — and for
/// every reference that names no local file, or names one this app will not read — the
/// alt text in brackets, which is what a reference has always been drawn as.
///
/// Nothing here reads, fetches or looks at the file system: the path is arithmetic on
/// the reference ([`LocalImages::frame_of`] resolves it), and whether the file is really
/// there is the worker's answer.
struct ImageFallback {
    images: Arc<LocalImages>,
}

impl MarkdownPlugin for ImageFallback {
    fn name(&self) -> &str {
        IMAGE
    }

    fn parse(&self, node: &Node, _cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Image(image) = node else {
            return None;
        };
        let alt = image.alt.trim().to_string();
        let label = bracket(&alt);
        Some(
            MarkdownNode::new(
                IMAGE,
                Reference {
                    url: image.url.clone(),
                    alt,
                },
            )
            .text(label),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let reference = node.data::<Reference>();
        let width = self.images.width();
        if let Some(reference) = reference {
            // The column's own width, and only ever set by the view that laid the row
            // out: a picture narrower than it keeps its own size, a wider one is drawn
            // to fit and keeps its shape.
            if f32::from(width) > 0. {
                if let Some(frame) = self.images.frame_of(&reference.url) {
                    // The source-range start disambiguates two references to the
                    // same file in one document, which would otherwise share one
                    // element-id slot and panic in the test registry.
                    let offset = node.source_range().map_or(0, |range| range.start);
                    return Some(InlineElement::new(
                        imgcheck::picture_within(frame, width)
                            .id(ElementId::from(format!(
                                "{IMAGE_ID}:{}:{offset}",
                                reference.url
                            )))
                            .test_support(),
                    ));
                }
            }
        }
        // Either the reference names no local file, or the picture is on its way, or it
        // could not be read: the alt text, as a reference nobody can follow is drawn.
        let label = match reference {
            Some(reference) => bracket(&reference.alt),
            None => node.as_text().to_string(),
        };
        Some(InlineElement::new(
            div()
                .text_color(Palette::from_app(cx).muted_foreground)
                .child(label),
        ))
    }
}

/// What a reference is drawn as when it is not the picture: its alt text in brackets,
/// which is what a transcript has always said about an image it does not show.
fn bracket(alt: &str) -> String {
    if alt.is_empty() {
        "[image]".to_string()
    } else {
        format!("[image: {alt}]")
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
