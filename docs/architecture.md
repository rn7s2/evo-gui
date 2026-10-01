# evo-desktop architecture (crate ownership)

The contract is [`CONTRACT.md`](../CONTRACT.md) at the workspace root: launch and
lifecycle (§1), the offline CLIs (§2), the view model (§4), the HTTP protocol
(§5), and the swarm's internals (§6). The design rationale is
[`evo-serve-redesign.html`](../evo-serve-redesign.html). `docs/PROMPT.md` is the
build prompt; references like §9.4 in the code point at it.

| crate | kind | owns |
|---|---|---|
| `swarm_client` | pure Rust, no gpui | one `serve` process: the spawn (`store::launch`'s argv, the child's stdin pipe, the ready file), the client (snapshot, one stream, `POST /ops` with `rid`), the transport (blocking HTTP/1.1, the SSE reader and its backoff, the redaction every error and log tail goes through), the ready-file/op/frame types, and a fake server for this crate's own tests. |
| `store` | pure Rust | everything on disk: `~/.evo/desktop/` (app.json, the single-instance lock and its activation socket, `model-cache.json`, `tabs/<id>/`), and the readers of the offline CLIs (`cli`, `catalog`, `history`, `launch` — the argv of either program, `check`, ready-file paths). |
| `tab_engine` | pure Rust, no gpui | one tab's I/O, off the UI thread: spawn → snapshot → one stream → ops. Commands in, updates out over an `async-channel`; the UI folds the updates into `session`. |
| `session` | pure Rust | the tab's view model: a mirror of the server's topics (`TabModel`: items, topic states, the lane list, the selection), the empty tab's `Launcher` (the four choosers, the history rows, the check's problems), and `OpRequest`/`OpSink` — the UI's actions as the ops of §5.5, handed to the transport rather than sent from here. |
| `widgets` | gpui | the pieces more than one surface draws, from the design: the breathing dot a working agent wears, the effort slider, the chip. `store::design` owns their numbers and colours. |
| `transcript` | gpui | the reading column: one agent's items, rendered markdown that stays rendered while it streams, tool/notice/lane rows, the whole journal in a virtual list with older pages loading themselves, the thinking reveal. |
| `agent_list` | gpui | the lanes column: its band (`Lanes`, and how many are busy), then `main` first and one row per lane — breathing dot, task, state or step clock, selection, reconnecting badge, the lane's own Stop. |
| `composer` | gpui | the box at the foot of the conversation: the todo strip across its top, the drawers a chip folds out inside it, the input (2 rows to half the pane, Enter/Shift+Enter/Esc), and the foot row — the agent's segments as chips, and the one Send/Stop button. |
| `workspace` | gpui | the window: title bar and tab strip, the empty tab (choosers, folder button, history, the check's problems), the tab page's two columns and the split between them, and the tab lifecycle that wires `tab_engine` to `session`. |
| `settings` | gpui | the Settings panel: the two binaries and the theme, and nothing else. |
| `proofs` | tests | the real-binary proofs: one per `tests/tNN_*.rs`, a hermetic stub home, real servers. `docs/proofs.md`. |
| `evo-desktop` (`crates/app`) | bin | the shell: single instance, the window and its remembered bounds, the launch-time loads (catalog, sessions), quit, the menu bar, About, and Settings. |

Rules:

- The pure crates (`swarm_client`, `store`, `tab_engine`, `session`) must not depend on gpui.
- The UI thread never blocks: I/O happens on the crates' own threads, and results reach
  the UI through `workspace::Bridge` — a channel plus a task that applies them through
  `WeakEntity::update` with a revision check, so stale work is dropped.
- The app reads no journal and writes no project file. What it shows comes from the
  offline CLIs and from the running server (CONTRACT §2, §9).

## Commit convention

Conventional commits: `type(scope): subject` — type ∈ feat, fix, refactor, test, docs, chore, perf, build;
scope = crate name (`swarm_client`, `store`, `session`, `tab_engine`, `transcript`, `composer`, `workspace`,
`app`) or `repo`/`scripts`/`docs`. Subject imperative, lower-case, no trailing period, ≤ 72 chars. Then a blank
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
  its width is shared by every tab and kept in `app.json`; the conversation keeps 420 px.
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
