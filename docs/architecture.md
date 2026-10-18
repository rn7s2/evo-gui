# evo-desktop architecture (crate ownership)

The contract is [`CONTRACT.md`](../CONTRACT.md) at the workspace root: launch and
lifecycle (§1), the offline CLIs (§2), the view model (§4), the HTTP protocol
(§5), and the swarm's internals (§6). The design rationale is
[`evo-serve-redesign.html`](../evo-serve-redesign.html). `docs/PROMPT.md` is the
build prompt; references like §9.4 in the code point at it.

The tree has two delivery paths — the native app and the server + browser system
— over this one set of crates. [`web.md`](web.md) covers the second: what each
shell owns, the architecture, the toolchain the wasm build needs and why, and
what is still missing.

| crate | kind | owns |
|---|---|---|
| `swarm_client` | pure Rust, no gpui | one `serve` process: the spawn (`store::launch`'s argv, the child's stdin pipe, the ready file), the client (snapshot, one stream, `POST /ops` with `rid`), the ready-file/op/frame types, the redaction every error and log tail goes through, and a fake server for this crate's own tests. Its **transport is split by target**: `transport` holds the types and the two shapes a request can take, `http` is the native half (blocking HTTP/1.1, the SSE reader and its backoff) and `fetch` the wasm one. The spawn, the process-group reaping and the blocking client are native-only — a browser has none of them. |
| `store` | pure Rust | everything on disk: `~/.evo/desktop/` (app.json, the single-instance lock and its activation socket, `model-cache.json`, `tabs/<id>/`), and the readers of the offline CLIs (`cli`, `catalog`, `history`, `launch` — the argv of either program, `check`, ready-file paths). |
| `tab_engine` | pure Rust, no gpui | one tab's I/O, off the UI thread: spawn → snapshot → one stream → ops. Commands in, updates out over an `async-channel`; the UI folds the updates into `session`. **Split by target**: a host gets `TabEngine::start(config)`, which spawns and owns the child's stdin, and a browser gets `TabEngine::connect(client, folder)`, which attaches to a tab the server already runs — the same handle and the same `Update`s, except that `Exited` is compiled out (a page has no process of its own) and `Ready` is never sent (the server was ready before the page loaded). |
| `session` | pure Rust | the tab's view model: a mirror of the server's topics (`TabModel`: items, topic states, the lane list, the selection), the empty tab's `Launcher` (the four choosers, the history rows, the check's problems), and `OpRequest`/`OpSink` — the UI's actions as the ops of §5.5, handed to the transport rather than sent from here. |
| `widgets` | gpui | the pieces more than one surface draws, from the design: the breathing dot a working agent wears, the effort slider, the chip. `store::design` owns their numbers and colours. |
| `transcript` | gpui | the reading column: one agent's items, rendered markdown that stays rendered while it streams, tool/notice/lane rows, the whole journal in a virtual list with older pages loading themselves, the thinking reveal. |
| `agent_list` | gpui | the lanes column: its band (`Lanes`, and how many are busy), then `main` first and one row per lane — breathing dot, task, state or step clock, selection, reconnecting badge, the lane's own Stop. |
| `composer` | gpui | the box at the foot of the conversation: the todo strip across its top, the drawers a chip folds out inside it, the input (2 rows to half the pane, Enter/Shift+Enter/Esc), and the foot row — the agent's segments as chips, and the one Send/Stop button. |
| `workspace` | gpui | the window: title bar and tab strip, the empty tab (choosers, the folder card, history, the check's problems), the tab page's two columns and the split between them, and the tab lifecycle that wires `tab_engine` to `session`. Its host seam (`host.rs`) is the folder card — `rfd`'s dialog on the desktop, a typed path in a browser (`ENTERS_FOLDER`) — plus reading a file and running an offline CLI. Its launch path has two bodies under one name: `launch::start` spawns on native and POSTs `/api/tabs` to the server and connects on wasm. |
| `settings` | gpui | the Settings panel: the two binaries and the theme, and nothing else. |
| `theme` | gpui | the design's palette and light-or-dark: `install` (the palettes, as the component theme's light and dark themes), `apply` (which mode a window gets), `mode_for`/`follow_appearance` (the decision, pure). A crate of its own because it belongs to neither shell — `crates/app` reaches it through `follow_appearance` and `crates/webapp` through `install`/`apply`, and a theme installed by only one is a window that looks different in the other. |
| `terminal` | gpui | the terminal pane: a pty and the user's shell on it, `alacritty_terminal`'s grid parsing what the shell prints, and the pane that draws it. The pty and the grid are a desktop's; the wasm build keeps the pane's surface and says a terminal is not available here. |
| `webapp` (`crates/webapp`) | wasm cdylib, gpui | the browser shell: the `#[wasm_bindgen] start` the page calls, the panic hook and logging into the browser console, the palette and the fonts and the page's own origin for icons, one window on the shared `WorkspaceView`, and `transport` — the naming of the two transports. Deliberately short: a page has no single instance to take, no bounds to remember and no child to spawn. The launch path itself is `workspace`'s. |
| `web` (`crates/web`) | bin, axum | the browser UI's host and the thing that drives evo for it: one `evo-web` process serving `dist/` and answering `/api/…`, the tabs it opens (each one `serve` process, over `store::launch`'s argv and `swarm_client`'s spawn), the proxy that adds each tab's bearer token server-side so it never reaches the page, and the two cross-origin-isolation headers the page cannot work without. |
| `proofs` | tests | the real-binary proofs: one per `tests/tNN_*.rs`, a hermetic stub home, real servers. `docs/proofs.md`. |
| `evo-desktop` (`crates/app`) | bin | the shell: single instance, the window and its remembered bounds, the launch-time loads (catalog, sessions), quit, the menu bar, About, and Settings. |

Rules:

- The pure crates (`swarm_client`, `store`, `tab_engine`, `session`) must not depend on gpui.
- The UI crates (`widgets`, `transcript`, `agent_list`, `composer`, `workspace`,
  `settings`) must stay **host-free**: no pty, no process spawn, no native file
  dialog, no ambient clock, no direct read of the machine they run on. Each such
  thing sits behind a seam a target answers its own way — a `host.rs` per crate,
  plus `terminal`'s pane, `settings`'s `dialog` and `widgets`'s `clock` — so the
  same UI code compiles for the desktop and for the browser. The wasm answer is a
  stub that returns the same value a "no" would (a cancelled dialog's `None`, a
  file that is not there's `Err`), so no call site needs a branch. This is what
  makes `webapp` a shell rather than a fork, and it is the rule to reach for
  before adding a dependency to any of them.
- `swarm_client` carries the transport seam instead: `http` for a native build
  and `fetch` for a wasm one, sharing `transport`'s types. They are deliberately
  not one trait — a browser cannot block, so the wasm half is `async` where the
  native half is not.
- `theme` is neither shell's, and both must install it: the palettes are shared so
  the two deliveries render identically. `crates/app`'s own `theme.rs` moved here
  for exactly that reason.
- `webapp` may depend on the UI crates; nothing they depend on may depend on
  `webapp`, and neither shell may be depended on by the UI.
- The UI thread never blocks: I/O happens on the crates' own threads, and results reach
  the UI through `workspace::Bridge` — a channel plus a task that applies them through
  `WeakEntity::update` with a revision check, so stale work is dropped.
- The app reads no journal and writes no project file. What it shows comes from the
  offline CLIs and from the running server (CONTRACT §2, §9).

## Commit convention

Conventional commits: `type(scope): subject` — type ∈ feat, fix, refactor, test, docs, chore, perf, build;
scope = crate name (`swarm_client`, `store`, `session`, `tab_engine`, `transcript`, `composer`, `workspace`,
`theme`, `webapp`, `web`, `app`) or `repo`/`scripts`/`docs`. Subject imperative, lower-case, no trailing period, ≤ 72 chars. Then a blank
line and a body (wrapped at ~72) that says what changed and why — behaviour, notable decisions, how it was
verified; a title-only message is not acceptable. Every message ends with the trailer
`Co-authored-by: EvoAgent <evo@ruiqilei.com>`.

## Verified platform facts

- **`cx.http_client()`**: gpui-pre 0.3.7 exposes `App::http_client()`, but every non-test app built
  through `gpui_kit::application()` gets a `NullHttpClient` whose `send` fails, and test contexts get a
  `FakeHttpClient`. Nothing in gpui-kit installs a real client, so all I/O uses `swarm_client`'s own
  blocking HTTP/1.1 client on std threads.
- **Threading bridge**: GPUI's executor is not tokio's. Worker threads send over `async-channel`; a
  `cx.spawn` task awaits and applies results through `WeakEntity::update` with a revision check
  (`crates/workspace/src/bridge.rs`; its tests prove the cross-thread transfer and the stale-drop).
- **Window geometry**: `app.json`'s bounds are a request, clamped to the display's work area
  (`crates/app/src/bounds.rs`).

## Layout decisions

- The status line is the composer's foot row: one `Chip` per `segment`, in the order the
  server publishes them, so the same session reads the same here and in the TUI
  (`crates/composer`). The app composes none of it.
- The agent column is resizable by dragging the split (180–480 px, starting at 260), and
  its width is shared by every tab and kept in `app.json`; the conversation keeps 360 px.
  A double-click on the split puts it back.
- The transcript is `gpui::list` over the whole record (`crates/transcript/src/lib.rs`): only the
  rows the pane can reach are built, their heights live in the list's own sum tree, and every op
  splices the list where the row belongs rather than rebuilding it. Older pages are fetched by the
  view itself while its owner says the topic has more. The tab page embeds the view **cached**, so a
  notification elsewhere in the window (a lane's breathing dot asks for a frame every frame) does not
  re-render and re-lay-out the journal.
  Two limits come with gpui's list, both of which the reader can feel but not lose data over: the
  scrollbar's thumb is computed from the heights measured so far, so it is approximate until the rows
  have been reached once; and a single very large scroll from far away lands at the end of the
  *measured* region rather than at the record's foot, which the next scroll (or the pin, once the
  reader is at the measured foot) closes.
