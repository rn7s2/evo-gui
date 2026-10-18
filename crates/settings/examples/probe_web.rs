//! The wasm arm of [`settings::probe`], in a real browser.
//!
//! This is a **proof harness**, not part of the app. It exists because the browser
//! build's Settings surface does not (yet) draw the two binary rows:
//! `SettingsPanel` is constructed only by `crates/app`, which is not in the wasm
//! dependency tree, so nothing in the shipped bundle calls `probe` and there is no
//! way to see from the page whether its wasm arm works. This example puts that arm
//! on a page of its own, so a headless browser can run it for real:
//!
//! ```sh
//! cargo +nightly build --release -p settings --example probe_web --target wasm32-unknown-unknown
//! wasm-bindgen --target web --no-typescript --out-dir <dist> \
//!     target/wasm32-unknown-unknown/release/examples/probe_web.wasm
//! ```
//!
//! The page calls [`probe_rows`] and renders what it returns — the text a Settings
//! row would show ([`Check::text()`]) for each binary, plus whether the row would
//! wear the danger tone. Before the wasm arm queried the server it could only ever
//! say `not found`; it now says the binary's own `--version` line, which is the
//! whole point of the fix.
//!
//! Native builds get nothing — there is nothing here for a host to run, and
//! `cargo test -p settings` compiles examples, so it must stay harmless there.

// The wasm half is below; a host gets the empty `main` at the end of this file.
// `cargo test -p settings` builds every example, so this one has to link on both
// targets — and there is nothing for a host to run here, because the whole point
// is a *browser's* reading of the wasm arm.

// `wasm-bindgen` is a wasm-only dependency of this crate, so even the *import* of
// the attribute macro has to be gated: a host build has no such crate to name.
#[cfg(target_family = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg(target_family = "wasm")]
use settings::probe;

/// The two paths the app's Settings rows hold, and the names they expect — plus a
/// path nothing is at, on purpose.
///
/// The third entry is the control. A row's text must *track the server's answer*,
/// not be a constant: if this arm returned a version line for anything it was
/// asked, the first two lines would look right and mean nothing. The broken path
/// has to come back `not found` in the same run for the other two to count.
#[cfg(target_family = "wasm")]
const BINARIES: [(&str, &str); 3] = [
    ("/usr/local/bin/evo-swarm", "evo-swarm"),
    ("/usr/local/bin/evo-agent", "evo-agent"),
    ("/nonexistent/evo-swarm", "evo-swarm"),
];

/// What the two Settings rows would say, one line each:
/// `name<TAB>the row's text<TAB>ok|problem`.
///
/// `not found` is the failure this harness exists to disprove, and a version line
/// is the fix. The page renders this verbatim, so a browser's reading of it is a
/// browser's reading of the real wasm arm.
#[cfg(target_family = "wasm")]
#[wasm_bindgen]
pub async fn probe_rows() -> String {
    let mut lines = Vec::new();
    for (path, name) in BINARIES {
        let check = probe(std::path::Path::new(path), name).await;
        lines.push(format!(
            "{name}\t{}\t{}",
            check.text(),
            if check.is_problem() { "problem" } else { "ok" }
        ));
    }
    lines.join("\n")
}

/// Nothing to do at startup: the page calls [`probe_rows`] when it is ready. An
/// example still needs an entry point to link.
#[cfg(target_family = "wasm")]
fn main() {}

/// A host runs no part of this: the wasm arm of `probe` is what is being shown,
/// and on a host there is no browser to show it in.
#[cfg(not(target_family = "wasm"))]
fn main() {
    eprintln!(
        "probe_web: this harness runs in a browser. Build it for wasm32-unknown-unknown \
         and serve the page beside it — see the module docs."
    );
}
