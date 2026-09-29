//! Redaction: server text is not trusted to be free of credentials.
//!
//! A live server puts user configuration into its replies — the real
//! `GET /registry` 500 in docs/proofs-real.md echoes an MCP bearer token — and
//! that text ends up in the app's log and, when a probe fails, on screen as the
//! empty tab's caption. So every string this crate stores *because a server or
//! a server's log said it* goes through [`redact`] first:
//!
//! * [`StatusError`](crate::StatusError) — its `message` and its whole `raw`
//!   body (`POST` refusals and non-2xx replies);
//! * [`BootFailure`](crate::BootFailure) — the log tail, and the message;
//! * [`log_tail`](crate::log_tail) — what a caller reads out of a server log.
//!
//! The rules, in the order they are tried:
//!
//! 1. an auth scheme — `Bearer <value>`, `Basic <value>` — masks its value;
//! 2. a named key — `authorization:`, `api_key=`, `token=`, `apikey`, … —
//!    masks a credential-shaped value;
//! 3. `sk-…` masks to the end of the key;
//! 4. a bare high-entropy run — 32+ characters mixing upper, lower and digits
//!    with at least 17 distinct of them — masks itself, for a credential that
//!    arrived with no marker at all.
//!
//! A *value* only counts as a credential when it looks like one, so ordinary
//! prose survives: "a basic idea", "the token file", `api_key=` in a sentence,
//! paths, URLs and session ids are all left alone. Nothing here is a substitute
//! for not putting secrets on the wire — it is the last line, for text we did
//! not write.

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::Value;

/// What a credential becomes.
pub const MASK: &str = "<redacted>";

/// The key names whose value is a credential (`:`, `=` or `:` after spaces).
const KEY_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-api-token",
    "api-key",
    "api_key",
    "apikey",
    "access-token",
    "access_token",
    "refresh-token",
    "refresh_token",
    "client-secret",
    "client_secret",
    "token",
    "secret",
    "password",
    "passwd",
];

/// Bytes a credential may be made of: everything in a base64/hex/`sk-` key,
/// plus the separators and padding they carry.
fn is_credential_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'/' | b'=' | b'~' | b'.')
}

/// Bytes that do not carry a path or an address, for the unmarked-token rule:
/// `/`, `.`, `:` and `@` are what a path, a URL, a version and an email are
/// made of, and a credential that contains one of them is not this rule's job.
fn is_entropy_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'=' | b'~')
}

/// The longest credential run at the start of `text`.
fn run_of(text: &str, keep: impl Fn(u8) -> bool) -> &str {
    let end = text.bytes().take_while(|b| keep(*b)).count();
    &text[..end]
}

/// Whether the value after a scheme or a key name looks like a credential
/// rather than a word: long enough, made of credential bytes, and either long
/// in the absolute, or carrying a digit, both cases, or a separator.
fn looks_like_a_credential(value: &str) -> bool {
    let n = value.len();
    if n < 8 || !value.bytes().all(is_credential_byte) {
        return false;
    }
    n >= 24
        || value.bytes().any(|b| b.is_ascii_digit())
        || (value.bytes().any(|b| b.is_ascii_lowercase())
            && value.bytes().any(|b| b.is_ascii_uppercase()))
        || value
            .bytes()
            .any(|b| matches!(b, b'-' | b'_' | b'+' | b'~' | b'='))
}

/// `text` with every credential it holds replaced by [`MASK`].
///
/// Borrowed when there was nothing to mask, so the common case costs nothing.
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    let mut masked = false;
    for (i, _) in text.char_indices() {
        if i < copied {
            continue;
        }
        let Some(hit) = hit_at(text, i) else { continue };
        if !masked {
            out.reserve(text.len() + MASK.len());
            masked = true;
        }
        out.push_str(&text[copied..hit.start]);
        out.push_str(MASK);
        copied = hit.end;
    }
    if !masked {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// The same redaction over every string in a JSON value, structure untouched.
pub fn redact_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Cow::Owned(masked) = redact(text) {
                *text = masked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::Object(fields) => fields.values_mut().for_each(redact_json),
        _ => {}
    }
}

/// The range of the credential that starts at byte `i`, if one does.
fn hit_at(text: &str, i: usize) -> Option<Hit> {
    let rest = &text[i..];
    // 1. An auth scheme, then its value.
    for scheme in ["bearer", "basic"] {
        if !starts_with_ci(rest, scheme) || !boundary_before(text, i) {
            continue;
        }
        let after = &rest[scheme.len()..];
        let spaces = after.len() - after.trim_start_matches(' ').len();
        let value = run_of(&after[spaces..], is_credential_byte);
        if looks_like_a_credential(value) {
            let start = i + scheme.len() + spaces;
            return Some(Hit {
                start,
                end: start + value.len(),
            });
        }
    }
    // 2. A named key, then its value.
    for name in KEY_NAMES {
        if !starts_with_ci(rest, name) || !boundary_before(text, i) {
            continue;
        }
        let after_name = &rest[name.len()..];
        // A quoted key (`"apikey" = "…"`, `token: "…"`) puts punctuation between
        // the name and its separator; both come off before the value is read.
        let before_separator =
            after_name.len() - after_name.trim_start_matches([' ', '\t', '"', '\'']).len();
        let separator = after_name[before_separator..].chars().next();
        if !matches!(separator, Some(':') | Some('=')) {
            continue;
        }
        let after_separator = &after_name[before_separator + 1..];
        let skip = after_separator.len()
            - after_separator
                .trim_start_matches([' ', '\t', '"', '\''])
                .len();
        let value = run_of(&after_separator[skip..], is_credential_byte);
        if looks_like_a_credential(value) {
            let start = i + name.len() + before_separator + 1 + skip;
            return Some(Hit {
                start,
                end: start + value.len(),
            });
        }
    }
    // 3. An `sk-…` key, marker included: `sk-` plus a key-shaped run.
    if starts_with_ci(rest, "sk-") && boundary_before(text, i) {
        let value = run_of(&rest[3..], is_credential_byte);
        if value.len() >= 8 {
            return Some(Hit {
                start: i,
                end: i + 3 + value.len(),
            });
        }
    }
    // 4. A bare high-entropy run: no marker to go by.
    if boundary_before(text, i) {
        let value = run_of(rest, is_entropy_byte);
        if value.len() >= 32 && looks_random(value) {
            return Some(Hit {
                start: i,
                end: i + value.len(),
            });
        }
    }
    None
}

/// Whether a marker-free run of 32+ characters reads as a generated token: both
/// cases and a digit all present, over at least 17 distinct bytes. A hex digest
/// (one case, ≤ 16 distinct) and a session id (`20260929T121155Z_5455e533…`)
/// both fail this and stay readable.
fn looks_random(run: &str) -> bool {
    let distinct: BTreeSet<u8> = run.bytes().collect();
    distinct.len() >= 17
        && run.bytes().any(|b| b.is_ascii_digit())
        && run.bytes().any(|b| b.is_ascii_lowercase())
        && run.bytes().any(|b| b.is_ascii_uppercase())
}

/// The byte range of a value to mask.
struct Hit {
    start: usize,
    end: usize,
}

/// `rest` begins with `word`, ASCII-case-insensitively.
fn starts_with_ci(rest: &str, word: &str) -> bool {
    rest.as_bytes()
        .get(..word.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(word.as_bytes()))
}

/// Whether `i` starts a word: nothing alphanumeric immediately before it.
fn boundary_before(text: &str, i: usize) -> bool {
    text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_500_body_loses_its_token() {
        // The shape of the body the real server sends for /registry, with the
        // user's own token replaced by a squib of the same shape.
        let body = "{\"ok\":false,\"error\":\"The value\\n  \\\"Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2XFla\\\"\\nis not of type\\n  LIST\"}";
        let masked = redact(body);
        assert!(!masked.contains("Zm9vYmFy"), "{masked}");
        assert!(masked.contains("\"Bearer <redacted>"), "{masked}");
        // Everything that is not the credential is untouched.
        assert!(masked.contains("is not of type"), "{masked}");
        assert!(masked.contains("\\n  \\\""), "{masked}");
    }

    #[test]
    fn schemes_keys_and_sk_keys_are_masked() {
        for (text, secret) in [
            ("Authorization: Bearer abcd1234efgh", "abcd1234efgh"),
            (
                "authorization: Basic QWxhZGRpbjpvcGVuU2VzYW1l",
                "QWxhZGRpbjpvcGVuU2VzYW1l",
            ),
            (
                "api_key=Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
                "Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
            ),
            ("API-KEY: 9f8e7d6c5b4a3210", "9f8e7d6c5b4a3210"),
            ("apikey\" = \"aB3dE5fG7hI9jK1l\"", "aB3dE5fG7hI9jK1l"),
            (
                "token=Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
                "Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
            ),
            (
                "refresh_token: Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
                "Zm9vYmFyQjNyUXc3eExrMnA5VHV2",
            ),
            (
                "password=correct-horse-battery-staple",
                "correct-horse-battery-staple",
            ),
            (
                "sk-ant-oat01-AbCdEf0123456789XyZ",
                "sk-ant-oat01-AbCdEf0123456789XyZ",
            ),
            (
                "key sk-proj-AbCdEf0123456789XyZ here",
                "sk-proj-AbCdEf0123456789XyZ",
            ),
        ] {
            let masked = redact(text);
            assert!(!masked.contains(secret), "{text} → {masked}");
            assert!(masked.contains(MASK), "{text} → {masked}");
        }
    }

    #[test]
    fn an_unmarked_token_is_masked_and_a_digest_is_not() {
        let token = "Zm9vYmFyQjNyUXc3eExrMnA5VHV2XFlaMDc4OQ";
        assert_eq!(redact(token), MASK);
        assert_eq!(
            redact(&format!("header {token} footer")),
            format!("header {MASK} footer")
        );

        // Not credentials: a hex digest (one case), a session id (too few
        // distinct bytes), and the paths, ids and words around them.
        for text in [
            "3f2a9c1b5e7d4a6f8b0c2d4e6f8a1b3c5d7e9f0a1b2c3d4e6f7a8b9c0d1e2f34",
            "20260929T121155Z_5455e533887e7824",
            "/private/tmp/evo-m0-real-20260929T121155Z/project/hello.txt",
            "~/coding/evo-gui/crates/swarm_client/examples/m0_real.rs",
            "https://api.anthropic.com/v1/messages",
            "claude-opus-5-5 · high · ctx 151k/1000k (15%)",
        ] {
            assert_eq!(redact(text), text, "{text} was masked");
        }
    }

    #[test]
    fn ordinary_prose_survives() {
        for text in [
            "a basic idea, and the token file is at the tab directory",
            "the server exited during startup (exit status: 1)",
            "no ready server after 90s (name Some(\"evo-swarm\"), features [\"swarm\"])",
            "api_key= in a sentence is not a value",
            "token: the file the server writes",
            "Authorization: none",
            "the value is not of type LIST",
        ] {
            assert_eq!(redact(text), text, "{text} was masked");
        }
    }

    #[test]
    fn several_credentials_in_one_line_all_go() {
        let text = "Authorization: Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2 and token=sk-ant-oat01-AbCdEf0123456789";
        let masked = redact(text);
        assert_eq!(masked.matches(MASK).count(), 2, "{masked}");
        assert!(
            !masked.contains("Zm9vYmFy") && !masked.contains("oat01"),
            "{masked}"
        );
        assert!(
            masked.starts_with("Authorization: Bearer <redacted> and token="),
            "{masked}"
        );
    }

    #[test]
    fn nothing_to_mask_is_borrowed() {
        assert!(matches!(
            redact("nothing here"),
            Cow::Borrowed("nothing here")
        ));
        assert!(matches!(
            redact("Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2"),
            Cow::Owned(_)
        ));
    }

    #[test]
    fn a_json_body_is_masked_without_losing_its_shape() {
        let mut body = serde_json::json!({
            "ok": false,
            "error": "The value \"Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2\" is not of type LIST",
            "settings": { "mcp_servers": [{ "headers": [["Authorization", "Bearer Zm9vYmFyQjNyUXc3eExrMnA5VHV2"]] }] },
            "models": ["seed-evolving", 7, true, null],
        });
        redact_json(&mut body);
        let text = body.to_string();
        assert!(!text.contains("Zm9vYmFy"), "{text}");
        assert_eq!(
            body["models"],
            serde_json::json!(["seed-evolving", 7, true, null])
        );
        assert!(body["settings"]["mcp_servers"][0]["headers"][0][1]
            .as_str()
            .unwrap()
            .contains(MASK));
    }
}
