//! What the frontend's tabs talk to, and how a build decides.
//!
//! The desktop app's tabs spawn `evo-swarm serve` themselves and hold the child's
//! stdin; a browser cannot. So the web frontend's tabs reach a server that is
//! already running the evo binaries — the `evo-web` axum server that served this
//! bundle — through the same protocol, proxied under `/api/tabs/<id>/...`.
//!
//! This module is the one place that knows which of the two a build is, so that
//! nothing in the shared UI has to. It is deliberately small: the *client* for
//! both transports is `swarm_client`'s (whose transport is selected the same way,
//! by target), and what a frontend needs on top of it is only **where** to point
//! it.
//!
//! ```text
//!   native  tab ──▶ spawn evo-swarm serve ──▶ 127.0.0.1:<port>  + bearer token
//!   browser tab ──▶ evo-web (axum)         ──▶ /api/tabs/<id>   (token added server-side)
//! ```

/// How a tab reaches its session in this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// A server on this machine, at a port a tab's own spawn announced, with the
    /// bearer token the server wrote in its ready file. The desktop app's shape.
    Loopback {
        host: String,
        port: u16,
        token: String,
    },
    /// The same protocol, through the axum server that served this bundle, under
    /// a same-origin path prefix. No token: the server holds it and adds it to
    /// every request it forwards, so a credential never reaches the page.
    Proxied {
        /// A same-origin path prefix, e.g. `/api/tabs/7f3a`.
        base_path: String,
    },
}

impl Endpoint {
    /// The endpoint for a tab served by the `evo-web` server this bundle came
    /// from.
    ///
    /// `id` is the tab's id as the server named it; it is percent-encoded here so
    /// that an id can never escape its own path segment and address another
    /// endpoint.
    pub fn proxied(id: &str) -> Endpoint {
        Endpoint::Proxied {
            base_path: format!("{}/tabs/{}", crate::API_ROOT, encode_segment(id)),
        }
    }

    /// The path prefix, for a transport that takes one.
    pub fn base_path(&self) -> Option<&str> {
        match self {
            Endpoint::Proxied { base_path } => Some(base_path),
            Endpoint::Loopback { .. } => None,
        }
    }
}

/// One path segment, percent-encoded: everything outside the unreserved set
/// becomes `%XX`.
///
/// A tab id is the server's to choose and this build never invents one, but a
/// value that reached a URL unencoded could address a different endpoint — `..`
/// or a `/` is the whole of that risk — and encoding is cheaper than trusting it.
pub fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxied_endpoint_is_a_path_on_this_origin() {
        let endpoint = Endpoint::proxied("7f3a");
        assert_eq!(endpoint.base_path(), Some("/api/tabs/7f3a"));
    }

    #[test]
    fn a_tab_id_cannot_escape_its_segment() {
        // The two shapes that would address something else: a separator and a
        // parent step. Both are encoded, so the id stays one segment.
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        assert_eq!(encode_segment(".."), "..");
        assert_eq!(Endpoint::proxied("../../etc").base_path(),
                   Some("/api/tabs/..%2F..%2Fetc"));
    }

    #[test]
    fn an_unreserved_id_is_left_alone() {
        assert_eq!(encode_segment("Tab-1_2.3~4"), "Tab-1_2.3~4");
    }

    #[test]
    fn a_loopback_endpoint_has_no_path() {
        let endpoint = Endpoint::Loopback {
            host: "127.0.0.1".to_owned(),
            port: 8080,
            token: "t".to_owned(),
        };
        assert_eq!(endpoint.base_path(), None);
    }
}
