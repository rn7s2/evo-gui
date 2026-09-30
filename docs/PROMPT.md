# evo-desktop — build prompt

**Mission.** Build `evo-desktop`: a native desktop GUI for the evo agent runtime. The app lives
in this repository (`~/coding/evo-gui`), is written in Rust with
[gpui-kit](https://gpui-kit.com/docs/), and is a *client*: evo already exposes an HTTP API for
exactly this, and the app must use it rather than re-implement anything.

**Architecture in one paragraph.** One window; its title bar is a browser-like tab strip. Each tab
is one `evo-swarm serve` process (a coordinator agent plus a pool of worker lanes) that this app
spawns in a chosen folder and drives over one loopback protocol: the child writes a **ready file**
(port, token, epoch) once it is listening, and the app reads a **snapshot**, follows one **stream**
of ops, and posts every action as an **op** — the whole of it is in `../evo-agent/docs/serve.md`,
and the binding contract between the two sides is `CONTRACT.md` in the workspace root (with the
design rationale in `evo-serve-redesign.html` beside it). Lanes are never addressed directly: they
are private children of the coordinator, and their topics arrive mirrored in the coordinator's own
stream. The app keeps its own data under `~/.evo/desktop/`.

---

## 1. Read first — the contract you implement against

| File | Why |
|---|---|
| `CONTRACT.md` (workspace root) | the binding contract: launch, the protocol, the view model, the swarm |
| `evo-serve-redesign.html` | why it is shaped that way (root causes, the removed surfaces, the plan) |
| `../evo-agent/docs/serve.md` | the protocol as the server side documents it |
| `../evo-agent/docs/swarm.md` | what a swarm is, and how its lanes are mirrored into topic `lane:N` |

gpui-kit docs are markdown: `curl -s https://gpui-kit.com/llms.txt` is the index, and every page is
`https://gpui-kit.com/<path>.md` (e.g. `/component/tabs.md`). Read at least `docs/installation`,
`docs/getting-started`, `docs/coding-guides`, `docs/entity`, `docs/context`, `docs/render`,
`docs/task`, `docs/window`, `docs/multi-window`, `component/title-bar`, `component/tabs`,
`component/select`, `component/text-view`, `component/message-scroller`, `component/textarea`,
`component/root`, `component/notification`. `cargo add gpui-kit` resolves 0.7.0 today; pin what you
use and read *that* version's docs (the site carries 0.6→0.7 migration notes).

## 2. Rules

1. **One instance.** A second launch must not open a second window: it activates the running
   instance's window and exits 0. Never leave a stale lock that blocks a later launch (the lock must
   die with the process).
2. **Spawn the binaries as-is.** No patched binaries, no wrapper scripts, no editing the swarm's
   files. Everything the UI shows comes from HTTP.
3. **Own only your own data.** `~/.evo/desktop/` is yours. Journals and swarm/lane directories
   belong to evo: ask evo for them (`sessions --json`, the `lane:N` topics) and never write them.
4. **Secrets stay in files.** The swarm's bearer token lives in the per-tab `ready.json` (0600),
   written by the server itself. Never log it, never put it in `app.json`, never show it.
5. **Don't invent a second protocol.** No polling loops where an event exists; no re-deriving
   session state from journals; no side channels to lanes.
6. **Never block the UI thread.** All I/O happens off the main thread; results land via weak-entity
   updates with a revision/id check so stale work is rejected.
7. **No raw sexprs and no raw JSON blobs in the UI.** Everything is rendered.
8. **Markdown is rendered live, while it streams.** Assistant text appears as *rendered* markdown
   from the first delta: headings, bold, lists, tables and code fences are formatted while the
   message is still growing — never raw markdown source, and never plain text that is formatted
   only once the run ends. Keep one retained markdown document per message
   (`TextViewState::markdown(source, cx)`, then `set_text(whole source so far)` per delta) so each
   update re-renders the blocks it changed; partially typed constructs (`**bo`, an open ``` fence)
   format as they complete — that is what the component does, and the requirement is to be wired so
   it can. Streaming must feel alive, not chunked-in: fade appended text in
   (`TextViewMotion::with_stream_fade`) and keep the growing row measured.

## 3. Starting a tab's swarm (exact)

A launch is the contract's §1 and §8: `store::launch::LaunchSpec` builds the argv
(`serve --ready-file <tab dir>/ready.json --watch-stdin --port 0 …`), the window
spawns it through `swarm_client` with stdin a pipe it holds, and **readiness is
the ready file the child writes** — port, token, epoch — not a poll. There is no
port picking, no token file, no `EVO_SERVE_WATCH_PID` and no kill ladder:
dropping the pipe is what stops the child, and EOF is what tells it the tab is
gone. `CONTRACT.md` §1/§8 and `../evo-agent/docs/serve.md` are the whole of it,
and nothing here repeats them.

Two rules that stay: `--workers` only when the user asks, and never
`--allow-remote`.

## 4. Endpoints you use

Three reads and one write, all of them CONTRACT.md §5 and
`../evo-agent/docs/serve.md`: `GET /snapshot` (one atomic read of every topic the
tab shows), `GET /stream` (one SSE stream carrying every topic's ops),
`GET /items` / `/items/<id>` / `/media/<id>/<n>` (paging back, one item whole,
image bytes) and `GET /catalog`. Every action is `POST /ops` with the envelope of
§5.5: the op list with its argument schemas and its error codes is the contract's,
and a running server prints the same table in `GET /catalog.ops`.

## 5. Events you consume

One stream, one vocabulary: the ops of CONTRACT.md §5.3 — `hello`, `item.add`,
`item.append`, `item.patch`, `item.remove`, `state.patch`, `topic.reset`,
`stream.reset`. What an item *means* comes from its `kind` field and the fields
beside it, never from its text (§4.1): `lane_report`, `lane_event`, `goal`,
`notice`, `command_note` and the rest are items a UI renders, not prose to parse.
A `topic.reset` means re-read that one topic's snapshot; a `stream.reset` means
re-read everything and reconnect with the cursor the server named.

## 6. App data

```
~/.evo/desktop/
  app.json           window bounds, tab set, binary paths, schema version
  lock               single-instance lock (flock; pid inside)
  activate.sock      a second launch knocks here so the first one can raise it
  model-cache.json   the last `catalog --json` body, for the empty tab's choosers
  tabs/<id>/         ready.json (written by the server, 0600), swarm.log, tab.json
```

The bearer token lives in the server's own ready file; the app never copies it
out of there.

## 7. UI spec

### 7.1 Window and chrome

- Opens huge: default 1600×1000 clamped to the display work area; minimum ~1000×700; bounds
  restored from `app.json`. Custom title bar (`TitleBar::window_options()`), because the title bar is
  the tab strip.
- Title bar = `TitleBar` containing a `TabBar`:
  - one `Tab` per swarm, label = folder name (tooltip: full path + swarm state), `suffix` = a close
    button, `prefix` on macOS left blank for the traffic lights;
  - `suffix` of the bar = an **Add icon button**, always present, exactly like a browser's `+`; it
    appends a new empty tab and selects it;
  - overflow scrolls (`track_scroll`); tab width capped with `max_width`.
- One window only. No dock/panel layout library needed for v1.

### 7.2 Empty tab (initial tab and every new tab)

```
┌───────────────────────────────────────────────────────────────┐
│ Coordinator model  [ Select ▾ ]                               │
│ Lanes model        [ Select ▾ ]         [ Select folder… ]    │  ← button spans
│ Workers            [ 6 ▾ ]                                    │    all three rows
├───────────────────────────────────────────────────────────────┤
│ History                                                       │
│  ~/coding/foo       4 lanes · 2h ago · coordinator: gpt-…     │
│  ~/coding/bar       6 lanes · yesterday                       │
│  …                                                            │
└───────────────────────────────────────────────────────────────┘
```

- Row 1: coordinator model chooser (`--model`); row 2: lanes model chooser; row 3: worker count.
  Each defaults to **Default** — the coordinator model is passed only when chosen, the lanes model
  only when chosen (§9.6), and the workers only when chosen (else evo's own `:swarm-workers`, else 6).
  A tab's models are fixed when it is created — the tab page has no model selector (§7.3, §14.7).
- To the right, spanning the three rows: **Select folder…** → native folder dialog (`rfd`) → on pick,
  the tab spawns its swarm in that folder and becomes a tab page (§7.3). Cancel leaves the empty tab.
- Below the separator: **history**, the resumable swarms (§9.5). Clicking a row opens it as a new
  tab, resuming that swarm (`--resume <session path>`, cwd = that folder). A resumed swarm keeps the
  lane count its own record kept — show that count read-only on the row; a worker count chosen here
  applies to folder-created swarms only.
- The empty tab is a real tab: it can be closed, and the app always keeps at least one tab (closing
  the last one leaves a fresh empty tab rather than an empty window).

### 7.3 Tab page

```
┌────────────┬───────────────────────────────────┬──────────────────┐
│ ● main     │  transcript (markdown, rendered   │ ┌──────────────┐ │
│ ◐ Lane 1   │   live as it streams)             │ │ input        │ │
│ ○ Lane 2   │                                   │ └──────────────┘ │
│ ✗ Lane 3   │                                   │ model · max ·    │
│            │                                   │ ctx 48k/936k ·   │
│            ├───────────────────────────────────┤ 97% cached ·     │
│            │ ☑ todos of the selected agent     │ goal a1b2 (…)    │
│            │                                   │    [  Send  ]    │
└────────────┴───────────────────────────────────┴──────────────────┘
```

- **Left** (~260 px): the agent list — `main` (the coordinator) first, then one row per lane. Each
  lane row carries a **status icon**: `●` working, `◐` compacting, `○` idle, `◌` starting, `✗` down
  (a colored dot + tooltip is fine; keep the meaning). Show the current task (truncated) and the
  step clock when working. Driven by the `swarm` topic's `lanes[]`, published on every transition
  (a lane coming up idle included). Selection highlights and drives the center column.
- **Center**: the selected agent's items — the coordinator's `session` topic, or that lane's
  `lane:N` topic. Assistant text is **rendered markdown that stays rendered while it
  streams**: headings, lists, tables and code fences are formatted as deltas arrive, so a
  half-finished message already reads as the finished one will (§2.8) — not raw source during the
  stream and not "format it when it is done". Mechanics: one `TextViewState::markdown` per message,
  extended with `set_text(source so far)` on each `text-delta`, mounted in a tail-following
  `MessageScroller` (`TextView::new(&state).motion(TextViewMotion::default().with_stream_fade(...))`)
  that remeasures the growing row. Lifecycle: a message's document is created on `message-start`,
  extended per delta, and finalised when the item's `status` says so; rows re-read from a topic
  snapshot after a `topic.reset` (§9.1) render their stored markdown source the same way. Tool items,
  results and `notice` items are plain text or fixed-format rows, not markdown documents.
  Rows: a `user` item; an `assistant` as markdown; a `tool` as a collapsed one-liner
  (`name — ok/error`, expandable to the whole result, `/items/<id>` when the snapshot truncated it);
  a lane's `lane_report` as a distinct report row; `notice`, `lane_event`, `run_outcome` and
  `command_note` items as their own lines. Auto-follow the tail while the reader is at the bottom; show a jump
  affordance when they are not.
- **Center bottom**: the **selected agent's todo list**, above the fold of the input area, hidden
  when that agent has none. Both come from the selected agent's topic state — `state.todos` of
  `session`, or of that lane — seeded by the snapshot and kept current by `state.patch`.
  Each item: status glyph (`☑`/`◐`/`☐`) + text, compact, clickable only if you add interactions.
- **Right** (~360 px): the coordinator's composer — the **Input** (plain multiline text editor for
  now — `Textarea` + `TextareaState`, auto-grow 2→8 rows; Enter sends, Shift+Enter is a newline) and
  **one status row beneath it: the session readout on the left, the action button on the right** —
  same line, text flush left, button flush right, one line high. When the readout is wider than the
  row, truncate it with an ellipsis and carry the whole line in a tooltip; never let it wrap the
  button onto a line of its own.
  The readout **renders the topic's `segments`**, in the order the server publishes them. A core
  registry (`evo:define-status-segment`) builds that list, so the TUI's status line and this row
  cannot drift apart, and an extension's own segment (cache-stats, say) reaches both the same way:
  the app computes none of it, and walks no journal.

  | Segment | Shown as | From |
  |---|---|---|
  | model | the id, or `id (provider)` when that id is registered under more than one provider | `state.model` |
  | thinking | the effort level, lower-cased (`low` … `max`) | `state.thinking` |
  | context | `ctx 48k/936k (5%)` — the server's own numbers and units | `state.context` |
  | goal | `goal <id> (<status>) <tokens>[/<budget>]` | `state.goal` |

  Each segment carries its own `text`, `order` and `side`; render them as they are, in the theme's
  muted color, left segments flush left and right segments flush right, and leave out a segment the
  server does not publish. Nothing is re-anchored from `usage` on this side: `state.context` already
  says whether its number is a usage figure or an estimate.

  The **button** shares that row, and its face and function follow the coordinator's status — never a
  Send and a Stop side by side:
  - status `idle` (also before the first run, and after a failed one): the button reads **Send**
    (primary), enabled only when the input has text; clicking posts `input.send`.
  - status `running` or `compacting` (from the topic's `state.status`): the *same* button reads
    **Stop** (secondary, square glyph) and clicking posts `run.interrupt` — it interrupts and
    **leaves the draft untouched**; it never sends. A coordinator that is only `waiting` on its lanes
    gets scope `swarm`, because that is what that face means there.
  - `Esc` is a second route to that same interrupt. Enter, in both states, keeps the TUI's meaning:
    it sends (`input.send` lands at the running turn's next boundary, as a `user` item whose status
    is `queued`), so text can be queued while the agent works — the button's face always says what
    the button does.
  There is **no model selector in a tab** (§14.7): the model is chosen when the tab is created, or
  inherited from the resumed journal, and the readout above shows it; nothing in the tab changes it.
- Input and button always target the **coordinator**; selecting a lane changes only the center
  column (lane control belongs to the coordinator, §D21).

## 8. Act, don't reimplement

Nothing in evo's behavior is duplicated in Rust: an op that refuses answers
`{"ok":false,"error":{"code","message"}}` and the code is what the UI branches on,
a model that cannot be registered is a `problem` line from `check --json`, and an
unknown command is `unknown_op`. None of it is re-validated locally.

## 9. Frontend behavior

1. **Transcript assembly.** On tab open: one `GET /snapshot` for `session`,
   `swarm` and `lane:*`, then one `GET /stream` with the same topics, from the
   cursor the snapshot answered with. The tab keeps its own mirror of each topic
   (`session::TabModel`) and applies every op to it: `item.add` inserts by `after`,
   `item.append` grows a field, `item.patch` merge-patches, `item.remove` takes a
   row back (a cancelled queued input). Older items are paged in with
   `GET /items?before=`; the model's own context is `GET /debug/context`, which is
   never rendered.
2. **Sending, and the one button.** Enter posts `input.send {text, queue}`: idle →
   the run starts, running → the input is queued and arrives as a `user` item whose
   `status` is `queued` until evo drains it. Clear the input only once the reply
   says `ok`; a queued row can be taken back with `input.cancel`. The button's face
   tracks the topic's `status` (Send when idle, Stop while running or compacting)
   and a click never does something other than what that face says: Stop posts
   `run.interrupt`, with scope `swarm` when the coordinator is only `waiting` on
   its lanes.
3. **Lanes.** One stream carries them: `lane:*` is subscribed with the session, so
   every lane's state and items are already in the mirror. There is no per-lane
   subscription, no lane port, no lane token, and no relay to ask for.
4. **Model catalog** (for the empty tab's choosers, before any server exists):
   `evo-swarm catalog --json` — one process printing one document — at startup and
   again when Settings changes the binaries; the body is kept in
   `model-cache.json`. The **lanes** chooser offers only the models `/catalog.lanes`
   reports `ok`, with evo's own `reason` beside the ones it does not; the lines
   under the choosers are `evo-swarm check --json`'s `problems[]`, re-run
   (debounced) on every chooser change. Never hardcode model names, and never start
   a server to learn the catalog.
5. **History** (a background thread, never blocking the UI): `evo-agent
   sessions --json` — one process, one document, no journal is read or parsed
   here — merged with the app's own recents from `app.json`, deduped by session
   path, newest first. A row's title, folder, when, lane count and models come from
   that; `session::HistoryRow` is what it looks like.
6. **Lane configuration is launch flags.** `--lane-model ID@PROVIDER`,
   `--lane-thinking L` and `--workers N` are what the choosers produce, and the
   swarm records them in its own record, so a resume restores them. The app never
   writes a project file: `<folder>/.evo/swarm.lisp` stays the user's.
7. **Failure surfaces.** A boot failure is the reason plus the tail of
   `swarm.log` in the tab with a Retry button (the client's `BootFailure`). A lane
   that goes down and comes back is a `lane_event` item in its own topic and in the
   swarm's lane list. A stream that is cut is `stream.reset`: re-snapshot and
   reconnect, with a "reconnecting" badge while the client backs off. None of these
   is inferred from a sentence in a transcript.
8. **Quit.** `app.json` first, then `server.shutdown` and the pipe dropped (§3):
   the tabs' servers see EOF and stop, so a crash of the app leaves nothing
   running and there is nothing to wait for.

## 10. `../evo-agent` is read-only

Do not modify evo. Everything this app needs is done in the client through the documented API: the
lanes model goes through `--lane-model` (§9.6), lane state and todos come from that lane's topic, and
resume/reset are the `session.*` ops. If you
conclude something is genuinely missing from the API, stop and report it with the evidence rather
than patching evo — a local patch would silently diverge the GUI from the binaries it ships against.

## 11. Stack and code conventions

- Rust, `gpui-kit` (pin 0.7.x), `serde`/`serde_json`, `rfd` for the folder dialog, an
  HTTP/SSE stack of your choice (`reqwest` + `tokio` in a runtime you own, or a blocking client on
  a dedicated thread), a lock-file single-instance guard. Verify in M0 that the streaming path
  keeps the UI responsive — GPUI's executor is not tokio's, so bridge explicitly and prove it with
  two tabs streaming at once.
- **Verify before building on it**: whether this `gpui-pre` snapshot exposes `cx.http_client()`; if
  it does not (it is feature-gated upstream), say so and use your own client. Do not build on an
  unverified threading assumption.
- Follow gpui-kit's *Coding Guides*: feature crates by capability (e.g. `app` shell + `swarm_client`
  + `workspace` + `transcript` + `composer` + `settings`), `Entity<T>` for retained state,
  `RenderOnce` for value-like pieces, stable domain ids for repeated rows, theme tokens instead of
  literal colors, no state feedback loops, weak entities in async work.
- Tests: `cargo test` for the client layer against a **real** `evo-swarm serve`: a temp `HOME` whose
  `init.lisp` registers the stub provider, `evo-agent/tests/stub-messages.py` as the scripted model
  (`EVO_STUB_MESSAGES`), and the built binaries through `EVO_SWARM_BIN` / `EVO_AGENT_BIN`
  (`scripts/stub_home.sh` is the manual half of the same thing). The real-binary proofs live in
  `crates/proofs`; the UI crates keep gpui-kit `TestAppContext` tests for the tab strip, the empty
  tab, transcript row building, and the todo panel.

## 12. Milestones (each ends with proof, not with intent)

- **M0 — spike.** Window with a custom title bar; spawn `evo-swarm serve` in a folder; one snapshot
  and one stream into a plain view; `input.send` sends and the item grows. *Proof: screenshot +
  a transcript of a real run against the installed `/usr/local/bin/evo-swarm`.*
- **M1 — one tab, real.** Tab page layout (list / transcript / todos / input), markdown rendered
  live while streaming (§2.8), tail following, the Send/Stop button, re-read on `topic.reset`. *Proof: a run where a lane is
  delegated work and the coordinator's transcript renders it.*
- **M2 — tabs and the empty page.** Tab strip with Add/Close/Select, tab persistence, empty tab with
  the choosers + folder button + history list, resume a swarm from history. *Proof: close the
  app with three tabs, relaunch, resume one from history, everything still renders.*
- **M3 — lanes.** Lane list with status icons, lane items, lane todos, the `swarm` topic's live
  lane updates, lane failure states. *Proof: kill a lane's process and watch the app report it down and
  then up (the swarm restarts it).*
- **M4 — hardening.** Single instance + activation, crash/restart of the coordinator, log tails on
  boot failure, `~/.evo/desktop` state, quit shutting everything down, `cargo test` green,
  `cargo build --release` and a bundle you can launch from Finder.

## 13. Non-goals (v1)

Rich-text composer, image paste, runtime model switching and any command surface, lane control endpoints, remote/non-loopback servers, TLS, multiple
windows, Windows/Linux packaging, an embedded browser, editing evo's journals, a settings UI beyond
binary paths and theme.

## 14. Settled decisions

1. **Lanes model** is a launch flag, `--lane-model ID@PROVIDER` (§9.6) — recorded in the swarm's own
   record and restored on resume. Nothing in the app writes `<folder>/.evo/swarm.lisp`.
2. **Workers** are chosen in the empty tab (row 3, 1–64, default = evo's own). The count is passed
   for a folder-created swarm only: a resumed swarm keeps the lane count its record kept.
3. **History** lists every resumable swarm on disk (any folder), merged with the app's own recents —
   not only tabs this app created.
4. **Input and its button** always target the coordinator; lanes are watched, never typed to.
5. **Closing a tab** shuts its swarm down (`server.shutdown`, then the pipe), and the session stays
   on disk and reappears in history.
6. The app never auto-starts swarms on launch: it opens one empty tab.
7. **No model selector in a tab, and one action button.** The model is chosen when the tab is created
   (or inherited from the resumed journal) and shown read-only; the composer's single button is Send
   when the coordinator is idle and Stop while it runs, and Enter always sends through `input.send`.
