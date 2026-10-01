//! TeX's own math delimiters, made visible to the Markdown parser.
//!
//! A model writes `\(x\)` for inline math at least as often as `$x$`, and `\[x\]`
//! for display math — TeX's own spelling. The Markdown parser has no construct
//! for either: by the time a plugin is handed a node, `\(x\)` has become the prose
//! `(x)` and the backslashes are gone. So the message is rewritten **before it
//! becomes a document**, into the one math construct the kit does have, in a form
//! that still carries what the model wrote:
//!
//! ```text
//! \(x\)   ->   $\(x\)$          \[x\]   ->   $$\[x\]$$
//! ```
//!
//! The math plugin recognises the wrapper, reads the LaTeX inside it, and hands
//! back the alias — delimiters and all — as the text a copy gives and as what is
//! drawn when no picture can be made. The dollars exist only for the parser.
//!
//! A display alias alone on its own lines keeps the block it was written as:
//!
//! ```text
//! \[            $$
//! x        ->   \[
//! \]            x
//!               \]
//!               $$
//! ```
//!
//! because a `$$` that opens a line is a math **fence** to the parser, and it has
//! to be closed by one that stands on a line of its own.
//!
//! What is left exactly as it was, and why:
//!
//! * **fenced code** (``` or `~~~`, three or more, closed by a run of the same
//!   marker at least as long), **inline code spans** (a run of backticks, closed
//!   by a run of the same length) and **indented code** — code is the parser's
//!   own business and this pass must agree with it, so an alias-looking sequence
//!   inside code is never touched;
//! * **an even run of backslashes**: `\\(x\\)` is an escaped backslash followed by
//!   ordinary punctuation, not math;
//! * **an alias with no closer** — a formula still streaming, or prose that
//!   merely mentions `\(` — which is left the alias it is: the opener is written
//!   as the two backslashes Markdown reads as one, so the backslash the model
//!   wrote is what the reader is shown instead of the escaped parenthesis
//!   Markdown would have made of it;
//! * **a body holding a `$`**, which would end the marker early;
//! * **an alias spanning a blank line**, a paragraph break, where math does not
//!   run;
//! * **a `$` against either delimiter** — `\(x\)$$`, `$$\(x\)` — which the wrapper
//!   would merge with, turning a run of three dollars into text the model never
//!   wrote;
//! * **an alias written across lines** that is not a display alias alone on its
//!   own lines: wrapping one would let the next line open a list or a heading, and
//!   a `$$` opening a line would fence off the rest of the message;
//! * **raw HTML** — a comment, or a `<pre>`, `<code>`, `<script>`, `<style>` or
//!   `<textarea>` run, attributes and line endings and all — which the transcript
//!   draws as the markup it is, so a dollar written into it would be seen;
//! * **a link or image destination**, and the title after it — not prose, and no
//!   place to write a dollar into; a reference definition's line is skipped whole
//!   for the same reason. A *label* is prose, and is scanned like any other;
//! * **a bare URL** — `http://…`, `https://…`, `www.…` — which GFM reads as a link
//!   as surely as a destination is one, and where a dollar would be shown to the
//!   reader and would break the link's own target.
//!
//! The pass is a pure function of the text — the same message always normalizes
//! the same way — so a streamed message that grows a character at a time settles
//! on the same document it would have had all at once.

use std::borrow::Cow;

/// Rewrite TeX's `\(…\)` and `\[…\]` into the kit's `$…$` math, keeping the alias
/// inside so nothing about the model's own text is lost. Borrowed unchanged when
/// there is nothing to rewrite.
pub fn normalize(source: &str) -> Cow<'_, str> {
    let bytes = source.as_bytes();
    let mut rewrites: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i < source.len() {
        if at_line_start(source, i) {
            if let Some((marker, len)) = opening_fence(source, i) {
                i = fence_end(source, i, marker, len);
                continue;
            }
            if opens_indented_code(source, i) {
                i = indented_code_end(source, i);
                continue;
            }
            if let Some(end) = definition_end(source, i) {
                i = end;
                continue;
            }
            if let Some((end, replacement)) = standalone_dollar_block(source, i) {
                rewrites.push((i, end, replacement));
                i = end;
                continue;
            }
        }
        match bytes[i] {
            b'`' => {
                let run = run_len(bytes, i, b'`');
                i = closing_run(source, i + run, b'`', run).unwrap_or(i + run);
            }
            b'$' => {
                // The kit's own math, and the markers this pass writes: left
                // exactly as they are, which also makes a second pass a no-op.
                let run = run_len(bytes, i, b'$').min(2);
                i = closing_run(source, i + run, b'$', run).unwrap_or(i + run);
            }
            b'\\' => {
                let run = run_len(bytes, i, b'\\');
                match alias_at(source, i, run) {
                    Some((end, replacement)) => {
                        rewrites.push((i + run - 1, end, replacement));
                        i = end;
                    }
                    None => {
                        if let Some(replacement) = opener_with_nothing_to_close(source, i, run) {
                            rewrites.push((i, i + 2, replacement));
                        }
                        i += run;
                    }
                }
            }
            // A bare URL is a link: GFM reads `http://`, `https://` and `www.` as
            // autolinks, and an address is not prose to write a dollar into.
            b'h' | b'H' | b'w' | b'W' => i = autolink_end(source, i).unwrap_or(i + 1),
            // A `<` may open raw HTML, whose text the transcript shows as it is.
            b'<' => i = html_skip(source, i).unwrap_or(i + 1),
            // A `]` may close a label and open a destination.
            b']' => i = link_tail_end(source, i).unwrap_or(i + 1),
            _ => i += char_len(source, i),
        }
    }

    if rewrites.is_empty() {
        return Cow::Borrowed(source);
    }
    let mut out = String::with_capacity(source.len() + 8 * rewrites.len());
    let mut at = 0;
    for (start, end, replacement) in rewrites {
        out.push_str(&source[at..start]);
        out.push_str(&replacement);
        at = end;
    }
    out.push_str(&source[at..]);
    Cow::Owned(out)
}

/// The alias that starts at a run of `run` backslashes, as `(end, replacement)`,
/// with `end` the index just past its closing delimiter — or `None`, in which
/// case the alias is left exactly as it was written.
///
/// `None` for an even run (an escaped backslash), for a delimiter that is not a
/// parenthesis, for an alias with nothing to close it, for a body holding a `$`,
/// for a `$` against either delimiter, and for an alias written across lines that
/// is not a display one standing alone on its own lines.
fn alias_at(source: &str, i: usize, run: usize) -> Option<(usize, String)> {
    if run.is_multiple_of(2) {
        return None;
    }
    let bytes = source.as_bytes();
    let (close, display) = match bytes.get(i + run) {
        Some(b'(') => (b')', false),
        Some(b'[') => (b']', true),
        _ => return None,
    };
    let body_at = i + run + 1;
    let close_at = closing_delimiter(source, body_at, close)?;
    let body = &source[body_at..close_at];
    // A `$` in the body would end the marker the parser is about to read.
    if body.contains('$') {
        return None;
    }
    let head = i + run - 1;
    let end = close_at + 2;
    // A `$` against either delimiter would merge with the wrapper: `\(x\)$$`
    // becomes `$\(x\)$$$`, a run of three the parser reads as text, and the model's
    // own dollars come out as characters on the screen.
    if bytes.get(end) == Some(&b'$') || i.checked_sub(1).is_some_and(|before| bytes[before] == b'$')
    {
        return None;
    }
    let alias = &source[head..end];
    if !alias.contains('\n') {
        let dollars = if display { "$$" } else { "$" };
        return Some((end, format!("{dollars}{alias}{dollars}")));
    }
    // An alias written across lines. Only a display one standing alone on its own
    // lines can be wrapped: a `$$` opening a line is a math *fence* to the parser,
    // closed only by one that stands alone, and any wrapper whose alias runs past a
    // line ending breaks the paragraph if the next line opens a list or a heading.
    // So: a block form, or nothing at all.
    (display && opens_its_own_line(source, head) && ends_its_own_line(source, end))
        .then(|| (end, format!("$$\n{alias}\n$$")))
}

/// Whether nothing but spaces — at most three of them — stands between the start
/// of `at`'s line and `at`. Three is the most a fence may be indented by and still
/// be read as one; a fourth would make the `$$` this pass writes an indented code
/// block, and the dollars would be shown as text.
fn opens_its_own_line(source: &str, at: usize) -> bool {
    let start = source[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let prefix = &source[start..at];
    prefix.len() <= 3 && prefix.bytes().all(|byte| byte == b' ')
}

/// Whether nothing but spaces or tabs stands between `at` and the end of its line.
fn ends_its_own_line(source: &str, at: usize) -> bool {
    let rest = &source[at..];
    let stop = rest.find('\n').unwrap_or(rest.len());
    rest[..stop]
        .bytes()
        .all(|byte| byte == b' ' || byte == b'\t')
}

/// Where an alias's body ends: the index of the backslash of the first `\)` or
/// `\]` that is not itself escaped. The search stops at a blank line.
fn closing_delimiter(source: &str, from: usize, close: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = from;
    while i < source.len() {
        if blank_line_at(source, i) {
            return None;
        }
        if bytes[i] == b'\\' {
            let run = run_len(bytes, i, b'\\');
            if run % 2 == 1 && bytes.get(i + run) == Some(&close) {
                return Some(i + run - 1);
            }
            i += run;
            continue;
        }
        i += char_len(source, i);
    }
    None
}

/// The text to write in place of a lone `\(` or `\[` that closes nowhere — the two
/// backslashes Markdown reads as one — or `None` when what stands at `i` is not
/// one.
///
/// Markdown reads a backslash before punctuation as an escape and drops it, so an
/// alias that never closes would lose a character the model wrote: the reader
/// would be shown `(x^2` for `\(x^2`. Doubling the opener puts the backslash back
/// on screen, and a run of two is an escaped backslash to this pass, which is what
/// makes a second pass a no-op. A run of three or more already shows a backslash
/// through the pairs before it, and is left as it was written.
fn opener_with_nothing_to_close(source: &str, i: usize, run: usize) -> Option<String> {
    if run != 1 {
        return None;
    }
    let close = match source.as_bytes().get(i + 1) {
        Some(b'(') => b')',
        Some(b'[') => b']',
        _ => return None,
    };
    // A closer anywhere before the paragraph ends means this is an alias — one
    // this pass chooses not to wrap, but not an opener with nothing to close.
    if closing_delimiter(source, i + 2, close).is_some() {
        return None;
    }
    Some(format!("\\{}", &source[i..i + 2]))
}

/// A `$$…$$` that is a paragraph of its own, written as the block the parser reads
/// as **flow** math: a `$$` fence on a line, the model's own `$$…$$` line inside
/// it, and the closing `$$`. `None` when what stands at `i` is anything else.
///
/// The parser has a math node for `$$…$$` written inside a line, and that node is
/// laid out *inline* — its scroller shrinks to the picture, so a `$$…$$` of its own
/// sits against the left edge while a display alias, which the pass fences, is
/// centred in the column. Fencing the dollars form gives it the same block, the
/// width of the reading column, with the picture centred in it. The model's own
/// line stays the fence's content, so a copy gives back exactly what was written
/// (and the fallback for a formula nobody can draw shows it too).
///
/// Only a `$$…$$` that is its own paragraph is fenced: wrapping one that shares a
/// paragraph with prose would break that paragraph in two, and a formula in the
/// middle of a sentence is a formula in the middle of a sentence.
fn standalone_dollar_block(source: &str, i: usize) -> Option<(usize, String)> {
    let line = source[i..].split('\n').next().unwrap_or("").trim_end();
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 || !owns_its_paragraph(source, i, i + line.len()) {
        return None;
    }
    let formula = line[indent..].strip_prefix("$$")?.strip_suffix("$$")?;
    if formula.trim().is_empty() || formula.contains('$') {
        return None;
    }
    // The wrapper this pass writes for a display alias is a `$$…$$` line too, and
    // what it wrote is its own answer: a second pass must leave it alone.
    if formula.starts_with("\\[") && formula.ends_with("\\]") {
        return None;
    }
    Some((i + line.len(), format!("$$\n{}\n$$", &line[indent..])))
}

/// Whether the line that runs from `start` to `end` stands as a block of its own:
/// the message begins, or a blank line comes before it, and it ends the message or
/// a blank line follows.
fn owns_its_paragraph(source: &str, start: usize, end: usize) -> bool {
    let blank_before = if start == 0 {
        true
    } else {
        let head = &source[..start];
        head.ends_with('\n')
            && head[..head.len() - 1]
                .rsplit('\n')
                .next()
                .is_some_and(|line| line.trim().is_empty())
    };
    let rest = &source[end..];
    let blank_after = match rest.strip_prefix('\n') {
        Some(rest) => rest
            .split('\n')
            .next()
            .is_some_and(|line| line.trim().is_empty()),
        None => rest.is_empty(),
    };
    blank_before && blank_after
}

/// The names the parser reads as raw-HTML text containers: everything between the
/// tags is markup — attributes, line endings and all — and the transcript draws it
/// as the markup it is.
const HTML_TEXT_CONTAINERS: [&str; 5] = ["pre", "code", "script", "style", "textarea"];

/// The end of a raw-HTML run starting at the `<` at `i`, or `None` when that `<`
/// is ordinary text (`a < b`).
///
/// A comment runs to `-->`. A text container runs from its opening tag — its
/// attributes may hold spaces, `>`s and line endings — to its closing tag, or to
/// the end of the message when it has none: markup whose end this pass cannot see
/// is markup it does not write into. Anything else is left for the ordinary
/// scan, `div` included: outside the text containers, raw HTML still gets
/// markdown, and treating it as inert would be worse than the risk.
fn html_skip(source: &str, i: usize) -> Option<usize> {
    if source[i..].starts_with("<!--") {
        return Some(match source[i + 4..].find("-->") {
            Some(at) => i + 4 + at + 3,
            None => source.len(),
        });
    }
    for name in HTML_TEXT_CONTAINERS {
        if !tag_at(source, i + 1, name) {
            continue;
        }
        // `<prefix>` is a different tag: the name has to end here.
        let after_name = i + 1 + name.len();
        match source.as_bytes().get(after_name) {
            Some(b'>' | b' ' | b'\t' | b'\n' | b'/') => {}
            _ => continue,
        }
        let open_end = source[after_name..].find('>')? + after_name + 1;
        return Some(closing_tag_end(source, open_end, name));
    }
    None
}

/// Whether `tag` — already lowercase — is written at `from`, in any case.
fn tag_at(source: &str, from: usize, tag: &str) -> bool {
    let bytes = source.as_bytes();
    tag.bytes().enumerate().all(|(n, byte)| {
        bytes
            .get(from + n)
            .is_some_and(|found| found.eq_ignore_ascii_case(&byte))
    })
}

/// Just past the `>` of the closing tag of `name` at or after `from`, or the end
/// of the message when there is none.
fn closing_tag_end(source: &str, from: usize, name: &str) -> usize {
    let bytes = source.as_bytes();
    let mut i = from;
    while i < source.len() {
        if bytes[i] == b'<'
            && bytes.get(i + 1) == Some(&b'/')
            && tag_at(source, i + 2, name)
            && bytes.get(i + 2 + name.len()) == Some(&b'>')
        {
            return i + 2 + name.len() + 1;
        }
        i += char_len(source, i);
    }
    source.len()
}

/// Just past the end of a GFM autolink literal starting at `i` — `http://`,
/// `https://` or `www.` — or `None` when what stands here is not one.
///
/// A bare URL in a sentence is a link, and its address is not prose: a dollar
/// written into it would be shown to the reader and would break the link's own
/// target. What GFM reads (`markdown-1.0.0`, `construct/gfm_autolink_literal.rs`)
/// is a scheme no ASCII letter stands before, or a `www.` at the start of the
/// text or after one of the marks a word may follow, and then an address: one or
/// more characters up to whitespace, a `<`, or the `)` that closes no `(`.
fn autolink_end(source: &str, i: usize) -> Option<usize> {
    let at = protocol_prefix(source, i).or_else(|| www_prefix(source, i))?;
    let something_follows = matches!(
        source.as_bytes().get(at),
        Some(byte) if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'<')
    );
    something_follows.then(|| url_end(source, at))
}

/// Just past `http://` or `https://` written at `i`, case-insensitively, or
/// `None`: GFM reads no other scheme, and a letter before the `h` makes the run
/// a word that merely ends in one.
fn protocol_prefix(source: &str, i: usize) -> Option<usize> {
    if source.as_bytes()[..i]
        .last()
        .is_some_and(u8::is_ascii_alphabetic)
    {
        return None;
    }
    ["http://", "https://"]
        .into_iter()
        .find(|scheme| {
            source[i..]
                .get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        })
        .map(|scheme| i + scheme.len())
}

/// Just past a `www.` written at `i` that starts a word, or `None`. GFM allows
/// one after the start of the text, a space, a tab, a line ending, or one of
/// `(`, `*`, `_`, `[`, `]` and `~`.
fn www_prefix(source: &str, i: usize) -> Option<usize> {
    let opens_a_word = match source[..i].chars().next_back() {
        None => true,
        Some(before) => matches!(
            before,
            '\t' | '\n' | ' ' | '(' | '*' | '_' | '[' | ']' | '~'
        ),
    };
    (opens_a_word
        && source[i..]
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("www.")))
    .then_some(i + 4)
}

/// Where the address that starts at `from` ends. Whatever address it is — a
/// path, a query, a fragment — it runs to whitespace, to a `<`, or to the `)`
/// that closes no `(`; parentheses that pair are part of it, as GFM reads them.
fn url_end(source: &str, from: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = from;
    let mut open = 0usize;
    while i < source.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' | b'<' => break,
            b'(' => {
                open += 1;
                i += 1;
            }
            b')' if open == 0 => break,
            b')' => {
                open -= 1;
                i += 1;
            }
            _ => i += char_len(source, i),
        }
    }
    i
}

/// The end of the tail of an inline link or image — `](destination "title")` —
/// when the `]` at `i` opens one, or `None` when it does not.
///
/// A destination and a title are not prose, and there is nothing in them to draw:
/// they are skipped whole, so no dollar is ever written into a URL. The label
/// *before* the `]` is prose and is scanned like any other text.
fn link_tail_end(source: &str, i: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(i + 1) != Some(&b'(') {
        return None;
    }
    let mut at = spaces(source, i + 2);
    at = if bytes.get(at) == Some(&b'<') {
        bracketed_destination(source, at)?
    } else {
        bare_destination(source, at)
    };
    at = spaces(source, at);
    if bytes.get(at) == Some(&b')') {
        return Some(at + 1);
    }
    at = title_end(source, at)?;
    at = spaces(source, at);
    (bytes.get(at) == Some(&b')')).then_some(at + 1)
}

/// Past the run of spaces and tabs at `at`, and at most one line ending with its
/// indentation after it.
fn spaces(source: &str, at: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = at;
    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    if bytes.get(i) == Some(&b'\n') {
        i += 1;
        while matches!(bytes.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
    }
    i
}

/// Past a `<…>` destination, or `None` when it never closes on its line.
fn bracketed_destination(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = at + 1;
    while i < source.len() {
        match bytes[i] {
            b'>' => return Some(i + 1),
            b'\n' => return None,
            b'\\' => i = past_escape(source, i),
            _ => i += char_len(source, i),
        }
    }
    None
}

/// Past a bare destination: `\` escapes, parentheses balance, and it ends at a
/// space, a line ending, or the `)` that closes the tail.
fn bare_destination(source: &str, at: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = at;
    let mut depth = 0usize;
    while i < source.len() {
        match bytes[i] {
            b'\\' => i = past_escape(source, i),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' if depth == 0 => break,
            b')' => {
                depth -= 1;
                i += 1;
            }
            b' ' | b'\t' | b'\n' => break,
            _ => i += char_len(source, i),
        }
    }
    i
}

/// Past a title — `"…"`, `'…'` or `(…)`, with `\` escapes — or `None` when there
/// is no title here.
fn title_end(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let close = match bytes.get(at)? {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let mut i = at + 1;
    while i < source.len() {
        match bytes[i] {
            b'\\' => i = past_escape(source, i),
            byte if byte == close => return Some(i + 1),
            b'\n' if blank_line_at(source, i) => return None,
            _ => i += char_len(source, i),
        }
    }
    None
}

/// Just past the character a backslash at `i` escapes.
fn past_escape(source: &str, i: usize) -> usize {
    let mut i = i + 1;
    if i < source.len() {
        i += char_len(source, i);
    }
    i
}

/// The end of a line that is a link reference definition — `[label]:
/// destination "title"` — when the line at `i` is one. A definition's destination
/// is not prose either, and the line holds nothing else, so it is skipped whole.
fn definition_end(source: &str, i: usize) -> Option<usize> {
    let rest = &source[i..];
    let indent = rest.len() - rest.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let line = rest.split('\n').next().unwrap_or("");
    let label = line.get(indent..)?;
    if !label.starts_with('[') {
        return None;
    }
    let close = label.find(']')?;
    if !label[close + 1..].starts_with(':') {
        return None;
    }
    Some(i + line.len())
}

/// The end of a code span opened by a run of `len` backticks: just past the next
/// run of exactly that many, or `None` when the paragraph ends first.
fn closing_run(source: &str, from: usize, marker: u8, len: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = from;
    while i < source.len() {
        if blank_line_at(source, i) {
            return None;
        }
        if bytes[i] == marker {
            let run = run_len(bytes, i, marker);
            if run == len {
                return Some(i + run);
            }
            i += run;
            continue;
        }
        i += char_len(source, i);
    }
    None
}

/// The fence a line opens, with its marker and length: three or more backticks or
/// tildes after at most three spaces.
fn opening_fence(source: &str, i: usize) -> Option<(u8, usize)> {
    let rest = &source[i..];
    let indent = rest.len() - rest.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let marker = *rest.as_bytes().get(indent)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = run_len(rest.as_bytes(), indent, marker);
    (len >= 3).then_some((marker, len))
}

/// Where the fence opened at `i` runs out: a line whose first non-space run is the
/// same marker, at least as long, with nothing else on it.
fn fence_end(source: &str, i: usize, marker: u8, len: usize) -> usize {
    let mut at = line_end(source, i);
    while at < source.len() {
        let rest = &source[at..];
        let indent = rest.len() - rest.trim_start_matches(' ').len();
        if indent <= 3 && rest.as_bytes().get(indent) == Some(&marker) {
            let run = run_len(rest.as_bytes(), indent, marker);
            let after = &rest[indent + run..];
            if run >= len && after.split('\n').next().unwrap_or("").trim().is_empty() {
                return line_end(source, at);
            }
        }
        at = line_end(source, at);
    }
    source.len()
}

/// Whether a line opens an indented code block: four spaces or a tab, with a
/// blank line before it — code cannot interrupt a paragraph.
fn opens_indented_code(source: &str, i: usize) -> bool {
    let rest = &source[i..];
    if !(rest.starts_with("    ") || rest.starts_with('\t')) {
        return false;
    }
    if i == 0 {
        return true;
    }
    let previous = source[..i - 1].rsplit('\n').next().unwrap_or("");
    previous.trim().is_empty()
}

/// Where an indented code block runs out: the first non-blank line that is not
/// indented.
fn indented_code_end(source: &str, i: usize) -> usize {
    let mut at = i;
    while at < source.len() {
        let line = source[at..].split('\n').next().unwrap_or("");
        if !line.trim().is_empty() && !(line.starts_with("    ") || line.starts_with('\t')) {
            break;
        }
        at = line_end(source, at);
    }
    at
}

fn at_line_start(source: &str, i: usize) -> bool {
    i == 0 || source.as_bytes()[i - 1] == b'\n'
}

/// Whether a blank line starts at `i` (a newline, then only spaces and tabs).
fn blank_line_at(source: &str, i: usize) -> bool {
    if source.as_bytes().get(i) != Some(&b'\n') {
        return false;
    }
    let rest = &source[i + 1..];
    let stop = rest.find('\n').unwrap_or(rest.len());
    rest[..stop].trim().is_empty()
}

/// Just past the newline that ends the line `i` is on (or the end of the source).
fn line_end(source: &str, i: usize) -> usize {
    source[i..].find('\n').map_or(source.len(), |n| i + n + 1)
}

fn run_len(bytes: &[u8], i: usize, byte: u8) -> usize {
    let mut at = i;
    while bytes.get(at) == Some(&byte) {
        at += 1;
    }
    at - i
}

fn char_len(source: &str, i: usize) -> usize {
    source[i..].chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(source: &str) -> String {
        normalize(source).into_owned()
    }

    #[test]
    fn an_inline_alias_becomes_a_marker_that_keeps_it() {
        assert_eq!(rewritten(r"a \(x^2\) b"), r"a $\(x^2\)$ b");
        assert_eq!(rewritten(r"a \(\frac{a}{b}\) b"), r"a $\(\frac{a}{b}\)$ b");
    }

    #[test]
    fn a_display_alias_becomes_a_display_marker() {
        assert_eq!(rewritten(r"\[y^2\]"), r"$$\[y^2\]$$");
        assert_eq!(
            rewritten("before\n\n\\[y^2\\]\n\nafter"),
            "before\n\n$$\\[y^2\\]$$\n\nafter"
        );
    }

    #[test]
    fn several_aliases_in_one_message_are_all_rewritten() {
        assert_eq!(
            rewritten(r"\(p\) then \[q\] then \(r\)"),
            r"$\(p\)$ then $$\[q\]$$ then $\(r\)$"
        );
    }

    #[test]
    fn text_without_an_alias_is_borrowed_untouched() {
        for source in [
            "plain prose",
            "a $x$ b and $$y$$",
            "costs $5 and $10",
            r"a $\(\frac{1}{2}\)$ b after a pass",
        ] {
            match normalize(source) {
                Cow::Owned(text) => assert_eq!(text, source),
                Cow::Borrowed(text) => assert_eq!(text, source),
            }
        }
        assert!(matches!(normalize("plain prose"), Cow::Borrowed(_)));
        assert!(matches!(normalize("costs $5 and $10"), Cow::Borrowed(_)));
    }

    /// An opener with nothing to close it is not math, but it must not lose the
    /// backslash the model wrote either: Markdown reads a lone `\(` as an escaped
    /// parenthesis and drops the backslash, so the pass writes the two
    /// backslashes that are read as one. A run of two is an escaped backslash to
    /// this pass, which is what makes the second pass a no-op.
    #[test]
    fn an_alias_without_a_closer_keeps_its_backslash() {
        assert_eq!(rewritten(r"a \(x"), r"a \\(x");
        assert_eq!(
            rewritten(r"streaming \(\frac{a}{"),
            r"streaming \\(\frac{a}{"
        );
        assert_eq!(rewritten(r"\[y"), r"\\[y");
        // A paragraph break is not a formula's span.
        assert_eq!(rewritten("a \\(x\n\nb\\) c"), "a \\\\(x\n\nb\\) c");
        // Longer odd runs already show a backslash through the pairs before them,
        // and the pass leaves them as they were written.
        assert_eq!(rewritten(r"a \\\(x"), r"a \\\(x");
        // The closer arriving is the formula arriving: the streamed text is the
        // alias it turned into, not the escaped opener it was.
        assert_eq!(rewritten(r"a \(x\)"), r"a $\(x\)$");
    }

    /// An opener with a closer this pass chooses not to wrap — a `$` in the body,
    /// a `$` against a delimiter, an alias across lines that does not stand alone
    /// — is an alias, not an orphan: its opener is left exactly as it was.
    #[test]
    fn a_closed_alias_that_is_left_alone_keeps_its_opener() {
        for source in [
            r"a \(a$b\) c",
            r"x \(y\)$$",
            r"$$\(y\)",
            "before \\(a\n+ b\\) after",
        ] {
            assert_eq!(rewritten(source), source, "{source}");
        }
    }

    /// A bare URL is a link, and an address is not prose: an alias written inside
    /// one is part of the address, and is left exactly as it was — the reader
    /// would otherwise see the dollars the pass wrote, and the link's own target
    /// would carry them too.
    #[test]
    fn a_bare_url_is_never_rewritten() {
        for source in [
            r"bare https://example.com/\(z\)/q end",
            r"see http://e.com/a/\(x\) here",
            r"www.example.com/\(x\)/y",
            r"https://e.com/a_(b)/\(z\)?q=1&r=2#frag",
            r"(https://e.com/\(z\))",
            r"https://e.com/\(z\)",
            r"a link https://e.com/\(z\) and text",
            r"https://e.com/\(z\) then \(y\)",
        ] {
            let expected = if source.contains("then \\(y\\)") {
                source.replace(r"\(y\)", r"$\(y\)$")
            } else {
                source.to_string()
            };
            assert_eq!(rewritten(source), expected, "{source}");
        }
        assert!(matches!(
            normalize(r"see https://e.com/\(z\) here"),
            Cow::Borrowed(_)
        ));
    }

    /// What only looks like a URL is prose: a word that ends in `http`, a scheme
    /// GFM does not read, a `www` with no dot after it.
    #[test]
    fn what_is_not_a_url_is_still_prose() {
        assert_eq!(
            rewritten(r"nothttps://e.com/\(z\)"),
            r"nothttps://e.com/$\(z\)$"
        );
        assert_eq!(rewritten(r"ftp://e.com/\(z\)"), r"ftp://e.com/$\(z\)$");
        assert_eq!(rewritten(r"www\(x\)"), r"www$\(x\)$");
        assert_eq!(rewritten(r"https:\(x\)"), r"https:$\(x\)$");
    }

    /// The pass still works either side of a URL: punctuation at the address's end
    /// belongs to the sentence, and an alias after it is the formula it is.
    #[test]
    fn an_alias_beside_a_url_still_rewrites() {
        assert_eq!(
            rewritten(r"https://e.com/x and \(y\)"),
            r"https://e.com/x and $\(y\)$"
        );
        assert_eq!(
            rewritten(r"see https://e.com/x. \(y\)"),
            r"see https://e.com/x. $\(y\)$"
        );
    }

    #[test]
    fn an_escaped_backslash_is_not_an_alias() {
        assert_eq!(rewritten(r"a \\(x\\) b"), r"a \\(x\\) b");
        assert_eq!(rewritten(r"a \\\\\(x\) b"), r"a \\\\$\(x\)$ b");
    }

    #[test]
    fn an_escaped_closing_delimiter_does_not_close() {
        // `\\)` is an escaped backslash and an ordinary parenthesis: the formula
        // runs on to the real closer.
        assert_eq!(rewritten(r"\(a\\)b\)"), r"$\(a\\)b\)$");
        // An odd run is the closer itself.
        assert_eq!(rewritten(r"\(a\\\)b\)"), r"$\(a\\\)$b\)");
    }

    #[test]
    fn a_body_holding_a_dollar_is_left_alone() {
        assert_eq!(rewritten(r"a \(a$b\) c"), r"a \(a$b\) c");
    }

    #[test]
    fn an_alias_inside_a_code_span_is_not_math() {
        assert_eq!(rewritten("a `\\(x\\)` b"), "a `\\(x\\)` b");
        assert_eq!(rewritten("a ``\\(x\\)`` b"), "a ``\\(x\\)`` b");
        assert_eq!(rewritten("`\\(x\\)`"), "`\\(x\\)`");
    }

    #[test]
    fn an_alias_inside_a_fence_is_not_math() {
        assert_eq!(
            rewritten("```\n\\(x\\)\n```\n\nafter \\(y\\)"),
            "```\n\\(x\\)\n```\n\nafter $\\(y\\)$"
        );
        assert_eq!(
            rewritten("```rust\nlet x = \\(y\\);\n```"),
            "```rust\nlet x = \\(y\\);\n```"
        );
        assert_eq!(rewritten("~~~\n\\(x\\)\n~~~"), "~~~\n\\(x\\)\n~~~");
        // A closing fence must be at least as long as the one that opened.
        assert_eq!(
            rewritten("````\n\\(x\\)\n```\n\\(y\\)\n````"),
            "````\n\\(x\\)\n```\n\\(y\\)\n````"
        );
    }

    #[test]
    fn an_alias_inside_indented_code_is_not_math() {
        assert_eq!(
            rewritten("text\n\n    \\(x\\)\n\nafter \\(y\\)"),
            "text\n\n    \\(x\\)\n\nafter $\\(y\\)$"
        );
    }

    #[test]
    fn an_alias_in_a_paragraph_after_code_still_rewrites() {
        assert_eq!(
            rewritten("    code\n\nprose \\(x\\) more"),
            "    code\n\nprose $\\(x\\)$ more"
        );
    }

    #[test]
    fn a_display_alias_across_lines_becomes_a_block_of_its_own() {
        // The fence is written on lines of its own, so the alias it holds is one
        // Math node and the paragraph after it is still a paragraph.
        assert_eq!(
            rewritten("before\n\n\\[\n\\frac{a}{b}\n\\]\n\nafter"),
            "before\n\n$$\n\\[\n\\frac{a}{b}\n\\]\n$$\n\nafter"
        );
        // No blank line before it: a fence interrupts a paragraph, and this one
        // does not wait for a paragraph break.
        assert_eq!(
            rewritten("prose\n\\[\nx\n\\]\nmore prose"),
            "prose\n$$\n\\[\nx\n\\]\n$$\nmore prose"
        );
        // Up to three spaces of indentation belong to the block, and trailing space
        // after the closer does not stop it being alone on its line.
        assert_eq!(
            rewritten("  \\[\nx\n\\]\nlast"),
            "  $$\n\\[\nx\n\\]\n$$\nlast"
        );
        // Two of them in one message, each with a fence of its own.
        assert_eq!(
            rewritten("\\[\na\n\\]\n\n\\[\nb\n\\]"),
            "$$\n\\[\na\n\\]\n$$\n\n$$\n\\[\nb\n\\]\n$$"
        );
        // The inline form is what it was: only a display alias is a block.
        assert_eq!(rewritten(r"a \(x\) b"), r"a $\(x\)$ b");
    }

    /// An alias written across lines is wrapped as a block or not at all: any other
    /// wrapper would let the line after it open a list or a heading, and a `$$`
    /// opening a line is a fence that would run away with the rest of the message.
    #[test]
    fn a_multiline_alias_that_is_not_standalone_is_left_alone() {
        for source in [
            // Display, but not standing on lines of its own.
            "before \\[\na\n\\] after",
            "before\n\n\\[\na\n\\] and more prose",
            "before\n\n  \\[\na\\] more",
            // Four spaces in: a `$$` written there would be indented code instead.
            "prose\n    \\[\nx\n    \\]",
            // An inline alias across lines, whatever the line after it holds.
            "before \\(a\n+ b\\) after",
            "before \\(a\n# b\\) after",
            "before \\(a\nb\\) after",
            "\\[\n\\frac{a}{b}\n\\] and more",
        ] {
            assert_eq!(rewritten(source), source, "{source}");
            assert!(
                !rewritten(source).contains('$'),
                "no dollar is written into it: {source}"
            );
        }
    }

    /// A wrapper written against a `$` merges with it, and the parser then reads a
    /// run of three as text: the model's own dollars come out on the screen.
    #[test]
    fn an_alias_beside_a_dollar_is_left_alone() {
        assert_eq!(rewritten(r"x \(y\)$$"), r"x \(y\)$$");
        assert_eq!(rewritten(r"$$\(y\)"), r"$$\(y\)");
        assert_eq!(rewritten("\\[y\\]$$"), "\\[y\\]$$");
        // A space between them is not adjacency: the two stay separate constructs.
        assert_eq!(rewritten(r"$$ \(y\) x"), r"$$ $\(y\)$ x");
        assert_eq!(rewritten(r"\(y\) $$ x"), r"$\(y\)$ $$ x");
    }

    /// Raw HTML is drawn as the markup it is, so no dollar may be written into it —
    /// and the pass still works either side of it.
    #[test]
    fn raw_html_is_left_exactly_as_it_was() {
        for source in [
            "<pre>\n\\(x\\)\n</pre>",
            "<code>\\(x\\)</code>",
            "<PRE>\n\\(x\\)\n</PRE>",
            "<script type=\"text/x\" defer>\n\\(x\\)\n</script>",
            "<style media=\"screen\">\n\\(x\\)\n</style>",
            "<textarea rows=\"3\">\\(x\\)</textarea>",
            "<!-- \\(x\\) -->",
            // No closer: markup whose end this pass cannot see is markup it leaves.
            "<pre>\n\\(x\\)\n",
            "<!-- \\(x\\)",
        ] {
            assert_eq!(rewritten(source), source, "{source}");
        }
        assert_eq!(
            rewritten("<pre>\n\\(x\\)\n</pre>\n\n\\(y\\)"),
            "<pre>\n\\(x\\)\n</pre>\n\n$\\(y\\)$"
        );
        // A tag that is not one of the text containers is left to the ordinary
        // scan: outside them, raw HTML still gets markdown.
        assert_eq!(
            rewritten("<div>\n\\(x\\)\n</div>"),
            "<div>\n$\\(x\\)$\n</div>"
        );
        // And a `<` that opens nothing is ordinary text.
        assert_eq!(rewritten(r"a < b and \(x\) c"), r"a < b and $\(x\)$ c");
    }

    /// A destination and a title are not prose — nothing is written into a URL —
    /// while the label before them is scanned like any other text.
    #[test]
    fn a_destination_is_never_rewritten() {
        for source in [
            r"[a](http://e.com/\(x\))",
            r"[a](<http://e.com/\(x\)>)",
            r#"[a](http://e.com "a \(title\)")"#,
            r"[a](http://e.com 'a \(title\)')",
            r"![a](img/\(x\).png)",
            "[label]: http://e.com/\\(x\\)",
        ] {
            assert_eq!(rewritten(source), source, "{source}");
        }
        // A display alias after a link's tail is its own construct; the tail is
        // still not touched.
        assert_eq!(
            rewritten(r"[a](<http://e.com/\(x\)>) and \[b\]"),
            r"[a](<http://e.com/\(x\)>) and $$\[b\]$$"
        );
        assert_eq!(
            rewritten(r"[a \(x\)](http://e.com)"),
            r"[a $\(x\)$](http://e.com)",
            "the label is prose, and is scanned"
        );
        assert_eq!(
            rewritten(r"[a](http://e.com) then \(x\)"),
            r"[a](http://e.com) then $\(x\)$",
            "and a link does not reach past its own tail"
        );
    }

    #[test]
    fn normalizing_twice_changes_nothing() {
        let once = rewritten(r"a \(x\) and \[y\]");
        assert_eq!(
            normalize(&once).into_owned(),
            once,
            "a marker is inside the kit's own math, where a second pass looks at nothing"
        );
        for source in [
            "before\n\n\\[\n\\frac{a}{b}\n\\]\n\nafter",
            "$$\n\\[\n\\frac{a}{b}\n\\]\n$$",
            "<pre>\n\\(x\\)\n</pre>\n\n\\[y^2\\]\n",
            r"[a](http://e.com/\(x\)) then \(y\)",
        ] {
            let once = rewritten(source);
            assert_eq!(normalize(&once).into_owned(), once, "{source}");
        }
    }
}
