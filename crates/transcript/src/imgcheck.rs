//! Decoding the bytes an image row fetched, and drawing one.
//!
//! A media fetch hands over raw bytes with their content type; this is the one place
//! they become something GPUI draws, so the decode happens once and the frame is
//! shared (`Arc<RenderImage>`).

use std::sync::Arc;

use gpui_kit::{img, px, ImageSource, IntoElement, RenderImage, Styled as _, StyledImage as _};

/// The bytes of one image, decoded. `None` when they are not an image GPUI can draw —
/// the row then says so rather than showing nothing.
pub fn decode_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = ::image::load_from_memory(bytes).ok()?.into_rgba8();
    let frame = ::image::Frame::new(decoded);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

/// One picture at `max` tall, keeping its own aspect ratio, with a fallback for a frame
/// GPUI cannot draw.
pub fn picture(image: Arc<RenderImage>, max: gpui_kit::Pixels) -> impl IntoElement {
    img(ImageSource::Render(image))
        .object_fit(gpui_kit::ObjectFit::Contain)
        .max_h(max)
        .max_w(px(520.))
}
