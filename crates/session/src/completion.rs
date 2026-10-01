//! What a caret in an input box is on, and what could fill it — the `complete` op
//! (CONTRACT §5.6, `docs/serve.md` *Completion*).
//!
//! The rules are evo's, and this crate does not hold a copy of them: whether the
//! caret is in a `/command` word or inside `/eval`'s content, where that unit
//! begins and ends, which names match it, and what a candidate would replace are
//! all the server's answer. The client asks with its own text and caret and reads
//! the reply back.
//!
//! Two things are the client's, and both are here:
//!
//! * **the offsets.** The protocol counts characters — Unicode scalar values, the
//!   Lisp `length` of the text — and an input box works in bytes. `complete_request`
//!   converts the caret going out; [`completion`] converts the range coming back, so
//!   a caret after `✓` or `中文` asks about the position it is really at;
//! * **the question's identity.** An answer is only ever an answer about the text it
//!   was asked for, so it is read back against that same text, never against
//!   whatever is in the box a keystroke later.
//!
//! `complete` is a read: it needs nothing idle and it is not gated by
//! `--no-http-eval`, because reading a name list is not evaluating anything.

use serde_json::Value;

use crate::OpRequest;

/// Which of the two things the caret is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    /// A `/command` word, anywhere in the text. A candidate's name replaces the
    /// word *after* the slash, so a name never carries one.
    Command,
    /// A symbol of the running image, inside `/eval`'s content — the only place
    /// only the image can answer for.
    Symbol,
}

/// One candidate, exactly as the server offered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    /// What accepting it types: a command word without its slash, a symbol as it
    /// would be typed (package qualifier and all).
    pub name: String,
    /// What it is: the registry's line for a command, `function` or `variable` for a
    /// symbol.
    pub description: String,
}

/// `complete`'s answer for one question, in **bytes of the text that was asked**.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// What the caret is on, or `None` when it is on nothing completable — prose, a
    /// path, an empty token. A caret on nothing is an answer, not an error.
    pub kind: Option<CompletionKind>,
    /// Where the unit the caret is in begins, as a byte offset into the text: what a
    /// chosen candidate replaces.
    pub start: usize,
    /// Where it ends. A caret in the *middle* of a word or token names the whole one
    /// — the server replaces the unit the caret is in, not half of it — so this can
    /// be past the caret.
    pub end: usize,
    /// The candidates, in the server's own order.
    pub items: Vec<CompletionItem>,
}

/// What the caret is on in `text` at `cursor` (a byte offset), as the `complete`
/// request that asks about it.
pub fn complete_request(text: &str, cursor: usize) -> OpRequest {
    OpRequest::complete(text, char_offset(text, cursor))
}

/// The answer to a question about `text`, read back as a [`Completion`] whose offsets
/// are bytes of that same `text`.
///
/// Total: a result with no `kind` (the caret was on nothing), a reply whose `kind`
/// this build does not know, and an error's empty `result` all read as
/// [`Completion::default`] — nothing to complete — which is what the popup draws as
/// nothing at all. That is also how a server that does not offer the op reads: its
/// refusal leaves no popup, silently, as a refusal above somebody's typing would be
/// the wrong thing to say.
pub fn completion(text: &str, result: &Value) -> Completion {
    let kind = match result.get("kind").and_then(Value::as_str) {
        Some("command") => CompletionKind::Command,
        Some("symbol") => CompletionKind::Symbol,
        _ => return Completion::default(),
    };
    let (Some(start), Some(end)) = (
        range_offset(text, result.get("start")),
        range_offset(text, result.get("end")),
    ) else {
        // An offset this text cannot hold says the answer is about another text.
        return Completion::default();
    };
    if end < start {
        return Completion::default();
    }
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name").and_then(Value::as_str)?;
                    Some(CompletionItem {
                        name: name.to_string(),
                        description: item
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Completion {
        kind: Some(kind),
        start,
        end,
        items,
    }
}

/// A byte offset in `text` as the protocol counts it: characters — Unicode scalar
/// values — from the start.
///
/// A caret that is not on a character boundary is counted to the character it is
/// inside of, which is the position the reader would say they are at.
pub fn char_offset(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    text[..cursor].chars().count()
}

/// A protocol offset — characters — back as a byte offset into `text`, (`None` when
/// the text is shorter than the offset says).
pub fn byte_offset(text: &str, offset: usize) -> Option<usize> {
    if offset == 0 {
        return Some(0);
    }
    text.char_indices()
        .nth(offset)
        .map(|(byte, _)| byte)
        .or_else(|| (offset == text.chars().count()).then_some(text.len()))
}

/// One end of the range the answer names, as a byte offset: a missing or unusable
/// offset reads as `None`, which is the whole answer being about another text.
fn range_offset(text: &str, offset: Option<&Value>) -> Option<usize> {
    byte_offset(text, offset?.as_u64()? as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_caret_goes_out_counted_in_characters() {
        // ASCII: bytes and characters are the same count, so the request says what the
        // box knows.
        let request = complete_request("run /comp now", 9);
        assert_eq!(request.op, "complete");
        assert_eq!(request.args["cursor"], 9);
        assert_eq!(request.args["text"], "run /comp now");

        // A caret past characters that are not one byte: `✓` is three bytes in UTF-8,
        // and the protocol counts one character for it.
        let text = "please ✓ ✓ /mo now";
        // `/mo` ends at byte 18 — the two ✓ are three bytes each — and the caret there
        // is the protocol's 14th character.
        assert_eq!("please ✓ ✓ /mo".len(), 18);
        assert_eq!(complete_request(text, 18).args["cursor"], 14);
        // The same for a caret inside a character, if one ever gets that far: the
        // position it counts is the character the caret is inside (`✓` is the bytes
        // 7, 8 and 9, and the 8th character of the text).
        assert_eq!(char_offset(text, 9), 7);
        assert_eq!(char_offset(text, 10), 8);
    }

    #[test]
    fn an_offset_comes_back_as_a_byte_position() {
        let text = "please ✓ ✓ /mo now";
        assert_eq!(byte_offset(text, 0), Some(0));
        assert_eq!(byte_offset(text, 7), Some(7), "the ✓ begins at byte 7");
        assert_eq!(
            byte_offset(text, 8),
            Some(10),
            "and the next character at 10"
        );
        assert_eq!(byte_offset(text, 12), Some(16), "`m` is the 13th character");
        assert_eq!(byte_offset(text, 14), Some(18), "and `/mo` ends at byte 18");
        assert_eq!(byte_offset(text, text.chars().count()), Some(text.len()));
        assert_eq!(byte_offset(text, 99), None, "past the end is not an offset");
        // Round trip: every character boundary survives the two conversions.
        for (byte, _) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            let char = char_offset(text, byte);
            assert_eq!(byte_offset(text, char), Some(byte), "byte {byte}");
        }
    }

    #[test]
    fn a_command_answer_reads_back_in_bytes() {
        let text = "please ✓ ✓ /mo now";
        let answer = json!({
            "kind": "command", "start": 12, "end": 14,
            "items": [{"name": "model", "description": "pick the model"},
                      {"name": "memory", "description": "what is remembered"}]
        });
        let completion = completion(text, &answer);
        assert_eq!(completion.kind, Some(CompletionKind::Command));
        assert_eq!(
            (completion.start, completion.end),
            (16, 18),
            "characters 12..14 are the bytes 16..18 — one ✓ is three bytes — and they              are the word after the slash"
        );
        assert_eq!(&text[completion.start..completion.end], "mo");
        assert_eq!(
            completion.items,
            vec![
                CompletionItem {
                    name: "model".into(),
                    description: "pick the model".into()
                },
                CompletionItem {
                    name: "memory".into(),
                    description: "what is remembered".into()
                },
            ]
        );
    }

    #[test]
    fn a_symbol_answer_reads_back_with_the_whole_token() {
        // A caret in the middle of a token: the server names the token it is *in*, so
        // the range reaches past the caret — what a candidate would replace.
        let text = "/eval (evo.eval:tok";
        let answer = json!({
            "kind": "symbol", "start": 7, "end": 19,
            "items": [{"name": "evo.eval:token-start", "description": "function"}]
        });
        let completion = completion(text, &answer);
        assert_eq!(completion.kind, Some(CompletionKind::Symbol));
        assert_eq!((completion.start, completion.end), (7, 19));
        assert_eq!(&text[completion.start..completion.end], "evo.eval:tok");
        assert!(completion.end > char_offset(text, text.len()) - 1);
    }

    #[test]
    fn a_caret_on_nothing_is_an_answer_with_nothing_in_it() {
        let text = "nothing here, just prose";
        // The server's own answer for a caret on nothing.
        let nothing = completion(
            text,
            &json!({"kind": null, "start": null, "end": null, "items": []}),
        );
        assert_eq!(nothing, Completion::default());
        // An error's reply has no result at all, and a kind this build does not know
        // is not one it draws.
        assert_eq!(completion(text, &Value::Null), Completion::default());
        assert_eq!(completion(text, &json!({})), Completion::default());
        assert_eq!(
            completion(text, &json!({"kind": "path", "start": 0, "end": 3})).kind,
            None
        );
        // An answer about a text this one cannot be — offsets past its end — is not an
        // answer about this caret.
        assert_eq!(
            completion(text, &json!({"kind": "command", "start": 40, "end": 44})),
            Completion::default()
        );
        assert_eq!(
            completion(text, &json!({"kind": "command", "start": 4, "end": 2})),
            Completion::default(),
            "a range that runs backwards"
        );
    }

    #[test]
    fn a_candidate_with_no_name_is_not_a_candidate() {
        let completion = completion(
            "run /c",
            &json!({"kind": "command", "start": 4, "end": 6,
                    "items": [{"name": "compact", "description": "d"},
                              {"description": "no name"},
                              {"name": "check"}]}),
        );
        assert_eq!(
            completion
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["compact", "check"]
        );
        assert_eq!(completion.items[1].description, "");
    }
}
