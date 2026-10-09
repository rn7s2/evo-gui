//! Decoding the bytes an image row fetched, and drawing one.
//!
//! A media fetch hands over raw bytes with their content type; this is the one place
//! they become something GPUI draws, so the decode happens once and the frame is
//! shared (`Arc<RenderImage>`).
//!
//! A picture a message points at on disk comes through here too
//! ([`read_local_image`]), with the checks a file that was never fetched has to
//! carry: its size on disk, and the `image` crate's own limits on a decode.

use std::path::Path;
use std::sync::Arc;

use gpui_kit::{
    img, px, size, ImageSource, Img, ObjectFit, Pixels, RenderImage, Size, Styled as _,
    StyledImage as _,
};

/// The largest file [`read_local_image`] will read, in bytes.
///
/// A message can name any path, so the size check happens on the file's own metadata,
/// before a byte is read: a picture bigger than this fails like one that could not be
/// decoded, rather than being loaded into memory first.
pub(crate) const MAX_LOCAL_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// The largest picture [`read_local_image`] will decode, on either side, in pixels.
///
/// The compressed size says nothing about the decoded one — a few kilobytes can
/// describe a gigapixel bitmap — so the decoder is held to this as well, and a file
/// that claims more is refused before it is rasterised.
pub(crate) const MAX_LOCAL_IMAGE_DIMENSION: u32 = 8192;

/// The most memory [`read_local_image`] will let one decode allocate.
pub(crate) const MAX_LOCAL_IMAGE_ALLOC: u64 = 256 * 1024 * 1024;

/// The longest side of a picture the transcript keeps, in pixels.
///
/// A screenshot is read in a column a few hundred pixels wide, and every vision stack
/// resizes anything larger before it looks at it — so a picture far beyond this is kept
/// at it instead of at the size the file happens to be. What the cap buys is memory: a
/// 8000×8000 screenshot is 256 MB decoded and 16 MB here.
pub(crate) const MAX_LOCAL_PICTURE_DIMENSION: u32 = 2048;

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
    Some(render(::image::load_from_memory(bytes).ok()?.into_rgba8()))
}

/// The picture the file at `path` holds, read and decoded — or `None` when it is not
/// there, is not an image, or is bigger than this module will read or decode
/// ([`MAX_LOCAL_IMAGE_BYTES`], [`MAX_LOCAL_IMAGE_DIMENSION`]).
///
/// This is the whole of the work, so it belongs off the UI thread: the caller reads
/// this on a background task and hands the frame back.
pub(crate) fn read_local_image(path: &Path) -> Option<Arc<RenderImage>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_LOCAL_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let decoded = decode_bounded(&bytes, MAX_LOCAL_IMAGE_DIMENSION, MAX_LOCAL_IMAGE_ALLOC)?;
    Some(render(reduced(decoded, MAX_LOCAL_PICTURE_DIMENSION)))
}

/// The picture at most `max_dimension` on its longest side, its shape kept: itself when
/// it is already that small, and a whole-picture reduction when it is not.
fn reduced(picture: ::image::RgbaImage, max_dimension: u32) -> ::image::RgbaImage {
    let (width, height) = picture.dimensions();
    let longest = width.max(height);
    if longest <= max_dimension || longest == 0 {
        return picture;
    }
    let scale = f64::from(max_dimension) / f64::from(longest);
    let (to_width, to_height) = (
        (f64::from(width) * scale).round().max(1.) as u32,
        (f64::from(height) * scale).round().max(1.) as u32,
    );
    ::image::imageops::thumbnail(&picture, to_width, to_height)
}

/// The pixels `bytes` hold, decoded under the two limits that keep a file from becoming
/// an allocation of its own choosing: a side no longer than `max_dimension`, and no more
/// than `max_alloc` bytes of decode.
///
/// The size a file *is* says nothing about the size it decodes to, so this is checked by
/// the decoder as it reads the header — not after a frame has been made.
fn decode_bounded(bytes: &[u8], max_dimension: u32, max_alloc: u64) -> Option<::image::RgbaImage> {
    let mut reader = ::image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(max_dimension);
    limits.max_image_height = Some(max_dimension);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    Some(reader.decode().ok()?.into_rgba8())
}

/// One decoded picture as the frame GPUI draws: baked up to `MIN_PICTURE` if it is
/// smaller than that, in the byte order `RenderImage` carries.
fn render(decoded: ::image::RgbaImage) -> Arc<RenderImage> {
    Arc::new(RenderImage::new(vec![::image::Frame::new(baked(
        swap_red_and_blue(decoded),
    ))]))
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

/// The same, bounded by one width and nothing else: the form a picture takes inside a
/// line of prose, where the column's width is the only thing it has to fit and its
/// height is whatever its own shape makes it.
///
/// A picture already narrower than the bound is drawn at its own size — the fit never
/// enlarges.
pub(crate) fn picture_within(image: Arc<RenderImage>, max_w: Pixels) -> Img {
    picture(image, px(f32::MAX), max_w)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of this test's own, thrown away when the test ends.
    struct File {
        path: std::path::PathBuf,
    }

    impl File {
        /// A real PNG of `width`×`height` pixels at a path of the test's own.
        fn png(name: &str, width: u32, height: u32) -> File {
            let path = std::env::temp_dir().join(format!(
                "evo-transcript-img-{name}-{}.png",
                std::process::id()
            ));
            ::image::RgbaImage::new(width, height)
                .save(&path)
                .expect("a picture to point at");
            File { path }
        }

        /// A file of `len` bytes that is not a picture at all, and costs nothing to
        /// make: its length is set, not written.
        fn not_a_picture(name: &str, len: u64) -> File {
            let path = std::env::temp_dir()
                .join(format!("evo-transcript-img-{name}-{}", std::process::id()));
            let file = std::fs::File::create(&path).expect("a file to point at");
            file.set_len(len).expect("a length");
            File { path }
        }
    }

    impl Drop for File {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn a_picture_on_disk_is_read_and_decoded() {
        let file = File::png("read", 60, 40);
        let frame = read_local_image(&file.path).expect("the picture");
        assert_eq!(frame.size(0).width.0, 60);
        assert_eq!(frame.size(0).height.0, 40);

        // A picture too small to be one is baked up to `MIN_PICTURE` like any other,
        // by the same one decode.
        let tiny = File::png("tiny", 3, 2);
        let frame = read_local_image(&tiny.path).expect("the picture");
        assert_eq!(frame.size(0).width.0, MIN_PICTURE as i32);
        assert_eq!(frame.size(0).height.0, 32);
    }

    #[test]
    fn what_is_not_a_picture_reads_as_nothing() {
        // Not there at all.
        assert!(read_local_image(Path::new("/tmp/evo-transcript-img-missing")).is_none());
        // There, and not an image.
        let file = File::not_a_picture("text", 64);
        assert!(read_local_image(&file.path).is_none());
        // There, and cut off mid-header: a decode that fails is a fallback, never a
        // panic.
        let png = File::png("cut", 4, 4);
        let bytes = std::fs::read(&png.path).expect("the picture");
        std::fs::write(&png.path, &bytes[..bytes.len() / 2]).expect("half a picture");
        assert!(read_local_image(&png.path).is_none());
    }

    #[test]
    fn a_file_too_big_to_read_is_refused_before_it_is_read() {
        let file = File::not_a_picture("huge", MAX_LOCAL_IMAGE_BYTES + 1);
        assert!(read_local_image(&file.path).is_none());
        // A folder is not a picture either, whatever its size.
        assert!(read_local_image(&std::env::temp_dir()).is_none());
    }

    #[test]
    fn a_picture_too_large_to_decode_is_refused_by_the_limits() {
        let file = File::png("limits", 8, 8);
        let bytes = std::fs::read(&file.path).expect("the picture");
        // The same bytes, under limits of the test's own: over a side, and over the
        // whole allocation.
        assert!(
            decode_bounded(&bytes, 4, MAX_LOCAL_IMAGE_ALLOC).is_none(),
            "a side over the limit is refused"
        );
        assert!(
            decode_bounded(&bytes, MAX_LOCAL_IMAGE_DIMENSION, 8).is_none(),
            "an allocation over the limit is refused"
        );
        assert!(
            decode_bounded(&bytes, 8, MAX_LOCAL_IMAGE_ALLOC).is_some(),
            "and inside them it decodes"
        );
    }

    #[test]
    fn a_picture_far_bigger_than_the_column_is_kept_at_a_size_a_column_can_use() {
        // An 8×8 picture with the cap of a small one: the reduction keeps the shape.
        let decoded =
            decode_bounded(&png_bytes(64, 32), 4096, MAX_LOCAL_IMAGE_ALLOC).expect("a picture");
        let kept = reduced(decoded, 16);
        assert_eq!(kept.dimensions(), (16, 8));
        // One already inside the cap is itself, pixel for pixel.
        let decoded =
            decode_bounded(&png_bytes(8, 4), 4096, MAX_LOCAL_IMAGE_ALLOC).expect("a picture");
        assert_eq!(reduced(decoded, 16).dimensions(), (8, 4));
    }

    #[test]
    fn a_picture_is_bounded_by_one_width_and_keeps_its_shape() {
        // A 200×100 picture in a 100-wide column is drawn 100×50.
        let wide = decode_image(&png_bytes(200, 100)).expect("a picture");
        let size = fitted_size(&wide, px(f32::MAX), px(100.));
        assert_eq!((f32::from(size.width), f32::from(size.height)), (100., 50.));

        // One already narrower is drawn at its own size: the fit never enlarges.
        let small = decode_image(&png_bytes(60, 30)).expect("a picture");
        let size = fitted_size(&small, px(f32::MAX), px(100.));
        assert_eq!((f32::from(size.width), f32::from(size.height)), (60., 30.));
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        ::image::RgbaImage::new(width, height)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                ::image::ImageFormat::Png,
            )
            .expect("a picture in memory");
        bytes
    }
}
