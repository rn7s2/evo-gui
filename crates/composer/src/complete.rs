//! The inline completion the caret's word raises (§7.3).
//!
//! Two things complete here, and both are evo's to decide:
//!
//! * a `/command` word, against the commands `GET /catalog` lists;
//! * a symbol inside the `/eval` command's content, against the *live image* —
//!   the one asked of the image itself (the `eval` op), because only the image
//!   knows what its own packages hold.
//!
//! What lives here is the other half: where the word is, which candidates match
//! it, whether there is anything left to choose, and what accepting one does to
//! the text. All of it is pure, so the popup's behaviour is unit-tested without
//! a window.

use std::ops::Range;

/// How many candidates the popup shows at once. Past this the list scrolls.
pub(crate) const MAX_ROWS: usize = 8;

/// Which of the two things the caret is completing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A `/command` word: the command layer's own registry.
    Command,
    /// A symbol of the running image, inside the `/eval` command's content.
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

/// What the caret sits on, and what would finish it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) kind: Kind,
    /// The typed half of the word, which the candidates match.
    pub(crate) prefix: String,
    /// The whole word, in bytes of the message: what accept replaces.
    pub(crate) word: Range<usize>,
}

/// The popup as it stands: the caret's word, and what could finish it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Popup {
    pub(crate) kind: Kind,
    /// The word as it was when these rows were chosen. A prefix that has moved
    /// on since means the rows are stale, and the popup is rebuilt.
    pub(crate) prefix: String,
    pub(crate) word: Range<usize>,
    pub(crate) rows: Vec<Candidate>,
    /// Which row is highlighted.
    pub(crate) index: usize,
}

impl Popup {
    /// The rows, and the word they were chosen for.
    pub(crate) fn new(target: &Target, rows: Vec<Candidate>) -> Popup {
        Popup {
            kind: target.kind,
            prefix: target.prefix.clone(),
            word: target.word.clone(),
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
            Kind::Command => format!("/{}", candidate.name),
            Kind::Symbol => candidate.name.clone(),
        }
    }
}

/// What the caret's word would complete to, or `None` when it completes to
/// nothing: prose, a path, an already-whole command word, an empty symbol.
pub(crate) fn target(text: &str, caret: usize) -> Option<Target> {
    let caret = caret.min(text.len());
    if !text.is_char_boundary(caret) {
        return None;
    }
    // The content of the symbol-completing command is Lisp, not a command line:
    // inside it nothing completes against the command registry, not even a word
    // that happens to start with a slash.
    if eval_content(text, caret) {
        let word = token_at(text, caret)?;
        let prefix = text[word.start..caret].to_string();
        return Some(Target {
            kind: Kind::Symbol,
            prefix,
            word,
        });
    }
    command_word(text, caret)
}

/// The `/command` word the caret sits in, when there is one.
///
/// The word is a run of non-whitespace with the caret somewhere inside it: it
/// has to start with the slash, what has been typed of it has to look like a
/// command (`[A-Za-z0-9:_-]`), and a second slash ends it — `/usr/local/bin` is
/// a path, and `//` is how a message is written that begins with one. A word
/// anywhere in the message counts: `/help` is a command word in
/// `run /help now` too, and accepting it there replaces just the word.
fn command_word(text: &str, caret: usize) -> Option<Target> {
    let start = word_start(text, caret);
    // The slash itself, and nothing but a command's own characters after it.
    let typed = text.get(start + 1..caret)?;
    if !text[start..].starts_with('/') || !typed.chars().all(is_command_char) {
        return None;
    }
    let end = word_end(text, caret);
    // A second slash ends the word: `/usr/local` is a path, not a command.
    if text[start + 1..end].contains('/') {
        return None;
    }
    Some(Target {
        kind: Kind::Command,
        prefix: typed.to_string(),
        word: start..end,
    })
}

/// The message is an invocation of `/eval`, and the caret is past its command
/// word: the whitespace that ends the word is what starts the content.
fn eval_content(text: &str, caret: usize) -> bool {
    let Some(end) = text.find(char::is_whitespace) else {
        return false;
    };
    if !text[..end].eq_ignore_ascii_case(EVAL_COMMAND) {
        return false;
    }
    // Past the word. A token can never reach back into the command word: the
    // whitespace that ended it is one of the token's own delimiters.
    caret > end
}

/// The command whose content completes against the image rather than a name
/// list (`evo.eval`'s own `/eval`).
pub(crate) const EVAL_COMMAND: &str = "/eval";

/// The symbol token ending at the caret in TEXT, as bytes, or `None` when there
/// is nothing to ask the image about.
///
/// The delimiters are the ones evo's own completer uses
/// (`evo.eval::*token-delimiters*`): the token the image is asked about has to
/// be the token the answer replaces.
fn token_at(text: &str, caret: usize) -> Option<Range<usize>> {
    let start = token_start(text, caret);
    (start < caret).then_some(start..caret)
}

/// Where the symbol token ending at `caret` begins.
fn token_start(text: &str, caret: usize) -> usize {
    let mut start = caret.min(text.len());
    while start > 0 {
        let before = text[..start].chars().next_back();
        match before {
            Some(c) if !is_token_delimiter(c) => start -= c.len_utf8(),
            _ => break,
        }
    }
    start
}

/// Characters that end a symbol token (`evo.eval::*token-delimiters*`). Colon is
/// absent deliberately, here as there: a package qualifier completes as one
/// piece.
fn is_token_delimiter(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '(' | ')' | '\'' | '"' | '`' | ',' | ';' | '#'
    )
}

/// The characters a command word is written with, and the ones a message has to
/// begin with to be that command rather than prose that mentions it.
fn is_command_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-')
}

/// Where the whitespace-delimited word holding `caret` begins.
fn word_start(text: &str, caret: usize) -> usize {
    text[..caret.min(text.len())]
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1)
}

/// Where that word ends: the caret when it is mid-word, otherwise the next
/// whitespace after it.
fn word_end(text: &str, caret: usize) -> usize {
    let caret = caret.min(text.len());
    text[caret..]
        .find(char::is_whitespace)
        .map_or(text.len(), |index| caret + index)
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

/// The message after accepting `name` over `word`, and where the caret lands.
///
/// A command replaces its whole word — and at the message's own start opens its
/// argument with a space, since that is what comes next. A symbol replaces just
/// the token under the caret, leaving the rest of the form alone.
pub(crate) fn accepted(text: &str, kind: Kind, word: &Range<usize>, name: &str) -> (String, usize) {
    let insertion = match kind {
        Kind::Command if word.start == 0 => format!("/{name} "),
        Kind::Command => format!("/{name}"),
        Kind::Symbol => name.to_string(),
    };
    let mut out = String::with_capacity(text.len() + insertion.len());
    out.push_str(&text[..word.start]);
    out.push_str(&insertion);
    out.push_str(&text[word.end..]);
    let caret = word.start + insertion.len();
    (out, caret)
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

    /// The target's kind, prefix and word, as one string — enough to see.
    fn seen(text: &str, caret: usize) -> Option<(Kind, String, String)> {
        target(text, caret).map(|target| {
            (
                target.kind,
                target.prefix,
                text[target.word.clone()].to_string(),
            )
        })
    }

    #[test]
    fn a_slash_word_at_the_caret_is_a_command_target() {
        assert_eq!(
            seen("/hel", 4),
            Some((Kind::Command, "hel".into(), "/hel".into()))
        );
        // Just the slash: every command is a candidate.
        assert_eq!(
            seen("/", 1),
            Some((Kind::Command, String::new(), "/".into()))
        );
        // Mid-message, and mid-word: the word is what accept replaces.
        assert_eq!(
            seen("run /hep now", 7),
            Some((Kind::Command, "he".into(), "/hep".into()))
        );
    }

    #[test]
    fn prose_and_paths_are_not_command_words() {
        // No slash.
        assert_eq!(seen("hello", 5), None);
        assert_eq!(seen("and/or", 7), None);
        // A second slash ends the word: a path is not a command.
        assert_eq!(seen("/usr/lo", 7), None);
        assert_eq!(seen("run /usr/local/bin", 18), None);
        // `//` is how a message that begins with a slash is written.
        assert_eq!(seen("//note", 6), None);
        // A word that has already stopped looking like a command.
        assert_eq!(seen("/hel!p", 6), None);
        assert_eq!(seen("/he lp", 6), None, "the word ended at the space");
    }

    #[test]
    fn the_eval_content_completes_against_the_image() {
        assert_eq!(
            seen("/eval (evo:comp", 16),
            Some((Kind::Symbol, "evo:comp".into(), "evo:comp".into()))
        );
        // Until the space ends the command word, it is a command word.
        assert_eq!(
            seen("/eval", 5),
            Some((Kind::Command, "eval".into(), "/eval".into()))
        );
        assert_eq!(
            seen("/eval ", 6),
            None,
            "an empty token asks the image nothing"
        );
        // A package qualifier is one token, colon and all.
        assert_eq!(
            seen("/eval (evo.eval::token-st", 25),
            Some((
                Kind::Symbol,
                "evo.eval::token-st".into(),
                "evo.eval::token-st".into()
            ))
        );
        // The token ends at a delimiter, and only the token is replaced.
        assert_eq!(
            seen("/eval (car (li", 14),
            Some((Kind::Symbol, "li".into(), "li".into()))
        );
        // Past the first line, a token is a token wherever it sits.
        assert_eq!(
            seen("/eval (foo\n  (ba", 16),
            Some((Kind::Symbol, "ba".into(), "ba".into()))
        );
        // A newline ends the command word as well as a space does.
        assert_eq!(
            seen("/eval\n(fo", 9),
            Some((Kind::Symbol, "fo".into(), "fo".into()))
        );
    }

    #[test]
    fn only_the_eval_command_completes_its_content() {
        // Another command's content is its own: nothing completes there.
        assert_eq!(seen("/lore evo:foo", 14), None);
        assert_eq!(
            seen("/EVAL (ev", 10),
            Some((Kind::Symbol, "ev".into(), "ev".into()))
        );
        // An `eval` word that is not the command: it is a command word of its
        // own, and nothing of it completes against the image.
        assert_eq!(
            seen("/evaluate", 9),
            Some((Kind::Command, "evaluate".into(), "/evaluate".into()))
        );
        // A message that mentions /eval is the reader's words: the word is a
        // command word, and `command_message` is what keeps it prose.
        assert_eq!(
            seen("and /eval", 9),
            Some((Kind::Command, "eval".into(), "/eval".into())),
            "a word that starts with a slash anywhere is a command word"
        );
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
            accepted("/he", Kind::Command, &word, "help"),
            ("/help ".to_string(), 6),
            "at the start a space is what comes next"
        );
        // Mid-message the word is replaced and nothing is appended.
        let text = "run /he now";
        assert_eq!(
            accepted(text, Kind::Command, &(4..7), "help"),
            ("run /help now".to_string(), 9)
        );
    }

    #[test]
    fn accepting_a_symbol_replaces_only_its_token() {
        let text = "/eval (car (ev";
        assert_eq!(
            accepted(text, Kind::Symbol, &(12..14), "evo:eval"),
            ("/eval (car (evo:eval".to_string(), 20),
        );
        let text = "/eval (evo:comp)";
        assert_eq!(
            accepted(text, Kind::Symbol, &(7..15), "evo:completions-for"),
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
