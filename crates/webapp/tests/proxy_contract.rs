//! The contract between a tab in the page and the `evo-web` server behind it.
//!
//! `transport::encode_segment` is what stands between a server-chosen tab id and
//! a URL the page hands to `fetch`. If it let a separator or a parent step
//! through, one tab's request would address another endpoint — or an endpoint
//! that is not a tab at all. These tests pin that boundary from the client side,
//! without a browser, so `cargo test -p evo-webapp` catches a regression in the
//! encoder long before `scripts/web_check.sh` would have to.
//!
//! They live beside the crate rather than inside `transport.rs` so that the
//! module stays one lane's file; nothing here reaches past the public API.

use evo_webapp::transport::{encode_segment, Endpoint};
use evo_webapp::API_ROOT;

/// Ids a server could plausibly issue, plus the shapes an attacker would try to
/// get through the encoder. Everything that is not a bare `.`/`..` step (see
/// `a_parent_step_id_is_the_one_id_this_encoder_cannot_make_safe`) belongs here.
fn adversarial_ids() -> Vec<&'static str> {
    vec![
        "7f3a",
        "Tab-1_2.3~4",
        // A separator, in both directions, and a whole traversal as one id.
        "a/b",
        "a\\b",
        "../../etc/passwd",
        "..%2F..",
        // The characters that end a URL's path or start a query or fragment.
        "a?b",
        "a#b",
        "a&b=c",
        "a;b",
        "a:b",
        "a@b",
        "a+b",
        "a=b",
        "a%b",
        "a[b]",
        "a{b}",
        "a|b",
        "a^b",
        "a`b",
        "a\"b",
        "a'b",
        "a<b>",
        "a,b",
        "a$b",
        "a!b",
        "a*b",
        "a(b)",
        // Whitespace and control characters.
        "a b",
        "a\nb",
        "a\tb",
        "a\u{0}b",
        // Non-ASCII, one BMP character and one astral one.
        "\u{4e2d}\u{6587}",
        "\u{1f600}",
    ]
}

/// RFC 3986's unreserved set: the only bytes a segment may carry literally.
fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn is_uppercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'A'..=b'F')
}

/// `true` when `segment` is exactly one path segment spelled only the two ways
/// the encoder allows: unreserved bytes literally, and every other byte as an
/// uppercase `%XX`. Nothing else can be read as a separator, a query or a
/// fragment start, so a URL parser has nothing to act on.
fn is_one_harmless_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        if is_unreserved(bytes[i]) {
            i += 1;
        } else if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && is_uppercase_hex(bytes[i + 1])
            && is_uppercase_hex(bytes[i + 2])
        {
            i += 3;
        } else {
            return false;
        }
    }
    true
}

#[test]
fn an_encoded_id_is_always_exactly_one_path_segment() {
    for id in adversarial_ids() {
        let encoded = encode_segment(id);
        assert!(
            is_one_harmless_segment(&encoded),
            "id {id:?} encoded to {encoded:?}, which is not one harmless path segment"
        );
    }
}

#[test]
fn the_base_path_is_well_formed_for_every_id() {
    let prefix = format!("{API_ROOT}/tabs/");
    for id in adversarial_ids() {
        let base = Endpoint::proxied(id)
            .base_path()
            .expect("a proxied endpoint always has a path")
            .to_owned();

        assert!(
            base.starts_with(&prefix),
            "id {id:?} produced {base:?}, which is not under {prefix:?}"
        );
        assert!(
            !base.ends_with('/'),
            "id {id:?} produced {base:?}, which ends in a separator"
        );
        assert!(
            !base.contains("//"),
            "id {id:?} produced {base:?}, which has an empty segment"
        );
        assert!(
            !base.contains(['?', '#']),
            "id {id:?} produced {base:?}, which carries a query or fragment"
        );
        // The id is the *last* segment and nothing else: one more separator
        // anywhere after the prefix would mean the id addressed a nested path.
        let tail = &base[prefix.len()..];
        assert!(
            !tail.contains('/'),
            "id {id:?} produced {base:?}, so the id spans more than one segment"
        );
        assert!(
            base.is_ascii(),
            "id {id:?} produced {base:?}, which is not ASCII and so not URL-safe"
        );
    }
}

#[test]
fn a_separator_in_an_id_never_reaches_the_path() {
    assert_eq!(encode_segment("a/b"), "a%2Fb");
    assert_eq!(encode_segment("a\\b"), "a%5Cb");
    // The whole traversal is one segment once encoded, which is the point: it
    // names a tab called `../../etc/passwd`, not the file behind it.
    assert_eq!(encode_segment("../../etc/passwd"), "..%2F..%2Fetc%2Fpasswd");
    assert_eq!(
        Endpoint::proxied("../../etc").base_path(),
        Some("/api/tabs/..%2F..%2Fetc")
    );
}

#[test]
fn a_percent_in_an_id_cannot_inject_an_escape() {
    // An id is not a URL: a `%` it contains is data, so it is escaped again
    // rather than passed through as an escape the page would have written.
    assert_eq!(encode_segment("a%2Fb"), "a%252Fb");
    assert_eq!(encode_segment("%"), "%25");
}

#[test]
fn a_non_ascii_id_encodes_byte_by_byte_in_uppercase_hex() {
    // UTF-8, then one escape per byte, so the result is ASCII however the id
    // is spelled. `中` is e4 b8 ad.
    assert_eq!(encode_segment("\u{4e2d}"), "%E4%B8%AD");
    assert_eq!(encode_segment("\u{6587}"), "%E6%96%87");
    // An astral character is four bytes, so four escapes.
    assert_eq!(encode_segment("\u{1f600}"), "%F0%9F%98%80");
    // A control character is escaped rather than emitted.
    assert_eq!(encode_segment("a\u{0}b"), "a%00b");
    assert_eq!(encode_segment("a b"), "a%20b");
}

#[test]
fn only_the_unreserved_set_survives_encoding() {
    // RFC 3986's unreserved set, and nothing else, is what comes through
    // untouched — so a caller can predict the path without running the encoder.
    assert_eq!(encode_segment("AZaz09-_.~"), "AZaz09-_.~");
    for byte in b'!'..=b'~' {
        let character = byte as char;
        let encoded = encode_segment(&character.to_string());
        if is_unreserved(byte) {
            assert_eq!(encoded, character.to_string(), "{character:?} was escaped");
        } else {
            assert_eq!(
                encoded,
                format!("%{byte:02X}"),
                "{character:?} was not escaped"
            );
        }
    }
}

#[test]
fn a_parent_step_id_is_the_one_id_this_encoder_cannot_make_safe() {
    // A `.` is unreserved, so it survives encoding — and a URL parser then
    // collapses the segment it is in. A page that built `/api/tabs/..` would
    // send `GET /api/` and never reach the tab. Escaping it would not help:
    // browsers normalize `%2E%2E` the same way.
    //
    // This test records where the encoder's guarantee stops, so the boundary is
    // visible rather than assumed: the server must never name a tab exactly
    // `.`, `..`, or the empty string. Every other id is safe — that is what the
    // tests above establish. See the note in `transport.rs`.
    assert_eq!(encode_segment(".."), "..");
    assert_eq!(encode_segment("."), ".");
    assert_eq!(Endpoint::proxied("..").base_path(), Some("/api/tabs/.."));

    // An empty id is the other shape the encoder cannot repair: it produces a
    // path that ends in a separator, naming no tab at all.
    assert_eq!(encode_segment(""), "");
    assert_eq!(Endpoint::proxied("").base_path(), Some("/api/tabs/"));

    // Both are the server's to avoid, and one character of padding is enough to
    // leave the unsafe set, which is why the risk is narrow.
    assert_eq!(Endpoint::proxied("..x").base_path(), Some("/api/tabs/..x"));
}

#[test]
fn a_loopback_endpoint_names_no_path() {
    // The desktop shape: a host, a port and a token, so nothing is proxied and
    // there is no path for an id to be encoded into.
    let endpoint = Endpoint::Loopback {
        host: "127.0.0.1".to_owned(),
        port: 8080,
        token: "t".to_owned(),
    };
    assert_eq!(endpoint.base_path(), None);
}

#[test]
fn the_api_root_is_an_absolute_same_origin_path() {
    // The page and the API share an origin, which is what lets the frontend
    // build every URL without a host — and what makes the server's own token
    // handling the only credential in play.
    assert!(
        API_ROOT.starts_with('/'),
        "API_ROOT must be origin-relative"
    );
    assert!(
        !API_ROOT.ends_with('/'),
        "API_ROOT must not end in a separator"
    );
    assert!(
        !API_ROOT.contains("//"),
        "API_ROOT must not contain an empty segment"
    );
}
