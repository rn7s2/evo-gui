//! The local pictures a message's Markdown points at: where they are, and the frames
//! the transcript has decoded of them.
//!
//! A message writes `![alt](path)` and the file is on the machine this app runs on, so
//! the picture can simply be shown — there is no fetch and no server in the way. What
//! makes it more than a `fs::read` is everything around that:
//!
//! * **Where the path points.** `file://…`, an absolute path and a relative one (from
//!   the folder the transcript runs in) all mean a file; an address, a `data:` URL or a
//!   bare word do not, and keep the bracketed fallback a reference has always been drawn
//!   as. Resolving is arithmetic on strings — **no look at the file system on the UI
//!   thread**, since a row is built every frame it is on screen. Whether the file is
//!   really there is the worker's answer, on the far side of the read.
//! * **What is decoded.** Frames are held by resolved path, so two messages naming the
//!   same screenshot share one decode, in a cache bounded by entries and by decoded
//!   bytes ([`MAX_LOCAL_IMAGES`], [`MAX_LOCAL_IMAGE_BYTES`]). FIFO — the oldest decoded
//!   entry goes first — and the entries the frame is drawing and the ones still loading
//!   are never the ones dropped. A failure is drawn-from by nobody, but it is asked for
//!   like any other entry, so it is protected while the frame is asking for it too.
//! * **When.** Nothing is read until a row carrying the picture is built, which is only
//!   ever a row the list has scrolled to.
//!
//! One cache belongs to one [`TranscriptView`](crate::TranscriptView) and dies with it;
//! nothing here is written to disk, and there is no copy anywhere else.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui_kit::{Pixels, RenderImage};

/// How many decoded pictures one transcript keeps.
pub(crate) const MAX_LOCAL_IMAGES: usize = 32;

/// How much decoded pixel memory one transcript keeps, in bytes.
///
/// The bound an entry count cannot make: pictures are not all the same size, so what a
/// full cache costs in memory is bounded by this rather than by how many there are.
pub(crate) const MAX_LOCAL_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

/// What is known about one local picture.
#[derive(Clone)]
enum Local {
    /// Asked for, not here yet.
    Loading,
    /// Decoded, once.
    Ready(Arc<RenderImage>),
    /// Not a picture this app shows: not there, not decodable, too big to read — or one
    /// there was no room to keep. The row draws the fallback instead, and it is not
    /// asked for again while this entry stands.
    Failed,
}

/// One entry: what is known, what it costs, and which frame pass last reached for it.
struct Entry {
    local: Local,
    bytes: u64,
    /// The pass this entry was last wanted in ([`LocalImages::begin_pass`]). An eviction
    /// leaves the last two passes alone: the pass being laid out and the one before it
    /// are the frames a reader is looking at, and a message with more pictures on screen
    /// than the cache holds would otherwise spend its life dropping one to decode the
    /// next, over and over.
    drawn: u64,
}

struct Inner {
    entries: HashMap<PathBuf, Entry>,
    /// The keys in the order they were decoded: FIFO is the eviction order.
    order: VecDeque<PathBuf>,
    /// Paths rows asked for that nobody has started reading yet.
    wanted: Vec<PathBuf>,
    /// The running total of every `Ready` entry's `bytes`.
    bytes: u64,
    /// Which frame pass this is; see [`LocalImages::begin_pass`].
    pass: u64,
    /// A reference the frame that just laid out was refused room for, in a cache whose
    /// oldest entries were drawn a pass ago: the frame after this one can take one of
    /// them, so one more frame is asked for. Only ever set by a refusal that has
    /// something to gain from it; see [`LocalImages::frame_of`].
    retry: bool,
    /// Which cache this is. Bumped whenever the cache is emptied, so a read started
    /// before that — which belongs to the record that asked for it — cannot land in the
    /// entry a later request for the same path made.
    generation: u64,
}

/// The transcript's own cache of the local pictures its Markdown points at.
///
/// Shared with the Markdown plugin (which must be `Send + Sync`) rather than owned by a
/// row, because the row that draws a picture is inside the parsed document while the one
/// that reads it is the view.
pub(crate) struct LocalImages {
    inner: Mutex<Inner>,
    /// The folder a relative path is measured from. It lives here because the plugin
    /// that resolves a path cannot see the view that knows the folder.
    folder: Mutex<Option<PathBuf>>,
    /// The reader's home, for a `~/…` path: read once, from the environment.
    home: PathBuf,
    /// The width a picture may be drawn at, as `f32` bits. Written by the view each
    /// frame from the column it is actually given — never from the design's maximum,
    /// which is not what a narrow pane holds.
    width: AtomicU32,
    /// The two budgets, so a test can hold the cache to a small one without decoding
    /// megabytes to fill it.
    max_entries: usize,
    max_bytes: u64,
}

impl LocalImages {
    pub(crate) fn new() -> LocalImages {
        LocalImages::budgeted(MAX_LOCAL_IMAGES, MAX_LOCAL_IMAGE_BYTES)
    }

    /// A cache with budgets of a given size: the tests use a small one, and the view
    /// hands one size to every transcript it makes.
    pub(crate) fn budgeted(max_entries: usize, max_bytes: u64) -> LocalImages {
        LocalImages {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                order: VecDeque::new(),
                wanted: Vec::new(),
                bytes: 0,
                pass: 0,
                retry: false,
                generation: 0,
            }),
            folder: Mutex::new(None),
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            width: AtomicU32::new(0),
            max_entries,
            max_bytes,
        }
    }

    fn held(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The folder a relative path is measured from.
    pub(crate) fn set_folder(&self, folder: Option<PathBuf>) {
        let mut held = self
            .folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *held = folder;
    }

    /// The width a picture may be drawn at: the column's own, so a picture in a narrow
    /// pane is narrow too.
    pub(crate) fn set_width(&self, width: Pixels) {
        self.width
            .store(f32::from(width).max(0.).to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn width(&self) -> Pixels {
        Pixels::from(f32::from_bits(self.width.load(Ordering::Relaxed)))
    }

    /// One frame pass: what a row reaches for from now on is *current*, and an eviction
    /// may not take it.
    pub(crate) fn begin_pass(&self) {
        let mut held = self.held();
        held.pass += 1;
        // A refusal is the frame's own business: what this frame was refused room for is
        // asked about again next frame, if that refusal had anything to gain from it.
        held.retry = false;
    }

    /// The picture `url` names, if this transcript already has it.
    ///
    /// `None` covers every reason to draw the fallback instead — the reference names no
    /// local file, the file could not be read, there was no room to keep it — and the
    /// one case that is not the fallback's own: a picture nobody has started reading
    /// yet. That is recorded here, and the view drains it into a worker. A row only asks
    /// when it is built, so only what is on screen is ever asked for.
    pub(crate) fn frame_of(&self, url: &str) -> Option<Arc<RenderImage>> {
        let path = self.resolve(url)?;
        let mut held = self.held();
        let pass = held.pass;
        if let Some(entry) = held.entries.get_mut(&path) {
            // Whatever this entry holds, the frame laying out is asking for it: it is
            // what the reader is looking at, and an eviction may not take it. A failure
            // is asked for like the rest — the row draws the fallback for it either way,
            // but taking it away only puts the same missing file back in the queue, so a
            // message with more missing references on screen than the cache holds would
            // otherwise read one of them again on every frame, for ever.
            entry.drawn = pass;
            return match &entry.local {
                Local::Ready(image) => Some(image.clone()),
                Local::Loading | Local::Failed => None,
            };
        }
        // Room for one more? What the frame is drawing and what is still in flight are
        // never dropped for it, so a cache full of those has nowhere for a new picture:
        // the row falls back, nothing at all is recorded, and no read is started.
        if held.entries.len() >= self.max_entries && !evict_one(&mut held) {
            // Whether the next frame could do better. Everything held is being drawn
            // right now or is still in flight — the frame after this one takes nothing
            // either — unless something held was last asked for a pass ago and not this
            // one: that is off the screen now, and the next frame may take it. Only then
            // is another frame worth asking for, since a frame that can take nothing
            // would refuse the same reference again, and again after that.
            if held
                .entries
                .values()
                .any(|entry| !matches!(entry.local, Local::Loading) && entry.drawn + 1 == pass)
            {
                held.retry = true;
            }
            return None;
        }
        held.entries.insert(
            path.clone(),
            Entry {
                local: Local::Loading,
                bytes: 0,
                drawn: pass,
            },
        );
        held.wanted.push(path);
        None
    }

    /// Whether a refusal this frame asked for another frame, and clears it: one frame,
    /// one ask.
    ///
    /// A refusal that asked for a frame leaves nothing behind but this — the reference
    /// itself is not held, so the frame it asks for comes back round to it as a new
    /// reference, with a cache whose oldest entries are a pass older than they were.
    pub(crate) fn take_retry(&self) -> bool {
        let mut held = self.held();
        // Later references in this pass may have used every protected entry.
        std::mem::take(&mut held.retry)
            && held
                .entries
                .values()
                .any(|entry| !matches!(entry.local, Local::Loading) && entry.drawn + 1 == held.pass)
    }

    /// The paths rows asked for since this was last called, and the cache generation
    /// they were asked in: what to read next, and what the answer belongs to.
    pub(crate) fn take_wanted(&self) -> (u64, Vec<PathBuf>) {
        let mut held = self.held();
        let wanted = std::mem::take(&mut held.wanted);
        (held.generation, wanted)
    }

    /// What the worker made of one requested path: the decoded frame, or `None` for a
    /// file that is not there, is not a picture, or is one this app will not read.
    ///
    /// An answer older than the cache it was asked in, and one whose entry is no longer
    /// held, are both dropped: a read landing after the record that asked for it is gone
    /// — a replaced topic, another session, another folder — has nothing to belong to,
    /// and must not fill an entry a later request for the same path made.
    ///
    /// Returns whether it changed anything.
    pub(crate) fn finish(
        &self,
        generation: u64,
        path: &Path,
        image: Option<Arc<RenderImage>>,
    ) -> bool {
        let mut held = self.held();
        if generation != held.generation {
            return false;
        }
        let (max_entries, max_bytes) = (self.max_entries, self.max_bytes);
        let pass = held.pass;
        match image {
            None => {
                if let Some(entry) = held.entries.get_mut(path) {
                    entry.local = Local::Failed;
                    entry.bytes = 0;
                    held.order.push_back(path.to_path_buf());
                    return true;
                }
                return false;
            }
            Some(image) => {
                let bytes = decoded_bytes(&image);
                let Some(entry) = held.entries.get_mut(path) else {
                    return false;
                };
                entry.local = Local::Ready(image);
                entry.bytes = bytes;
                // The picture that was just read is what this pass is drawing: an
                // eviction made for it must take something else, or the file would be
                // read again on the next frame, and again on the one after that.
                entry.drawn = pass;
                held.order.push_back(path.to_path_buf());
                held.bytes += bytes;
            }
        }
        while held.entries.len() > max_entries || held.bytes > max_bytes {
            if !evict_one(&mut held) {
                break;
            }
        }
        // The bytes are over the budget and nothing could be dropped for them —
        // everything else is in flight or on screen. The picture is not kept, and the
        // path is not asked for again: the row falls back, which is what a picture too
        // big to hold looks like.
        if held.bytes > max_bytes {
            if let Some(entry) = held.entries.get_mut(path) {
                if entry.bytes > 0 {
                    let freed = entry.bytes;
                    entry.bytes = 0;
                    entry.local = Local::Failed;
                    held.bytes -= freed;
                }
            }
        }
        true
    }

    /// Forget every decoded picture: the record that asked for them is not this one any
    /// more, or the folder they were measured from moved.
    pub(crate) fn clear(&self) {
        let mut held = self.held();
        held.entries.clear();
        held.order.clear();
        held.wanted.clear();
        held.bytes = 0;
        // Every read already on its way belongs to the record that asked for it.
        held.generation += 1;
    }

    /// How many pictures are held, and what they cost — for the tests.
    #[cfg(test)]
    fn held_now(&self) -> (usize, u64) {
        let held = self.held();
        (held.entries.len(), held.bytes)
    }

    /// The path `url` names on this machine, if it names one at all.
    pub(crate) fn resolve(&self, url: &str) -> Option<PathBuf> {
        let folder = self
            .folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        resolve(url, folder.as_deref(), &self.home)
    }
}

/// Drop the oldest entry that is neither in flight nor being drawn this pass. Returns
/// whether anything was dropped: a cache of nothing but current and in-flight pictures
/// is full, and stays full.
///
/// The pass being drawn and the one before it are what the reader is looking at; a
/// picture drawn then may be drawn again before this frame is over, and dropping it would
/// only read it back. A *failure* is protected by the same rule as a picture: nothing is
/// drawn from one, but evicting it puts the same missing file back in the queue, and a
/// frame that shows more missing references than the cache holds would spend its life
/// reading one of them again. A failure that is no longer being asked for is another
/// matter, and goes — it costs nothing to hold and nothing to let go. Every settled path
/// is in the order, failures included, or the entry slots they take would never come
/// back.
fn evict_one(held: &mut Inner) -> bool {
    let pass = held.pass;
    let oldest = held.order.iter().find(|path| {
        held.entries
            .get(*path)
            .is_some_and(|entry| match entry.local {
                Local::Loading => false,
                Local::Failed | Local::Ready(_) => entry.drawn + 1 < pass,
            })
    });
    let Some(oldest) = oldest.cloned() else {
        return false;
    };
    held.order.retain(|path| path != &oldest);
    if let Some(entry) = held.entries.remove(&oldest) {
        held.bytes = held.bytes.saturating_sub(entry.bytes);
    }
    true
}

/// What one decoded frame costs in memory: its pixels, four bytes each.
fn decoded_bytes(image: &RenderImage) -> u64 {
    let size = image.size(0);
    (size.width.0.max(0) as u64) * (size.height.0.max(0) as u64) * 4
}

/// Where one Markdown image reference points, or `None` when it points somewhere this
/// app does not read.
///
/// Pure: `folder` and `home` are the only context, and nothing here touches the disk.
pub(crate) fn resolve(url: &str, folder: Option<&Path>, home: &Path) -> Option<PathBuf> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // `file:///tmp/a.png`, and the same with the host spelled out
    // (`file://localhost/…`), which is the same file: what follows is the path.
    if let Some(rest) = strip_scheme(url, "file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let path = PathBuf::from(decode_percent(rest));
        return path.is_absolute().then_some(path);
    }
    if has_scheme(url) {
        return None;
    }
    if let Some(rest) = url.strip_prefix("~/") {
        return (!home.as_os_str().is_empty()).then(|| home.join(rest));
    }
    let path = Path::new(url);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    folder.map(|folder| folder.join(path))
}

/// The bytes a URL escapes, decoded: `%20` is a space, which is what a path written as
/// a URL has instead of one.
///
/// Only the escapes a path needs are decoded; an escape that is not two hex digits is
/// left as it was written, since a file name may hold a bare `%`.
fn decode_percent(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `url` without the given scheme, case-insensitively.
fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let head = url.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme)
        .then(|| &url[scheme.len()..])
}

/// Whether the reference names a scheme at all — `http:`, `data:`, `mailto:` and
/// anything else a URL can start with that is not a path this app reads.
fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/home/reader")
    }

    fn frame(width: u32, height: u32) -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![::image::Frame::new(
            ::image::RgbaImage::new(width, height),
        )]))
    }

    #[test]
    fn a_file_url_with_escapes_is_the_path_they_stand_for() {
        assert_eq!(
            resolve("file:///tmp/a%20b%2Bc.png", None, &home()),
            Some(PathBuf::from("/tmp/a b+c.png"))
        );
        // An escape that is not one stays as it was written: a file name may hold `%`.
        assert_eq!(
            resolve("file:///tmp/100%.png", None, &home()),
            Some(PathBuf::from("/tmp/100%.png"))
        );
    }

    #[test]
    fn a_file_url_is_the_path_it_names() {
        assert_eq!(
            resolve("file:///tmp/shot.png", None, &home()),
            Some(PathBuf::from("/tmp/shot.png"))
        );
        assert_eq!(
            resolve("file://localhost/tmp/shot.png", None, &home()),
            Some(PathBuf::from("/tmp/shot.png"))
        );
        // A URL with a host is a file on that host, not one here.
        assert_eq!(resolve("file://shot.png", None, &home()), None);
    }

    #[test]
    fn an_absolute_path_stands_as_it_is() {
        assert_eq!(
            resolve("/var/tmp/a.png", Some(Path::new("/work")), &home()),
            Some(PathBuf::from("/var/tmp/a.png"))
        );
    }

    #[test]
    fn a_relative_path_is_measured_from_the_folder() {
        assert_eq!(
            resolve("shots/a.png", Some(Path::new("/work")), &home()),
            Some(PathBuf::from("/work/shots/a.png"))
        );
        assert_eq!(
            resolve("./a.png", Some(Path::new("/work")), &home()),
            Some(PathBuf::from("/work/./a.png"))
        );
        // Without a folder there is nothing to measure from, and no guess is made.
        assert_eq!(resolve("shots/a.png", None, &home()), None);
    }

    #[test]
    fn a_tilde_is_the_readers_home() {
        assert_eq!(
            resolve("~/Pictures/a.png", None, &home()),
            Some(PathBuf::from("/home/reader/Pictures/a.png"))
        );
        // A home the platform did not name resolves nothing.
        assert_eq!(resolve("~/a.png", None, Path::new("")), None);
    }

    #[test]
    fn a_reference_this_app_does_not_read_names_nothing() {
        for url in [
            "https://example.com/a.png",
            "http://example.com/a.png",
            "data:image/png;base64,iVBORw0KGgo=",
            "mailto:someone@example.com",
            "a.png:1",
            "",
            "   ",
        ] {
            assert_eq!(
                resolve(url, Some(Path::new("/work")), &home()),
                None,
                "{url}"
            );
        }
    }

    #[test]
    fn one_picture_is_decoded_once_and_drawn_by_everyone_who_names_it() {
        let images = LocalImages::new();
        let path = PathBuf::from("/tmp/first.png");
        let picture = frame(4, 4);
        // Nobody has asked yet: the first look asks, and draws the fallback.
        assert!(images.frame_of("file:///tmp/first.png").is_none());
        assert_eq!(images.take_wanted(), (0, vec![path.clone()]));
        // Asked twice before it lands is asked once.
        assert!(images.frame_of("file:///tmp/first.png").is_none());
        assert!(images.take_wanted().1.is_empty());

        images.finish(0, &path, Some(picture.clone()));
        let drawn = images
            .frame_of("file:///tmp/first.png")
            .expect("the picture");
        assert!(Arc::ptr_eq(&drawn, &picture));
        assert!(images.take_wanted().1.is_empty(), "nothing more to read");
    }

    #[test]
    fn a_picture_that_could_not_be_read_is_a_failure_that_stays_said() {
        let images = LocalImages::new();
        assert!(images.frame_of("/tmp/gone.png").is_none());
        assert_eq!(
            images.take_wanted(),
            (0, vec![PathBuf::from("/tmp/gone.png")])
        );
        images.finish(0, Path::new("/tmp/gone.png"), None);
        // The file appearing later is not looked for again: the row falls back and stays
        // fallen back.
        assert!(images.frame_of("/tmp/gone.png").is_none());
        assert!(images.take_wanted().1.is_empty());
    }

    #[test]
    fn the_cache_holds_its_budget_and_drops_the_oldest_decoded_first() {
        let images = LocalImages::budgeted(3, u64::MAX);
        let paths: Vec<PathBuf> = (0..5)
            .map(|n| PathBuf::from(format!("/tmp/{n}.png")))
            .collect();
        for path in &paths {
            // A frame, then the read the view would start.
            images.begin_pass();
            assert!(images.frame_of(&path.display().to_string()).is_none());
            images.finish(0, path, Some(frame(4, 4)));
            assert_eq!(images.take_wanted(), (0, vec![path.clone()]));
        }
        let (entries, bytes) = images.held_now();
        assert_eq!((entries, bytes), (3, 192), "the entry budget is the budget");
        for gone in &paths[..2] {
            images.begin_pass();
            assert!(
                images.frame_of(&gone.display().to_string()).is_none(),
                "{gone:?} was evicted"
            );
            // An evicted picture is read once more, which is the cost of eviction —
            // one read, not one per frame.
            assert_eq!(images.take_wanted(), (0, vec![gone.clone()]));
        }
        images.begin_pass();
        assert!(images.frame_of(&paths[4].display().to_string()).is_some());
    }

    #[test]
    fn the_bytes_budget_drops_the_oldest_the_same_way() {
        // A 4×4 frame is 64 bytes decoded: a 100-byte budget holds one of them.
        let images = LocalImages::budgeted(MAX_LOCAL_IMAGES, 100);
        let (a, b) = (PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png"));
        images.frame_of("/tmp/a.png");
        images.finish(0, &a, Some(frame(4, 4)));
        // Frames go by: the first picture is no longer being drawn.
        for _ in 0..3 {
            images.begin_pass();
        }
        images.frame_of("/tmp/b.png");
        images.finish(0, &b, Some(frame(4, 4)));
        let (entries, bytes) = images.held_now();
        assert_eq!((entries, bytes), (1, 64), "one frame of 4×4 RGBA fits");
        assert!(images.frame_of("/tmp/b.png").is_some(), "the newest stands");
    }

    #[test]
    fn a_picture_being_drawn_is_not_dropped_for_the_one_beside_it() {
        // A message with more pictures on screen than the cache holds: the ones the
        // reader is looking at are not dropped to make room for the one next to them —
        // that would evict one, decode it back, and do it again every frame.
        let images = LocalImages::budgeted(2, u64::MAX);
        let (a, b, c) = (
            PathBuf::from("/tmp/a.png"),
            PathBuf::from("/tmp/b.png"),
            PathBuf::from("/tmp/c.png"),
        );
        images.begin_pass();
        for path in [&a, &b] {
            images.frame_of(&path.display().to_string());
            images.finish(0, path, Some(frame(4, 4)));
            let _ = images.take_wanted();
        }
        images.begin_pass();
        assert!(images.frame_of("/tmp/a.png").is_some(), "still drawn");
        assert!(images.frame_of("/tmp/b.png").is_some(), "still drawn");
        // The third has nowhere to go: it is not read, and nothing is dropped for it.
        assert!(images.frame_of("/tmp/c.png").is_none());
        assert!(images.take_wanted().1.is_empty(), "not read");
        assert!(images.frame_of("/tmp/a.png").is_some());
        assert!(images.frame_of("/tmp/b.png").is_some());

        // Two passes later neither is being drawn, and the third takes one's place.
        for _ in 0..2 {
            images.begin_pass();
        }
        assert!(images.frame_of("/tmp/c.png").is_none());
        assert_eq!(
            images.take_wanted().1,
            vec![c.clone()],
            "read once there is room"
        );
        // The one the reader is still looking at stayed; the oldest went, and is not
        // read back while there is no room for it.
        assert!(
            images.frame_of("/tmp/b.png").is_some(),
            "the picture being drawn stays"
        );
        assert!(images.frame_of("/tmp/a.png").is_none(), "the oldest went");
        assert!(images.take_wanted().1.is_empty(), "and is not read again");
        images.finish(0, &c, Some(frame(4, 4)));
        assert!(images.frame_of("/tmp/c.png").is_some());
        assert_eq!(images.held_now().0, 2, "inside the budget");
    }

    #[test]
    fn the_picture_being_drawn_and_the_one_still_loading_are_never_evicted() {
        // A budget of one entry, with the one on screen and a second in flight: the
        // newest has nowhere to go, so it is not read at all.
        let images = LocalImages::budgeted(1, u64::MAX);
        images.frame_of("/tmp/on-screen.png");
        assert_eq!(
            images.take_wanted(),
            (0, vec![PathBuf::from("/tmp/on-screen.png")])
        );
        images.finish(0, Path::new("/tmp/on-screen.png"), Some(frame(4, 4)));
        images.begin_pass();
        assert!(
            images.frame_of("/tmp/on-screen.png").is_some(),
            "drawn this pass"
        );
        assert!(images.frame_of("/tmp/loading.png").is_none());
        assert!(
            images.take_wanted().1.is_empty(),
            "a full cache of current and in-flight pictures reads nothing more"
        );
        assert!(
            images.frame_of("/tmp/loading.png").is_none(),
            "and it is not asked for again"
        );
        assert!(
            images.frame_of("/tmp/on-screen.png").is_some(),
            "still there"
        );
    }

    #[test]
    fn a_picture_too_big_for_the_whole_budget_falls_back_and_stays_that_way() {
        let images = LocalImages::budgeted(MAX_LOCAL_IMAGES, 1024);
        images.frame_of("/tmp/huge.png");
        assert_eq!(
            images.take_wanted(),
            (0, vec![PathBuf::from("/tmp/huge.png")])
        );
        images.finish(0, Path::new("/tmp/huge.png"), Some(frame(64, 64)));
        let (entries, bytes) = images.held_now();
        assert_eq!(
            (entries, bytes),
            (1, 0),
            "kept as a failure, costing nothing"
        );
        images.frame_of("/tmp/huge.png");
        assert!(images.take_wanted().1.is_empty(), "not read a second time");
    }

    #[test]
    fn failures_do_not_fill_the_cache_for_good() {
        // Every one of these paths is missing, and each is asked about once.
        let images = LocalImages::budgeted(4, u64::MAX);
        let missing: Vec<PathBuf> = (0..4)
            .map(|n| PathBuf::from(format!("/tmp/missing-{n}.png")))
            .collect();
        for path in &missing {
            images.begin_pass();
            images.frame_of(&path.display().to_string());
            images.finish(0, path, None);
            let _ = images.take_wanted();
        }
        assert_eq!(images.held_now().0, 4, "four failures, four entries");

        // A real picture, read: a failure is not worth a slot that a picture could
        // have, so one of them goes rather than the picture being refused.
        images.begin_pass();
        let good = PathBuf::from("/tmp/good.png");
        assert!(images.frame_of("/tmp/good.png").is_none());
        assert_eq!(
            images.take_wanted(),
            (0, vec![good.clone()]),
            "the picture is read"
        );
        images.finish(0, &good, Some(frame(4, 4)));
        assert!(
            images.frame_of("/tmp/good.png").is_some(),
            "and it is drawn"
        );
        assert_eq!(images.held_now().0, 4, "still inside the budget");
    }

    #[test]
    fn a_frame_of_missing_files_that_is_still_does_not_read_one_of_them_again() {
        // A pane showing two references that are not there, and a third with nowhere to
        // go: the two held ones are asked for by name every frame, the third is refused
        // every frame — and not one of them is read again, however many frames go by.
        let images = LocalImages::budgeted(2, u64::MAX);
        // Both on screen in one pass, and both read: neither is there.
        images.begin_pass();
        for path in ["/tmp/a.png", "/tmp/b.png"] {
            assert!(images.frame_of(path).is_none());
            images.finish(0, Path::new(path), None);
        }
        assert_eq!(images.take_wanted().1.len(), 2, "each read once");
        assert_eq!(images.held_now().0, 2, "two failures, two entries");

        for pass in 3..8 {
            images.begin_pass();
            // The refused reference may be laid out before the cached ones.
            for path in ["/tmp/c.png", "/tmp/a.png", "/tmp/b.png"] {
                assert!(images.frame_of(path).is_none(), "pass {pass}");
            }
            assert!(
                images.take_wanted().1.is_empty(),
                "pass {pass}: nothing is read again"
            );
            assert!(
                !images.take_retry(),
                "pass {pass}: and no frame is asked for either"
            );
        }
    }

    #[test]
    fn the_references_a_later_pane_shows_are_read_without_anything_else_happening() {
        // A cache of two holding two failures that were on screen a pass ago and are not
        // asked for now: the two references the pane shows instead are refused room on
        // the frame they appear, because what they need is held for one more pass. That
        // frame asks for another, and the next one takes them — nobody has to scroll,
        // nothing else has to arrive.
        let images = LocalImages::budgeted(2, u64::MAX);
        let (a, b, c, d) = (
            PathBuf::from("/tmp/a.png"),
            PathBuf::from("/tmp/b.png"),
            PathBuf::from("/tmp/c.png"),
            PathBuf::from("/tmp/d.png"),
        );
        images.begin_pass();
        for path in [&a, &b] {
            images.frame_of(&path.display().to_string());
            images.finish(0, path, None);
        }
        let _ = images.take_wanted();

        // The pane moves on: neither of the two is asked for, and there is room for
        // neither of the two that are.
        images.begin_pass();
        assert!(images.frame_of("/tmp/c.png").is_none());
        assert!(images.frame_of("/tmp/d.png").is_none());
        assert!(images.take_wanted().1.is_empty(), "nothing is read yet");
        assert!(images.take_retry(), "the frame asks for one more");
        assert!(!images.take_retry(), "asked for once, not once a frame");

        // The frame it asked for: what is held is a pass older, and both fit.
        images.begin_pass();
        assert!(images.frame_of("/tmp/c.png").is_none());
        assert!(images.frame_of("/tmp/d.png").is_none());
        assert_eq!(
            images.take_wanted().1,
            vec![c.clone(), d.clone()],
            "both are read"
        );
        assert!(!images.take_retry(), "and there is nothing more to ask for");
        images.finish(0, &c, Some(frame(4, 4)));
        images.finish(0, &d, None);

        // Settled: the picture is drawn, the missing file falls back, and neither is
        // read again.
        images.begin_pass();
        assert!(images.frame_of("/tmp/c.png").is_some(), "drawn");
        assert!(images.frame_of("/tmp/d.png").is_none(), "fallen back");
        assert!(images.take_wanted().1.is_empty(), "nothing more to read");
        assert!(!images.take_retry());
        assert_eq!(images.held_now().0, 2, "inside the budget");
    }

    #[test]
    fn a_read_from_before_a_replacement_cannot_fill_the_new_records_entry() {
        let images = LocalImages::new();
        let path = PathBuf::from("/tmp/a.png");
        // A read started for one record…
        images.frame_of("/tmp/a.png");
        let (generation, wanted) = images.take_wanted();
        assert_eq!(wanted, vec![path.clone()]);
        // …and the record is replaced, then asks for the same path again — the entry
        // this request made is not the one the old read belongs to.
        images.clear();
        images.begin_pass();
        images.frame_of("/tmp/a.png");
        let (now, wanted) = images.take_wanted();
        assert_eq!(wanted, vec![path.clone()]);
        assert_ne!(now, generation, "a new cache, a new generation");

        assert!(
            !images.finish(generation, &path, Some(frame(4, 4))),
            "too old"
        );
        assert!(
            images.frame_of("/tmp/a.png").is_none(),
            "the new record's picture is still on its way"
        );
        assert!(
            images.finish(now, &path, Some(frame(4, 4))),
            "this one belongs"
        );
        assert!(images.frame_of("/tmp/a.png").is_some());
    }

    #[test]
    fn a_replaced_record_takes_its_pictures_with_it() {
        let images = LocalImages::new();
        let path = PathBuf::from("/tmp/a.png");
        // A read is in flight when the record it was asked for is replaced.
        images.frame_of("/tmp/a.png");
        assert_eq!(images.take_wanted(), (0, vec![path.clone()]));
        images.clear();
        // The read lands afterwards and belongs to no record: nothing is put back, and
        // the next look — of the record that replaced it — asks afresh.
        assert!(
            !images.finish(0, &path, Some(frame(4, 4))),
            "too old to land"
        );
        assert!(images.frame_of("/tmp/a.png").is_none());
        assert_eq!(images.take_wanted(), (1, vec![path]));
    }
}
