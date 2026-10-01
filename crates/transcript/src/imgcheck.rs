//! Decoding the bytes an image row fetched, and drawing one.
//!
//! A media fetch hands over raw bytes with their content type; this is the one place
//! they become something GPUI draws, so the decode happens once and the frame is
//! shared (`Arc<RenderImage>`).

use std::sync::Arc;

use gpui_kit::{img, ImageSource, RenderImage, Styled as _, StyledImage as _};

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
pub fn picture(
    image: Arc<RenderImage>,
    max_h: gpui_kit::Pixels,
    max_w: gpui_kit::Pixels,
) -> gpui_kit::Img {
    img(ImageSource::Render(image))
        .object_fit(gpui_kit::ObjectFit::Contain)
        .max_h(max_h)
        .max_w(max_w)
}
