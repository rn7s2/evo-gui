//! Decoding the bytes an image row fetched, and drawing one.
//!
//! A media fetch hands over raw bytes with their content type; this is the one place
//! they become something GPUI draws, so the decode happens once and the frame is
//! shared (`Arc<RenderImage>`).

use std::sync::Arc;

use gpui_kit::{
    img, px, size, ImageSource, Img, ObjectFit, Pixels, RenderImage, Size, Styled as _,
    StyledImage as _,
};

/// The smallest picture a row draws, in logical pixels at 100%: a screenshot that is 1×1,
/// or an icon that is 8×8, is a box a reader can see rather than a dot.
///
/// `rows::image_row` floors its frame at this, and `decode_image` bakes anything smaller
/// up to it, so the box has something of the picture in it.
pub(crate) const MIN_PICTURE: f32 = 48.;

/// The bytes of one image, decoded. `None` when they are not an image GPUI can draw —
/// the row then says so rather than showing nothing.
///
/// A picture smaller than `MIN_PICTURE` is baked up first (`baked`), which is also what
/// keeps the thumbnail and the full-size picture the same bitmap.
pub fn decode_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = ::image::load_from_memory(bytes).ok()?.into_rgba8();
    Some(Arc::new(RenderImage::new(vec![::image::Frame::new(
        baked(swap_red_and_blue(decoded)),
    )])))
}

/// The picture in the order GPUI's `RenderImage` carries it: **BGRA**.
///
/// GPUI's own decoders swap the red and blue of every pixel before handing a frame over
/// ("Convert from RGBA to BGRA", `platform.rs`), and a frame handed over as RGBA is drawn
/// with the two exchanged — a red screenshot comes out blue. The swap is here, once, rather
/// than in the renderer's expectation.
fn swap_red_and_blue(mut decoded: ::image::RgbaImage) -> ::image::RgbaImage {
    for pixel in decoded.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    decoded
}

/// The picture a row draws: itself, unless it is smaller than `MIN_PICTURE` — in which case
/// it is blown up by a whole number of pixels, an 8×8 icon to 48×48 and a 1×1 to a 48×48
/// block, in blocks that are never interpolated.
///
/// Whole pixels because GPUI paints a scaled image through its own filter and offers no
/// nearest-neighbour one: leaving the enlargement to the draw would blur an icon into mush.
/// The factor is the largest whole one that reaches the box — `floor(MIN_PICTURE / the
/// longer side)` — so a 20×10 doubles to 40×20 and anything already as large as the box is
/// untouched. Done here, once per picture rather than once per frame.
fn baked(decoded: ::image::RgbaImage) -> ::image::RgbaImage {
    let (width, height) = decoded.dimensions();
    let factor = (MIN_PICTURE as u32) / width.max(height).max(1);
    if factor <= 1 {
        return decoded;
    }
    let mut baked = ::image::RgbaImage::new(width * factor, height * factor);
    for y in 0..height {
        for x in 0..width {
            let ink = *decoded.get_pixel(x, y);
            for dy in 0..factor {
                for dx in 0..factor {
                    baked.put_pixel(x * factor + dx, y * factor + dy, ink);
                }
            }
        }
    }
    baked
}

/// One picture at `max_h` tall and `max_w` wide, keeping its own aspect ratio, with a
/// fallback for a frame GPUI cannot draw. Both bounds come from the caller: a picture in
/// a row is content, so it is drawn at the design's own size times the reader's zoom.
pub fn picture(image: Arc<RenderImage>, max_h: Pixels, max_w: Pixels) -> Img {
    let fitted = fitted_size(&image, max_h, max_w);
    img(ImageSource::Render(image))
        // The element *is* the picture's own box, so filling it is what drawing the picture
        // in its own shape means. `Contain` would round the two together and could leave a
        // half-pixel band on one side of a picture it had already sized.
        .object_fit(ObjectFit::Fill)
        .w(fitted.width)
        .h(fitted.height)
}

/// Whether a frame must be held open for this picture: one so small that hugging it exactly
/// would leave a dot — a 20×10, whose bake stops at 40×20, or a one-pixel-tall strip.
///
/// A picture this module baked *up* to `MIN_PICTURE` is not one of these: a 1×1 or an 8×8
/// arrives 48×48 and holds its own frame open, which is what "baked up to fill the box"
/// means. Everything else — a screenshot, an icon that was always 64×64 — hugs.
pub fn is_tiny(image: &RenderImage) -> bool {
    let pixels = image.size(0);
    pixels.width.0.max(pixels.height.0) < MIN_PICTURE as i32
}

/// The size a picture is drawn at: its own shape, inside the caps, and no larger than the
/// picture itself.
///
/// GPUI fits a picture *inside* the element's box and leaves the rest of the box empty, so
/// an element sized the picture's own way would draw a 1000×600 shot 200×120 in a 520×120
/// box — a 320px band beside it — and a 300×900 one 40×120 in a 300×120 box. The frame
/// around it must hug what is drawn, which means the element has to be the fitted size, so
/// the fit is computed here from the picture's own dimensions.
pub fn fitted_size(image: &RenderImage, max_h: Pixels, max_w: Pixels) -> Size<Pixels> {
    let pixels = image.size(0);
    let (width, height) = (pixels.width.0 as f32, pixels.height.0 as f32);
    // A `RenderImage` this module built is one logical pixel per pixel (`decode_image`,
    // whose frames come from `RenderImage::new`, which leaves the scale factor at 1).
    if width <= 0. || height <= 0. {
        return size(px(0.), px(0.));
    }
    let scale = (f32::from(max_w) / width)
        .min(f32::from(max_h) / height)
        .min(1.);
    size(px(width * scale), px(height * scale))
}
