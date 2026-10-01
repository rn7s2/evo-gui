//! The inline completion the caret's word raises (§7.3).
//!
//! What the caret is on is evo's answer, not this crate's: the `complete` op says
//! whether it is in a `/command` word or inside `/eval`'s content, where that unit
//! begins and ends, and what a candidate would replace
//! ([`crate::Composer::set_completion`] is where it arrives). The rules are the
//! TUI's own, so the two frontends agree by construction — and a client that
//! re-derived them would be the third opinion about what a word is.
//!
//! What lives here is the other half, and it is all pure, so the popup's behaviour
//! is unit-tested without a window:
//!
//! * **the answer held against the text** ([`held`]): an answer names a word in the
//!   text it was asked about, and the caret has usually typed into it since;
//! * which candidates match a prefix ([`matches`], [`prefix_matches`]), whether
//!   there is anything left to choose ([`settled`]), and which characters the prefix
//!   hit ([`matched_ranges`]);
//! * what accepting one does to the text ([`accepted`]).

use std::ops::Range;

/// How many candidates the popup shows at once. Past this the list scrolls.
pub(crate) const MAX_ROWS: usize = 8;

/// Which of the two things the caret is completing, as `complete` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// A `/command` word, anywhere in the text: the command layer's own registry.
    Command,
    /// A symbol of the running image, inside `/eval`'s content — the one place only
    /// the image can answer for.
    Symbol,
}

/// One candidate, exactly as the server offers it: a command of `GET /catalog`
/// (`name`, `description`), or one of the image's symbols (`name`,
/// `description` — what it is: `function`, `variable`).
///
/// The client never makes one up: no candidate appears here that the server did
/// not list or answer with (§8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What accepting it inserts: a command word without its slash, a symbol
    /// without the token it replaces.
    pub name: String,
    /// The dim line beside it.
    pub description: String,
}

/// The popup as it stands: the caret's word, and what could finish it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Popup {
    pub(crate) kind: CompletionKind,
    /// The word as it was when these rows were chosen. A prefix that has moved
    /// on since means the rows are stale, and the popup is rebuilt.
    pub(crate) prefix: String,
    /// The whole word, in bytes of the message: what accept replaces.
    pub(crate) word: Range<usize>,
    pub(crate) rows: Vec<Candidate>,
    /// Which row is highlighted.
    pub(crate) index: usize,
}

impl Popup {
    /// The rows, and the word they were chosen for.
    pub(crate) fn new(held: &Held<'_>, rows: Vec<Candidate>) -> Popup {
        Popup {
            kind: held.kind,
            prefix: held.prefix.clone(),
            word: held.word.clone(),
            rows,
            index: 0,
        }
    }

    /// Move the highlight by `delta` rows, wrapping at both ends: the list is a
    /// ring, so ↓ on the last row is the first one.
    pub(crate) fn walk(&mut self, delta: isize) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let step = delta.rem_euclid(len as isize);
        self.index = (self.index + step as usize) % len;
    }

    /// The highlighted candidate.
    pub(crate) fn chosen(&self) -> Option<&Candidate> {
        self.rows.get(self.index)
    }

    /// How the candidate reads in the popup: a command wears its slash, a
    /// symbol is shown exactly as it would be typed.
    pub(crate) fn label(&self, candidate: &Candidate) -> String {
        match self.kind {
            CompletionKind::Command => format!("/{}", candidate.name),
            CompletionKind::Symbol => candidate.name.clone(),
        }
    }
}

/// The word the server named, and the candidates it offered for it.
///
/// This is one answer, held against the question it answers: `complete` is asked about
/// a text and a caret, and what it says is only ever true of *that* text. The popup
/// reads it back through [`held`], which is what makes a keystroke since the answer
/// one keystroke's worth of difference rather than a wrong word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// What the caret is on, or nothing when it was on nothing completable.
    pub kind: Option<CompletionKind>,
    /// The range a chosen candidate's **name** replaces, in bytes of the question's
    /// text: for a command, from just past its slash, because a name is typed without
    /// one.
    pub name: Range<usize>,
    /// The candidates, in the server's own order.
    pub items: Vec<Candidate>,
}

impl Answer {
    /// Nothing to complete: the server's answer for a caret on prose, and this crate's
    /// for a question that was never answered (a tab with no server to ask).
    pub fn none() -> Answer {
        Answer {
            kind: None,
            name: 0..0,
            items: Vec::new(),
        }
    }
}

/// The text a question was asked about, and where the caret was in it (a byte offset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// The text the question is about.
    pub text: String,
    /// Where the caret was in it, as a byte offset.
    pub cursor: usize,
}

/// What the answer still says about the word under the caret, as the text stands now.
///
/// The word is the one the server named — this crate does not decide what a word is —
/// carried forward over what has been typed into it since: a caret that typed `p` into
/// `/com` is still in that word, one character longer. Anything else is not this
/// answer's text any more (a paste before the caret, a delete, a caret somewhere else)
/// and the popup waits for the answer that is coming, which every edit asks for.
#[derive(Debug)]
pub(crate) struct Held<'a> {
    pub(crate) kind: CompletionKind,
    /// The whole unit, in bytes of the text as it stands now: what accept replaces. A
    /// command's includes the slash its name is typed without.
    pub(crate) word: Range<usize>,
    /// The typed half of the word, which the candidates are matched against.
    pub(crate) prefix: String,
    /// What the server offered for the answered prefix.
    pub(crate) items: &'a [Candidate],
}

/// The word in `answer`, held against `text` at `caret`: see [`Held`].
pub(crate) fn held<'a>(
    answer: &'a Answer,
    question: &Question,
    text: &str,
    caret: usize,
) -> Option<Held<'a>> {
    let kind = answer.kind?;
    let asked = question.cursor;
    if !text.is_char_boundary(asked) || !text.is_char_boundary(caret) {
        return None;
    }
    // The caret was somewhere inside the unit the server named — that is what "the word
    // holding the caret" means — so a keystroke at the caret lands inside it too.
    if !(answer.name.start <= asked && asked <= answer.name.end) {
        return None;
    }
    // Either the text is the very one the question carried — the caret may have moved
    // inside the word since, which is still that word — or it is that text with what
    // the reader has typed at the caret added to it, the caret being right after what it
    // typed. A text that is neither is not this answer's.
    let typed = text.len().checked_sub(question.text.len())?;
    let rest = asked.checked_add(typed)?;
    if rest > text.len() || text[..asked] != question.text[..asked] {
        return None;
    }
    if text[rest..] != question.text[asked..] {
        return None;
    }
    if typed > 0 && caret != rest {
        return None;
    }
    // The unit the server named grows by exactly what was typed into it.
    let name = answer.name.start..answer.name.end + typed;
    // A command's name is typed without its slash: the unit starts at the slash, which
    // the answer's own range is just past.
    let (word, prefix_start) = match kind {
        CompletionKind::Command => (name.start.checked_sub(1)?..name.end, name.start),
        CompletionKind::Symbol => (name.start..name.end, name.start),
    };
    if prefix_start > caret || caret > word.end || word.end > text.len() {
        return None;
    }
    Some(Held {
        kind,
        prefix: text.get(prefix_start..caret)?.to_string(),
        word,
        items: &answer.items,
    })
}

/// The candidates PREFIX leaves, in the order they are drawn: the names it
/// begins first — case-insensitively, since a command word is typed in the case
/// it is written in — then the names it is a subsequence of (`/rl` finds
/// `reload`), each group keeping the server's own order.
pub(crate) fn matches(commands: &[Candidate], prefix: &str) -> Vec<Candidate> {
    let begins = |candidate: &&Candidate| begins_with(&candidate.name, prefix);
    let mut out: Vec<Candidate> = commands.iter().filter(begins).cloned().collect();
    out.extend(
        commands
            .iter()
            .filter(|candidate| !begins(candidate) && is_subsequence(&candidate.name, prefix))
            .cloned(),
    );
    out
}

/// The candidates whose names begin with PREFIX, in the order they came in.
///
/// This is what the image's own answers are filtered by — and nothing else,
/// because prefix matching is exactly what `evo.eval:completions-for` offered:
/// a list that came back one keystroke ago covers the token as far as it has
/// been typed.
pub(crate) fn prefix_matches(rows: &[Candidate], prefix: &str) -> Vec<Candidate> {
    rows.iter()
        .filter(|candidate| begins_with(&candidate.name, prefix))
        .cloned()
        .collect()
}

/// Whether `name` starts with `prefix`, ignoring case.
fn begins_with(name: &str, prefix: &str) -> bool {
    name.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Whether `prefix`'s characters appear in `name` in order, ignoring case.
fn is_subsequence(name: &str, prefix: &str) -> bool {
    let mut rest = name.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|want| rest.any(|c| c == want))
}

/// Whether the popup has nothing left to choose: the only candidate is the word
/// already typed, so it would show the reader their own input back — and, worse,
/// capture the ↑/↓ that browse the history. A name completes itself out of
/// existence the moment it is whole.
pub(crate) fn settled(prefix: &str, rows: &[Candidate]) -> bool {
    rows.len() == 1 && rows[0].name == prefix
}

/// The parts of NAME the prefix actually matched, as byte ranges of NAME: one run
/// for a name the prefix begins, and one run per character for a name the prefix is
/// a subsequence of.
///
/// These are the ranges a row draws heavier — the reader sees *why* a candidate is
/// on the list, which for a `/lo` that finds `reload` is not obvious.
pub(crate) fn matched_ranges(name: &str, prefix: &str) -> Vec<Range<usize>> {
    if prefix.is_empty() {
        return Vec::new();
    }
    if begins_with(name, prefix) {
        return std::iter::once(0..prefix.len()).collect();
    }
    // The same walk `is_subsequence` does, keeping the positions it passed.
    let mut wanted = prefix.chars().flat_map(char::to_lowercase).peekable();
    let mut ranges = Vec::new();
    for (offset, c) in name.char_indices() {
        let Some(want) = wanted.peek().copied() else {
            break;
        };
        if c.to_lowercase().eq([want]) {
            wanted.next();
            ranges.push(offset..offset + c.len_utf8());
        }
    }
    // A walk that ran out of name is not a match at all: nothing is drawn heavier.
    if wanted.next().is_some() {
        return Vec::new();
    }
    ranges
}

/// The message after accepting `name` over `word`, and where the caret lands.
///
/// A command replaces its whole word — and at the message's own start opens its
/// argument with a space, since that is what comes next. A symbol replaces just
/// the token under the caret, leaving the rest of the form alone.
pub(crate) fn accepted(
    text: &str,
    kind: CompletionKind,
    word: &Range<usize>,
    name: &str,
) -> (String, usize) {
    let insertion = match kind {
        CompletionKind::Command if word.start == 0 => format!("/{name} "),
        CompletionKind::Command => format!("/{name}"),
        CompletionKind::Symbol => name.to_string(),
    };
    let mut out = String::with_capacity(text.len() + insertion.len());
    out.push_str(&text[..word.start]);
    out.push_str(&insertion);
    out.push_str(&text[word.end..]);
    let caret = word.start + insertion.len();
    (out, caret)
}

/// The characters a command word is written with: what a message that *is* one
/// command may hold after its slash.
fn is_command_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-')
}

/// The slash command a whole message is, as `(name, args)`, or `None` when the
/// message is the reader's words — including a message that only mentions a
/// command.
///
/// One command, from its start: `/` and then a word of command characters, with
/// everything after it as arguments (newlines included, so `/eval` takes a whole
/// form). `//` is how a message is written that begins with a slash and means
/// it.
pub(crate) fn command_message(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_matches(char::is_whitespace);
    let rest = text.strip_prefix('/')?;
    if rest.starts_with('/') {
        return None;
    }
    let end = rest
        .find(|c: char| !is_command_char(c))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some((
        &rest[..end],
        rest[end..].trim_start_matches(char::is_whitespace),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str) -> Candidate {
        Candidate {
            name: name.to_string(),
            description: format!("{name} does something"),
        }
    }

    fn catalogue() -> Vec<Candidate> {
        ["help", "reload", "lore", "memory", "run", "quit"]
            .iter()
            .map(|name| candidate(name))
            .collect()
    }

    /// What the answer says about the caret now, as the row it draws: the kind, the
    /// typed prefix, and the word as the text holds it.
    fn seen(
        answer: &Answer,
        question: &Question,
        text: &str,
        caret: usize,
    ) -> Option<(CompletionKind, String, String)> {
        held(answer, question, text, caret).map(|held| {
            (
                held.kind,
                held.prefix.clone(),
                text[held.word.clone()].to_string(),
            )
        })
    }

    /// An answer as a server sends one for `/word`: the range is the word after the
    /// slash, which is what a name replaces.
    fn command_answer(name: Range<usize>) -> Answer {
        an_answer("", CompletionKind::Command, name)
    }

    fn an_answer(_text: &str, kind: CompletionKind, name: Range<usize>) -> Answer {
        Answer {
            kind: Some(kind),
            name,
            items: vec![candidate("help")],
        }
    }

    /// The state the popup is really in most of the time: the caret asked, the server
    /// answered about the text as it was then, and nothing has moved since.
    #[test]
    fn an_answer_about_the_text_the_caret_is_still_in_is_the_word_itself() {
        let text = "run /hep now";
        let question = Question {
            text: text.to_string(),
            cursor: 8,
        };
        // The server said: the caret is in the command word `/hep`, the name replaces
        // the bytes 5..8 (`hep`, just past the slash).
        let answer = command_answer(5..8);
        assert_eq!(
            seen(&answer, &question, text, 8),
            Some((CompletionKind::Command, "hep".into(), "/hep".into())),
            "the word includes the slash its name is typed without"
        );
        // Mid-word: the word is still the whole one, and the prefix is what is typed.
        assert_eq!(
            seen(&answer, &question, text, 7),
            Some((CompletionKind::Command, "he".into(), "/hep".into()))
        );
        // The caret at the word's own start: nothing typed yet, every candidate.
        assert_eq!(
            seen(&answer, &question, text, 5),
            Some((CompletionKind::Command, String::new(), "/hep".into()))
        );
    }

    /// A symbol's own answer: there is no slash, so the range is the token itself.
    #[test]
    fn a_symbol_answer_is_the_token_and_nothing_else() {
        let text = "/eval (evo:comp";
        let question = Question {
            text: text.to_string(),
            cursor: 15,
        };
        let answer = an_answer("", CompletionKind::Symbol, 7..15);
        assert_eq!(
            seen(&answer, &question, text, 15),
            Some((CompletionKind::Symbol, "evo:comp".into(), "evo:comp".into()))
        );
        // A caret in the middle of a token the server named whole: the token is what a
        // candidate replaces, and the prefix is up to the caret.
        assert_eq!(
            seen(&answer, &question, text, 10),
            Some((CompletionKind::Symbol, "evo".into(), "evo:comp".into()))
        );
    }

    /// The keystroke the answer is a moment behind: the word grows by what was typed
    /// into it, and the prefix with it — so the list can be filtered for the longer
    /// prefix rather than the popup flickering away between answers.
    #[test]
    fn a_word_the_caret_typed_into_grows_with_it() {
        let asked = "run /hep now";
        let question = Question {
            text: asked.to_string(),
            cursor: 8,
        };
        let answer = command_answer(5..8);
        let text = "run /hepl now";
        assert_eq!(
            seen(&answer, &question, text, 9),
            Some((CompletionKind::Command, "hepl".into(), "/hepl".into())),
            "one character longer, and the same word"
        );
        // Inside `/eval`, where the token runs on: the typed characters are inside it.
        let asked = "/eval (evo:comp";
        let question = Question {
            text: asked.to_string(),
            cursor: 15,
        };
        let answer = an_answer("", CompletionKind::Symbol, 7..15);
        assert_eq!(
            seen(&answer, &question, "/eval (evo:complete", 19),
            Some((
                CompletionKind::Symbol,
                "evo:complete".into(),
                "evo:complete".into()
            ))
        );
        // A caret that typed and then went back is a text this answer cannot place:
        // which part of it the word still covers is not something the answer knows.
        assert_eq!(seen(&answer, &question, "/eval (evo:complete", 18), None);
        // Characters that are not one byte count in bytes, as the text does.
        let asked = "run /hé";
        let question = Question {
            text: asked.to_string(),
            cursor: "run /hé".len(),
        };
        let answer = command_answer(5.."run /hé".len());
        assert_eq!(
            seen(&answer, &question, "run /hép", "run /hép".len()),
            Some((CompletionKind::Command, "hép".into(), "/hép".into()))
        );
    }

    /// Anything else is a text the answer says nothing about, and the popup waits for
    /// the answer to *this* one — which every edit asks for.
    #[test]
    fn an_answer_is_about_the_very_text_it_was_asked_about() {
        let asked = "run /hep now";
        let question = Question {
            text: asked.to_string(),
            cursor: 8,
        };
        let answer = command_answer(5..8);
        // A paste before the caret: the characters the question carried are not there
        // any more.
        assert_eq!(seen(&answer, &question, "please run /hep now", 15), None);
        // A delete.
        assert_eq!(seen(&answer, &question, "run /ep now", 7), None);
        // The caret taken somewhere else entirely — outside the word the answer named.
        assert_eq!(seen(&answer, &question, asked, 0), None);
        assert_eq!(seen(&answer, &question, asked, 11), None);
        // A keystroke in the middle of the word, rather than at the caret: the answer's
        // own text is not what is in the box.
        assert_eq!(seen(&answer, &question, "run /hxep now", 9), None);
        // Characters after the caret, with the caret not at the end of what was typed:
        // the answer cannot say which part of the text its word still covers.
        assert_eq!(seen(&answer, &question, "run /hepx now", 8), None);
        // The answer for nothing at all is not a word.
        let nothing = Answer::none();
        assert_eq!(seen(&nothing, &question, asked, 8), None);
    }

    #[test]
    fn commands_are_filtered_by_prefix_first_then_by_subsequence() {
        let names = |rows: Vec<Candidate>| rows.into_iter().map(|row| row.name).collect::<Vec<_>>();
        // The prefix group keeps the server's order, and comes first.
        assert_eq!(
            names(matches(&catalogue(), "re")),
            vec!["reload", "lore"],
            "`lore` only contains r-then-e; `reload` begins with both"
        );
        assert_eq!(
            names(matches(&catalogue(), "")),
            vec!["help", "reload", "lore", "memory", "run", "quit"]
        );
        // A subsequence finds what the prefix did not, after it.
        assert_eq!(
            names(matches(&catalogue(), "lo")),
            vec!["lore", "reload"],
            "`lore` begins with l-o; `reload` only contains them"
        );
        assert_eq!(names(matches(&catalogue(), "rl")), vec!["reload"]);
        assert!(matches(&catalogue(), "zz").is_empty());
        // Case-insensitively, both groups.
        assert_eq!(names(matches(&catalogue(), "RE")), vec!["reload", "lore"]);
    }

    /// The ranges as the text they cover, so a test reads like the row it draws.
    fn highlighted(name: &str, prefix: &str) -> Vec<String> {
        matched_ranges(name, prefix)
            .into_iter()
            .map(|range| name[range].to_string())
            .collect()
    }

    #[test]
    fn a_name_the_prefix_begins_emphasizes_its_own_beginning() {
        assert_eq!(highlighted("reload", "re"), vec!["re"]);
        assert_eq!(
            highlighted("reload", "RE"),
            vec!["re"],
            "what is drawn is the name's own characters, whatever case was typed"
        );
        assert_eq!(highlighted("reload", "reload"), vec!["reload"]);
        assert!(highlighted("reload", "").is_empty(), "nothing was typed");
    }

    #[test]
    fn a_name_the_prefix_is_a_subsequence_of_emphasizes_the_characters_it_hit() {
        // `lo` finds `reload`: the l and the o, and nothing between them.
        assert_eq!(highlighted("reload", "lo"), vec!["l", "o"]);
        assert_eq!(
            highlighted("global-memory", "mo"),
            vec!["m", "o"],
            "the first m in the name, then the first o after it"
        );
        assert_eq!(highlighted("memory", "mo"), vec!["m", "o"]);
        // A walk that runs out is not a match, and emphasizes nothing.
        assert!(highlighted("reload", "zz").is_empty());
        assert!(highlighted("car", "lo").is_empty());
    }

    #[test]
    fn a_popup_whose_only_candidate_is_the_word_is_settled() {
        assert!(settled("help", &[candidate("help")]));
        assert!(!settled("help", &[candidate("help"), candidate("helpme")]));
        assert!(!settled("hel", &[candidate("help")]));
        assert!(!settled("", &[]));
    }

    #[test]
    fn accepting_a_command_opens_its_arguments_at_the_message_start() {
        let word = 0..3;
        assert_eq!(
            accepted("/he", CompletionKind::Command, &word, "help"),
            ("/help ".to_string(), 6),
            "at the start a space is what comes next"
        );
        // Mid-message the word is replaced and nothing is appended.
        let text = "run /he now";
        assert_eq!(
            accepted(text, CompletionKind::Command, &(4..7), "help"),
            ("run /help now".to_string(), 9)
        );
    }

    #[test]
    fn accepting_a_symbol_replaces_only_its_token() {
        let text = "/eval (car (ev";
        assert_eq!(
            accepted(text, CompletionKind::Symbol, &(12..14), "evo:eval"),
            ("/eval (car (evo:eval".to_string(), 20),
        );
        let text = "/eval (evo:comp)";
        assert_eq!(
            accepted(
                text,
                CompletionKind::Symbol,
                &(7..15),
                "evo:completions-for"
            ),
            ("/eval (evo:completions-for)".to_string(), 26),
            "the closing paren is left where it was"
        );
    }

    #[test]
    fn a_whole_message_that_is_one_command_is_one() {
        assert_eq!(command_message("/help"), Some(("help", "")));
        assert_eq!(
            command_message("/lore  ship it "),
            Some(("lore", "ship it"))
        );
        assert_eq!(
            command_message("/eval (+ 1 2)\n"),
            Some(("eval", "(+ 1 2)"))
        );
        // Arguments keep their own shape, newlines included.
        assert_eq!(
            command_message("/eval (\n  + 1 2)"),
            Some(("eval", "(\n  + 1 2)"))
        );
        assert_eq!(command_message("  /help  "), Some(("help", "")));
        assert_eq!(command_message("/a-_b:c x"), Some(("a-_b:c", "x")));
    }

    #[test]
    fn anything_else_is_the_readers_words() {
        assert_eq!(command_message("hello"), None);
        assert_eq!(command_message("run /help now"), None);
        assert_eq!(command_message("//a literal slash"), None);
        assert_eq!(command_message("/"), None);
        assert_eq!(command_message("/ help"), None);
        assert_eq!(command_message("/help!"), Some(("help", "!")));
        assert_eq!(command_message(""), None);
        assert_eq!(command_message("   "), None);
    }
}
