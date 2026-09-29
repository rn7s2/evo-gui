# Using evo-desktop

One window, one tab per swarm. A **tab** is one `evo-swarm serve` process —
a coordinator agent plus a pool of worker lanes — running in a folder you pick,
spawned by the app and driven over evo's HTTP API. Lanes are never addressed
directly: their tokens and ports stay inside the coordinator.

## Starting, and the second launch

Launch `evo-desktop` (or `cargo run -p evo-desktop`). Only one instance ever
runs: launching it again raises the running window and exits 0, so a second
`open` from the Dock does not give you a second window. Every launch opens **one
empty tab**, whatever the last session left behind: the tab set is recorded in
`app.json` but not reopened, and the sessions that were open then come back at
the top of the empty tab's history wearing an `open at last quit` badge. The
app never starts a swarm by itself.

Closing the window (or `⌘Q`) is the whole quit sequence: every tab's
swarm is stopped (a clean `POST /shutdown`, then `SIGTERM`, then `SIGKILL`, on
threads of its own, with a 20 s deadline), `app.json` is written, and the process
exits when the last tab is gone. If the app is terminated some other way — a
logout, or the Dock's Quit before our key binding saw the keystroke — nothing is
left running: the swarms are told this app's pid at launch and stop with it.

## The keyboard

| key | what |
|---|---|
| `⌘T` | new empty tab |
| `⌘W` | close the tab being shown (the last one is never closed — a fresh empty tab replaces it) |
| `⌃⇥`, `⌘⇧]` | the next tab, round the end |
| `⌃⇧⇥`, `⌘⇧[` | the previous tab, round the start |
| `⌘1` … `⌘8` | the tab with that number (nothing when there are fewer) |
| `⌘9` | the last tab, whatever the count |
| `⌘M` / Window ▸ Minimize | minimize the window |
| Window ▸ Zoom | zoom the window |
| `⌘,` | Settings… |
| `⌘Q` | quit |
| `⌘C` `⌘V` `⌘X` `⌘A` `⌘Z` | the Edit menu: cut/copy/paste/select all/undo, in whatever field has the caret |

The File, Edit and Window items are also in the menu bar. The Window menu carries
the three tab actions — Select Next Tab, Select Previous Tab, Select Last Tab —
which are the same actions as `⌃⇥`, `⌃⇧⇥` and `⌘9`; `⌘1`…`⌘8` exist only as
shortcuts.

Switching tabs takes the keyboard with it: the composer of the tab you land on
gets the caret, and an empty tab hands it to its first chooser — a keystroke
nobody hears is worse than one the window handles.

## Tabs

The title bar **is** the tab strip: one tab per swarm, labelled with the folder's
name (an unused tab says `New tab`), and hovering one shows the whole path plus what
the tab is doing — `no folder chosen`, `starting the swarm…`, `swarm: running`,
`swarm: reconnecting…`, `stopping the swarm…`, `failed to start`, or
`swarm gone: the server exited`.

A middle click closes a tab, the way a browser's does. The `+` after the last tab
appends a new empty tab; when the tabs need more room than there is, they scroll
inside the strip and the `+` stays where it is. Closing a tab shuts its swarm down;
the session stays on disk and comes back in the history list.

Two small marks answer questions a label cannot:

- A **green dot** before a tab's name while that tab's coordinator has a run in
  flight. When a background tab's run *ends*, its dot turns muted — "something
  happened here" — and looking at that tab is what clears it.
- The **window's own title** — what Mission Control and the app switcher show — names
  the tab being shown: `evo-gui — Evo Desktop`, or `Evo Desktop` alone on an empty
  tab.

## The empty tab: choosing a swarm

```
New swarm
Pick models, then a folder

Coordinator model  [ Default ▾ ]            ┌────────────────────────┐
Lanes model        [ Default ▾ ]            │  [folder icon]         │
                   Saved to <folder>/.evo/  │  Select folder…        │
                   swarm.lisp — shared …    │  The swarm starts in   │
Workers            [ Default ▾ ]            │  the folder you pick   │
                                            └────────────────────────┘

History  6 resumable
  evo-gui  [open at last quit]
           ~/coding/evo-gui · 6 lanes · 11m ago · coordinator: ark-…
  foo      ~/coding/foo     · 4 lanes · 2h ago  · coordinator: …
```

- **Coordinator model** — passed to the swarm as `--model`. On **Default**
  (detail line `evo's own default`) nothing is passed and evo's own default
  applies. The menu lists every model the catalog knows, sorted by provider then
  id, each with a detail line — `200k ctx · vision · effort low–max`. An id
  registered under two providers can only be reached by its bare id, so only the
  registration `--model` resolves to is choosable; the others are listed greyed,
  saying which one the flag would pick.
- **Lanes model** — what a *lane* starts with. evo-swarm has no flag for this,
  so the choice is written into the project's `.evo/swarm.lisp` (below). On
  **Default** (`follows the coordinator`) nothing is written and every lane
  inherits the coordinator's model. The list is the same catalog, but a model a
  lane could not register is greyed with
  `needs an extension API — set it in swarm.lisp`: a lane only has the kernel's
  own APIs, which the app learns from a `--no-userspace` probe. Until that probe
  has reported, nothing is greyed out.
- **Workers** — how many lanes, 1–64. On **Default** evo's own
  `:swarm-workers` setting decides (else 6); when the catalog reports that
  setting, the option reads `Default (6)` and its detail says which. Only a
  swarm created from a folder takes this count: a resumed swarm keeps the lane
  count its journal records.
- **Select folder…** — the native folder dialog. Picking a folder starts the
  swarm there and turns the tab into a tab page; cancelling leaves the tab empty.
  Under the card sits a line that only appears when nothing can be launched from this
  app at all, naming the path you configured:
  `evo-swarm not found at /usr/local/bin/evo-swarm — fix it in Settings…`. It is
  clickable — it opens Settings — and its hover carries the whole line when the path
  is long.

Under the lanes chooser, one line carries the three states that have something
to say: `Loading models…` until the catalog is known, the note naming the file a
lanes model will be written to (with a `<folder>` placeholder until a folder is
chosen), or — when the catalog could not be read — one amber sentence about what
still works: `Couldn't load the model list — Default models will be used.`, or
`Couldn't refresh the model list — using the last one it loaded.` when the choosers
already hold a cached list. The server's own error is what that line hovers, and
every chooser stays usable on **Default**, so a swarm can still be started.

The tab's models are fixed when the swarm starts — the tab page has no model
selector, only the readout that shows what is running.

### Keyboard on the empty tab

Tab moves the keyboard between the three choosers, the folder card and the
history list (the card and the history frame draw a hairline focus ring while
they hold it). A chooser opens with `Enter`, `Space` or an arrow; the arrows walk
its options, `Enter` picks, `Esc` closes it without leaving the tab (and without
choosing). The history frame takes the arrows and `Home`/`End`; `Enter` resumes
the selected row, the same as clicking it.

### The lanes model lives in your project

Choosing a lanes model writes a managed block at the **top** of
`<folder>/.evo/swarm.lisp`:

```lisp
;;; evo-desktop:begin — the lanes' default model (managed; edit outside the markers)
(evo.swarm:in-lanes ()
  (evo:set-setting :model "ark-deepseek-v4.1-flash")
  (evo:set-setting :model-provider :aiden))
;;; evo-desktop:end
```

It is yours to edit outside the markers, and it is rewritten in place — never
duplicated, never touching the rest of the file. The file belongs to the
*project*, not to the tab: two tabs in one folder share it, and the newest write
decides what the next lane initialization gets. Choosing **Default** removes the
block, and the file itself if that was all it held.

### History

The history lists every resumable swarm on disk — any folder, not only the ones
this app started — merged with the app's own recent sessions, newest first. A session
the app remembers whose journal is gone is not a row — nothing ever wrote it (a tab
that was opened and quit again), or something deleted it — because `--resume` on a
file that is not there is a launch that cannot come back. A row leads with a folder
glyph and the folder's own name; under it sit the `~`-shortened path and what is known
about the session, joined by `·` — the lanes, how long ago, the coordinator's model
(`4 lanes · 2h ago · coordinator: ark-deepseek-v4.1-flash`) — each part left out when
it is unknown. A session the app had open when it last quit wears an `open at last quit`
pill. Hovering shows the full path, the journal's name, the absolute time, and the
models.

Click a row (or select it and press `Enter`) to open it as a new tab: the swarm
resumes that session (`--resume`) in that folder, and the lane count comes from
the journal (the workers chooser does not apply to it). The list has three quiet
states of its own: `Scanning sessions…` with a spinner while the disk scan runs,
`No resumable swarms yet`, and the scan's error in the danger colour. The calm one is
what a first run shows: `~/.evo/sessions` does not exist until evo has written its
first journal, and a directory that is not there yet is nothing to resume, not a
failure to read.

## The tab page

```
┌──────────────────────┬────────────────────────────┬──────────────────┐
│ 6 lanes · 2 busy     │ ● main  delegate the …     │ ┌──────────────┐ │
│ ● main               │                            │ │ input        │ │
│ ◐ Lane 1             │ ◐ Lane 1  SLOW: walk …     │ └──────────────┘ │
│ ○ Lane 2             │    transcript (markdown,   │ model · max ·    │
│ ✗ Lane 3             │    rendered live as it     │ ctx 48k/936k ·   │
│                      │    streams)                │ 97% cached ·     │
│                      │                            │ goal a1b2 (…)    │
│                      ├────────────────────────────┤    [  Send  ]    │
│ ~/coding/evo-gui     │ ☑ todos of the agent shown │                  │
└──────────────────────┴────────────────────────────┴──────────────────┘
```

### Left — the agents

The column opens with a summary — `6 lanes · 2 busy` — and then `main`, the
coordinator, followed by one row per lane: a status glyph, the task it is on
(truncated), and how long the current step has taken. A lane that is down shows
the reason in place of its task; hovering any row gives the whole story.

| glyph | meaning |
|---|---|
| `●` | working |
| `◐` | compacting |
| `○` | idle |
| `◌` | starting |
| `✗` | down — the row says why (the lane's own `output` lines say more) |

Clicking a row selects that agent: it changes what the centre column shows and
nothing else. The keyboard walks the rows with `↓`/`↑` and jumps to the ends
with `Home`/`End`. Input always goes to the coordinator. The folder the swarm
runs in is pinned at the bottom of the column, `~`-shortened, elided in the
middle, with the full path on hover.

### Centre — the transcript, and the todos

A slim header names the agent being shown (`main`, `Lane 1`) with the same
status glyph the left column uses, the task that agent was given, and — when
that agent's transcript carries thinking text — a **Show thinking** button that
reveals it (**Hide thinking** puts it back). The toggle is per agent: switching
agents shows each transcript the way you left it.

Below it, the selected agent's transcript: your turns; the assistant's text
rendered as markdown *while it streams*, so a half-finished message already reads
as the finished one will; tool calls and results as one-line `name — ok/error`
rows that open onto their arguments and result; a lane's `report` as a report
row; `output` and status events as dim lines. Rows are a reading surface —
drag to select, `⌘C` to copy — and an assistant message carries a copy icon for
its markdown source (it appears on hover, at the top of the message, and answers
`Copied` when pressed), with a separate **Copy** on every fenced code block for
the code alone.

The markdown is the subset a model actually writes — headings, lists, task lists,
tables, quotes, rules, inline code — with four decisions worth knowing:

- a **fence** is coloured for the grammars the app ships (`rust`, `python`,
  `javascript`, `typescript`, `bash`, `json`, `toml`) and plain otherwise;
- an **image reference** is drawn as `[image: alt]` and nothing is fetched: an
  `<img>` would make the app request a URL a model chose;
- **raw HTML** is shown as its source, mono and muted — markup reads as markup;
- a **link** opens only `http`, `https` or `mailto`; a `file:`, `javascript:`,
  relative or bare-fragment target is left alone.

A tool call's payload is expanded as rows: a nested object is indented under its
key, a long list gets a row per item, and past four levels (or for an array of
more than twenty), a muted `{…3 keys}` / `[…42 items]` row keeps the whole thing
in its tooltip.

Some messages in a transcript are not yours even though evo sends them in your
voice, and each is drawn as what it is — a quiet line that opens on click, never a
turn of its own:

- **Context** — what an extension injected into the session (your global and
  project memory, IDE context): `Context · global memory`;
- **Goal** — evo keeping a goal going: `Goal · continue — <objective> · <budget>`,
  `Goal · objective updated — <objective>`, `Goal · budget exhausted — wrap up`;
- **Command** — an extension answering a command you ran mid-run (`/memory`,
  `/global-memory`, `/lore`, `/notify doctor`): `Command · /global-memory`;
- **the swarm about a lane** — a lane's report is a `Lane 1 report` card, and
  `[lane 1] run ended …`, `failed to start`, `is down` and similar lines are one
  line each, `Lane 1 · run ended (stop) — task: …`, red when something failed.

When you scroll away from the bottom the view stops following and offers a
**Jump to latest** button. An agent with nothing to show says so in its own words:
`Ask the coordinator to get started` for `main`, and
`Lane 1 hasn't been given work yet.` for a lane.

Under the transcript, the todos of the selected agent (`☑` done, `◐` in
progress, `☐` pending), hidden when that agent has none.

### Right — the input, the readout, and one button

The input grows from two to eight rows. **Enter** sends, **Shift+Enter** is a
newline, **Esc** interrupts the turn. An empty input's **↑** walks back through
the prompts this tab has sent (**↓** walks forward again), so a prompt can be
sent twice without retyping it; typing anything makes the recalled text a draft
like any other. **⌘C** with nothing selected in the input copies the window's own
selection instead, so a passage picked in the transcript can be copied while the
caret sits in the input.

Sending while the agent is working is normal: the text lands at the running
turn's next boundary, exactly as typing into the TUI does. The draft is cleared
only when the send was accepted, and **Esc never touches it**. While a set of
text is on its way to the server the button is disabled, and it re-enables when
the server answers.

The single button follows the coordinator's status, and its face is what it
does: **Send** while idle (enabled once there is text), **■ Stop** while running
or compacting (which interrupts). There is never a Send and a Stop side by side.

The readout mirrors the TUI's status line, segment for segment, so the same
session reads the same in both front ends:

```
ark-deepseek-v4.1-flash · max · ctx 48k/936k (5%) · 97% cached · goal a1b2c3d4 (active) 12k/50k
```

| segment | shown as |
|---|---|
| model | the id, or `id (provider)` when the id is registered under more than one provider |
| thinking | the effort level, lower-cased (`low` … `max`) |
| context | `ctx 48k/936k (5%)` — the window is left out when it is unknown |
| cache | `97% cached` — only when the project's `cache-stats` extension has journaled totals |
| goal | `goal <id> (<status>) <tokens>[/<budget>]` |

The context figure re-anchors on every finished message, so it moves with the
run rather than jumping when the turn settles. The line is truncated with an
ellipsis when the column is too narrow for it; hovering shows the whole of it.

## Settings, About, and light or dark

**Settings…** (App menu, `⌘,`) edits the two binaries this app spawns and the
app's theme — nothing else is configurable. It opens as a dialog over the
window, and closing it is the only way it does anything: **Save** writes, and
**Cancel** (or `Esc`) puts the window's theme back and writes nothing.

- Each path is checked by running it (`<path> --version`) as you type, and the
  row says what was found: `checking…`, the version line the binary printed,
  `not found`, `not executable`, `not an evo binary`, or `no path`. **Save** is
  off while a field is empty. **Reset to defaults** puts back
  `/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent`.
- The theme is applied as you pick it, so you can judge it against the window
  behind the dialog; **Save** writes it to `app.json`.
- New tabs use the paths you saved; a tab that is already running keeps the
  binaries it started with, as the panel's note says.

**About Evo Desktop** (App menu, first item) shows the app's version, each
binary's own `--version` line, and the two paths worth knowing: where the app
state lives and where its log is.

Light or dark follows the system by default — live, so switching the macOS
appearance switches the window. Choosing Light or Dark under Settings stops
following; `app.json`'s `theme` field is what is stored (`system`, `light`,
`dark`).

## Failures

- **The swarm does not boot.** The tab shows *Could not start a swarm*, the
  folder, the engine's one-line reason, the server's log tail in a monospace box,
  and **Retry** and **Close**. That log is
  `~/.evo/desktop/tabs/<id>/swarm.log`.
- **A lane is down.** Its row shows `✗` and the reason in place of its task; the
  swarm restarts it, and the row comes back.
- **The connection drops.** The tab reconnects with `Last-Event-ID` and
  exponential backoff (0.5 s, doubling up to 10 s) and shows a reconnecting badge
  on the `main` row and above the transcript. If the supervisor restarts the
  coordinator, the same tab keeps working: the app notices the new event log and
  refetches state and transcript.
- **The server says no.** A refusal (`409`, busy) is a dim notice above the
  composer, never a modal; a command that ran and failed (`422`) shows the
  server's own error text. A POST the server never answered (`503`, or no reply
  at all) gets a plain sentence with the raw error on hover. Notices clear
  themselves after a few seconds.
- **A run ends badly.** The transcript says so in its own rows — compaction,
  provider retries, and the failure itself, once: the failing message carries it
  (`error: …`), and only a failure no message holds is said by the run's own
  outcome row (`Run failed: …`).

## Where things are written

The app keeps its own data in `~/.evo/desktop/`:

| path | what |
|---|---|
| `app.json` | window bounds, the recorded tab set, binary paths, recent sessions, theme |
| `lock`, `activate.sock` | the single-instance lock and its activation socket |
| `model-cache.json` | the last model catalog, for the empty tab's choosers |
| `probe/` | scratch folder used once to learn that catalog |
| `tabs/<id>/tab.json` | that tab's folder, resumed session, swarm id, models, workers |
| `tabs/<id>/token` | the swarm's bearer token, written by the server (0600) — never logged, never shown |
| `tabs/<id>/swarm.log` | the swarm's stdout and stderr |
| `app.log` | the app's own log, one line per event, each with a UTC timestamp |

Everything else it touches is read-only: the journals under `~/.evo/sessions`
(which is what the history list is), a swarm's own directory under
`~/.evo/swarm/<id>`, and the one managed block in a project's `.evo/swarm.lisp`
described above. Model names are never hardcoded: the catalog comes from a live
server, or from a one-off probe the first time the app runs (and again when the
cached catalog is more than a day old).

## Running it against a scripted model

`scripts/stub_home.sh` builds a throwaway `HOME` whose evo home registers the
stub provider — no API key, no network, nothing written to your real `~/.evo`:

```sh
scripts/stub_home.sh run -- ./dist/evo-desktop.app/Contents/MacOS/evo-desktop
```

The model answers `ok: <your text>` unless you script it in the prompt:

| prompt | what the model does |
|---|---|
| `CALL bash {"command": "ls"}` | one tool call to that tool, with that JSON |
| `SLOW hello` | streams 60 deltas, a tenth of a second apart |
| `DELAY2 hello` | waits 2 s, then answers |
| anything else | `ok: <the first 40 characters>` |

For a longer session use `start` (prints the exports and a `stop`), and see the
script's header for `EVO_STUB_MESSAGES`/`EVO_AGENT_REPO` if your `evo-agent`
checkout is somewhere else.

## Not here yet

- A command surface (slash commands) and lane control endpoints — deliberate
  v1 non-goals; lanes are watched, never typed to.
- Windows and Linux packaging: the bundle target is macOS.
- Settings covers the binaries and the theme and nothing else; there is no other
  preference to change.
