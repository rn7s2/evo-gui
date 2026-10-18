#!/usr/bin/env bash
#
# Build the browser frontend: dist/web/, the directory `evo-web` serves.
#
#   scripts/web.sh              build the bundle
#   scripts/web.sh --no-build   re-copy the page over an existing bundle
#
# Two artifacts make a page: the wasm module `crates/webapp` compiles to, and
# the HTML that mounts it. wasm-bindgen's CLI turns the first into the `.wasm`
# plus the ES-module glue the page imports.
#
# The wasm build needs the *nightly* toolchain: gpui's web platform pulls
# `wasm_thread`, which uses an unstable intrinsic
# (`stdarch_wasm_atomic_wait`) on wasm32. It also needs gpui's tree-sitter
# features OFF — gpui-component declares its tree-sitter dependency only for
# non-wasm targets, so enabling those features for wasm cannot resolve the
# crate. The workspace and `crates/webapp` are already set up that way; see the
# note in the root Cargo.toml.
#
# wasm-bindgen must match the version the module was built against, so the CLI
# is installed at that exact version rather than whatever is newest.
set -euo pipefail

case "${1:-}" in
--no-build) build=no ;;
"") build=yes ;;
*)
    echo "usage: ${0##*/} [--no-build]" >&2
    exit 2
    ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/dist/web"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
WASM="$TARGET_DIR/wasm32-unknown-unknown/release/evo_webapp.wasm"
PAGE="$ROOT/crates/webapp/web/index.html"
TOOLCHAIN="${WASM_TOOLCHAIN:-nightly}"

if [ "$build" = yes ]; then
    (cd "$ROOT" && cargo "+$TOOLCHAIN" build --release -p evo-webapp --target wasm32-unknown-unknown)
fi

if [ ! -f "$WASM" ]; then
    echo "web: no wasm module at $WASM — run without --no-build first" >&2
    exit 1
fi

# The version the module was built against, read from the lock so the CLI cannot
# drift from it. (awk, not sed: the block form this needs is a GNU extension.)
BINDGEN_VERSION="$(awk '
    /^name = "wasm-bindgen"$/ { found = 1; next }
    found && /^version = / { gsub(/[^0-9.]/, "", $0); print; exit }
' "$ROOT/Cargo.lock")"
if [ -z "$BINDGEN_VERSION" ]; then
    echo "web: cannot read wasm-bindgen's version from Cargo.lock" >&2
    exit 1
fi

if ! command -v wasm-bindgen >/dev/null 2>&1; then
    echo "web: installing wasm-bindgen-cli $BINDGEN_VERSION (matching the module)"
    cargo install wasm-bindgen-cli --version "$BINDGEN_VERSION"
fi

# --target web: an ES module the page imports, rather than a bundler's input.
# --no-typescript: the page is plain JavaScript, and the .d.ts files would be
# the only artifacts nothing reads.
mkdir -p "$OUT"
wasm-bindgen --target web --no-typescript --out-dir "$OUT" "$WASM"
cp "$PAGE" "$OUT/index.html"

# The kit's icons, beside the page.
#
# `gpui-kit`'s web asset source does not bundle its icons: it *fetches* them, one
# `{endpoint}/assets/icons/<name>.svg` per icon, and caches them as they arrive
# (`gpui-kit-assets`'s wasm backend). The desktop build reads the same files out of
# the crate, which is why this is a web-only step. Without them every icon is a
# failed fetch and the widget that wanted it logs "Wasm assets loading, will be
# available soon..." — which is what a missing copy looks like from the page.
#
# They are copied from the crate that owns them rather than checked in here, so
# there is one copy of an icon and it is the one the code expects. The crate is a
# registry dependency, not a workspace member, so it is located through cargo's
# own registry cache rather than `cargo metadata --no-deps` (which lists only
# this workspace).
ICONS=""
for candidate in "$HOME"/.cargo/registry/src/*/gpui-kit-assets-*/assets/icons; do
    [ -d "$candidate" ] && ICONS="$candidate"
done
if [ -n "$ICONS" ]; then
    mkdir -p "$OUT/assets/icons"
    cp "$ICONS"/*.svg "$OUT/assets/icons/"
    echo "web: copied $(ls "$OUT/assets/icons" | wc -l | tr -d ' ') icons"
else
    echo "web: warning: could not find gpui-kit's icons; the UI will draw without them" >&2
fi

echo "web: wrote $OUT"
echo "web: serve it with   cargo run -p evo-web -- --dist $OUT"
