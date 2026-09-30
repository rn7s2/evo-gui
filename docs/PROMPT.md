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

Sibling repo `../evo-agent` = `/Users/bytedance/coding/evo-agent`:

| File | Why |
|---|---|
| `docs/serve.md` | the whole HTTP protocol: endpoints, statuses, SSE, the POST reply envelope |
| `docs/swarm.md` | what a swarm is; *Serving the swarm* = the lane endpoints this GUI needs |
| `tests/swarm-serve-e2e.py` | a working client of exactly this API — the reference client implementation |
| `swarm/routes.lisp`, `swarm/api.lisp` | exact `GET /lanes` and `lane-state` payloads |
| `src/serve/routes.lisp` | command list, `/state` fields, SSE details, status codes |
| `design.md` §D21 | why the API is shaped this way (identity + routes; lanes read-only) |

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
3. **Own only your own data.** `~/.evo/desktop/` is yours. Journals (`~/.evo/sessions/`), swarm and
   lane directories (`~/.evo/swarm/<id>/lane-N/`) belong to evo: read them only for the history scan
   (§9.5), never write them.
4. **Secrets stay in files.** The swarm's bearer token lives in the per-tab `token` file (0600),
   created by the server itself via `--token-file`. Never log it, never put it in `app.json`, never
   show it.
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

```
cwd:              the chosen folder
argv:             <evo-swarm> serve --port <free port> --token-file <tab dir>/token
                  [--workers <n>] [--model <coordinator model>]
                  [--resume <coordinator session path>]
env:              EVO_SERVE_WATCH_PID=<this app's pid>   ← the swarm and its lanes shut down if we die
                  (do not set EVO_SERVE_TOKEN: learning the token from the file is the contract)
stdout + stderr:  → <tab dir>/swarm.log   ← this log is what you show when boot fails
```

- Pick the port yourself (bind `127.0.0.1:0`, read it, close) and pass it; also accept the port the
  server prints if you ever use `--port 0`.
- `--workers` only when the user asks; otherwise evo's own default applies. Never `--allow-remote`.
- Ready = token file non-empty **and** `GET /health` → 200 with `name == "evo-swarm"` and
  `"swarm"` in `features`. Poll every 100 ms, deadline 90 s. Detect early exit at the same time; on
  failure or timeout, show the last ~40 lines of `swarm.log` in the tab.
- Shutdown (tab close, app quit, or boot failure): `POST /shutdown`, wait ≤10 s for exit, then
  `SIGTERM`, 5 s, then `SIGKILL`. Never kill first. Shut every tab down in parallel on quit.
- The swarm supervises its coordinator: a crash restarts it, and **the restarted server is a new
  event log whose ids start again at 1**, announced by a `hello` event with the new pid. The app must
  notice (id regression or `hello`), refetch state, and keep the view — not die, not duplicate rows.

## 4. Endpoints you use

All requests: `Authorization: Bearer <token>`. One request per connection (`Connection: close`);
HTTP/1.1, loopback only. POST replies are one envelope:
`{"ok","status","error","output":[{"style","text"}],"data","choices","cursor","task"}`.

Coordinator:

| Endpoint | Use |
|---|---|
| `GET /health` | readiness; `cursor` is the SSE bootstrap id |
| `GET /state` | status (`idle\|running\|compacting`), `task`, `turn`, `model`, `provider`, `model_ready`, `thinking`, `context_tokens`, `context_window`, `goal`, `todos[]`, `session`, `session_id`, `leaf` |
| `GET /transcript?limit=N` | the folded context the next turn sends: `{"messages":[…]}` (image base64 elided) |
| `GET /registry` | `models[]` (`id`, `provider`, `api`, `context-window`, `vision`, `effort`), `providers`, `tools`, `commands`, `languages`, `settings` |
| `GET /events` | SSE: every kernel + serve + swarm event (below) |
| `POST /prompt {"text"}` | the user's turn. Lands at the running turn's next boundary, exactly like typing in the TUI |
| `POST /interrupt` | the TUI's esc |
| `POST /command {"text":"/goal …"}` | any slash command, resolved exactly as the TUI does. **Not used in v1** — the app ships no command surface; it is here for a later one |
| `POST /shutdown` | stop the session (lanes included) |

Swarm feature (`features: ["swarm"]` on `/health`):

| Endpoint | Use |
|---|---|
| `GET /lanes` | `{"swarm":{id,dir,cwd,workers,busy,stopping}, "lanes":[{n,state,task,task-age,step-age,pid,worktree,branch,restarts,reports,goal}]}` |
| `GET /lanes/N/transcript?limit=N` | that lane's messages, relayed |
| `GET /lanes/N/events` | that lane's live SSE stream (ids are the lane's own, resumable with `?since=`/`Last-Event-ID`) |

Statuses: `400` bad argument, `401` bad token, `404` unknown, `409` **not now** (busy — a dim notice,
never a modal), `422` the command ran and failed (show `error`), `503` shutting down.

## 5. Events you consume

| Event | Fields | Where it goes |
|---|---|---|
| `run-start`, `turn-start` | `run_id`, `turn` | activity state, step clock |
| `message-start`, `text-delta` | `text` | append to the current assistant message, which re-renders as formatted markdown on every delta (§2.8) |
| `thinking-delta` | `text` | hidden by default; a per-tab toggle reveals it as dim text |
| `tool-call-start` | `name`, `id`, `arguments`, `arguments-json` | a new tool row |
| `tool-result` | `name`, `id`, `is-error`, `content`, `content-chars` | completes that tool row |
| `message-end` | `stop-reason`, `usage`, `error` | closes the assistant row; `usage` also feeds the `cached` segment (§7.3) |
| `run-end` | `outcome` | run state |
| `steering`, `user-input` | `text` | a user row |
| `compaction-start/end`, `provider-retry` | — | dim status rows |
| `todo-changed` | `todos[]` | the todo panel, for the agent that emitted it |
| `task-start`/`task-end`, `settled` | `outcome`, `goal` | idle/running; `settled` = resync point |
| `output` | `style`, `text` | dim/notice/error line in that agent's transcript |
| `session-switched` | `session` | refetch everything |
| `lane-state` | `lane`, `state`, `task`, `goal`, `restarts`, `pid` | the left lane list (coordinator stream only) |
| `report` | `done`, `evidence`, `next`, `blocked`, `requests`, `goal` | a report row (lane streams; the coordinator sees reports as input) |
| `hello`, `ready`, `shutdown`, `bye`, `gap` | — | lifecycle; `gap` = refetch state + transcript |
| `unprintable-event` | — | ignore (log only) |

SSE rules: ids are consecutive per server process; resume with `Last-Event-ID`/`?since=`; a `:`
comment every 15 s is a keepalive. One stream per server (coordinator) and one per watched lane;
never reopen a stream you already hold.

## 6. App data

```
~/.evo/desktop/
  app.json           window bounds, tab set, binary paths, schema version
  lock               single-instance lock (flock; pid inside)
  model-cache.json   last /registry snapshot, for the empty tab's choosers
  probe/             scratch cwd used only to learn the model catalog (§9.4)
  tabs/<uuid>/       token (0600), swarm.log, tab.json {folder, session, swarm_id, models, workers}
```

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
  lane count its journal records — show that count read-only on the row; a worker count chosen here
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
  step clock when working. Driven by `GET /lanes` + `lane-state` events. Selection highlights and
  drives the center column.
- **Center**: the selected agent's transcript — coordinator `/transcript`, lane
  `/lanes/N/transcript`. Assistant text is **rendered markdown that stays rendered while it
  streams**: headings, lists, tables and code fences are formatted as deltas arrive, so a
  half-finished message already reads as the finished one will (§2.8) — not raw source during the
  stream and not "format it when it is done". Mechanics: one `TextViewState::markdown` per message,
  extended with `set_text(source so far)` on each `text-delta`, mounted in a tail-following
  `MessageScroller` (`TextView::new(&state).motion(TextViewMotion::default().with_stream_fade(...))`)
  that remeasures the growing row. Lifecycle: a message's document is created on `message-start`,
  extended per delta, and finalised on `message-end`; rows rebuilt from `/transcript` on resync
  (§9.1) render their stored markdown source the same way. Tool calls, results and `output` lines are
  plain text or fixed-format rows, not markdown documents.
  Rows: user turn; assistant markdown; tool call/result as collapsed one-liners (`name — ok/error`,
  expandable to the truncated content); a lane's `report` as a distinct report row; `output` and
  status events as dim lines. Auto-follow the tail while the reader is at the bottom; show a jump
  affordance when they are not.
- **Center bottom**: the **selected agent's todo list**, above the fold of the input area, hidden
  when that agent has none. Coordinator todos: `/state.todos` + `todo-changed`. Lane todos:
  `todo-changed` on `/lanes/N/events` (a lane's stream replays its retained log with `?since=0` if you
  want a seed; otherwise the panel fills on the first change).
  Each item: status glyph (`☑`/`◐`/`☐`) + text, compact, clickable only if you add interactions.
- **Right** (~360 px): the coordinator's composer — the **Input** (plain multiline text editor for
  now — `Textarea` + `TextareaState`, auto-grow 2→8 rows; Enter sends, Shift+Enter is a newline) and
  **one status row beneath it: the session readout on the left, the action button on the right** —
  same line, text flush left, button flush right, one line high. When the readout is wider than the
  row, truncate it with an ellipsis and carry the whole line in a tooltip; never let it wrap the
  button onto a line of its own.
  The readout **mirrors the TUI's status line, segment for segment and in the same order**, so the
  same session reads the same in both frontends (the TUI builds it from ordered segments —
  `src/tui/tui.lisp`, `:model` 100 · `:thinking` 200 · `:context` 300 · `:goal` 400, plus
  `extensions/340-cache-stats.lisp`'s `:cache-stats` at 350, joined with `" · "` and dim):
  `ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%) · 97% cached · goal a1b2c3d4 (active) 12k/50k`

  | Segment | Shown as | From |
  |---|---|---|
  | model | the id, or `id (provider)` when that id is registered under more than one provider; hidden when there is none | `/state.model`, `/state.provider`, `/registry.models` |
  | thinking | the effort level, lower-cased (`low` … `max`); hidden when null | `/state.thinking` |
  | context | `ctx 48k/936k (5%)` — a count is `round(n/1000)` + `k`; the percent is `min(100, round(100 · used / window))`; `ctx 48k` when there is no window | `/state.context_tokens`, `/state.context_window` |
  | cache | `97% cached` — `round(100 · cache_read / (input + cache_read + cache_write))`; hidden while `cache_read + cache_write` is zero | see below |
  | goal | `goal <id> (<status>) <tokens>[/<budget>]`, same k-formatting, `tokens = tokens_used + tokens_used_live`, budget only when set; hidden when there is no goal | `/state.goal` |

  Keep each segment's text exactly as the TUI renders it — same numbers, same units, same
  abbreviation rules — and render them in the theme's muted text color, as the TUI's `dim` style.
  `/state` is the seed and the resync; between resyncs the TUI re-anchors its context figure from
  each `message-end`'s `usage`, and so should the readout: on every `message-end`, let
  `context_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write` and fold the
  cache totals as above, so the line moves with the run instead of jumping at `settled`.

  **The cache figure is not in `/state`.** The coordinator computes and persists it in an extension:
  a journal `:custom` entry under key `cache-stats` holding `{:input, :cache-read, :cache-write}`
  running totals. Seed it once per tab by walking `GET /journal?limit=N` backwards for the newest
  such entry — try `N=20`, and grow (100, 400) only while it is not found, so a long session is never
  re-sent — then keep it live by folding each `message-end`'s `usage` (`:input`, `:cache-read`,
  `:cache-write`; `:input` already excludes cached tokens, so the sum is the real input). Without the
  extension the entry never appears and the segment stays hidden — the same rule the TUI applies.

  The **button** shares that row, and its face and function follow the coordinator's status — never a
  Send and a Stop side by side:
  - status `idle` (also before the first run, and after a failed one): the button reads **Send**
    (primary), enabled only when the input has text; clicking posts `POST /prompt {"text": …}`.
  - status `running` or `compacting` (from `/state.status`; keep it current with `task-start`,
    `task-end`, `settled`): the *same* button reads **Stop** (secondary, square glyph) and clicking
    posts `POST /interrupt` — it interrupts and **leaves the draft untouched**; it never sends.
  - `Esc` is a second route to that same interrupt. Enter, in both states, keeps the TUI's meaning:
    it sends (`/prompt` lands at the running turn's next boundary), so text can be queued while the
    agent works — the button's face always says what the button does.
  There is **no model selector in a tab** (§14.7): the model is chosen when the tab is created, or
  inherited from the resumed journal, and the readout above shows it; nothing in the tab changes it.
- Input and button always target the **coordinator**; selecting a lane changes only the center
  column (lane control belongs to the coordinator, §D21).

## 8. Act, don't reimplement

Nothing in evo's behavior is duplicated in Rust: a refusal (`409`), a bad model (`400`), an unknown
command (`404`) are shown from the reply's `error`/`output`, not re-validated locally.

## 9. Frontend behavior

1. **Transcript assembly.** On tab open: `GET /health` (cursor) → `GET /transcript` → render the
   rows → open `GET /events?since=<cursor>`. Live events append/extend rows. On `settled`, on `gap`,
   on `hello`, or on reconnect, refetch `/transcript` (+ `/state`) and rebuild — the transcript is
   truth, events are liveness. Each agent keeps its own revision counter; late results from an old
   revision are dropped.
2. **Sending, and the one button.** Enter always posts `POST /prompt {"text": …}`: idle → starts a
   run; running → lands at the next turn boundary (the TUI's behaviour), so text typed while the
   agent works is queued, not lost. Clear the input only on `ok`, and disable the button only while
   its own request is in flight. The button's face tracks the status (§7.3): Send when idle, Stop
   while running or compacting — and a click never does something other than what that face says.
3. **Lane watching.** Subscribe to `/lanes/N/events` only for the lane currently shown (and to every
   lane's `lane-state` via the coordinator stream always). Never fetch a lane's token or URL.
4. **Model catalog** (for the empty tab's choosers, before any server exists): cache the last
   `/registry` under `~/.evo/desktop/model-cache.json`; on first run learn it by starting a throwaway
   `evo-agent serve --port <free> --token-file …` with cwd `~/.evo/desktop/probe`, `GET /registry`,
   `POST /shutdown`, and refresh the cache from every live server's `/registry` afterwards. Never
   hardcode model names. The **lanes** chooser only offers models a `--no-userspace` lane can register
   — those whose `api` is in the kernel's own set (today: `anthropic-messages`), which a
   `--no-userspace` probe's `/registry.apis` reports; mark others unavailable, because a lane whose
   model it cannot register fails to initialize (a model whose API an extension defines can only reach the lanes
   through the swarm's own `swarm.lisp`).
5. **History scan** (background thread, cached, never blocking the UI): walk
   `~/.evo/sessions/*/*.sexp`, newest first, budget ~500 files/5 s. Line 1 is the header
   `(:type :session :version 1 :id "…" :cwd "/abs/path/" :timestamp "…")`; a session is resumable as
   a swarm iff it contains a `:custom` entry with `:key "swarm"` (parse the last one for `:id`,
   `:workers`, and the lanes' `:cwd`). Merge with the app's own recent list from `app.json`, dedupe
   by session path, sort by recency. Row: folder name/path, lane count, when, models when known.
6. **The lanes model is project configuration, not a flag.** evo-swarm has no lanes-model option: a
   lane's default model is the coordinator's unless the project's `swarm.lisp` overrides it. So when
   the user picks a lanes model, write it into `<folder>/.evo/swarm.lisp` before spawning:

   ```lisp
   ;;; evo-desktop:begin — the lanes' default model (managed; edit outside the markers)
   (evo.swarm:in-lanes ()
     (evo:set-setting :model "ark-deepseek-v4.1-flash")
     (evo:set-setting :model-provider :aiden))
   ;;; evo-desktop:end
   ```

   - Both lines matter: a model id alone fails when that id is registered under another provider
     (`find-model` refuses a model/provider mismatch), so write the provider too — both come from
     `/registry`.
   - Put the block at the **top** of the file: `in-lanes` forms run in order and the last one wins,
     so whatever the user wrote below keeps the last word.
   - Rewrite the block in place, idempotently: never duplicate markers, never touch the rest of the
     file, create `.evo/` and the file if missing; with **Default** chosen remove the block (and the
     file too when that leaves it empty).
   - The file belongs to the folder, not the tab: two tabs in one folder share it and the newest
     write decides what the next lane initialization gets. Surface the path next to the chooser
     instead of pretending per-tab isolation.
7. **Failure surfaces.** Boot failure → log tail in the tab with a Retry button. A lane that cannot
   register its model says so in its lane row and in `swarm.log`. Lane down → `✗` plus
   the reason from the transcript's `output` lines. Server gone → reconnect with `Last-Event-ID`,
   exponential backoff 0.5→10 s, and a "reconnecting" badge; after the supervisor restarts the
   coordinator, the same tab keeps working (§3).
8. **Quit.** Shut every tab's swarm down (§3) in parallel, persist `app.json`, exit. Because of
   `EVO_SERVE_WATCH_PID`, a crash of the app leaves nothing running.

## 10. `../evo-agent` is read-only

Do not modify evo. Everything this app needs is done in the client through the documented API: the
lanes model goes through the project's `swarm.lisp` (§9.6), lane todos come from `todo-changed` on
`/lanes/N/events`, and resume/reset comes from `--resume` plus the journal's swarm record. If you
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
- Tests: `cargo test` for the client layer against a **real** `evo-swarm serve` (borrow the harness
  shape of `../evo-agent/tests/swarm-serve-e2e.py`: a temp `HOME` whose `init.lisp` registers a stub
  provider, `--evo` pointing at the built `evo-agent`, two lanes, a `DELAY`/`SLOW` scripted model),
  plus gpui-kit `TestAppContext` UI tests for the tab strip, the empty tab, transcript row building,
  and the todo panel.

## 12. Milestones (each ends with proof, not with intent)

- **M0 — spike.** Window with a custom title bar; spawn `evo-swarm serve` in a folder; `/health` +
  `/events` streaming into a plain view; `POST /prompt` sends and deltas appear. *Proof: screenshot +
  a transcript of a real run against the installed `/usr/local/bin/evo-swarm`.*
- **M1 — one tab, real.** Tab page layout (list / transcript / todos / input), markdown rendered
  live while streaming (§2.8), tail following, the Send/Stop button, resync on `settled`. *Proof: a run where a lane is
  delegated work and the coordinator's transcript renders it.*
- **M2 — tabs and the empty page.** Tab strip with Add/Close/Select, tab persistence, empty tab with
  the choosers + folder button + history list, resume a swarm from history. *Proof: close the
  app with three tabs, relaunch, resume one from history, everything still renders.*
- **M3 — lanes.** Lane list with status icons, lane transcript, lane todos, `lane-state` live
  updates, lane failure states. *Proof: kill a lane's process and watch the app report it down and
  then up (the swarm restarts it).*
- **M4 — hardening.** Single instance + activation, crash/restart of the coordinator, log tails on
  boot failure, `~/.evo/desktop` state, quit shutting everything down, `cargo test` green,
  `cargo build --release` and a bundle you can launch from Finder.

## 13. Non-goals (v1)

Rich-text composer, image paste, runtime model switching and any command surface, lane control endpoints, remote/non-loopback servers, TLS, multiple
windows, Windows/Linux packaging, an embedded browser, editing evo's journals, a settings UI beyond
binary paths and theme.

## 14. Settled decisions

1. **Lanes model** is written into `<folder>/.evo/swarm.lisp` as a managed block (§9.6) — no flag,
   no change to evo.
2. **Workers** are chosen in the empty tab (row 3, 1–64, default = evo's own). The count is passed
   for a folder-created swarm only: a resumed swarm keeps the lane count in its journal record.
3. **History** lists every resumable swarm on disk (any folder), merged with the app's own recents —
   not only tabs this app created.
4. **Input and its button** always target the coordinator; lanes are watched, never typed to.
5. **Closing a tab** shuts its swarm down (a clean `POST /shutdown`), and the session stays on disk
   and reappears in history.
6. The app never auto-starts swarms on launch: it opens one empty tab.
7. **No model selector in a tab, and one action button.** The model is chosen when the tab is created
   (or inherited from the resumed journal) and shown read-only; the composer's single button is Send
   when the coordinator is idle and Stop while it runs, and Enter always sends through `/prompt`.
