# evo-desktop — build prompt

**Mission.** Build `evo-desktop`: a native desktop GUI for the evo agent runtime. The app lives
in this repository (`~/coding/evo-gui`), is written in Rust with
[gpui-kit](https://gpui-kit.com/docs/), and is a *client*: evo already exposes an HTTP API for
exactly this, and the app must use it rather than re-implement anything.

**Architecture in one paragraph.** One window; its title bar is a browser-like tab strip. Each tab
is one evo `serve` process — `evo-swarm`'s coordinator plus a pool of worker lanes, or `evo-agent`'s
one agent (§7.2) — that this app spawns in a chosen folder and drives over one loopback protocol:
the child writes a **ready file**
(port, token, epoch) once it is listening, and the app reads a **snapshot**, follows one **stream**
of ops, and posts every action as an **op** — the whole of it is in the agent's own
`docs/serve.md`,
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
| `evo-agent/docs/serve.md` | the protocol as the server side documents it (evo's own checkout, beside this repo) |
| `evo-agent/docs/swarm.md` | what a swarm is, and how its lanes are mirrored into topic `lane:N` |
| `design/doc28/` | the user's design for this app: the React and CSS source of truth for every colour, size and micro-interaction, and what §7 describes. Where the two disagree, the design and the code win |

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
gone. `CONTRACT.md` §1/§8 and the agent's own `docs/serve.md` are the whole of it,
and nothing here repeats them. Which binary runs is the launch's own program
(§7.2): the workers card's switch for a new session, the journal's own program for
a resumed one. `--evo`, `--workers` and the two lane flags are a swarm's alone —
a single agent is launched with the coordinator's `--model` and `--thinking` and
no lane flag of any kind.

Two rules that stay: `--workers` only when the user asks, and never
`--allow-remote`.

## 4. Endpoints you use

Three reads and one write, all of them CONTRACT.md §5 and
the agent's `docs/serve.md`: `GET /snapshot` (one atomic read of every topic the
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
  - one `Tab` per session, label = folder name (tooltip: full path + swarm state), `suffix` = a close
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
│ Workers  Use swarm [●]  [ 6 ▾ ]                               │    all three rows
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
- The workers card's own **Use swarm** switch (§7.2, §9.6): on by default, and remembered in
  `app.json` as an optional field, so a file written before it existed opens on a swarm. It is the
  window's, like the column widths: one value for every tab, read again at launch. Off, the lanes'
  model, its effort and the count grey **in place** and take nothing — the same boxes in the same
  places, no click, no keystroke, no tab stop, so nothing moves under the pointer that flipped it —
  and the page's fields resolve from one agent's own `evo-agent catalog --json` (§9.4) instead of
  `evo-swarm check --json`. The design draws no switch: this is the one control added, and nothing
  else moved to make room for it.
- To the right, spanning the three rows: **Select folder…** → native folder dialog (`rfd`) → on pick,
  the tab spawns its launch in that folder and becomes a tab page (§7.3): `evo-swarm serve` while the
  switch is on, `evo-agent serve` — the coordinator's `--model` and `--thinking`, no lane flag of any
  kind — while it is off. Cancel leaves the empty tab.
- Below the separator: **history**, the resumable sessions (§9.5) of either program. Clicking a row
  opens it as a new tab, resuming it with the program that wrote it (`--resume <session path>`, cwd =
  that folder) — only that program can open that journal — and the row says which kind it is
  (`Agent session` or `Swarm session`, the kind's own mark). A lane's own journal is not a row:
  a lane is the swarm's, and the swarm's session is the row for it. A resumed swarm keeps the
  lane count its own record kept — show that count read-only on the row; a worker count chosen here
  applies to folder-created swarms only.
- The empty tab is a real tab: it can be closed, and the app always keeps at least one tab (closing
  the last one leaves a fresh empty tab rather than an empty window).

### 7.3 Tab page

The design's own page (`design/doc28/Workspace.tsx`): **two columns with one split, and the
composer at the foot of the conversation**. One band runs across both columns — the lanes band and
the agent header are the same height on the same surface, so the rule under them is straight.

```
┌──────────────────────┬────────────────────────────────────────────┐
│ Lanes 2 of 6 busy    │ main  coordinator                          │
├──────────────────────┼────────────────────────────────────────────┤
│ ● main   coordinator │        the agent's transcript              │
│ ◐ lane 1 the view …  │                                            │
│ ○ lane 2        idle │  ┌──────────────────────────────────────┐  │
│ ✗ lane 3        down │  │ Todos 1/3         ⌃                  │  │
│                      │  │ Message the coordinator…             │  │
│ ~/coding/evo-gui     │  │ (Enter to send, Shift+Enter)         │  │
│                      │  │ [stub-a medium] [ctx 48k/936k]       │  │
│                      │  │                     [ ↑ Send ]       │  │
│                      │  └──────────────────────────────────────┘  │
└──────────────────────┴────────────────────────────────────────────┘
```

- **The lanes column** (left, 180–480 px, 260 at rest): the band says `Lanes` on one side and
  `N of M busy` on the other, and under it `main` (the coordinator) comes first, then one row per
  lane at 32 px: a 9 px dot, the name, the task it was given (truncated, dim), and the state — or
  the step clock while it works — at the far end, right-aligned in tabular figures. In a
  single-agent session (§7.2) the band carries no `N of M busy` at all and the column is its one
  row, named `Main`: there are no lanes to count, and no lane rows — whatever a swarm's snapshot
  left in the list is dropped. The dot is
  **breathing** while that agent works: its fill mixes between the ink and the surface of the row it
  sits on, on a cosine with a 1600 ms period; an idle agent wears a muted ring instead. Hovering a
  row mixes 5% of the ink into it, selecting one 9% and the name goes medium. While the pointer is
  on a **working** lane's row that row offers a small **Stop** — `run.interrupt`, scope `lane`, the
  one thing a person may do to a lane. A click selects that agent, and `↓`/`↑` with `Home`/`End`
  walks the rows. `main`'s state is the session's own — `idle`, `running`, `compacting`, or
  `waiting on lanes` while the swarm holds it for them. All of it is the `swarm` topic's `lanes[]`
  (§4.3) plus the session's status; nothing is inferred — and a single agent's column is that
  session's status alone, its server publishing no lanes at all. The folder the swarm runs in is
  pinned at the bottom of the column, one line, `~`-shortened and trimmed from the front — whole
  directories at a time, so the tail that names the place is the last thing to go — with the whole
  path on hover.
- **The split**: a 9 px band with a 1 px hairline down it, and the pill that grows on hover, press
  and drag (20 / 28 / 44 px tall at 35 / 60 / 90%). A drag clamps the agent column to 180–480 px and
  leaves the conversation at least 420; a double-click puts the column back to 260, and the width is
  shared by every tab and kept in `app.json`.
- **The conversation** (right, at least 420 px): the agent header — the name (`coordinator`, `Main`
  in a single-agent session, or `lane N`), its task (dim, truncated), and, when that agent's
  transcript carries thinking text, a quiet **Show thinking**
  toggle — then that agent's items — the coordinator's `session` topic, or that lane's `lane:N`
  topic. Assistant text is **rendered markdown that stays rendered while it
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
  `command_note` items as their own lines. The list holds the agent's **whole journal** and draws it
  with a virtual list: only the rows the pane can reach are built and measured, so a record of
  thousands of items costs a pane of rows a frame. Older items behind the oldest one held are **paged
  in by the view itself** — one `GET /items?before=<oldest>` in flight at a time, for as long as the
  topic's `has_more` says there is more — and a quiet `Loading earlier items…` line sits at the head
  of the list while one is on its way; there is no button to press, and a page that lands is spliced
  in above the reader without moving what they are reading. Auto-follow the tail while the reader is
  at the bottom; show a jump affordance when they are not. **Links**: an address or a path in a row's
  *own words* — a user's turn, an assistant's markdown, a notice, a lane's line, a report's field, the
  body of a quiet row — is drawn underlined and is pressable: `http`, `https` and `mailto` through
  `App::open_url` (the browser, the mail client), and a file or a folder through
  `App::open_with_system` (the platform's application; the Finder for a folder). A path is resolved
  the way the tab is: `~/…` from the reader's home, a relative path from the folder the tab runs in,
  and a trailing `:line` or `:line:col` names a line of the file rather than a file of its own —
  `crates/transcript/src/lib.rs:42` opens that file. A path that is not on disk stays words, and the
  disk is asked once per token and the answer remembered for a few seconds (`linkify::Paths`: 3 s,
  512 tokens), so no frame stats anything; a tab whose folder moves re-reads its rows. A tool call's
  arguments and its result are **data**: drawn exactly as they came, with nothing in them to press.
  The opener is the view's own handler (`TranscriptView::on_open_link`), so a test can hold what was
  pressed without opening anything on the machine.
- **The composer, at the foot of the conversation** — the selected agent's, on the transcript's own
  reading measure (an 800 px measure, inset 16). `design/doc28/Composer.tsx`: an `input`-surface box,
  12 px radius, 1 px border; with the caret in it the border mixes 55% of the primary into the
  border and a 3 px ring of the primary at 12% sits outside it. Inside the box, in the order the
  design draws them:
  1. the **goal strip** of the selected agent across the top, in the todo strip's own style — a
     32 px row reading `Goal`, its status in the dim half and its budget beside it while evo
     tracks one, with a chevron that turns over 120 ms — and, folded out under it, the objective
     itself as prose, scrolling past 156 px. No goal, no strip. The goal's **id** evo's status
     line spells out (`goal g-b7ba (active) 12k/50k`) is its own handle on the goal, never a
     reader's fact: the `goal` segment is not drawn as a chip, and neither the row nor the
     objective writes the id. Unlike a drawer the strip is not dismissed by a press outside the
     box — only its own row folds its objective, and each composer keeps that.
  2. the **todo strip** of the selected agent — a 32 px row reading `Todos d/n` with
     a chevron that turns over 120 ms — and, folded out under it, the items (`☑` done, `◐` in
     progress, `☐` pending), scrolling past 156 px. No todos, no strip: `Todos 0/0` over nothing is
     chrome that says only that there is nothing to say. The todos come from the selected agent's
     topic state — `state.todos` of `session`, or of that lane — seeded by the snapshot and kept
     current by `state.patch`.
  3. the **model drawer** a chip folds out, inside the box, in the strips' own style: a 32 px title
     row that folds it back, and the body under it. A click outside the box, or selecting another
     agent, folds it back too.
  4. the **input** (plain multiline text editor for now — `Textarea` + `TextareaState`, 14 px on a
     20 px line): two rows at rest, growing with what is typed to **half the conversation pane**,
     and scrolling inside itself past that. Enter sends, Shift+Enter is a newline, `Esc` interrupts
     the coordinator's turn, and `↑`/`↓` walk the prompts this tab has sent while the input is
     empty. An empty box says what it is addressed to: `Message the coordinator…` in a swarm, and
     `Message the agent…` in a single-agent session (§7.2), where there is no coordinator to name.
  5. the **completion popup**: a word being typed is completed inline, as an editor does — the
     commands `GET /catalog` lists for a `/word`, and the *running image's* own symbols for a
     token inside `/eval `. What the caret is on is evo's own answer (`complete`, §5.6): the box
     asks with its text and its caret, and the reply is the kind, the range a candidate's name
     replaces and the candidates — the rules the TUI's Tab popup uses, so the two frontends
     agree by construction, and a read rather than the evaluation the old `eval`-form path was
     (`docs/api-gaps.md` is where that workaround used to be written down). `cursor` counts
     characters, so the client converts the byte offset an input box works in, both ways. One
     question is out at a time, asked after a keystroke's rest of about a frame; the answer is
     held against the text it was asked about, so the caret's `/com` plus one typed `p` is that
     word one character longer while a paste or a delete is a text the answer says nothing
     about (the word waits for the answer to *this* text, which is a round trip away). It is a
     drawer's item list floating at the word: the sidebar
     surface, one hairline, a 10 px radius, the box's own shadow, standing over the *word's* own
     line — not the caret's end — a small gap off it, and under it when the window has no room
     above (it is a deferred draw, so neither the box nor the page can clip it). Every row's
     label starts at the x the typed word does, the popup being placed by that column rather
     than by its own edge; a word near the window's right edge pulls the whole list back inside
     it, which is the one case the labels leave the word. It is as wide as its widest row —
     measured over every candidate of the list, not only the rows it shows, so ↑/↓ never
     resizes it — between 240 and 560 px and never wider than the window, and past that ceiling
     a description is what truncates. Up to eight rows, the list scrolling inside its own fold
     and a dim `… n/m` counter under it while there are more. A row reads `label` then the
     description dim, in the drawer's own shape, with the characters the word matched drawn
     heavier — the prefix a name begins with, or the positions a fuzzy match took, so `/re`
     offering `/lore` says which two letters it found; the highlighted row wears the drawer
     item's chosen fill. The keys are the list's while it is
     up: `↑`/`↓` walk the rows and wrap, `Tab`/`Enter` take the highlighted one — **Enter takes
     the row and does not send** — and `Esc` dismisses until the word changes (so it is never
     the coordinator's interrupt). A `/word` completes anywhere in the message, and a second
     `/` ends the word, so `/usr/local/bin` offers nothing — that is the server's rule, stated
     to the client as "nothing completable"; what the *client* owns is the ranking
     (`crates/composer/src/complete.rs`): a command word ranks the catalog's own commands, with
     the names it begins first and the ones it is a subsequence of, because evo's own answer for
     a command is the prefix-matched half of that same document (`/rl` finding `/reload` is what
     the fuzzy half is for), while a symbol's rows can only be the image's own answer. Accepting
     a command replaces the whole `/word` and opens its argument with a space when it stands at
     the message's start; accepting a symbol replaces the token
     under the caret alone, leaving the rest of the form where it is. A list whose only
     candidate is the word already typed is never drawn (it would show the reader their own
     input back and capture the `↑`/`↓` the history browses with), and neither is one raised
     by recalled history. A message that **is** one slash command from its start is the
     command layer's — `command.run`, `/eval` and every extension command included — and the
     server's `not_found` is what says a word is not one; a message that merely mentions one
     is the reader's words. A command's output arrives **twice**: as the reply's own `notices`
     and as session `notice` items, the same lines word for word (`evo.command:host-notice`
     publishes what the reply also carries). The transcript draws the items, so the reply's
     copy is not drawn — one line, one place — and of the rest of that reply one field is
     read: `data.draft`, the message `/rewind` and `/tree` hand back for editing, which is the
     whole point of those commands and cannot be read off the topic.
  6. the **foot row**: the agent's status line as chips, then the one action button.
- **The chips** are the topic's **`segments`**, one chip per segment and in the order the server
  publishes them — the same core registry (`evo:define-status-segment`) the TUI's status line uses,
  so the two cannot drift apart, and an extension's own segment (cache-stats, say) arrives the same
  way: the app computes none of it and walks no journal.

  | Segment | Chip | From |
  |---|---|---|
  | model | the id, or `id (provider)` when that id is registered under more than one provider, with the effort in the chip's dim half | `state.model`, `state.thinking` |
  | thinking | the effort level, when there is no model chip to ride on | `state.thinking` |
  | context | `ctx 48k/936k (5%)` — the server's own numbers and units | `state.context` |
  | goal | **not a chip** — the goal has its own strip above the todo strip (the box's item 1): the row states `Goal` and the status, folds out the objective, and never writes the id evo's segment spells out | `state.goal` |

  Render a segment's `text` as it is, `order` left to right (right-hand segments are the swarm's
  own summary, which the lanes column already states), and **leave out a segment the server does not
  publish** — a session with no cache activity and no goal shows no cache chip and no goal strip,
  which is the whole of that status line. The `model` chip opens the drawer holding the models `/catalog`
  lists and the effort ladder **that model** declares (`models[].effort_levels`, in the server's
  own order, in full — a provider's ladder need not be a run of levels, so `low, high, max` is
  stated as three rungs and never as `low–max`; the session's `thinking_levels` stands in only
  when the catalog names no entry for the chosen model), and changes the **coordinator's own**
  model and effort with `model.set` / `thinking.set` (§5.5) —
  the session's, not a lane's: for a lane the drawer states what the swarm runs and says so
  read-only.
- **The button** shares that row, and its face and function follow what is going on — never a
  Send and a Stop side by side:
  - nothing going on: the button reads **Send**, in the primary face at rest whatever the draft
    (`.composer-send` has no disabled look) — with nothing in the input there is nothing to send,
    so the click and `Enter` do nothing; with text, clicking posts `input.send`. The one thing
    that greys it is this composer's own request in flight.
  - anything going on — the coordinator's own run (`running`/`compacting` from `state.status`), a
    coordinator held `waiting` on its lanes, or a lane still working: the *same* button reads
    **■ Stop swarm** and clicking posts `run.interrupt` with scope `swarm` — it interrupts and
    **leaves the draft untouched**; it never sends.
  - in a single-agent session (§7.2) the same button reads **■ Stop** and posts `run.interrupt` with
    scope `session`: one agent's own run is the only run there is, and the session scope is the only
    one its server knows.
  - `Esc` is a second route to that same interrupt. Enter, in both states, keeps the TUI's meaning:
    it sends (`input.send` lands at the running turn's next boundary, as a `user` item whose status
    is `queued`), so text can be queued while the agent works — the button's face always says what
    the button does.
- Input and button always target the **coordinator**; selecting a lane changes the conversation, and
  the composer's chips and todo strip follow the selection — a lane's are read-only, because a
  lane's model and effort are the swarm's (lane control belongs to the coordinator, §D21).

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
   `GET /items?before=<oldest>`, asked for by the transcript itself for as long as the
   topic says there is more behind the oldest item it holds — one page in flight at a
   time, no button; the model's own context is `GET /debug/context`, which is
   never rendered.
2. **Sending, and the one button.** Enter posts `input.send {text, queue}`: idle →
   the run starts, running → the input is queued and arrives as a `user` item whose
   `status` is `queued` until evo drains it. Clear the input only once the reply
   says `ok`; a queued row can be taken back with `input.cancel`. The button's face
   tracks what is going on (Send when nothing is, Stop while the coordinator runs,
   compacts or waits on its lanes, or while any lane is working) and a click never
   does something other than what that face says: Stop posts `run.interrupt` with
   scope `swarm`. A single-agent session (§7.2) has no lanes to wait on: there the
   same button reads `Stop` and posts scope `session` — the only scope its server
   knows.
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

   With the workers card's switch off (§7.2) there is no `check` to ask — `evo-agent`
   has no `check` subcommand, and `--workers`/`--lane-model` are not flags it takes —
   so the fields resolve from `evo-agent catalog --json` instead: one agent's own
   registrations, from its own init file, which is exactly why one read cannot stand
   in for the other. The line under the cards is then evo's own `reason` for the
   registration the launch resolved, and nothing is said about lanes, the count or
   the effort: those are `check`'s answers. Each read's failure keeps its own words,
   and an answer that arrives after the switch moved is dropped rather than applied.
5. **History** (a background thread, never blocking the UI): `evo-agent
   sessions --json` — one process, one document, no journal is read or parsed
   here — merged with the app's own recents from `app.json`, deduped by session
   path, newest first. Every program in that one read: a swarm's coordinator and a
   single agent alike, while a lane's own journal is not a row at all — a lane is the
   swarm's, and the swarm's session is the row for it. Each row says which program
   wrote it (`Agent session` or `Swarm session`, the mark on the row) and a click
   resumes it with that program — `--resume <path>` to `evo-agent` or `evo-swarm`,
   because only the program that wrote a journal can open it; a record too old to
   name a program reads as a swarm, which is what this app started then. A row's
   title, folder, when, lane count and models come from that;
   `session::HistoryRow` is what it looks like.
6. **Lane configuration is launch flags.** `--lane-model ID@PROVIDER`,
   `--lane-thinking L` and `--workers N` are what the choosers produce, and the
   swarm records them in its own record, so a resume restores them. They are a
   swarm's alone: with the workers card's switch off (§7.2) the launch is
   `evo-agent serve` with the coordinator's `--model` and `--thinking` and none of
   them. The switch is not itself a flag — it is which program runs. The app never
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
  a dedicated thread), a lock-file single-instance guard. The streaming path has to keep the
  UI responsive — GPUI's executor is not tokio's, so bridge explicitly, and the case to check is
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
  tab, transcript row building, and the composer.

## 12. Where the work is

The app is built. What each crate owns is `docs/architecture.md`; what it does, a
surface at a time, is `docs/usage.md`; what is proven against real binaries —
`evo-swarm` / `evo-agent`, never a mock — is `docs/proofs.md`; the pictures are
`docs/screens.md`. `CONTRACT.md` in the workspace root is the binding half of all
four, and where they and the code disagree, the code and the contract are what is
true.

Roughly: `crates/app` is the shell (single instance, the window and its bounds, the
launch-time loads, Settings, About, the quit sequence), `crates/workspace` is the
tab strip, the empty tab and the tab page, `crates/transcript` and
`crates/composer` are the two surfaces a person types into and reads,
`crates/agent_list` is the lanes column, `crates/settings` the panel, and
`crates/tab_engine` + `crates/swarm_client` are one tab's I/O over
`crates/session` (the topic mirrors and the view model) and `crates/store` (the
on-disk layout, the offline CLI reads, the launch argv).

## 13. Non-goals (v1)

Rich-text composer, image paste, a command *palette* (a `/word` completes inline and runs —
§7.3 — but there is no list of every command to browse, no argument hinting, and no surface
for a command's own `choices`), lane control endpoints, remote/non-loopback servers, TLS,
multiple windows, Windows/Linux packaging, an embedded browser, editing evo's journals, a
settings UI beyond binary paths and theme.

## 14. Settled decisions

1. **Lanes model** is a launch flag, `--lane-model ID@PROVIDER` (§9.6) — recorded in the swarm's own
   record and restored on resume. Nothing in the app writes `<folder>/.evo/swarm.lisp`.
2. **Workers** are chosen in the empty tab (row 3, 1–64, default = evo's own). The count is passed
   for a folder-created swarm only: a resumed swarm keeps the lane count its record kept.
3. **History** lists every resumable session on disk (any folder) — a swarm coordinator's or a
   single agent's, never a lane's — merged with the app's own recents, not only tabs this app created.
4. **Input and its button** always target the coordinator; lanes are watched, never typed to.
5. **Closing a tab** shuts its swarm down (`server.shutdown`, then the pipe), and the session stays
   on disk and reappears in history.
6. The app never auto-starts swarms on launch: it opens one empty tab.
7. **One action button, and only the coordinator's own model is settable in a tab.** A swarm's model
   is chosen when the tab is created (or inherited from the resumed journal); the composer's model
   drawer changes the **coordinator's** model and effort (`model.set` / `thinking.set`), which are
   the session's own — a lane's chip states what the swarm runs, read-only (§7.3). The button is Send
   when nothing is going on and Stop while anything is (the coordinator's run, its wait for the
   lanes, or a working lane), and Enter always sends through `input.send`.
