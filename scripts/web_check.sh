#!/usr/bin/env bash
#
# Does the browser frontend actually run in a browser? Not "does it compile" —
# whether a page served from the bundle boots the wasm module, mounts gpui's
# canvas, and says nothing uncaught on the way.
#
#   scripts/web_check.sh                 build the bundle, serve it, check it
#   scripts/web_check.sh --no-build      check the dist/web that is already there
#   scripts/web_check.sh --dist DIR      serve DIR instead of building
#   scripts/web_check.sh --http-only     skip the browser, assert only over HTTP
#   scripts/web_check.sh --require-browser
#                                        fail if no browser can be driven
#   scripts/web_check.sh --port N        pin the port instead of picking one
#   scripts/web_check.sh --proxy         also drive the tab API from the page
#
# Three legs, and the script says which of them it actually got to:
#
#   1. **Build.** `scripts/web.sh` builds `crates/webapp` for wasm and runs
#      wasm-bindgen over it. Reused, never duplicated: there is one definition of
#      how the bundle is made and this is not it.
#   2. **Serve + HTTP.** The bundle is served — by `evo-web` when that crate
#      exists, otherwise by a built-in static server — and the page and its
#      assets are fetched and asserted: status, content type, the wasm magic
#      number, the glue the page imports, and that a path traversal out of the
#      served directory is refused.
#   3. **Browser.** A headless browser is driven over the DevTools protocol and
#      the page is *watched*: its console, its uncaught errors, whether the
#      module instantiated, whether gpui's canvas mounted with a real size, and
#      whether the assets the UI asked for actually arrived. This is the only leg
#      that proves anything about running the frontend, and if it cannot run the
#      script says so instead of implying otherwise.
#
# One `console.error` is known benign and is matched literally, whole-message:
# `gpui-kit`'s wasm asset source returns
# `Err(anyhow!("Wasm assets loading, will be available soon..."))` on purpose so
# GPUI retries the icon load. It is the *retry* announcing itself. Everything
# else stays fatal, and an uncaught exception is never excused.
#
# That classification is deliberately narrow, because it is the kind of rule that
# rots into a blind spot. It is paired with a positive assertion — that
# `assets/icons/*.svg` requests really left the browser — since a warm-up with no
# request behind it is not the mechanism working, it is a fetch that can never
# succeed. (Measured: `gpui-kit`'s wasm asset source builds
# `"{endpoint}/assets/icons/<name>.svg"`, and reqwest's wasm backend rejects the
# relative URL that the default endpoint produces with "builder error" before any
# request is sent. The harness reports that case with its own message.)
#
# # The proxy leg (`--proxy`)
#
# The three legs above prove the UI boots; none of them opens an evo session, so
# `/api/tabs/...` — the path every tab's request takes — is untouched by them.
# `--proxy` adds a fourth leg that drives it **from the page**, in the browser, so
# what is proven is the browser's own `fetch` through the proxy rather than
# curl's. It opens a tab through `POST /api/tabs`, reads its snapshot, posts an
# op, checks the tab list, and stops it — asserting on the **body** each time, so
# a 200 carrying an error JSON fails.
#
# It needs a real `evo-agent` (or `evo-swarm`). When there is none it **skips**
# and says so, exactly as `crates/tab_engine/tests/real_tab.rs` does; a skip is
# never reported as a pass. The child runs in a throwaway `HOME` under the work
# directory, so no real provider is called and the real `~/.evo` is not written.
#
# What it proves: the browser's own `fetch`, same-origin, through the proxy —
# open a tab, read the snapshot, post an op, see a refusal stay a refusal, list
# the tabs, stop the tab, and find it gone. Each step asserts on the body, so a
# 200 carrying an error JSON fails. The last step asserts the requests carried
# `Sec-Fetch-Site: same-origin` and a `Referer`, which only a browser sends —
# that is what makes this a browser proof rather than the Rust test in disguise.
#
# What it does NOT prove: `/stream` (SSE through the proxy), `/items` paging,
# `/media`, `/catalog`, resume, and anything about the frontend's own JavaScript
# (this leg drives the API from the page, not through the app's client). The
# round trip is also over a session with **no model configured** — the tab comes
# up and answers, but no prompt is sent, so nothing here proves a turn runs.
#
# The browser leg needs no npm packages and no driver: Chrome's own
# `--remote-debugging-port=0` plus Node's global `WebSocket` is the whole client.
# Node's only job is to hold that socket; the JSON it prints is the evidence.
#
# Measured here (Chrome 155, `--headless=new`): WebGPU *is* available, and gpui
# selects `BrowserWebGpu` — so the preferred path is the one that runs. WebGL2 is
# there too, as the fallback gpui would take if WebGPU were not. The
# `--enable-unsafe-swiftshader` flag below is what gives headless Chrome a
# software WebGL2, and is kept so the fallback path stays exercisable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$ROOT/target/web-check"
DIST="$ROOT/dist/web"

build=yes
http_only=no
require_browser=no
proxy=no
port=""

usage() {
    sed -n '3,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
    --no-build) build=no ;;
    --dist)
        [ $# -ge 2 ] || { echo "web_check: --dist needs a directory" >&2; exit 2; }
        DIST="$2"
        build=no
        shift
        ;;
    --http-only) http_only=yes ;;
    --require-browser) require_browser=yes ;;
    --proxy) proxy=yes ;;
    --port)
        [ $# -ge 2 ] || { echo "web_check: --port needs a number" >&2; exit 2; }
        port="$2"
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "web_check: unknown argument $1" >&2
        usage >&2
        exit 2
        ;;
    esac
    shift
done

# --- reporting --------------------------------------------------------------
#
# Every check announces itself and its verdict, and the failures are collected
# so the run ends with the list of what was wrong rather than the last thing that
# happened to be printed.

failures=()
notes=()
step=0

step() {
    step=$((step + 1))
    printf '\n== %d. %s\n' "$step" "$1"
}

ok() { printf '   ok    %s\n' "$1"; }

bad() {
    printf '   FAIL  %s\n' "$1"
    failures+=("$1")
}

note() {
    printf '   note  %s\n' "$1"
    notes+=("$1")
}

# --- 1. the toolchain the legs below need -----------------------------------

step "what this machine can do"

have_node=no
if command -v node >/dev/null 2>&1; then
    have_node=yes
    ok "node $(node --version) — drives the browser leg"
else
    note "no node: the browser leg cannot run, so this will be an HTTP-only check"
fi

# The browser's evidence arrives as JSON, and reading it is what turns "the page
# did something" into a verdict. Without a reader the leg would run and then
# misreport, so it is a prerequisite rather than a convenience.
have_jq=no
if command -v jq >/dev/null 2>&1; then
    have_jq=yes
    ok "jq $(jq --version 2>/dev/null | tr -d 'jq-') — reads the browser's evidence"
else
    note "no jq: the browser's JSON cannot be read, so this will be an HTTP-only check"
fi

# Used to ask the OS for a free port, and to stand in for evo-web if that crate
# is not in the tree.
have_python=no
if command -v python3 >/dev/null 2>&1; then
    have_python=yes
    ok "python3 $(python3 --version 2>&1 | awk '{print $2}')"
else
    note "no python3: ports are picked by probing instead, and there is no"
    note "fallback static server if crates/web is absent"
fi

# A browser, in the order that prefers the ones a person would have installed.
# `CHROME_PATH` wins so a run can point at a specific build.
browser=""
if [ -n "${CHROME_PATH:-}" ]; then
    if [ -x "$CHROME_PATH" ]; then
        browser="$CHROME_PATH"
    else
        note "CHROME_PATH=$CHROME_PATH is not an executable file; ignoring it"
    fi
fi

if [ -z "$browser" ]; then
    for candidate in google-chrome google-chrome-stable chromium chromium-browser; do
        if found="$(command -v "$candidate" 2>/dev/null)"; then
            browser="$found"
            break
        fi
    done
fi

if [ -z "$browser" ]; then
    for app in \
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
        "/Applications/Chromium.app/Contents/MacOS/Chromium" \
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"; do
        if [ -x "$app" ]; then
            browser="$app"
            break
        fi
    done
fi

# The browsers a test runner downloaded, which are on nobody's PATH.
if [ -z "$browser" ]; then
    for glob in \
        "$HOME/Library/Caches/ms-playwright"/chromium-*/chrome-mac/Chromium.app/Contents/MacOS/Chromium \
        "$HOME/.cache/ms-playwright"/chromium-*/chrome-linux/chrome \
        "$HOME/.cache/puppeteer"/*/*/chrome-headless-shell-*/chrome-headless-shell \
        "$HOME/.cache/puppeteer"/*/*/chrome-*/chrome-*/chrome; do
        for candidate in $glob; do
            if [ -x "$candidate" ]; then
                browser="$candidate"
                break 2
            fi
        done
    done
fi

if [ -n "$browser" ]; then
    ok "browser: $browser"
else
    note "no Chrome/Chromium/Edge found: the browser leg cannot run, so this will be an HTTP-only check"
fi

# --- 2. the bundle ----------------------------------------------------------

step "build the bundle"

if [ "$build" = yes ]; then
    # scripts/web.sh owns how the bundle is made, including which toolchain and
    # which wasm-bindgen. Its output goes through here rather than into a log
    # nobody reads, because a wasm build that fails says why in its own words.
    if bash "$ROOT/scripts/web.sh"; then
        ok "scripts/web.sh built $DIST"
    else
        bad "scripts/web.sh failed: there is no bundle to check"
        echo
        echo "web_check: stopping here — the browser leg needs a bundle, and" >&2
        echo "           the build above is the reason there is not one." >&2
        exit 1
    fi
else
    if [ -d "$DIST" ]; then
        ok "reusing the bundle in $DIST"
    else
        bad "no bundle at $DIST: run without --no-build, or point --dist at one"
        exit 1
    fi
fi

# The two artifacts a page needs. wasm-bindgen derives the glue's name from the
# module's, so it is discovered rather than spelled out: `<name>_bg.wasm` beside
# `<name>.js`. The page's own `import` is then checked against what was found,
# which is what catches a bundle built from a differently-named module.
wasm_file="$(cd "$DIST" && ls ./*_bg.wasm 2>/dev/null | head -1 || true)"
glue_file=""
if [ -n "$wasm_file" ]; then
    glue_file="./$(basename "${wasm_file#./}" _bg.wasm).js"
    [ -f "$DIST/${glue_file#./}" ] || glue_file=""
fi
page_file="$DIST/index.html"

if [ -n "$wasm_file" ]; then
    ok "wasm module: ${wasm_file#./}"
else
    bad "no *_bg.wasm in $DIST: wasm-bindgen did not write a module"
fi
if [ -n "$glue_file" ]; then
    ok "js glue: ${glue_file#./}"
elif [ -n "$wasm_file" ]; then
    bad "no glue beside ${wasm_file#./}: wasm-bindgen did not write the JS the page imports"
else
    bad "no *.js in $DIST: wasm-bindgen did not write the glue the page imports"
fi
if [ ! -f "$page_file" ]; then
    bad "no index.html in $DIST"
fi

# The page names the glue it imports; if wasm-bindgen ever renames the module the
# page would 404 at load and this is where that shows up.
if [ -f "$page_file" ] && [ -n "$glue_file" ]; then
    if grep -qF "${glue_file#./}" "$page_file"; then
        ok "index.html imports ${glue_file#./}"
    else
        bad "index.html does not mention ${glue_file#./}: the page would not find its module"
    fi
fi

# --- 3. serve it ------------------------------------------------------------

step "serve the bundle"

if [ -n "$port" ]; then
    chosen_port="$port"
else
    chosen_port=""
    if [ "$have_python" = yes ]; then
        chosen_port="$(python3 - <<'PY' 2>/dev/null || true
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
    fi
    if [ -z "$chosen_port" ]; then
        # No python3 to ask: take a port in the ephemeral range and check it is
        # not already answering. A race is possible and harmless — the server
        # below fails to bind and the readiness wait times out with a clear word.
        for candidate in $(seq 8100 8199); do
            if ! nc -z 127.0.0.1 "$candidate" 2>/dev/null; then
                chosen_port="$candidate"
                break
            fi
        done
    fi
fi
[ -n "$chosen_port" ] || { echo "web_check: could not find a free port" >&2; exit 1; }

base="http://127.0.0.1:$chosen_port"
server_pid=""
server_kind=""

cleanup() {
    if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

mkdir -p "$WORK"

# evo-web is the real server: the one that also proxies `/api/tabs/...` and adds
# the session token server-side. Its command line is its own lane's to settle, so
# it is read from EVO_WEB_CMD when that is set and otherwise assumed to take
# `--dist` and `--port`.
#
# The placeholders are substituted by hand rather than with `${VAR:-default}`:
# a default holding `}` ends the expansion early, which silently left a stray
# brace in the command line (`--port "54532}"`) the first time this was run.
#
# A *built* evo-web binary is preferred over `cargo run`: it is what a real run
# uses, it needs no cargo on PATH, and it starts in milliseconds. `cargo run` is
# the fallback for a tree that has not been built yet.
start_static_server() {
    if [ "$have_python" = no ]; then
        bad "no python3 and no evo-web: there is nothing to serve the bundle with"
        server_kind="none"
        server_pid=""
        return
    fi
    note "the bundle is being served by python3's http.server instead, which does"
    note "NOT implement /api/tabs/... — the proxy is untested by this run"
    (exec python3 -m http.server "$chosen_port" --bind 127.0.0.1 --directory "$DIST") \
        >"$WORK/static-server.log" 2>&1 &
    server_pid=$!
    server_kind="python3 http.server (stand-in)"
}

if [ -d "$ROOT/crates/web" ]; then
    evo_web_cmd="${EVO_WEB_CMD:-}"
    if [ -z "$evo_web_cmd" ]; then
        if [ -x "$ROOT/target/debug/evo-web" ]; then
            evo_web_cmd="$ROOT/target/debug/evo-web --dist {dist} --port {port}"
        elif command -v cargo >/dev/null 2>&1; then
            evo_web_cmd="cargo run --quiet -p evo-web -- --dist {dist} --port {port}"
        else
            bad "crates/web exists but there is no built evo-web and no cargo to build one"
            evo_web_cmd=""
        fi
    fi

    if [ -z "$evo_web_cmd" ]; then
        start_static_server
    else
        cmd="${evo_web_cmd//"{dist}"/$DIST}"
        cmd="${cmd//"{port}"/$chosen_port}"
        # An array, not `eval`: EVO_WEB_CMD is a command line, and word splitting
        # it is all it needs. Nothing here re-parses shell syntax out of an
        # environment variable.
        read -r -a cmd_argv <<<"$cmd"
        (cd "$ROOT" && exec "${cmd_argv[@]}") >"$WORK/evo-web.log" 2>&1 &
        server_pid=$!
        server_kind="evo-web"
    fi
else
    note "crates/web does not exist yet, so there is no evo-web to serve with:"
    start_static_server
fi

# Readiness, bounded: a server that never answers is a failure, not a hang. The
# test is a *200*, not merely a reply — a server that is up but not serving this
# bundle answers 404 or 503, and treating that as ready would send the browser at
# a page that was never going to load.
ready=no
for _ in $(seq 1 100); do
    if [ -z "$server_pid" ] || ! kill -0 "$server_pid" 2>/dev/null; then
        break
    fi
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 "$base/" 2>/dev/null || echo 000)"
    if [ "$code" = 200 ]; then
        ready=yes
        break
    fi
    sleep 0.2
done

if [ "$ready" = yes ]; then
    ok "$server_kind answering on $base (pid $server_pid)"
else
    log_file="$WORK/static-server.log"
    [ "$server_kind" = evo-web ] && log_file="$WORK/evo-web.log"
    bad "$server_kind never served $base/ with a 200 (last status: ${code:-none})"
    if [ -s "$log_file" ]; then
        echo "   --- last lines of $log_file ---"
        tail -20 "$log_file" | sed 's/^/   /'
    fi
    if [ "$server_kind" = evo-web ]; then
        note "if evo-web takes different flags, set EVO_WEB_CMD (it may use {dist} and {port})"
    fi
fi

# --- 4. the page and its assets, over HTTP ----------------------------------

step "the page and its assets"

# `code content_type` for a URL, body to a file. curl failing is a result here,
# not an error: a refused connection is a failure this script wants to report.
fetch() {
    curl -sS -o "$2" -w '%{http_code} %{content_type}' --max-time 15 "$1" 2>/dev/null || echo "000 -"
}

expect_asset() {
    local url="$1" out="$2" want_type="$3" label="$4"
    local result code type
    result="$(fetch "$url" "$out")"
    code="${result%% *}"
    type="${result#* }"
    if [ "$code" != 200 ]; then
        bad "$label: GET $url returned $code"
        return 1
    fi
    case "$type" in
    *"$want_type"*) ok "$label: 200 $type" ;;
    *)
        bad "$label: served as '$type', expected a $want_type type"
        return 1
        ;;
    esac
    return 0
}

if [ "$ready" = yes ]; then
    if expect_asset "$base/" "$WORK/index.html" "text/html" "page"; then
        # The page's own contract, both halves of which the boot script depends
        # on: the `#evo-boot` placeholder it removes once `init()` resolves, and
        # the glue it imports. The page deliberately ships no canvas of its own —
        # gpui's platform creates one and appends it to <body> — so a canvas is
        # not something to assert about the *page*.
        if grep -q 'id="evo-boot"' "$WORK/index.html"; then
            ok "the page has the #evo-boot placeholder the boot script removes"
        else
            bad "the served page has no #evo-boot element"
        fi
        if grep -qF "${glue_file#./}" "$WORK/index.html"; then
            ok "the served page imports ${glue_file#./}"
        else
            bad "the served page does not import ${glue_file#./}"
        fi
    fi

    if [ -n "$glue_file" ]; then
        expect_asset "$base/${glue_file#./}" "$WORK/glue.js" "javascript" "js glue" ||
            true
    fi

    if [ -n "$wasm_file" ]; then
        if expect_asset "$base/${wasm_file#./}" "$WORK/module.wasm" "application/wasm" "wasm module"; then
            # A 200 with an HTML error page in it is not a wasm module. The magic
            # number is the only thing that says the bytes are the module.
            if [ "$(head -c 4 "$WORK/module.wasm" | od -An -tx1 | tr -d ' \n')" = "0061736d" ]; then
                ok "the module really is wasm (magic 00 61 73 6d)"
            else
                bad "the .wasm URL did not return a wasm module (bad magic number)"
            fi
        fi
    fi

    # Nothing above the served directory. `--path-as-is` stops curl from
    # normalizing the traversal away, which is the whole point of the request.
    for traversal in "/../Cargo.toml" "/..%2fCargo.toml" "/%2e%2e/Cargo.toml"; do
        code="$(curl -sS --path-as-is -o /dev/null -w '%{http_code}' --max-time 10 "$base$traversal" 2>/dev/null || echo 000)"
        if [ "$code" = 200 ]; then
            bad "path traversal: GET $traversal returned 200 — the server serves files above its root"
        else
            ok "path traversal refused: $traversal -> $code"
        fi
    done
else
    bad "skipped: nothing was served"
fi

# --- 5. the browser ---------------------------------------------------------

step "the page in a browser"

browser_proof=no

if [ "$http_only" = yes ]; then
    note "--http-only: the browser leg was skipped on request"
elif [ -z "$browser" ]; then
    note "no browser to drive: the browser leg cannot run"
elif [ "$have_node" = no ]; then
    note "no node to hold the DevTools socket: the browser leg cannot run"
elif [ "$have_jq" = no ]; then
    note "no jq to read the browser's evidence: the browser leg cannot run"
elif [ "$ready" != yes ]; then
    note "nothing is serving: the browser leg cannot run"
else
    cat >"$WORK/cdp.mjs" <<'MJS'
// Watch a page through Chrome's DevTools protocol, over Node's global WebSocket.
// No npm packages and no driver: this file plus a Chrome binary is the whole
// client. One JSON object goes to stdout; the exit code is about the *harness*,
// not the page — the page's verdict is in the JSON.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, basename } from 'node:path';

const argv = process.argv.slice(2);
const arg = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : dflt;
};

const url = arg('url');
const chromePath = arg('chrome');
const timeoutMs = Number(arg('timeout', '30000'));
const width = Number(arg('width', '1280'));
const height = Number(arg('height', '800'));
const screenshot = arg('screenshot');
// The glue filename this build produced, e.g. `evo_webapp.js`. The page in the
// tab is identified by naming it in its markup — see `hasGlue` below.
const glue = arg('glue', 'evo_webapp.js');
// `--proxy`: after the page settles, drive the tab API *from the page* — every
// request below is the browser's own fetch, same-origin, through the proxy.
const proxyLeg = argv.includes('--proxy');
const tabFolder = arg('tab-folder', '');
const tabProgram = arg('tab-program', 'agent');
// How long to keep watching after the page reaches a terminal state (mounted,
// threw, gave up). Trailing activity — a late error, an icon fetch attempt —
// lands just after mount, and stopping the instant the canvas appears is what
// made an earlier version of this harness non-deterministic: the same bundle
// passed or failed depending on whether the last console line beat the snapshot.
const graceMs = Number(arg('grace', '2500'));

if (!url || !chromePath) {
  console.error('cdp: --url and --chrome are required');
  process.exit(2);
}

// The ONE console.error this harness treats as benign.
//
// `gpui-kit`'s wasm asset source starts an async icon fetch and returns
// `Err(anyhow!("Wasm assets loading, will be available soon..."))` *on purpose*,
// so that GPUI retries the load (`gpui-kit-assets-0.7.0/src/wasm_assets.rs:98`,
// whose own comment reads "Return error so GPUI will retry (but only log once per
// icon)"). GPUI's `asset_entry` caches the pending load and re-polls it, so the
// message is the retry mechanism announcing itself, not a failure.
//
// The match is literal and whole-message: the log prefix (`[ERROR] target: `) is
// stripped and what remains must equal the string exactly. Nothing else — no
// similar message, no message that merely contains it — is ever treated as
// benign. And this rule is consulted for `console.error` ONLY: `exceptions` and
// the `window.onerror`/`unhandledrejection` records stay fatal unconditionally,
// so a genuine throw can never be excused by a warm-up string.
const ICON_WARMUP = 'Wasm assets loading, will be available soon...';
const LOG_PREFIX = /^\[(?:ERROR|WARN|INFO|DEBUG|TRACE)\]\s*[^:]*:\s*/;
const logBody = (text) => String(text).replace(LOG_PREFIX, '');
const isIconWarmup = (text) => logBody(text) === ICON_WARMUP;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// `chrome-headless-shell` is already headless and rejects the flag; the full
// browser needs it. One binary path, two shapes.
const headlessFlags = basename(chromePath).includes('headless-shell') ? [] : ['--headless=new'];

const profile = mkdtempSync(join(tmpdir(), 'evo-webcheck-'));
// `detached` puts Chrome in its own process group, so the kill below reaches the
// helper processes it forks — killing only the parent leaves those holding the
// profile directory, which is how a run leaves one behind.
const child = spawn(chromePath, [
  ...headlessFlags,
  '--remote-debugging-port=0',
  `--user-data-dir=${profile}`,
  `--window-size=${width},${height}`,
  '--no-first-run',
  '--no-default-browser-check',
  '--disable-background-networking',
  '--disable-component-update',
  '--disable-default-apps',
  '--disable-sync',
  '--mute-audio',
  // Headless has no GPU: WebGPU is absent and WebGL2 comes from SwiftShader.
  // gpui prefers WebGPU and falls back to WebGL2, so without this flag the
  // fallback has no implementation and the canvas cannot initialize.
  '--enable-unsafe-swiftshader',
  'about:blank',
], { stdio: ['ignore', 'pipe', 'pipe'], detached: true });

const cleanup = () => {
  try { process.kill(-child.pid, 'SIGKILL'); } catch {
    try { child.kill('SIGKILL'); } catch {}
  }
  // The group kill is asynchronous: the directory cannot go until the processes
  // holding it are gone, so this retries briefly rather than once.
  for (let attempt = 0; attempt < 20; attempt++) {
    try {
      rmSync(profile, { recursive: true, force: true });
      if (!existsSync(profile)) return;
    } catch {}
    try {
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 50);
    } catch {
      break;
    }
  }
  try { rmSync(profile, { recursive: true, force: true }); } catch {}
};

let chromeLog = '';
const fail = (message) => {
  cleanup();
  console.error(`cdp: ${message}`);
  process.exit(3);
};

const wsUrl = await new Promise((resolve, reject) => {
  const timer = setTimeout(
    () => reject(new Error(`chrome never announced a devtools endpoint:\n${chromeLog}`)), 30000);
  child.stderr.on('data', (chunk) => {
    chromeLog += chunk;
    const match = chromeLog.match(/ws:\/\/\S+/);
    if (match) { clearTimeout(timer); resolve(match[0]); }
  });
  child.on('exit', (code) => {
    clearTimeout(timer);
    reject(new Error(`chrome exited (${code}) before announcing a devtools endpoint:\n${chromeLog}`));
  });
}).catch((error) => fail(error.message));

const ws = new WebSocket(wsUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = () => reject(new Error('devtools websocket refused'));
}).catch((error) => fail(error.message));

let nextId = 0;
const pending = new Map();
const events = [];
ws.onmessage = (raw) => {
  const message = JSON.parse(raw.data);
  if (message.id !== undefined && pending.has(message.id)) {
    const { resolve, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) reject(new Error(`${message.error.message} (${message.error.code})`));
    else resolve(message.result);
  } else if (message.method) {
    events.push(message);
  }
};
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = ++nextId;
  pending.set(id, { resolve, reject });
  ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
});

// The page may catch its own failure and swallow it — the bundle's boot script
// does exactly that, so a panic inside `start()` never becomes an uncaught error
// and the only trace is a `console.error`. These listeners run before any page
// script and keep the record the page itself might discard.
//
// `consoleErrors` here is a *convenience* record, not the verdict: this wrapper
// is installed on the page and can miss entries logged before it runs or from a
// worker. The CDP event stream (`Runtime.consoleAPICalled`, collected below) sees
// every one, and that is what the verdict is built from. Measured on this bundle:
// 12 here against 16 there.
const PRELUDE = `
  globalThis.__evoHarness = { errors: [], rejections: [], consoleErrors: [] };
  addEventListener('error', (e) => {
    __evoHarness.errors.push({ message: String(e.message), source: e.filename, line: e.lineno });
  }, true);
  addEventListener('unhandledrejection', (e) => {
    __evoHarness.rejections.push(String((e.reason && e.reason.stack) || e.reason));
  });
  const originalError = console.error.bind(console);
  console.error = (...args) => {
    __evoHarness.consoleErrors.push(
      args.map((a) => (a && a.stack) || String(a)).join(' '));
    originalError(...args);
  };
`;

const result = {
  ok: false,
  url,
  userAgent: null,
  note: null,
  console: [],
  exceptions: [],
  logEntries: [],
  pageErrors: [],
  iconWarmups: [],
  iconRequests: [],
  resources: [],
  state: null,
  screenshot: null,
  proxy: null,
};

try {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('Runtime.enable', {}, sessionId);
  await send('Log.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  // Network, so icon fetches are visible as they happen. This is the reliable
  // channel: `performance.getEntriesByType('resource')` reported
  // `responseStatus: 0` for a fetch that had just returned 200 in a control run,
  // so it cannot be trusted to say whether an asset arrived.
  await send('Network.enable', {}, sessionId);
  await send('Page.addScriptToEvaluateOnNewDocument', { source: PRELUDE }, sessionId);
  await send('Page.navigate', { url }, sessionId);

  const snapshot = async () => {
    const { result: value } = await send('Runtime.evaluate', {
      expression: `JSON.stringify({
        boot: document.getElementById('evo-boot') ? document.getElementById('evo-boot').textContent : null,
        // How "the page in the tab is ours" is decided. Not by a title or an
        // element the page happens to have today — both have already changed
        // under this harness — but by the artifact under test: the served page
        // imports the glue wasm-bindgen wrote, and it is named in its markup. A
        // 503 body or an error page does not mention it.
        hasGlue: document.documentElement.outerHTML.includes(${JSON.stringify(glue)}),
        readyState: document.readyState,
        canvases: [...document.querySelectorAll('canvas')].map((c) => ({
          id: c.id || null,
          width: c.width, height: c.height,
          clientWidth: c.clientWidth, clientHeight: c.clientHeight,
          hasWebgl2: (() => { try { return !!c.getContext('webgl2'); } catch { return false; } })(),
        })),
        harness: globalThis.__evoHarness || null,
      })`,
      returnByValue: true,
    }, sessionId);
    return value ? JSON.parse(value.value) : null;
  };

  // "Settled" is one of four terminal states: the module threw, the shell
  // mounted its canvas, the page gave up and said so in its boot element, or the
  // document is not the bundle's page at all and waiting longer cannot change
  // that. Waiting for one of those beats a fixed sleep — a fast page is not made
  // slow and a broken one is not waited out.
  //
  // Reaching one of them is not the end of the watch, though: a terminal state
  // can be reached before the page has finished complaining, and stopping there
  // made the verdict depend on which beat the snapshot. The grace window below is
  // what makes the same bundle give the same answer twice.
  const deadline = Date.now() + timeoutMs;
  let state = await snapshot();
  let terminal = false;
  for (;;) {
    if (!state) break;
    const harness = state.harness || { errors: [], rejections: [], consoleErrors: [] };
    const threw = harness.errors.length > 0 || harness.rejections.length > 0 ||
      (harness.consoleErrors || []).some((c) => !isIconWarmup(c));
    const mounted = state.canvases.some((c) => c.clientWidth > 0 && c.clientHeight > 0);
    const gaveUp = state.boot !== null && /could not start/i.test(state.boot);
    // The page in the tab is the bundle's: its markup names the glue this build
    // wrote. An error body does not.
    const notOurPage = state.readyState === 'complete' && !state.hasGlue;
    if (threw || mounted || gaveUp || notOurPage) {
      if (!terminal) {
        terminal = true;
        // The page is up (or has failed); give the trailing work a moment to
        // report. This does not wait out a broken page — `threw` already broke
        // the loop for that — it stops a *working* page's last line from being
        // missed by a hair.
        await sleep(graceMs);
        state = await snapshot();
      }
      break;
    }
    if (Date.now() >= deadline) break;
    await sleep(100);
    state = await snapshot();
  }
  result.state = state;
  if (Date.now() >= deadline) {
    result.note = `the page had not settled after ${timeoutMs}ms; this is what it looked like then`;
  }

  const { result: ua } = await send('Runtime.evaluate',
    { expression: 'navigator.userAgent', returnByValue: true }, sessionId);
  result.userAgent = ua.value;

  const { result: resources } = await send('Runtime.evaluate', {
    expression: `JSON.stringify(performance.getEntriesByType('resource').map((r) => ({
      name: r.name, initiatorType: r.initiatorType,
      transferSize: r.transferSize,
      responseStatus: r.responseStatus === undefined ? null : r.responseStatus,
    })))`,
    returnByValue: true,
  }, sessionId);
  result.resources = JSON.parse(resources.value || '[]');

  for (const event of events) {
    if (event.method === 'Runtime.consoleAPICalled') {
      result.console.push({
        type: event.params.type,
        text: event.params.args
          .map((a) => (a.value !== undefined ? a.value : (a.description ?? a.type)))
          .join(' '),
      });
    } else if (event.method === 'Runtime.exceptionThrown') {
      const d = event.params.exceptionDetails;
      result.exceptions.push({
        text: d.text,
        description: (d.exception && (d.exception.description || d.exception.value)) || null,
        line: d.lineNumber, column: d.columnNumber, url: d.url,
      });
    } else if (event.method === 'Log.entryAdded') {
      result.logEntries.push({
        level: event.params.entry.level,
        text: event.params.entry.text,
        source: event.params.entry.source,
      });
    }
  }

  // The verdict is built from the CDP event stream, not from the page-side
  // wrapper: the stream sees every console line (the wrapper can miss the ones
  // logged before it installed) and every network request, including ones that
  // never left the browser.
  const consoleErrors = [];
  const iconWarmups = [];
  const iconRequests = [];
  for (const event of events) {
    if (event.method === 'Runtime.consoleAPICalled' && event.params.type === 'error') {
      const text = event.params.args
        .map((a) => (a.value !== undefined ? a.value : (a.description ?? a.type)))
        .join(' ');
      if (isIconWarmup(text)) iconWarmups.push(text);
      else consoleErrors.push(text);
    } else if (event.method === 'Network.requestWillBeSent') {
      const requestUrl = event.params.request.url;
      if (/\/assets\/icons\/[^/]+\.svg(\?|$)/.test(requestUrl)) iconRequests.push(requestUrl);
    }
  }

  const harness = (result.state && result.state.harness) || { errors: [], rejections: [], consoleErrors: [] };
  result.iconWarmups = iconWarmups;
  result.iconRequests = iconRequests;
  result.pageErrors = [
    ...harness.errors.map((e) => `error: ${e.message} (${e.source}:${e.line})`),
    ...harness.rejections.map((r) => `unhandledrejection: ${r}`),
    // `console.error` is not an uncaught error, but in this bundle it is where a
    // failure the page caught ends up — and it is the only trace of a panic
    // inside `start()`. Every one is fatal except the single literal warm-up
    // string above, which is classified separately and counted in `iconWarmups`.
    ...consoleErrors.map((c) => `console.error: ${c}`),
  ];

  if (screenshot) {
    try {
      const { data } = await send('Page.captureScreenshot', { format: 'png' }, sessionId);
      writeFileSync(screenshot, Buffer.from(data, 'base64'));
      result.screenshot = screenshot;
    } catch (error) {
      result.screenshot = `failed: ${error.message}`;
    }
  }

  // --- the tab API, from the page ---------------------------------------
  //
  // Each step runs inside the loaded document, so the request is the browser's
  // own `fetch` through the proxy — the same call the frontend's wasm client
  // makes. The assertions are on the *body*: a 200 carrying an error JSON is a
  // failure, not a pass.
  if (proxyLeg) {
    const steps = [];
    const step = (name, ok, detail) => steps.push({ name, ok, detail });
    result.proxy = { ok: false, note: null, steps, tabId: null };

    // One `Runtime.evaluate` per step, awaited. `awaitPromise` is what makes the
    // fetch's promise resolve before the value comes back.
    const inPage = async (expression) => {
      const { result: value, exceptionDetails } = await send('Runtime.evaluate', {
        expression: `(async () => { ${expression} })()`,
        awaitPromise: true,
        returnByValue: true,
      }, sessionId);
      if (exceptionDetails) {
        throw new Error(exceptionDetails.exception?.description || exceptionDetails.text);
      }
      return value.value;
    };

    try {
      // A fetch helper, defined once in the page.
      await inPage(`
        globalThis.__evoFetch = async (path, init) => {
          const response = await fetch(path, init);
          const text = await response.text();
          let body = null;
          try { body = JSON.parse(text); } catch {}
          return { status: response.status, body, text };
        };
        return true;
      `);

      // 1. open a tab
      const opened = await inPage(`
        return await __evoFetch('/api/tabs', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ folder: ${JSON.stringify(tabFolder)}, program: ${JSON.stringify(tabProgram)} }),
        });
      `);
      const id = opened.body && opened.body.id;
      result.proxy.tabId = id || null;
      step('open a tab (POST /api/tabs)', opened.status === 201 && !!id && opened.body.state === 'running',
        `HTTP ${opened.status}, id ${id || '(none)'}, state ${opened.body ? opened.body.state : '?'}`);
      if (!id) throw new Error(`no tab id in the reply: ${opened.text}`);

      // 2. the snapshot, through the proxy
      const snap = await inPage(`return await __evoFetch(${JSON.stringify(`/api/tabs/${id}/snapshot`)});`);
      const topics = snap.body && snap.body.topics ? Object.keys(snap.body.topics) : [];
      const live = snap.body && snap.body.topics && snap.body.topics.session
        && snap.body.topics.session.state && snap.body.topics.session.state.model;
      step('read the snapshot (GET …/snapshot)',
        snap.status === 200 && !(snap.body && snap.body.error) && topics.includes('session') && !!live,
        `HTTP ${snap.status}, epoch ${snap.body ? snap.body.epoch : '?'}, topics [${topics.join(', ')}]`);

      // 3. an op, through the proxy
      const op = await inPage(`
        return await __evoFetch(${JSON.stringify(`/api/tabs/${id}/ops`)}, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ op: 'goal.set', args: { objective: 'proxy leg' } }),
        });
      `);
      const goal = op.body && op.body.result && op.body.result.goal;
      step('post an op (POST …/ops)',
        op.status === 200 && op.body && op.body.ok === true && !!goal && goal.objective === 'proxy leg',
        `HTTP ${op.status}, ok ${op.body ? op.body.ok : '?'}, goal ${goal ? goal.objective : '(none)'}`);

      // 4. a refused op stays refused — the proxy must not turn a refusal into a
      //    success, and the page must see the refusal it was sent.
      const refused = await inPage(`
        return await __evoFetch(${JSON.stringify(`/api/tabs/${id}/ops`)}, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ op: 'not.a.real.op', args: {} }),
        });
      `);
      const refusedOk = refused.body ? refused.body.ok : null;
      step('a refused op is not reported as ok', refusedOk !== true,
        `HTTP ${refused.status}, ok ${refusedOk === null ? '(absent)' : refusedOk}`);

      // 5. the tab list, from the page
      const list = await inPage(`return await __evoFetch('/api/tabs');`);
      const listed = list.body && Array.isArray(list.body.tabs)
        && list.body.tabs.some((tab) => tab.id === id);
      step('the tab is in the list (GET /api/tabs)', list.status === 200 && listed,
        `HTTP ${list.status}, ${list.body && list.body.tabs ? list.body.tabs.length : '?'} tab(s)`);

      // 6. the token never reaches the page. Read the tab's view and the
      //    snapshot and look for a 64-hex secret in either — the shape a serve
      //    token has. This cannot read the token itself (the page must not be
      //    able to), so it asserts the *shape* is absent.
      const view = await inPage(`return await __evoFetch(${JSON.stringify(`/api/tabs/${id}`)});`);
      const SECRET = /[0-9a-f]{64}/;
      const leaked = [view.text, snap.text, opened.text].filter((t) => SECRET.test(t));
      step('no bearer token in any reply the page can read', leaked.length === 0,
        leaked.length === 0 ? 'no 64-hex secret in the replies' : `${leaked.length} reply/replies carry one`);

      // 7. stop it, from the page
      const stopped = await inPage(`
        return await __evoFetch(${JSON.stringify(`/api/tabs/${id}`)}, { method: 'DELETE' });
      `);
      step('stop the tab (DELETE …)', stopped.status === 200 && stopped.body && stopped.body.id === id,
        `HTTP ${stopped.status}`);

      // 8. and it is really gone
      const after = await inPage(`return await __evoFetch(${JSON.stringify(`/api/tabs/${id}/snapshot`)});`);
      step('the stopped tab no longer answers', after.status === 404,
        `HTTP ${after.status} for a stopped tab`);

      // 9. These requests went through the *browser's* network stack, not some
      //    other client's. The evidence is the browser's own account of them.
      //
      //    The headers live in `Network.requestWillBeSentExtraInfo`, not in
      //    `requestWillBeSent` — that one carries only a few client-set headers,
      //    while the extra-info event carries what the browser actually put on
      //    the wire. `curl` sends neither `Sec-Fetch-Site` nor `Referer`, so a
      //    request carrying both was issued by this browser and no other client.
      //    That is what makes this leg a browser proof rather than the Rust test
      //    in disguise.
      const requestedTabUrls = new Set(
        events
          .filter((e) => e.method === 'Network.requestWillBeSent')
          .filter((e) => /\/api\/tabs(\/|\?|$)/.test(e.params.request.url))
          .map((e) => e.params.request.url));
      const browserish = events
        .filter((e) => e.method === 'Network.requestWillBeSentExtraInfo')
        .map((e) => Object.fromEntries(
          Object.entries(e.params.headers || {}).map(([k, v]) => [k.toLowerCase(), v])))
        .filter((h) => h['sec-fetch-site'] === 'same-origin' && !!h.referer);
      step('the requests were issued by this browser (Sec-Fetch-Site + Referer)',
        requestedTabUrls.size > 0 && browserish.length > 0,
        `${requestedTabUrls.size} /api/tabs request(s) seen, ` +
        `${browserish.length} carrying Sec-Fetch-Site: same-origin and a Referer`);

      result.proxy.ok = steps.every((s) => s.ok);
    } catch (error) {
      result.proxy.note = `the proxy leg threw: ${error.message}`;
      result.proxy.ok = false;
    }
  }

  result.ok = true;
} catch (error) {
  result.note = `the harness itself failed: ${error.message}`;
}

cleanup();
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
process.exit(result.ok ? 0 : 1);
MJS

    node --check "$WORK/cdp.mjs" >/dev/null 2>&1 ||
        bad "the CDP harness has a syntax error (this is a bug in web_check.sh)"

    if node "$WORK/cdp.mjs" \
        --url "$base/" \
        --chrome "$browser" \
        --glue "${glue_file#./}" \
        --timeout "${WEB_CHECK_TIMEOUT:-30000}" \
        --grace "${WEB_CHECK_GRACE:-2500}" \
        --screenshot "$WORK/page.png" >"$WORK/browser.json" 2>"$WORK/browser.err"; then
        browser_proof=yes
    else
        code=$?
        bad "the browser leg could not run (node exited $code)"
        sed 's/^/         /' "$WORK/browser.err" >&2 || true
    fi
fi

if [ "$browser_proof" = yes ]; then
    echo "   --- what the page said ---"
    # The icon warm-up repeats once per icon per retry (16 here), and printing it
    # 16 times would push the lines that matter past the `head` below. It is
    # collapsed to a single line and its count is reported by the check further
    # down; every other line is shown exactly as it was logged.
    jq -r '.console[]
        | if .type == "error" and (.text | test("Wasm assets loading, will be available soon\\.\\.\\.$"))
          then "   console.\(.type): \(.text)   [warm-up, collapsed]"
          else "   console.\(.type): \(.text)" end' "$WORK/browser.json" 2>/dev/null |
        awk '!/\[warm-up, collapsed\]/ || !seen[$0]++' | head -40 || true
    jq -r '.exceptions[] | "   UNCAUGHT: \(.description // .text) (\(.url):\(.line))"' \
        "$WORK/browser.json" 2>/dev/null | head -20 || true
    jq -r '.pageErrors[] | "   PAGE \(.)"' "$WORK/browser.json" 2>/dev/null | head -20 || true
    jq -r '.logEntries[] | select(.level != "verbose") | "   browser.\(.level): \(.text)"' \
        "$WORK/browser.json" 2>/dev/null | head -20 || true
    warmup_count="$(jq -r '.iconWarmups | length' "$WORK/browser.json" 2>/dev/null || echo 0)"
    if [ "${warmup_count:-0}" -gt 0 ]; then
        echo "   (${warmup_count}x the gpui-kit icon warm-up, treated as benign — see the check below)"
    fi

    ua="$(jq -r '.userAgent // "unknown"' "$WORK/browser.json")"
    ok "browser: $ua"

    if [ "$(jq -r '.ok' "$WORK/browser.json")" != true ]; then
        bad "the browser harness failed: $(jq -r '.note // "no reason given"' "$WORK/browser.json")"
    else
        if [ "$(jq -r '.note // empty' "$WORK/browser.json")" != "" ]; then
            note "$(jq -r '.note' "$WORK/browser.json")"
        fi

        # 1. The module instantiated. `#evo-boot` is removed immediately after
        #    `init()` resolves and before `start()` is called, so its absence is
        #    the page's own report that instantiation succeeded. It only means
        #    that, though, if the page that loaded is ours: an error body never
        #    had a boot element to remove, and reading its absence as success is
        #    how a 503 error body once passed a check.
        boot="$(jq -r '.state.boot // "gone"' "$WORK/browser.json")"
        has_glue="$(jq -r '.state.hasGlue' "$WORK/browser.json")"
        if [ "$has_glue" != true ]; then
            bad "the page in the tab is not the bundle's page: its markup does not name ${glue_file#./}"
        elif [ "$boot" = gone ]; then
            ok "the wasm module instantiated (the page removed #evo-boot)"
        else
            bad "the module did not instantiate: the page still shows #evo-boot ($boot)"
        fi

        # 2. The wasm actually came over the wire, 200 and not from a failed
        #    request the page then reported.
        wasm_status="$(jq -r '[.resources[] | select(.name | endswith(".wasm")) | .responseStatus] | first // "absent"' "$WORK/browser.json")"
        if [ "$wasm_status" = 200 ]; then
            ok "the browser fetched the wasm module (200)"
        else
            bad "the browser did not get the wasm module (status $wasm_status)"
        fi

        # 3. gpui mounted its canvas. The page ships none, so any canvas with a
        #    real size is the platform's — the shell having opened a window,
        #    which is `start()` having worked.
        mounted="$(jq -r '[.state.canvases[] | select(.clientWidth > 0 and .clientHeight > 0)] | length' "$WORK/browser.json")"
        if [ "$mounted" -ge 1 ]; then
            size="$(jq -r '[.state.canvases[]][0] | "\(.clientWidth)x\(.clientHeight)"' "$WORK/browser.json")"
            ok "gpui's canvas mounted ($size)"
        else
            bad "no gpui canvas mounted: the shell never opened a window"
            jq -r '.state.canvases[] | "         canvas id=\(.id) size=\(.clientWidth)x\(.clientHeight) webgl2=\(.hasWebgl2)"' \
                "$WORK/browser.json" 2>/dev/null || true
        fi

        # 4. Nothing threw. This covers the page's own `console.error` too, which
        #    is where a panic the boot script caught would otherwise hide. The
        #    one exception is the gpui-kit icon warm-up, classified in the CDP
        #    client by a whole-string literal match and counted separately — so a
        #    page that logs it 1000 times and nothing else passes, while the same
        #    page plus one real error does not. `exceptions` is never excused.
        if [ "$(jq -r '.exceptions | length' "$WORK/browser.json")" -gt 0 ]; then
            bad "the page threw an uncaught exception: see the UNCAUGHT lines above"
        elif [ "$(jq -r '.pageErrors | length' "$WORK/browser.json")" -eq 0 ]; then
            ok "no uncaught error, no unhandled rejection, no unexpected console.error"
        else
            bad "the page reported an error: see the PAGE lines above"
        fi

        # 5. The icons actually arrived. "Nothing complained" is weak evidence on
        #    its own: a fetch that is never attempted also complains about nothing
        #    much. This asserts the positive — that `assets/icons/*.svg` requests
        #    really left the browser — from raw CDP network events, because
        #    `performance` resource timing reported `responseStatus: 0` for a
        #    fetch that had just returned 200 in a control run.
        #
        #    The warm-up count is the tell that makes this worth asserting: gpui-kit
        #    returns that `Err` precisely because a fetch is in flight, so warm-ups
        #    with zero requests is the signature of a fetch that can never succeed
        #    (a relative URL, which reqwest's wasm backend rejects with "builder
        #    error" before any request is sent).
        icon_requests="$(jq -r '.iconRequests | length' "$WORK/browser.json")"
        warmups="$(jq -r '.iconWarmups | length' "$WORK/browser.json")"
        if [ "$icon_requests" -gt 0 ]; then
            ok "the browser fetched icons ($icon_requests request(s), $warmups warm-up log(s))"
        elif [ "$warmups" -gt 0 ]; then
            bad "the UI asked for icons $warmups time(s) but the browser sent 0 icon requests"
            echo "         the gpui-kit wasm asset source builds its URL as"
            echo "         '{endpoint}/assets/icons/<name>.svg'; with the default endpoint"
            echo "         that is relative, and reqwest's wasm backend rejects a relative"
            echo "         URL with 'builder error' before any request is sent — so the"
            echo "         warm-up retries forever and the icons never load."
        else
            note "the page requested no icons, so nothing was asserted about them"
        fi

        if [ -f "$WORK/page.png" ]; then
            ok "screenshot: $WORK/page.png"
        fi
    fi
fi

# --- 6. the tab API, driven from the page -----------------------------------

# This leg is where "the UI boots" becomes "the UI can talk to a session". It
# runs only under --proxy, so the default run's behaviour and exit code are what
# they were.

if [ "$proxy" = no ]; then
    :
else
    step "the tab API, from the page (--proxy)"

    # Cleared before anything can skip: a stale result from an earlier run must
    # never be read as this run's. Every other leg's evidence file is rewritten
    # by the client each time it runs; this is the same rule for the one leg that
    # can decide not to run at all.
    rm -f "$WORK/proxy.json" "$WORK/proxy.err"

    # The binary the tab will run. `EVO_AGENT_BIN`/`EVO_SWARM_BIN` first, then the
    # installed default — the same order `evo-web` itself uses, so a run that
    # finds one here finds it there. A variable set to a path that is not an
    # executable is a *skip*, not a failure: the caller asked for a binary that is
    # not there, and this leg cannot run without one.
    evo_bin=""
    evo_bin_rejected=""
    for candidate in "${EVO_AGENT_BIN:-}" "${EVO_SWARM_BIN:-}"; do
        [ -n "$candidate" ] || continue
        if [ -x "$candidate" ]; then
            evo_bin="$candidate"
            break
        fi
        evo_bin_rejected="$candidate"
    done
    if [ -z "$evo_bin" ] && [ -z "$evo_bin_rejected" ]; then
        for candidate in /usr/local/bin/evo-agent /usr/local/bin/evo-swarm; do
            if [ -x "$candidate" ]; then
                evo_bin="$candidate"
                break
            fi
        done
    fi

    if [ -n "$evo_bin_rejected" ]; then
        # Set but not executable: skip, and name what was wrong. Falling through
        # to the installed default would ignore what the caller asked for.
        note "SKIPPED: EVO_AGENT_BIN/EVO_SWARM_BIN names $evo_bin_rejected, which is not an executable file"
        proxy_ran=no
    elif [ -z "$evo_bin" ]; then
        # A skip, loudly, and never a pass. The proxy path cannot be exercised
        # without a real serve process, and pretending otherwise is the failure
        # this whole script exists to avoid.
        note "SKIPPED: no evo-agent/evo-swarm to open a tab with"
        note "  looked at EVO_AGENT_BIN, EVO_SWARM_BIN, /usr/local/bin/evo-agent,"
        note "  /usr/local/bin/evo-swarm — none is an executable file"
        note "  set EVO_AGENT_BIN=/path/to/evo-agent to exercise the proxy leg"
        proxy_ran=no
    elif [ "$browser_proof" != yes ]; then
        note "SKIPPED: the browser leg did not run, so there is no page to drive"
        proxy_ran=no
    elif [ "$server_kind" != evo-web ]; then
        note "SKIPPED: the bundle is served by a stand-in, which has no /api at all"
        proxy_ran=no
    else
        proxy_ran=yes

        # A throwaway HOME for the tab's own serve process. This is what keeps a
        # real provider from being called and the real ~/.evo from being written:
        # the child inherits the HOME the server was started with, and the server
        # was started by this script with the ambient one.
        proxy_home="$WORK/proxy-home"
        mkdir -p "$proxy_home"

        # Restart the server with that HOME, since the environment a child
        # inherits is fixed when the server starts. The old one is stopped first
        # — with SIGINT, which is the signal its shutdown ladder listens for (a
        # plain SIGTERM exits without running it; the tabs still die, because
        # each watches its stdin, but the ladder is the clean way).
        if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
            kill -INT "$server_pid" 2>/dev/null || true
            for _ in $(seq 1 40); do
                kill -0 "$server_pid" 2>/dev/null || break
                sleep 0.25
            done
            kill -TERM "$server_pid" 2>/dev/null || true
            wait "$server_pid" 2>/dev/null || true
        fi

        read -r -a cmd_argv <<<"$cmd"
        # `--evo-agent` is passed explicitly, and the two variables are unset for
        # this child. `evo-web`'s own precedence is flag, then environment, then
        # default — so an `EVO_AGENT_BIN` in the caller's shell would otherwise
        # reach the restarted server and change which binary a tab runs, which is
        # not what this leg resolved.
        proxy_argv=("${cmd_argv[@]}" --evo-agent "$evo_bin")
        # `env -u` drops the two variables before `evo-web` reads them; the
        # `exec` is the shell's, which replaces this subshell with the server so
        # `$!` is the server's own pid and a signal reaches it directly.
        (cd "$ROOT" && HOME="$proxy_home" EVO_HOME="$proxy_home/.evo" \
            exec env -u EVO_AGENT_BIN -u EVO_SWARM_BIN \
            "${proxy_argv[@]}") >"$WORK/evo-web-proxy.log" 2>&1 &
        server_pid=$!

        ready=no
        for _ in $(seq 1 100); do
            kill -0 "$server_pid" 2>/dev/null || break
            code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 2 "$base/api/health" 2>/dev/null || echo 000)"
            if [ "$code" = 200 ]; then
                ready=yes
                break
            fi
            sleep 0.2
        done

        if [ "$ready" != yes ]; then
            bad "evo-web (restarted with a throwaway HOME) never answered /api/health"
            tail -10 "$WORK/evo-web-proxy.log" | sed 's/^/         /'
        else
            ok "evo-web restarted for the proxy leg (pid $server_pid, HOME $proxy_home)"

            # The tab API, from the page. `--proxy` makes the CDP client run this
            # script inside the loaded document, so every request below is the
            # browser's own fetch, same-origin, through the proxy — not curl's.
            #
            # `--tab-folder` is the folder the tab opens in: a scratch directory
            # this leg makes, so a stray session cannot land in the repo.
            tab_folder="$WORK/proxy-work"
            mkdir -p "$tab_folder"

            if node "$WORK/cdp.mjs" \
                --url "$base/" \
                --chrome "$browser" \
                --glue "${glue_file#./}" \
                --timeout "${WEB_CHECK_TIMEOUT:-30000}" \
                --grace "${WEB_CHECK_GRACE:-2500}" \
                --proxy \
                --tab-folder "$tab_folder" \
                --tab-program "${WEB_CHECK_PROGRAM:-agent}" \
                >"$WORK/proxy.json" 2>"$WORK/proxy.err"; then
                :
            else
                bad "the proxy leg could not run (node exited $?)"
                sed 's/^/         /' "$WORK/proxy.err" >&2 || true
            fi

            if ! jq -e '.proxy' "$WORK/proxy.json" >/dev/null 2>&1; then
                # No result at all: the client never got as far as the leg. The
                # stderr is the only place the reason can be.
                bad "the proxy leg produced no result"
                sed 's/^/         /' "$WORK/proxy.err" >&2 2>/dev/null || true
            else
                echo "   --- the round trip, from the page ---"
                while IFS=$'\t' read -r ok_flag name detail; do
                    [ -n "$name" ] || continue
                    if [ "$ok_flag" = true ]; then
                        ok "$name: $detail"
                    else
                        bad "$name: $detail"
                    fi
                done < <(jq -r '.proxy.steps[]? | "\(.ok)\t\(.name)\t\(.detail)"' "$WORK/proxy.json" 2>/dev/null)

                # A leg that threw before recording a step says so in its note;
                # without this the output above would show steps and no reason.
                proxy_note="$(jq -r '.proxy.note // empty' "$WORK/proxy.json" 2>/dev/null)"
                if [ -n "$proxy_note" ]; then
                    bad "the proxy leg ended early: $proxy_note"
                fi

                # The tab is stopped by the page's own DELETE, but a failure
                # anywhere above can leave it running; this is the belt to that
                # pair of braces, and it runs however the leg ended.
                stray="$(jq -r '.proxy.tabId // empty' "$WORK/proxy.json" 2>/dev/null)"
                if [ -n "$stray" ]; then
                    curl -sS -X DELETE "$base/api/tabs/$stray" -o /dev/null --max-time 15 2>/dev/null || true
                fi
            fi
        fi
    fi
fi

# --- the verdict ------------------------------------------------------------

echo
echo "== summary"
if [ "$browser_proof" = yes ]; then
    # The browser leg running is not the same as the frontend working: say which
    # of the two happened, so a red run does not read as "no browser proof".
    if [ "${#failures[@]}" -eq 0 ]; then
        echo "   browser proof:   yes — a headless browser loaded the page, instantiated"
        echo "                    the module and mounted the shell's canvas"
    else
        echo "   browser proof:   attempted, and the frontend FAILED in the browser"
    fi
    echo "   browser:         $browser"
else
    echo "   browser proof:   NO — this run is HTTP-level only, and does not show"
    echo "                    that the frontend runs in a browser"
fi
if [ "$server_kind" = "python3 http.server (stand-in)" ]; then
    echo "   server:          a stand-in, not evo-web: /api/... is untested here"
fi
if [ "$proxy" = yes ]; then
    if [ "${proxy_ran:-no}" = yes ]; then
        echo "   proxy leg:       ran — /api/tabs/... driven from the page in the browser"
    else
        echo "   proxy leg:       SKIPPED — /api/tabs/... was NOT exercised (see notes)"
    fi
fi
echo "   artifacts:       $WORK"

if [ "${#notes[@]}" -gt 0 ]; then
    echo
    echo "   notes:"
    # Deduplicated: the same missing capability is noticed where it is needed
    # (node at the socket, python3 at the fallback server), and printing it twice
    # reads like two problems.
    printf '%s\n' "${notes[@]}" | awk '!seen[$0]++' | sed 's/^/     - /'
fi

if [ "${#failures[@]}" -gt 0 ]; then
    echo
    echo "   FAILURES:"
    for entry in "${failures[@]}"; do echo "     - $entry"; done
    echo
    echo "web_check: FAILED (${#failures[@]} check(s))"
    exit 1
fi

if [ "$require_browser" = yes ] && [ "$browser_proof" != yes ]; then
    echo
    echo "web_check: FAILED — --require-browser was given and no browser leg ran"
    exit 1
fi

echo
echo "web_check: passed"
