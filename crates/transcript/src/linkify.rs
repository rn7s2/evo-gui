//! What in a row's text is a link: a web address, or a path that is really there.
//!
//! A transcript is written by a model, about the machine: it is full of addresses
//! (`https://…`) and paths (`/tmp/…`, `~/.evo/state.json`, `crates/transcript/src/
//! lib.rs:42`). The kit already links what Markdown links and what GFM autolinks, so
//! the job here is the rest: a bare path in prose, and — for the rows the app draws as
//! plain text rather than as Markdown — the addresses too.
//!
//! Two passes, one rule each:
//!
//! * [`prose`] rewrites a **Markdown** source, leaving everything that is not prose
//!   alone: a fenced code block, an inline code span, a link's own tail and HTML are
//!   copied byte for byte. A path inside an inline code span *is* linked — the reader
//!   is meant to be able to press it — by wrapping the span as
//!   `` [`/tmp/x`](</tmp/x>) ``, so it still draws as code.
//! * [`literal`] turns **plain text** — a user's message, a notice line — into the
//!   Markdown that draws exactly the same characters ([`escape`]) with its addresses
//!   and paths wrapped as links.
//!
//! Both ask one question of the file system, through [`Paths`]: does this token point
//! at something? The answer is remembered per token for [`TTL`], so a frame costs no
//! `stat` for a path it has already looked at — the reader's own rows are rebuilt every
//! frame they are on screen, and a `stat` per token per frame would put the disk in the
//! middle of every scroll.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one look at the file system stands before it is taken again.
///
/// A path that appears while the reader is looking at the row is linked at the next
/// frame after this much; a path that is looked at twice inside it is looked at once.
const TTL: Duration = Duration::from_secs(3);

/// The most tokens one `Paths` remembers before it forgets the lot: a transcript can be
/// long, and what each token resolved to is not worth keeping forever.
const REMEMBERED: usize = 512;

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A web address: the platform's browser opens it.
    Web(String),
    /// A file or a folder that was there when the row was built.
    Path(PathBuf),
}

/// Whether the app opens `url` as a web address.
///
/// Only `http`, `https` and `mailto`, case-insensitively: `file:`, `javascript:` and an
/// unknown scheme are not something this app hands to the operating system.
pub(crate) fn web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

/// What a link's own href names, if the app may open it.
///
/// A web address is taken as it is written; anything else has to be an absolute path
/// ([`prose`] and [`literal`] resolve a relative one when they write the link), and has
/// to be there now — a row can outlive the file it names.
pub(crate) fn target(href: &str) -> Option<Target> {
    if web(href) {
        return Some(Target::Web(href.to_string()));
    }
    let path = PathBuf::from(href);
    (path.is_absolute() && path.exists()).then_some(Target::Path(path))
}

/// The file system a transcript looks at: the folder a relative path is measured from,
/// the reader's home, and what each token resolved to the last time it was looked at.
#[derive(Debug)]
pub(crate) struct Paths {
    /// `~`, from the environment. Empty when the platform does not say, in which case a
    /// token that starts with `~` is not resolved at all.
    home: PathBuf,
    /// The project folder the tab runs in.
    folder: Mutex<Option<PathBuf>>,
    /// What the tokens looked at so far resolved to, and when.
    seen: Mutex<HashMap<String, Seen>>,
}

#[derive(Clone, Debug)]
struct Seen {
    path: Option<PathBuf>,
    at: Instant,
}

impl Default for Paths {
    fn default() -> Self {
        Paths::new()
    }
}

impl Paths {
    pub(crate) fn new() -> Self {
        Paths {
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            folder: Mutex::new(None),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// The folder relative paths are measured from. Returns whether it changed: a
    /// transcript that has just learned its folder has to be read again.
    pub(crate) fn set_folder(&self, folder: Option<PathBuf>) -> bool {
        let mut held = self
            .folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *held == folder {
            return false;
        }
        *held = folder;
        self.forget();
        true
    }

    pub(crate) fn folder(&self) -> Option<PathBuf> {
        self.folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Forget what every token resolved to. A folder that changed moves every relative
    /// path in the record, so none of the old answers stand.
    pub(crate) fn forget(&self) {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// The Markdown source of one row's prose, with the paths in it that are there
    /// turned into links. See [`prose`].
    pub(crate) fn prose(&self, source: &str) -> String {
        prose(source, self)
    }

    /// The Markdown source of one row's plain text: the same characters, with the
    /// addresses and paths in them turned into links. See [`literal`].
    pub(crate) fn literal(&self, text: &str) -> String {
        literal(text, self)
    }

    /// Where `token` points, if something is there.
    ///
    /// The look is remembered for [`TTL`]: the caller is a frame, and a frame that asks
    /// twice about the same token in the same breath must not ask the disk twice.
    pub(crate) fn resolve(&self, token: &str) -> Option<PathBuf> {
        if token.is_empty() {
            return None;
        }
        let now = Instant::now();
        {
            let seen = self
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(seen) = seen.get(token) {
                if now.saturating_duration_since(seen.at) < TTL {
                    return seen.path.clone();
                }
            }
        }
        let path = self.look(token);
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if seen.len() >= REMEMBERED {
            seen.clear();
        }
        seen.insert(
            token.to_string(),
            Seen {
                path: path.clone(),
                at: now,
            },
        );
        path
    }

    /// The one look at the file system: `~` for the reader's home, a relative token
    /// against the tab's folder.
    fn look(&self, token: &str) -> Option<PathBuf> {
        let path = if let Some(rest) = token.strip_prefix("~/") {
            if self.home.as_os_str().is_empty() {
                return None;
            }
            self.home.join(rest)
        } else if token == "~" {
            if self.home.as_os_str().is_empty() {
                return None;
            }
            self.home.clone()
        } else {
            let path = Path::new(token);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.folder()?.join(path)
            }
        };
        path.exists().then_some(path)
    }
}

/// A link found in a row's text: the bytes it covers, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    /// The Markdown destination: a web address as written, or the path a token resolved
    /// to (absolute, so pressing it needs no folder).
    href: String,
}

/// The Markdown source of one row's prose, with every path that is there turned into a
/// link.
///
/// What is not prose is copied as it was: a fenced block, an indented block, an inline
/// code span (unless the span itself is a path), a link's own label and tail, an
/// autolink and an HTML tag.
pub(crate) fn prose(source: &str, paths: &Paths) -> String {
    let spans = scan(source, true, paths);
    if spans.is_empty() {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len() + 32 * spans.len());
    let mut at = 0;
    for span in spans {
        out.push_str(&source[at..span.start]);
        out.push('[');
        out.push_str(&source[span.start..span.end]);
        out.push_str("](");
        out.push_str(&destination(&span.href));
        out.push(')');
        at = span.end;
    }
    out.push_str(&source[at..]);
    out
}

/// The Markdown source of one row's **plain text**: the same characters the reader is
/// looking at, with the addresses and paths in them turned into links.
pub(crate) fn literal(text: &str, paths: &Paths) -> String {
    let spans = scan(text, false, paths);
    let mut out = String::with_capacity(text.len() + 16 * spans.len());
    let mut at = 0;
    for span in spans {
        out.push_str(&plain_source(&text[at..span.start]));
        out.push('[');
        out.push_str(&label(&text[span.start..span.end]));
        out.push_str("](");
        out.push_str(&destination(&span.href));
        out.push(')');
        at = span.end;
    }
    out.push_str(&plain_source(&text[at..]));
    out
}

/// One line of plain text, as the Markdown that draws it: everything that means
/// something to a parser is escaped, and a line break is a hard break rather than a new
/// paragraph — the text is the text, on the lines it was written on.
fn plain_source(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            // A backslash at the end of a line is Markdown's hard break: the lines stay
            // one paragraph, so nothing gains the air of a paragraph break.
            out.push_str("\\\n");
        }
        first = false;
        for character in line.chars() {
            if character.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(character);
        }
    }
    out
}

/// The text of a link, escaped so that a label draws exactly the characters it holds.
fn label(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '[' | ']' | '`' | '*' | '_' | '<' | '>' | '&'
        ) {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

/// A Markdown destination: a web address as it is written, a path inside the angle
/// brackets a path with spaces needs.
fn destination(href: &str) -> String {
    if web(href) {
        return href.to_string();
    }
    let mut out = String::with_capacity(href.len() + 2);
    out.push('<');
    for character in href.chars() {
        if matches!(character, '\\' | '<' | '>') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('>');
    out
}

/// Every link in `text`, in order.
///
/// `prose` says what the text is: a Markdown source, where code and links are not
/// prose, or a plain line, where everything is.
fn scan(text: &str, prose: bool, paths: &Paths) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < text.len() {
        if prose && line_start {
            if let Some(end) = fenced_block(text, i) {
                i = end;
                line_start = false;
                continue;
            }
            if let Some(end) = indented_line(text, i) {
                i = end;
                line_start = false;
                continue;
            }
        }
        if prose {
            match bytes[i] {
                b'`' => {
                    if let Some((end, span)) = code_span(text, i, paths) {
                        if let Some(span) = span {
                            spans.push(span);
                        }
                        i = end;
                        line_start = false;
                        continue;
                    }
                }
                b'<' => {
                    i = html_tag(text, i).unwrap_or(i + 1);
                    line_start = false;
                    continue;
                }
                b'[' | b'!' => {
                    if let Some(end) = link_tail(text, i) {
                        i = end;
                        line_start = false;
                        continue;
                    }
                }
                _ => {}
            }
        }
        if starts_a_token(text, i) {
            if let Some((end, href)) = web_at(text, i).or_else(|| path_at(text, i, paths)) {
                spans.push(Span {
                    start: i,
                    end,
                    href,
                });
                i = end;
                line_start = false;
                continue;
            }
        }
        line_start = bytes[i] == b'\n';
        i += next_char(text, i);
    }
    spans
}

/// Whether a token may start at `i`: not in the middle of a word, a path or an address.
fn starts_a_token(text: &str, i: usize) -> bool {
    match text[..i].chars().next_back() {
        None => true,
        Some(previous) => {
            !previous.is_alphanumeric()
                && !matches!(previous, '_' | '.' | '-' | '/' | '~' | '%' | '@' | ':')
        }
    }
}

/// The width of the character at `i`.
fn next_char(text: &str, i: usize) -> usize {
    text[i..].chars().next().map(char::len_utf8).unwrap_or(1)
}

/// What may be inside a path token.
fn path_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '.' | '-' | '/' | '~' | '+')
}

/// The web address starting at `i`, and where it ends.
///
/// Only `http` and `https`: an address a reader can press must be one a browser opens.
/// The trailing punctuation a sentence leaves on an address is not part of it — a full
/// stop, a comma, a closing bracket that does not match an opening one, and the
/// emphasis marks GFM strips as well.
fn web_at(text: &str, i: usize) -> Option<(usize, String)> {
    let rest = &text[i..];
    let lower = rest.get(..8)?.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let mut end = i;
    for character in rest.chars() {
        if character.is_whitespace() || matches!(character, '<' | '>' | '"' | '`') {
            break;
        }
        end += character.len_utf8();
    }
    end = trim_url_end(text, i, end);
    (end > i).then(|| (end, text[i..end].to_string()))
}

/// Where an address ends once a sentence's punctuation is given back to the sentence.
fn trim_url_end(text: &str, start: usize, mut end: usize) -> usize {
    loop {
        let Some(last) = text[start..end].chars().next_back() else {
            return end;
        };
        let drop = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' | '_' | '~' => true,
            ')' => {
                let body = &text[start..end];
                body.matches(')').count() > body.matches('(').count()
            }
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= last.len_utf8();
    }
}

/// The path starting at `i`, if the file system has one.
///
/// The path a reader writes is the path a tool takes: `/tmp/x`, `~/.evo/state.json`,
/// `crates/transcript/src/lib.rs`. A `:12` or `:12:5` after it names a place *in* the
/// file: the link covers what is written and opens the file.
fn path_at(text: &str, i: usize, paths: &Paths) -> Option<(usize, String)> {
    let mut end = i;
    for character in text[i..].chars() {
        if !path_char(character) {
            break;
        }
        end += character.len_utf8();
    }
    if end == i {
        return None;
    }
    // The line and the column are part of what the reader sees, never of the path.
    let label_end = line_suffix(text, end);
    let mut token = &text[i..end];
    loop {
        if !looks_like_a_path(token) {
            return None;
        }
        if let Some(path) = paths.resolve(token) {
            return Some((label_end, path.to_string_lossy().into_owned()));
        }
        // A sentence's punctuation may have been swallowed by the token: give it back
        // and look again.
        let trimmed = token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'']);
        if trimmed.len() == token.len() || trimmed.is_empty() {
            return None;
        }
        token = trimmed;
    }
}

/// The end of a `:12` or `:12:5` after a path, which is where the link's text ends.
fn line_suffix(text: &str, from: usize) -> usize {
    let mut end = from;
    let mut rest = &text[end..];
    let mut parts = 0;
    while parts < 2 {
        let Some(colon) = rest.strip_prefix(':') else {
            break;
        };
        let digits = colon.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            break;
        }
        end += 1 + digits;
        rest = &text[end..];
        parts += 1;
    }
    end
}

/// Whether a token is worth looking for on disk at all.
///
/// A path names a place: it is absolute, it starts at home, it has a folder in it, or
/// it ends in an extension. A bare word, an address and a word the reader wrote in
/// passing are none of those, and are never looked up.
fn looks_like_a_path(token: &str) -> bool {
    if token.is_empty() || token.contains("://") || token.contains('@') {
        return false;
    }
    if token.starts_with('/') || token.starts_with("~/") || token.starts_with("~") {
        return token != "~";
    }
    if token.starts_with("./") || token.starts_with("../") || token.contains('/') {
        return !token.ends_with("//");
    }
    match token.rsplit_once('.') {
        Some((name, extension)) => {
            !name.is_empty()
                && !extension.is_empty()
                && extension.len() <= 8
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// A fenced code block starting at a line start: where it ends.
fn fenced_block(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    let marker = if rest.starts_with("```") {
        "```"
    } else if rest.starts_with("~~~") {
        "~~~"
    } else {
        return None;
    };
    let mut end = i;
    let mut lines = text[i..].split_inclusive('\n');
    lines.next();
    for line in lines {
        end += line.len();
        if line.trim_start().starts_with(marker) {
            return Some(end);
        }
    }
    Some(text.len())
}

/// An indented code block's line: four spaces of indent, or a tab, at a line start.
fn indented_line(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    if !(rest.starts_with("    ") || rest.starts_with('\t')) {
        return None;
    }
    let end = rest.find('\n').map(|end| i + end + 1).unwrap_or(text.len());
    Some(end)
}

/// An inline code span: where it ends, and the link it makes if its own text is a path.
fn code_span(text: &str, i: usize, paths: &Paths) -> Option<(usize, Option<Span>)> {
    let rest = &text[i..];
    let ticks = rest.chars().take_while(|c| *c == '`').count();
    if ticks == 0 {
        return None;
    }
    let opener = "`".repeat(ticks);
    let body_start = i + ticks;
    let close = text[body_start..].find(&opener)?;
    let end = body_start + close + ticks;
    let content = &text[body_start..body_start + close];
    let span = paths.resolve(content.trim()).map(|path| Span {
        start: i,
        end,
        href: path.to_string_lossy().into_owned(),
    });
    Some((end, span))
}

/// An HTML tag or comment: where it ends.
fn html_tag(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    if rest.starts_with("<!--") {
        return rest.find("-->").map(|end| i + end + 3);
    }
    let mut end = i;
    for character in rest.chars() {
        end += character.len_utf8();
        if character == '>' {
            return Some(end);
        }
        if character == '\n' {
            return None;
        }
    }
    None
}

/// The tail of a link or an image — `](destination "title")` — which is Markdown, not
/// prose: the file it names is not a path in a sentence.
fn link_tail(text: &str, i: usize) -> Option<usize> {
    let rest = &text[i..];
    let close = rest.find(']')?;
    let after = &rest[close + 1..];
    if !after.starts_with('(') && !after.starts_with('[') {
        return None;
    }
    if after.starts_with('[') {
        // A reference link: the label is prose, the definition elsewhere is not.
        return after.find(']').map(|end| i + close + 1 + end + 1);
    }
    let mut depth = 0;
    for (offset, character) in after.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + close + 1 + offset + 1);
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file system made of names, with a home and a folder of its own: the tests are
    /// about what a transcript does with a path, not about the disk.
    struct Disk {
        paths: Paths,
        home: PathBuf,
        folder: PathBuf,
    }

    impl Disk {
        fn new() -> Disk {
            let home = PathBuf::from("/home/reader");
            let folder = PathBuf::from("/work/project");
            let paths = Paths {
                home: home.clone(),
                folder: Mutex::new(Some(folder.clone())),
                seen: Mutex::new(HashMap::new()),
            };
            Disk {
                paths,
                home,
                folder,
            }
        }

        /// A file or a folder that is there.
        fn has(&self, path: &str) -> &Self {
            self.put(path, true);
            self
        }

        fn put(&self, path: &str, there: bool) {
            let key = path.to_string();
            let full = self.full(&key);
            let mut seen = self.paths.seen.lock().unwrap();
            seen.insert(
                key,
                Seen {
                    path: there.then_some(full),
                    at: Instant::now(),
                },
            );
        }

        fn full(&self, token: &str) -> PathBuf {
            if let Some(rest) = token.strip_prefix("~/") {
                self.home.join(rest)
            } else if token.starts_with('/') {
                PathBuf::from(token)
            } else {
                self.folder.join(token)
            }
        }
    }

    /// The hrefs of the links found in `text`.
    fn links(text: &str, disk: &Disk) -> Vec<(String, String)> {
        scan(text, true, &disk.paths)
            .into_iter()
            .map(|span| (text[span.start..span.end].to_string(), span.href))
            .collect()
    }

    /// A path that is there is a link; one that is not stays text.
    #[test]
    fn a_path_is_a_link_only_when_it_is_there() {
        let disk = Disk::new();
        disk.has("/tmp/evo/report.md");
        assert_eq!(
            links("written to /tmp/evo/report.md just now", &disk),
            [(
                "/tmp/evo/report.md".to_string(),
                "/tmp/evo/report.md".to_string()
            )]
        );
        assert!(links("written to /tmp/evo/gone.md just now", &disk).is_empty());
    }

    /// `~` is the reader's home, and a relative path is the tab's folder.
    #[test]
    fn home_and_the_project_folder_are_where_a_path_starts() {
        let disk = Disk::new();
        disk.has("~/.evo/settings.json");
        disk.has("crates/transcript/src/lib.rs");
        assert_eq!(
            links("see ~/.evo/settings.json", &disk)[0].1,
            "/home/reader/.evo/settings.json"
        );
        assert_eq!(
            links("see crates/transcript/src/lib.rs:42", &disk)[0].1,
            "/work/project/crates/transcript/src/lib.rs"
        );
        // The line is part of what the reader sees, not of the path: one line, or a
        // line and a column, is the file the link names.
        assert_eq!(
            links("see crates/transcript/src/lib.rs:42", &disk)[0].0,
            "crates/transcript/src/lib.rs:42"
        );
        assert_eq!(
            links("see crates/transcript/src/lib.rs:42:7 for the rest", &disk)[0].0,
            "crates/transcript/src/lib.rs:42:7"
        );
        assert_eq!(
            links("see crates/transcript/src/lib.rs:42:7", &disk)[0].1,
            "/work/project/crates/transcript/src/lib.rs"
        );
    }

    /// A bare name is not looked up and a sentence's punctuation is not swallowed.
    #[test]
    fn a_sentence_keeps_its_punctuation() {
        let disk = Disk::new();
        disk.has("src/main.rs");
        disk.has("state");
        assert_eq!(
            links("read src/main.rs, then stop.", &disk)[0].0,
            "src/main.rs"
        );
        assert!(links("the state of the world", &disk).is_empty());
        assert!(links("e.g. this one", &disk).is_empty());
    }

    /// A web address is a link, and the sentence's own punctuation is given back.
    #[test]
    fn a_web_address_leaves_its_full_stop_behind() {
        let disk = Disk::new();
        assert_eq!(
            links("see https://evo.dev/state.json. Then press it", &disk)[0],
            (
                "https://evo.dev/state.json".to_string(),
                "https://evo.dev/state.json".to_string()
            )
        );
        // A bracket that closes the address is part of it; one that closes a word in the
        // sentence is not.
        assert_eq!(
            links("(see https://en.wikipedia.org/wiki/Foo_(bar))", &disk)[0].0,
            "https://en.wikipedia.org/wiki/Foo_(bar)"
        );
        assert_eq!(
            links("(see https://evo.dev/x)", &disk)[0].0,
            "https://evo.dev/x"
        );
    }

    /// Code tells the truth about itself: a fenced block is not prose, and a link's own
    /// tail is Markdown, not a path in a sentence.
    #[test]
    fn code_and_link_tails_are_left_alone() {
        let disk = Disk::new();
        disk.has("src/main.rs");
        assert!(
            links("```\nsee src/main.rs\n```\n", &disk).is_empty(),
            "a fenced block is source, not prose"
        );
        assert!(
            links("    src/main.rs\n", &disk).is_empty(),
            "an indented block is source too"
        );
        assert!(
            links("[the file](src/main.rs)", &disk).is_empty(),
            "a link's own destination is not prose"
        );
        assert!(
            links("![shot](src/main.rs)", &disk).is_empty(),
            "nor an image's"
        );
    }

    /// A path inside an inline code span is a link, and still draws as code.
    #[test]
    fn a_path_in_an_inline_code_span_links() {
        let disk = Disk::new();
        disk.has("/tmp/evo/report.md");
        let source = "the file `/tmp/evo/report.md` and more";
        assert_eq!(
            prose(source, &disk.paths),
            "the file [`/tmp/evo/report.md`](</tmp/evo/report.md>) and more"
        );
        // A span that is not a path is left exactly as it was.
        assert_eq!(
            prose("run `cargo test` first", &disk.paths),
            "run `cargo test` first"
        );
    }

    /// Prose keeps every character it had, and a path becomes a link around it.
    #[test]
    fn prose_keeps_its_own_text() {
        let disk = Disk::new();
        disk.has("src/lib.rs");
        assert_eq!(
            prose("the **bold** file src/lib.rs, see", &disk.paths),
            "the **bold** file [src/lib.rs](</work/project/src/lib.rs>), see"
        );
        assert_eq!(
            prose("nothing to link here", &disk.paths),
            "nothing to link here"
        );
    }

    /// Plain text is drawn as the characters it holds, whatever they mean to a parser,
    /// and the links in it are still links.
    #[test]
    fn plain_text_is_escaped_and_its_links_are_not() {
        let disk = Disk::new();
        disk.has("/tmp/evo/report.md");
        assert_eq!(
            literal("**not bold** and /tmp/evo/report.md", &disk.paths),
            "\\*\\*not bold\\*\\* and [/tmp/evo/report.md](</tmp/evo/report.md>)"
        );
        assert_eq!(
            literal("* a bullet\n# a heading", &disk.paths),
            "\\* a bullet\\\n\\# a heading"
        );
        assert_eq!(
            literal("a https://evo.dev/x. b", &disk.paths),
            "a [https://evo.dev/x](https://evo.dev/x)\\. b"
        );
    }

    /// What the app opens: a web address, a file that is there, and nothing else.
    #[test]
    fn only_a_web_address_or_a_path_is_a_target() {
        assert_eq!(
            target("https://evo.dev"),
            Some(Target::Web("https://evo.dev".to_string()))
        );
        assert_eq!(
            target("mailto:evo@ruiqilei.com"),
            Some(Target::Web("mailto:evo@ruiqilei.com".to_string()))
        );
        assert_eq!(target("file:///etc/passwd"), None);
        assert_eq!(target("javascript:alert(1)"), None);
        assert_eq!(target("crates/transcript/src/lib.rs"), None);
        // A path that is not there is not opened: the row can outlive the file.
        assert_eq!(target("/tmp/definitely/not/here-1234"), None);
    }

    /// The promise of [`literal`]: what it writes draws exactly the characters that
    /// went in — whatever they would have meant to a Markdown parser.
    #[gpui_kit::test]
    fn plain_text_draws_as_the_characters_it_holds(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::base::TextViewState;
        use gpui_kit::AppContext as _;

        cx.update(gpui_kit::init);
        let disk = Disk::new();
        disk.has("/tmp/evo/report.md");
        let cases = [
            "a plain line",
            "**not bold** and _not italic_",
            "# not a heading",
            "- not a bullet\n- and a second",
            "1. not a list\n2. and a second",
            "> not a quote",
            "a blank line\n\nand another",
            "| not | a | table |",
            "a `code span` and a <b>tag</b>",
            "a [link](https://x) shape",
            "an address https://evo.dev/x. here",
            "the file /tmp/evo/report.md is there",
            "with ~ and \\ backslashes",
            "---",
            "a & b = c + d",
        ];
        for case in cases {
            let source = literal(case, &disk.paths);
            let document = cx.new(|cx| TextViewState::markdown(&source, cx));
            cx.run_until_parked();
            let rendered = cx.read(|cx| document.read(cx).rendered_text().as_str().to_string());
            assert_eq!(
                rendered.trim_end_matches('\n'),
                case.trim_end_matches('\n'),
                "the source was {source:?}"
            );
        }
    }

    /// A frame asks the file system once per token, not once per row per frame.
    #[test]
    fn a_look_at_the_disk_is_remembered() {
        let paths = Paths::new();
        assert_eq!(paths.resolve("/tmp"), Some(PathBuf::from("/tmp")));
        let seen = paths.seen.lock().unwrap().len();
        assert_eq!(paths.resolve("/tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(paths.seen.lock().unwrap().len(), seen);
        paths.forget();
        assert_eq!(paths.seen.lock().unwrap().len(), 0);
    }

    /// A folder with one entry in it, on the real disk: what a relative path is
    /// measured against is the file system, not a fixture.
    fn a_folder_that_exists() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("transcript-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("evo")).expect("a temp folder");
        dir
    }

    /// A relative path has no answer until the tab says what folder it runs in, and a
    /// folder that changes gives every token a new answer.
    #[test]
    fn a_folder_is_what_a_relative_path_is_measured_from() {
        let dir = a_folder_that_exists();
        let paths = Paths::new();
        assert!(paths.set_folder(Some(dir.clone())));
        assert_eq!(paths.resolve("evo"), Some(dir.join("evo")));
        assert_eq!(paths.resolve("no-such-entry"), None);
        assert!(
            !paths.set_folder(Some(dir.clone())),
            "the same folder again"
        );
        paths.set_folder(Some(PathBuf::from("/")));
        assert_eq!(paths.resolve("evo"), None, "another folder, another answer");
        let _ = std::fs::remove_dir_all(dir);
    }
}
