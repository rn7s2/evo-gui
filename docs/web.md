# The web target: one UI, two shells

Evo is delivered two ways from one codebase:

| | what it is | binary | bundle |
|---|---|---|---|
| **Desktop** | a native macOS app: one window, one tab per session | `evo-desktop` (`crates/app`) | `dist/Evo Desktop.app` |
| **Web** | a server that hosts the UI and drives evo for it; you open a page | `evo-web` (`crates/web`) | `dist/web/` |

Both draw the **same** UI — the same views, and the same palette. Everything the
window shows lives in the shared crates — `workspace`, `transcript`, `composer`,
`agent_list`, `settings`, `widgets` — and neither shell draws a row of it. What
differs is the process around the window, and the transport underneath a tab.

The colours are shared too, and that is `crates/theme`'s whole reason to exist:
the design's palettes are a crate of its own rather than the desktop app's,
because a theme installed by only one shell is a window that looks different in
the other. `crates/app`'s own `theme.rs` moved there for exactly that reason, and
both shells reach it: the desktop through `follow_appearance` (which applies the
palette and then follows the system), the browser through `theme::install` and
`theme::apply`. Neither owns the colours. Before the split, the browser drew
gpui-kit's defaults — white where the design's paper is `#EFE9DF`
(`rgb(239, 233, 223)`, `store::design`).

```
  desktop    evo-desktop ──▶ workspace::WorkspaceView ──▶ spawn evo-swarm serve ──▶ 127.0.0.1:<port>
  web        evo-web     ──▶ dist/web/ (wasm)          ──▶ /api/tabs/<id>/…  ──▶ evo-swarm serve
                             crates/webapp                                          (on the server's machine)
```

## The two shells

**`crates/app` (`evo-desktop`) — the desktop shell.** It owns the process
around the window: the single-instance lock, the window's remembered bounds in
`app.json`, the launch-time loads (catalog, sessions), adopting a login
environment, pruning tab directories, the menu bar, About, Settings, and quit —
which stops every swarm the app started. It turns tree-sitter on for itself (see
[toolchain](#toolchain-requirements) below).

**`crates/webapp` (`evo-webapp`) — the browser shell.** A `cdylib` whose
artifact is a `.wasm` module, mounted into a page by `wasm-bindgen`; its `main`
is the `#[wasm_bindgen] start` in `crates/webapp/src/shell.rs`. It is
deliberately short, because a page has almost none of the desktop shell's work
to do: the browser owns the tab, the page owns the URL, there is no single
instance to take, no window bounds to remember, no child process to inherit an
environment, and no menu bar.

What it does have to do is the three things a page cannot take for granted, and
each is one call in that entry point:

| what | where | why it is needed |
|---|---|---|
| the design's palette | `theme::install` / `theme::apply` (`crates/theme`) | otherwise the browser draws gpui-kit's defaults, which is a different application to look at |
| the UI fonts | `install_fonts` (`shell.rs`) | gpui's web platform builds its text system with **no system fonts**, so the first text layout *panics* without one |
| an absolute icon origin | `gpui_kit::assets::Assets::new(page_origin())` | the kit fetches icons over HTTP; a relative URL makes `reqwest`'s wasm backend reject the request before it is sent |

The last two are covered in [three things that cost this project real
time](#three-things-that-cost-this-project-real-time). The shell registers the
palette before it opens a window, and the fonts after the palette deliberately:
`theme::install` registers the palettes, and the kit's own `mono_font.family` is
applied when a mode is first *loaded* (by `theme::apply`), so setting the family
first would be overwritten by it.

It does **not** reimplement any of the app's behaviour. The window it opens is
the same `workspace::WorkspaceView` the desktop app opens, and the launch path
behind it is the same function with two bodies — see [spawn or
connect](#a-tab-spawns-on-the-desktop-and-connects-in-the-browser).

## Architecture

A browser cannot spawn `evo-swarm`, so the web build's tabs do not talk to a
local child the way the desktop app's do. They talk to the **axum server that
served the bundle**.

```
  browser tab ──▶ evo-web (axum, crates/web)  ──▶ /api/tabs/<id>/…  ──▶ one evo `serve` process
                  serves dist/web/                                      per tab, on the server's machine
                  owns the tabs, the bearer tokens and the ports
```

One `evo-web` process:

- serves the frontend's static bundle (`dist/web/`). A route that does not name
  a file gets `index.html`, so a client-side route (`/tab/7f3a`) reaches the
  page; a name with an extension is an asset, and a missing one is a 404 rather
  than the HTML shell;
- opens **tabs**: each is one `evo-swarm serve` or `evo-agent serve` in a folder
  on the serving machine, started the way the desktop app starts one — the same
  argv (`store::launch`), the same ready file, the same tab directory under
  `~/.evo/desktop/tabs/`. A tab this server opens is a tab the desktop app could
  adopt;
- proxies the serve protocol under `/api/tabs/<id>/…`;
- stops the tabs it started when it is asked to stop. It handles **SIGTERM as
  well as SIGINT** — a person's `Ctrl-C` and a supervisor's or init system's
  `kill` are both a request to stop, and handling only the first would mean a
  service manager's stop skipped the tab-shutdown ladder (the tabs would still
  die, each `serve` watching the stdin that closes with this process, but without
  the graceful stop they would have had).

### Why the bearer token never reaches the browser

Each `serve` process mints a bearer token at launch and writes it into its own
`0600` ready file. The desktop app reads that file and uses the token directly.

In the web build the **server** reads it and keeps it. `evo-web` builds a
`swarm_client::Client` per tab, adds `Authorization: Bearer …` on every request
it forwards, and hands the browser only the reply. The token is never in a
response body, a log line or an error message, and `/api/tabs/<id>` reports the
tab's port and pid but not its token. The browser also never learns the tab's
port: it addresses a tab by **id**, and every request is a path on the origin
that served the page.

### How a tab is addressed

`/api/tabs/<id>/…` — the tab's id as the server minted it (a UUID v4;
`store::paths::TabId::parse` accepts nothing but `[A-Za-z0-9_-]{1,64}` and is
what validates a path segment before it is looked up).

The client side of this is `swarm_client`'s `transport` module, which names both
shapes a request can take — `Target::at(host, port, token)` for a loopback
server and `Target::base_path("/api/tabs/7f3a")` for the proxy — and refuses a
base path that is not a same-origin path (a URL, a `..` step, the origin root).

`crates/webapp/src/transport.rs` names the two so that a tab's transport is
decided in one place rather than in the shared UI:

```rust
enum Endpoint {
    Loopback { host, port, token },   // the desktop app's shape
    Proxied  { base_path },           // "/api/tabs/7f3a" — no token, same origin
}
```

`Endpoint::proxied(id)` percent-encodes the id so it cannot escape its own path
segment, and `crates/webapp/tests/proxy_contract.rs` pins that boundary from the
client side without a browser — including the one shape the encoder cannot
repair (a `.` or `..` is unreserved, so a URL parser collapses it; the server
must never name a tab those, or the empty string).

### The API surface

The authoritative list is the module doc at the top of `crates/web/src/main.rs`;
this is the same list.

| route | what | proven through the proxy |
|---|---|---|
| `GET /` | the frontend (`dist/`, `index.html` fallback) | yes — the browser leg |
| `GET /api/health` | this server: version, tabs, bind | yes — by hand |
| `GET /api/tabs` | every tab, oldest first | yes — the proxy leg |
| `POST /api/tabs` | open one (folder, program, model/effort/…, `resume`) | yes — the proxy leg; and by hand with a real `resume`, which opened a tab carrying the exact journal path back and refused a path that is not a file with a 400 |
| `GET /api/tabs/<id>` | one tab's state | yes — by hand |
| `DELETE /api/tabs/<id>` | stop it, and forget it | yes — the proxy leg |
| `GET /api/tabs/<id>/snapshot` | the tab's whole view, atomically | yes — the proxy leg |
| `GET /api/tabs/<id>/stream` | the tab's op stream (SSE, unbuffered) | yes — by hand: a live turn, frames arriving as they happen |
| `POST /api/tabs/<id>/ops` | one operation (`input.send`, `goal.*`, …) | yes — the proxy leg |
| `GET /api/tabs/<id>/items` | older items, paging back | yes — by hand: a page returned, and `before=` paged to an older item |
| `GET /api/tabs/<id>/items/<i>` | one item, whole | yes — by hand: the item's own JSON, 200 |
| `GET /api/tabs/<id>/media/<i>/<n>` | image bytes, with their content type | yes — by hand: a real PNG sent as an attachment came back byte-identical as `image/png`; a session with no such image relays the tab's own 404 with its status |
| `GET /api/tabs/<id>/catalog` | the running session's own registry | yes — by hand: the tab's registry, commands and models |
| `GET /api/catalog` | the machine's registry (offline CLI read) | yes — by hand: the machine's registry |
| `GET /api/sessions` | the machine's session history (offline read) | yes — by hand: the session that had just run |

Every proxied route is one route of the serve protocol. There is no catch-all
that forwards whatever path a caller typed.

"The proxy leg" means `scripts/web_check.sh --proxy` drives it from inside the
real page. "By hand" means a request made through `evo-web` outside that script
— real, but not something the gate re-runs. Neither column says the *frontend*
uses the route: `/api/catalog` and `/api/sessions` are called by nothing yet (see
[not implemented](#not-implemented-on-the-web-side)).

### Security

`evo-web` listens on a port and can start and stop processes, so:

- **Loopback by default.** `--bind` anything else is refused at startup unless
  `--auth-token` (or `EVO_WEB_TOKEN`) is given.
- **The token never leaves.** See above.
- **No shell, ever.** A folder or a model name goes into an argv **list**
  (`store::launch::LaunchSpec::argv`) handed to `Command`, never into a command
  string. There is no `sh -c` in the crate.
- **Same-origin only.** No CORS headers are sent. When a token is configured,
  `/api` requires it — as `Authorization: Bearer`, or as the `evo_web_token`
  cookie **and** a mutating request whose `Origin` matches this server, so a page
  on another site cannot drive a tab through the cookie alone.
- **Nothing is forwarded blindly.** A path segment (a tab id, an item id, a
  topic) is validated, and `swarm_client` percent-encodes what it puts in a
  request line.
- **The bundle is served by `tower-http`'s `ServeDir`**, which rejects a path
  that climbs out of the directory it was given.

### Cross-origin isolation, and what it does and does not change

`evo-web` sends two headers on every response:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Verified live — both are on `/` and on the wasm module alike, and a page served
by `evo-web` reports `crossOriginIsolated: true` with `SharedArrayBuffer`
defined, while the same bundle served by a plain static server reports `false`
and `undefined` for both.

They are there because gpui's web platform wants them: it spawns its background
workers on a dedicated OS thread (`gpui-pre-scheduler`'s
`spawn_dedicated_os_thread`), and on wasm that is `wasm_thread`, whose own README
says a page needs `SharedArrayBuffer` and therefore COOP and COEP. `require-corp`
is also what keeps the bundle's own subresources (the wasm module, the icons)
loadable from this origin.

**Be careful what you conclude from that, though — measured, not assumed.** The
module `scripts/web.sh` builds today declares **non-shared** memory (its memory
section's limits flag is `0x0`; a shared one would be `0x2`), so `wasm_thread`'s
shared-memory path cannot engage whether the headers are there or not. The page
notices and says so:

```
[WARN] gpui_web::dispatcher: Required WebAssembly threading APIs are unavailable;
       falling back to single-threaded dispatcher
```

and that warning appears **with the headers as well as without them** — the
dispatcher checks `shared_memory_supported()` and falls back rather than failing.
Serving the bundle with and without the headers gives a page that boots and draws
in both cases; what differs is `crossOriginIsolated` and `SharedArrayBuffer`.

So: the headers are what the platform asks for and what `evo-web` serves, and
they are what a build with atomics enabled would need. Today they are not what
makes the page work — the fallback is. **If you serve `dist/web` from anything
but `evo-web`** — a plain static server, a CDN, a reverse proxy — send both
headers yourself, on the page's response and on its subresources, or you are one
build flag away from a page that stops working.

## Toolchain requirements

The web build has three requirements the desktop build does not. Each one is a
build failure with an unhelpful message if you miss it, so here is what each is
and why.

### 1. The **nightly** toolchain

```
rustup toolchain install nightly
rustup target add wasm32-unknown-unknown --toolchain nightly
```

`gpui-pre-web`'s default `multithreaded` feature pulls `wasm_thread`, whose
crate root is:

```rust
#![cfg_attr(target_arch = "wasm32", feature(stdarch_wasm_atomic_wait))]
```

That intrinsic is unstable, so on stable you get `E0554: #![feature] may not be
used on the stable release channel`. `scripts/web.sh` builds with
`cargo +nightly` (override the toolchain with `WASM_TOOLCHAIN`), but it does
**not** install it — without nightly the build stops at
`error: toolchain 'nightly' is not installed`. The `target add` is needed too:
a fresh toolchain ships only the host target.

### 2. gpui's **tree-sitter features OFF**

`gpui-component` declares its tree-sitter dependency under
`[target.'cfg(not(target_family = "wasm"))'.dependencies]`, but its
`tree-sitter*` **features** name `dep:tree-sitter` unconditionally. Asking for
those features while compiling for wasm therefore cannot resolve the crate —
you get a wall of `unresolved import tree_sitter` / `cannot find crate
tree_sitter` errors from inside a dependency.

So the root `Cargo.toml` declares `gpui-kit = { version = "=0.7.0" }` with **no
features**, and `crates/app` — the native binary — turns the grammars on for
itself:

```toml
gpui-kit = { workspace = true, features = ["tree-sitter", "tree-sitter-rust", …] }
```

The consequence is honest and small: **the web build renders code fences without
syntax colouring.** The kit falls back to its wasm highlighter stub, so it is
the same fence, uncoloured. The grammar is C that no browser runs anyway.

### 3. `wasm-bindgen` matching the module

`wasm-bindgen`'s CLI has to match the version the module was built against. The
module carries a `wasm-bindgen` custom section naming its **schema version**, and
the CLI refuses a module whose schema differs — with this message (its own words,
from `wasm-bindgen-cli-support`; the current schema version is `0.2.128`):

```
it looks like the Rust project used to create this Wasm file was linked against
version of wasm-bindgen that uses a different bindgen format than this binary:

  rust Wasm file schema version: <the module's>
     this binary schema version: <the CLI's>

Currently the bindgen format is unstable enough that these two schema versions
must exactly match. You can accomplish this by either updating this binary or
the wasm-bindgen dependency in the Rust project.
```

`scripts/web.sh` reads the version out of `Cargo.lock` (not whatever is newest)
and installs the CLI at exactly that version **when no `wasm-bindgen` is on
`PATH` at all**. What it does **not** do is check the version of one that *is*
there — verified by putting a stub `wasm-bindgen` printing `0.0.1-OLD` on `PATH`,
and the script used it without complaint and wrote the bundle. So if you already
have a different `wasm-bindgen` installed, that is the one you will get.

At the time of writing the lock says **0.2.129**, and that is what is installed
here. If you hit a version error, install the matching one by hand:

```sh
cargo install wasm-bindgen-cli --version 0.2.129
```

## Build and run

```sh
scripts/web.sh                    # build dist/web/: the wasm module, its glue, the page
cargo run -p evo-web -- --dist dist/web
```

`scripts/web.sh` runs
`cargo +nightly build --release -p evo-webapp --target wasm32-unknown-unknown`,
then `wasm-bindgen --target web --no-typescript --out-dir dist/web <the .wasm>`,
copies `crates/webapp/web/index.html` beside it, and copies gpui-kit's 1830
icons into `dist/web/assets/icons/`. `--no-build` skips only the first step: the
wasm-bindgen pass and the icon copy still run, against the module that is already
there.

The icons are a step of their own because the kit's web asset source does not
bundle them: it **fetches** each one, `{origin}/assets/icons/<name>.svg`, and
caches them as they arrive. The desktop build reads the same files out of the
crate, which is why this is web-only. Without them every icon is a failed fetch
and the widget that wanted it logs `Wasm assets loading, will be available
soon…`.

`evo-web` prints one line when it is up:

```
evo-web ready — http://127.0.0.1:8787  (dist dist/web, root ~/.evo/desktop)
```

Then open that URL.

### Checking it

`scripts/web_check.sh` is the gate for "does this actually run in a browser?" —
not "does it compile". It has four legs and says which of them it got to:

1. **Build** — it calls `scripts/web.sh` rather than duplicating it.
2. **Serve + HTTP** — serves the bundle (through `evo-web`) and asserts the page,
   its content types, the wasm magic number, the glue the page imports, and that
   a path traversal out of the served directory is refused.
3. **Browser** — drives headless Chrome over the DevTools protocol and watches
   the page's console, its uncaught errors, whether the module instantiated, and
   whether gpui's canvas mounted with a real size.
4. **Proxy** (`--proxy`) — the strongest single check. It drives `/api/tabs/…`
   **from inside the real page**, so what is proven is the browser's own `fetch`
   through the proxy rather than `curl`'s: open a tab, read its snapshot, post an
   op, see a refusal stay a refusal, list the tabs, stop the tab, and find it
   gone. Every step asserts on the **body**, so a 200 carrying an error JSON
   fails. Its last step asserts the requests carried `Sec-Fetch-Site:
   same-origin` and a `Referer`, which only a browser sends — that is what makes
   it a browser proof and not a Rust test in disguise.

```sh
scripts/web_check.sh --no-build --proxy   # the whole thing, against the bundle you have
scripts/web_check.sh                      # build the bundle first, then check it
scripts/web_check.sh --no-build           # the browser legs, without the proxy round trip
scripts/web_check.sh --http-only          # degraded: skip the browser entirely
```

The proxy leg needs a real `evo-agent` (or `evo-swarm`). When there is none it
**skips** and says so; a skip is never reported as a pass. The child runs in a
throwaway `HOME` under the work directory, so no provider is called and the real
`~/.evo` is not written.

The browser leg needs no npm packages and no driver: Chrome's own
`--remote-debugging-port=0` plus Node's global `WebSocket` is the whole client.
Node's only job is to hold that socket; the JSON it prints is the evidence.

What the proxy leg does **not** prove is *which routes work* — it drives five of
them, and the rest of the table above was proven by hand instead. Two things it
does not prove at all: anything about the frontend's own JavaScript, since it
drives the API from the page rather than through the app's client; and a *turn*,
because its round trip is over a session with **no model configured** — the tab
comes up and answers, but no prompt is sent. (The stream check below is what
covers a turn.)

#### The whole journey, driven by hand

Beyond what the script asserts, the full path has been driven end to end in
headless Chrome — a person's journey, not an API's:

```
type an absolute folder  →  the session opens  →  the session page draws
   (lanes column, transcript, composer)  →  type a prompt  →  Enter
   →  the session takes a kind=user item  →  the agent runs a full turn (15 items)
   →  zero wasm panics
```

**If you want to reproduce it, the input is the fiddly part.** gpui-web does not
read keystrokes from the canvas: it takes text through a hidden IME-mirror
`<textarea>` (`gpui-pre-web`'s `ime_mirror`), and the platform listens for the
DOM's `input` and `beforeinput` events on it. So CDP's
`Input.dispatchKeyEvent` and `Input.insertText` do **not** reach the app, and a
probe built on them types into nothing. What works is the DOM path:

1. set that textarea's `value`, then dispatch an `input` event — the app imports
   the text;
2. dispatch a `beforeinput` with `inputType: "insertLineBreak"` to submit —
   `events.rs` maps `insertLineBreak`/`insertParagraph` to a synthetic Enter
   (`dispatch_synthetic_keystroke("enter", …)`) rather than inserting a newline,
   which is exactly the "Enter means submit" rule the composer wants.

That is why the check's browser leg proves the page *boots* and the proxy leg
proves the *API*, while the journey above needed a hand-written driver: neither
can type into a canvas.

#### SSE through the proxy is proven

`GET /api/tabs/<id>/stream` is a live stream through the proxy, not a buffered
body. A client against `/api/tabs/<id>/stream?topics=session` on a running tab
returns

```
status=200 content-type=text/event-stream cache-control=no-cache, no-transform x-accel-buffering=no
```

and then frames that arrive **as the turn happens**: with the connection already
open, `POST /api/tabs/<id>/ops` with `input.send` produced a turn's worth of
frames — a `hello`, then `item.add`, `item.append`, `item.patch` and
`state.patch` — at *distinct arrival times*, which is the thing that shows the
proxy relays line by line rather than collecting the body. (The scripted model's
six-second `SLOW` turn made it plainest: 74 frames, the first at t+0.01 s, more at
t+1.99 s, the rest as the deltas landed.)

The same turn against the tab's server **directly**, bypassing `evo-web`
altogether, produces the same 15 frames in the same mix — so the proxy is
transparent here: it neither drops a frame nor adds one.

**To see frames, send a prompt.** An `eval` op emits none: its answer comes back
in the `POST …/ops` reply body (`{"ok":true,"result":{"value":"⇒ 3"},…}`), not on
the stream. That is the evo server's own behaviour and not a proxy defect — the
same `eval` against the tab's server directly yields the same single `hello`
frame — so a probe that sends only an `eval` sees the `hello` and nothing else,
and that is correct.

### Options

Verbatim from `evo-web --help`:

```
--bind ADDR        address to listen on (default 127.0.0.1, env EVO_WEB_BIND).
                   Anything that is not loopback REQUIRES --auth-token.
--port N           port to listen on (default 8787, env EVO_WEB_PORT).
                   0 asks the OS for a free one; the chosen one is printed.
--dist DIR         the frontend bundle to serve (default dist, env EVO_WEB_DIST)
--root DIR         where tabs live (default ~/.evo/desktop, env EVO_WEB_ROOT)
--auth-token T     require this token on /api (env EVO_WEB_TOKEN)
--evo-agent PATH   the evo-agent to run (default $EVO_AGENT_BIN, else
                   /usr/local/bin/evo-agent)
--evo-swarm PATH   the evo-swarm to run (default $EVO_SWARM_BIN, else
                   /usr/local/bin/evo-swarm)
--prompt-note PATH markdown every session this server starts is told about
-h, --help         this text
```

Serving beyond loopback:

```sh
evo-web --bind 0.0.0.0 --auth-token "$(openssl rand -hex 32)" --dist dist/web
```

Without a token that bind is refused outright — it is a startup error, not a
warning.

## Three things that cost this project real time

All three are silent failures — the build is clean and the page loads; the
symptom appears later and looks like something else.

### 1. The bundle needs the fonts, or the first text layout panics

gpui's web platform builds its text system with **no system fonts** — a page has
no font database to ask — so `TextSystem::resolve_font` panics on the first text
layout unless a font has been registered. The desktop app never meets this: it
reads the machine's own fonts.

`crates/webapp/src/shell.rs` registers them before any window exists, embedded
with `include_bytes!` from `assets/fonts/` so the browser has them the moment the
module loads rather than after a fetch the first frame would wait on:

| family | files | why |
|---|---|---|
| IBM Plex Sans | `IBMPlexSans-Regular.ttf`, `-Bold.ttf` | the family gpui's web platform names as its system UI font, so `.SystemUIFont` — what `Font::default()` asks for — resolves. Shipping a different family would leave the default text unresolved and panic exactly as no font at all does. |
| IBM Plex Mono | `IBMPlexMono-Regular.ttf`, `-Bold.ttf` | the monospace half. The kit's monospace default is the platform's — `"DejaVu Sans Mono"` on a target that is neither macOS nor Windows, which is what wasm gets — and it does not resolve in a browser, so code blocks, the symbol popup and the step clock would silently fall back to a *proportional* face. The shell sets `theme.mono_font_family` to `"IBM Plex Mono"` outright, **after** `theme::install` (the install applies the kit's own family, so setting it first would be overwritten). |

Both are SIL Open Font License; the licences are in `assets/licenses/`.

### 2. The icon source needs an absolute origin

The kit's icon source fetches `{endpoint}/assets/icons/<name>.svg`, and its
default endpoint is the empty string — which makes that URL *relative*, and
`reqwest`'s wasm backend rejects a relative URL with `builder error` before any
request is sent. Every icon then failed and the widget that wanted one retried
forever.

So the shell passes this page's own origin:

```rust
.with_assets(gpui_kit::assets::Assets::new(page_origin()))   // window.location.origin
```

Same-origin is both correct and the only thing a page may reach without CORS:
`evo-web` serves `/assets/icons/…` from the same origin as the page.

### 3. Half of `std` panics on wasm, and none of it says so

This is the trap that cost the most, because every one of these **compiles
cleanly** for `wasm32-unknown-unknown`. There is no warning, no lint, no error —
the call site looks exactly like the one that works on a desktop, and it fails
only when a person reaches it in a browser.

Verified: a file calling all three of the below builds for wasm with `rustc
+nightly --target wasm32-unknown-unknown --crate-type=lib`, exit 0.

| call | what actually happens on `wasm32-unknown-unknown` | where it bit |
|---|---|---|
| `std::thread::spawn` | **panics.** `std`'s `sys::thread::unsupported::Thread::new` returns `io::Error::UNSUPPORTED_PLATFORM`, and `Builder::spawn` unwraps it. `wasm_thread` does **not** rescue this — it only offers its own `Builder`, so a plain `std::thread::spawn` is still a plain `std::thread::spawn`. | `workspace::bridge::Bridge::spawn`, called from `record_recent` inside `apply(Update::Ready)`. The panic unwound *before* `set_state(TabState::Running)`, which is the only state the session page renders from — so the window stayed on the empty tab and opening a session looked like nothing happening. Also two sites in `tab.rs`, and `settings::probe`'s binary check. |
| `std::time::SystemTime::now()` | **panics**: `time not implemented on this platform` (`sys/time/unsupported.rs`). `Instant::now` is the same. | `store::time::now_epoch`, reached by `paths::fill_random`'s fallback when `/dev/urandom` cannot be opened — i.e. every page. The launch path panicked before a session could be shown. |
| `Path::is_absolute()` | **always `false`**, for every path including `/Users/you/foo`. `std` compiles its path support from `sys/path/unsupported_backslash.rs`, where `parse_prefix` returns `None` for every input and `is_absolute` is `path.has_root() && path.prefix().is_some()`. `has_root()` is `true`; the conjunction can never hold. | Every path a person typed into the folder field was refused as "not an absolute path", no link was drawn for a path a transcript named, and no attachment resolved — all *silently*, because `false` is a perfectly ordinary answer. |

Each is now behind a seam answered by target: `js_sys::Date::now()` for the wall
clock (`crates/store/src/time.rs`), `session::path::is_absolute` — one definition
for the workspace, which is `Path::has_root` (`crates/session/src/path.rs`) — and
no browser-reachable `std::thread::spawn` at all.

**The rule for a future reader.** Before shipping any path a browser can reach,
grep it for the calls that lie on wasm:

```sh
rg -n 'std::thread::spawn|thread::Builder|SystemTime::now|Instant::now|is_absolute|std::fs|std::process::Command' <the path>
```

None of them fail at compile time. Each panics or silently misbehaves, and a
panic in a wasm module unwinds into whatever drove it — which, for anything on
the UI thread, is a window that simply never changes.

## Current state

The web target **builds, boots and drives a real session**. All 13 crates of the
frontend and its libraries compile for `wasm32-unknown-unknown`, `scripts/web.sh`
produces a bundle, and `scripts/web_check.sh` proves in a headless browser that
the page instantiates the module and mounts the shell's canvas. The full journey
has been driven by hand too — type a folder, open a session, the page draws,
send a prompt, the agent runs a turn, zero wasm panics (see [the whole
journey](#the-whole-journey-driven-by-hand)). What follows is what is genuinely
still missing.

### The build

All 13 crates that make up the frontend and its libraries are wasm-clean —
verified one at a time, each with

```sh
cargo +nightly check -p <crate> --target wasm32-unknown-unknown
```

returning 0 errors: `store`, `session`, `swarm_client`, `widgets`, `transcript`,
`agent_list`, `composer`, `settings`, `terminal`, `tab_engine`, `theme`,
`workspace`, `evo-webapp`. (The workspace's other three packages are not wasm
targets and are not meant to be: `evo-desktop` is the native binary, `evo-web` is
the server, and `proofs` is a test crate.)

The bundle itself is built by `scripts/web.sh`:

```sh
scripts/web.sh                    # → dist/web/
```

If a crate ever regresses, that per-crate command is the fastest way to find
which one — the build fails top-down, so the first crate in the list that fails
is the one to read.

### A tab spawns on the desktop and connects in the browser

`crates/workspace/src/launch.rs` has **one** `start`, with two bodies selected by
target, and both are wired:

```rust
// native — the app starts the process and holds its stdin
pub fn start(env: &LaunchEnv, launch: &Launch) -> std::io::Result<Started>

// wasm — the server owns the process; the page asks it to open one and attaches
#[cfg(target_family = "wasm")]
pub async fn start(env: &LaunchEnv, launch: &Launch) -> std::io::Result<Started>
```

The wasm body POSTs the launch to the server and attaches to what it opened:

```
client.fetch().post("/tabs", &open_request(launch))   →  { "id": … }
store::paths::TabId::parse(id)                        →  the id is parsed, not trusted
TabEngine::connect(client, folder)                    →  the same handle as a native tab
```

The prefix is `/api` and the route is `/tabs`, and the tab's client then talks to
`/api/tabs/<id>/…`. `crates/workspace/src/tab.rs` calls it (`Tab::launch`), so a
tab in the page is a tab the server opened, and the window says so while it
boots.

`tab_engine` mirrors the split with two constructors:

```rust
TabEngine::start(config: ServerConfig) -> (EngineHandle, Receiver<Update>)   // native
TabEngine::connect(client: Client, folder: PathBuf) -> (EngineHandle, Receiver<Update>)  // wasm
```

On the wasm path there is no `Update::Exited` at all — it is compiled out,
because there is no process of the page's to have exited and the stream ending is
the whole story — while `Update::Ready` is never sent, the server having been
ready before the page loaded.

**The session page is what made this visible.** `crates/workspace/src/tab_page.rs`
renders the lanes column, the transcript and the composer from `TabState::Running`
and from nothing else, and that state is set at the end of `apply(Update::Ready)`.
The browser used to fail *before* it: `apply` called `record_recent`, which called
`workspace::bridge::Bridge::spawn`, which is a plain `std::thread::spawn` — so the
panic unwound out of the match arm, `set_state(TabState::Running)` never ran, and
the window stayed on the empty tab while the session really ran behind it. A
browser that "opened a session and nothing happened" was that, and the fix was to
split `record_recent` and `on_media` by target so no browser-reachable path spawns
a thread.

### Host-only pieces, and where the seam is

The convention is a `host.rs` module per crate: the things a window cannot do
for itself because the machine it draws on is not the machine the work happens
on — open a system dialog, read a file, run a binary. Each is answered by target,
and the wasm answer is a **stub** that returns the same value a "no" would (a
cancelled dialog's `None`, a file that is not there's `None`), so no call site
needs a branch and no behaviour silently changes.

| what | native | browser |
|---|---|---|
| asking for a folder on the empty tab | `rfd`'s async panel, opened by the card's button (`host::pick_folder`) | **a panel, not a dialog**: an icon, the label `Folder to run in`, a text field (`folder-input`) and an `Open session` button (`folder-go`), with the hint `An absolute path on the machine that served this page.` (`crates/workspace/src/empty_tab.rs`, gated on `target_family = "wasm"`; `host::ENTERS_FOLDER` is what selects it). A typed path is checked for being rooted here and the rest is the server's to refuse. `host::pick_folder` is still a `None` stub — the browser simply does not call it for this. |
| reading a file the UI names | `std::fs` | **stub**: `Err` from `read_to_string`, `None`/`false` from the `transcript` and `composer` readers — nothing is drawn, and a path is not linked (`host.rs` in `workspace`, `transcript`, `composer`). `canonicalize` is *not* a stub: it returns the path as written, which is what the desktop's own fallback does for a path that cannot be resolved. |
| running an offline CLI (`catalog`, `sessions`, `check`) | a real child process | **stub**: `CliError::NotFound` — literally true, the binary is on the serving machine |
| the Settings file dialog | `rfd`'s async panel | **stub**: `None` (`crates/settings/src/dialog.rs`) |
| the binary probe behind a Settings row | `store::cli::version_within` — a child and two reader threads | **stub**: `Check::Missing`, and it never reaches `store::cli` (`crates/settings/src/probe.rs` is split by target). This one is not cosmetic: `std::thread::spawn` panics on wasm, so the ungated version took the whole Settings page down. |
| the terminal pane | a real pty and `alacritty_terminal`'s grid | **stub**: the same `TerminalPane` type, built the same way, saying a terminal is not available here (`crates/terminal/src/pane_wasm.rs`). `alacritty_terminal` cannot even be built for wasm32 — it reaches `polling`, `home` and `filedescriptor`, none of which support the target — so the grid goes with the shell it was there to draw. |
| the clock | `std::time::Instant` | `web_time::Instant` (`Performance.now()`) — `std::time::Instant::now` compiles to a `panic!` on wasm32, and every animated widget reads it (`crates/widgets/src/clock.rs`) |
| the wall clock | `std::time::SystemTime::now()` | `js_sys::Date::now()` (`crates/store/src/time.rs`) — `SystemTime::now` panics on wasm32, and `paths::fill_random` falls back to it when `/dev/urandom` cannot be opened, which is every page |
| whether a path is absolute | `Path::is_absolute` | `session::path::is_absolute`, which is `Path::has_root` (`crates/session/src/path.rs`) — `is_absolute` is **always false** on wasm32, so an ungated call silently refuses every path a person types |
| a thread | `std::thread::spawn` | **must not be called**: it panics on wasm32, and `wasm_thread` does not rescue a plain `std::thread::spawn` — it only offers its own `Builder`. Every browser-reachable spawn is behind a seam (`workspace::bridge`, `settings::probe`, `tab.rs`). |
| the HTTP transport | a blocking socket (`swarm_client::http`) | `fetch` and a `ReadableStream` (`swarm_client::fetch`) — a browser can do neither of the things the native half is made of: it cannot open a raw TCP connection, and it cannot block a thread on a read |
| syntax colouring in code fences | tree-sitter grammars | **off** — the kit's wasm stub, same fence, uncoloured |
| spawning a session | the app spawns `evo-swarm serve` itself and holds the pipe | **wired**: the page asks `evo-web` to open one (`POST /api/tabs`) and attaches — the server owns the process, and the browser owns neither a pipe nor a pid |
| single instance, window bounds, menu bar | the app owns them | **not applicable** — the browser owns the tab, the page owns the URL |

### Not implemented on the web side

Each of these was re-checked against the code, not carried forward.

- **The file-reading seams still answer "no".** No local file read and no offline
  CLI run from the page: a path a transcript names is not linked, and the
  choosers have no catalog from the *page*. (They do have one from the server —
  `GET /api/catalog` and `GET /api/sessions` exist — but nothing in the frontend
  asks for them yet.) This is narrower than it used to be: the folder card and
  the Settings page are both wired now, so it is the *reads*, not the page, that
  are missing.
- **No folder *browser*.** The empty tab takes a typed absolute path, and the
  server has no folder-listing endpoint — the complete route table is the one in
  [the API surface](#the-api-surface), and none of it lists a directory. So
  `POST /api/tabs` requires an **absolute path that exists on the serving
  machine** (`crates/web/src/tabs.rs`: `folder is required`, `must be an absolute
  path`, `is not a directory on the serving machine`), and a page cannot offer a
  picker that walks a filesystem it cannot see. A path has to be known.
- **One shared token, and nothing finer.** No users, no sessions, no per-tab
  authorization: `require_token` compares the configured token and that is the
  whole of it. The token is one secret for the whole server.
- **Tabs live only in the server's memory.** `Tabs` is a `BTreeMap` behind a
  mutex, so restarting `evo-web` forgets every tab it had open — though the
  sessions themselves stay on disk and are resumable through `/api/sessions`.
- **No TLS.** The server speaks plain HTTP and reads no forwarding headers, so if
  it must leave the host it goes behind something that terminates TLS.

## Related

- [`architecture.md`](architecture.md) — which crate owns what, and the rules
  that keep the shared crates shareable.
- [`usage.md`](usage.md) — driving the UI itself: tabs, the transcript, the
  composer, Settings. It describes behaviour both shells share.
- [`../README.md`](../README.md) — the desktop path and how to build it.
