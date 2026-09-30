# Using evo-desktop

One window, one tab per swarm. A **tab** is one `evo-swarm serve` process — a
coordinator agent plus a pool of worker lanes — running in a folder you pick,
spawned by the app and driven over one loopback protocol: the ready file the
child writes, one snapshot, one stream of ops, and `POST /ops`
([`CONTRACT.md`](../CONTRACT.md)). Lanes are not addressed by the app at all:
they are the coordinator's own children, and their topics arrive mirrored in the
coordinator's stream.

## Starting, and the second launch

Launch `evo-desktop` (or `cargo run -p evo-desktop`). Only one instance ever
runs: launching it again raises the running window and exits 0, so a second
`open` from the Dock does not give you a second window. Every launch opens **one
empty tab**, whatever the last session left behind: the tab set is recorded in
`app.json` but not reopened, and the sessions that were open then come back at
the top of the empty tab's history wearing an `open at last quit` badge. The app
never starts a swarm by itself.

Closing the window (or `⌘Q`) is the whole quit sequence: `app.json` is written,
every tab's server is stopped by closing the pipe it was started with
(`--watch-stdin`, so EOF is immediate and needs no signal), and the process
exits. Nothing is left
running if the app is killed instead: the pipe closes with the process, which is
what tells each swarm its tab is gone.

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
name (an unused tab says `New tab`), and hovering one shows the whole path plus
what the tab is doing — `no folder chosen`, `starting the swarm…`, `swarm:
running`, `swarm: reconnecting…`, `stopping the swarm…`, `failed to start`, or
`swarm gone: the server exited`.

A middle click closes a tab, the way a browser's does. The `+` after the last tab
appends a new empty tab; when the tabs need more room than there is, they scroll
inside the strip and the `+` stays where it is. Closing a tab stops its server;
the session stays on disk and comes back in the history list.

Two small marks answer questions a label cannot:

- The **dot** before a tab's name breathes while that tab's swarm is working —
  any lane at work, or the coordinator's own run, which is the same rule the
  composer's button follows. When a background tab's run *ends*, its dot turns to
  a full ring — "something happened here" — and looking at that tab is what
  clears it. A tab with nothing going on wears the ring quietly.
- The **window's own title** — what Mission Control and the app switcher show —
  names the tab being shown: `evo-gui — Evo Desktop`, or `Evo Desktop` alone on
  an empty tab.

## The empty tab: choosing a swarm

```
New swarm
Pick models, then a folder

Coordinator model  [ Default ▾ ]            ┌────────────────────────┐
Lanes model        [ Default ▾ ]            │  [folder icon]         │
Workers            [ Default ▾ ]            │  Select folder…        │
                                            │  The swarm starts in   │
                                            │  the folder you pick   │
                                            └────────────────────────┘
  ⚠ lane_model_not_found: no registration … — fix the lanes model

History  6 resumable
  evo-gui  [open at last quit]
           ~/coding/evo-gui · 6 lanes · 11m ago · coordinator: ark-…
  foo      ~/coding/foo     · 4 lanes · 2h ago  · coordinator: …
```

- **Coordinator model** — passed to the swarm as `--model ID@PROVIDER`. On
  **Default** (`evo's own default`) nothing is passed and evo's own default
  applies. The menu lists the models the catalog knows, sorted by provider then
  id, each with a detail line — `200k ctx · vision · reasons`. A model
  registered under several providers is listed per registration, and the option
  carries the `id@provider` the flag will be given.
- **Lanes model** — passed as `--lane-model ID@PROVIDER`, which is what the
  lanes register. On **Default** (`follows the coordinator`) nothing is passed
  and every lane inherits the coordinator's model. The list is the same catalog
  filtered by `/catalog.lanes.models`: a model a lane could not register is
  greyed with evo's own reason beside it. The app writes no project file — the
  lanes' model is a launch flag, and `<folder>/.evo/swarm.lisp` stays yours.
- **Lanes thinking** — `--lane-thinking`, the effort the lanes start at; Default
  follows the coordinator's.
- **Workers** — `--workers`, 1–64. On **Default** evo's own `:swarm-workers`
  setting decides (else 6). Only a swarm created from a folder takes this count:
  a resumed swarm keeps the lane count its own record kept.
- **Select folder…** — the native folder dialog. Picking a folder starts the
  swarm there and turns the tab into a tab page; cancelling leaves the tab empty.

Under the choosers, one caption carries what has something to say: `Loading
models…` until the catalog is known; `Couldn't load the model list — Default
models will be used.` (or `Couldn't refresh the model list — using the last one
it loaded.` when a cached catalog is already in the choosers), with evo's own
error in the hover; and, when `evo-swarm check --json` judges the launch the
choosers describe, one calm line per problem — the lanes model that is not
registered, a missing key, a worker count evo refuses. A problem line is
clickable: it opens the chooser it is about, or Settings when it is about the
machine. The check runs again on every chooser change, so these lines describe
what **Select folder…** would actually start.

The tab's models are fixed when the swarm starts — the tab page has no model
selector, only the readout that shows what is running.

### Keyboard on the empty tab

Tab moves the keyboard between the three choosers, the folder drop target and the
history list (the target and the history frame draw a hairline focus ring while
they hold it). A chooser opens with `Enter`, `Space` or an arrow; the arrows walk
its options, `Enter` picks, `Esc` closes it without leaving the tab (and without
choosing). The history frame takes the arrows and `Home`/`End`; `Enter` resumes
the selected row, the same as clicking it.

### History

The history lists every resumable swarm `evo-agent sessions --json` reports —
any folder, not only the ones this app started — merged with the app's own recent
sessions, newest first. The app reads no journal: evo keeps an index and prints
it. A row leads with the session's own title (its first user text) and the
folder's name under it, then what is known about the session, joined by `·` —
the lanes, how long ago, the coordinator's model (`4 lanes · 2h ago ·
coordinator: ark-deepseek-v4.1-flash`); each part is left out when it is unknown.
A session the app had open when it last quit wears an `open at last quit` pill.
Hovering shows the full path, the journal's name, the absolute time, and the
models.

Click a row (or select it and press `Enter`) to open it as a new tab: the swarm
resumes that session (`--resume <that exact journal>`) in the folder it ran in,
so two tabs in one folder can never cross sessions. The list has three quiet
states of its own: `Scanning sessions…` while the index is being read, `No
resumable swarms yet`, and the read's error in the danger colour. The calm one is
what a first run shows: there is nothing to resume yet, which is not a failure.

## The tab page

```
┌──────────────────────┬────────────────────────────┬──────────────────┐
│ 6 lanes · 2 busy     │ ● main  delegate the …     │ ┌──────────────┐ │
│ ● main               │                            │ │ input        │ │
│ ◐ Lane 1       [Stop]│ ◐ Lane 1  SLOW: walk …     │ └──────────────┘ │
│ ○ Lane 2             │    transcript (markdown,   │                  │
│ ✗ Lane 3             │    rendered live as it     │    [■ Stop swarm]│
│                      │    streams)                │                  │
│                      ├────────────────────────────┤                  │
│ ~/coding/evo-gui     │ ☑ todos of the agent shown │                  │
│                      │ model · medium · ctx 48k…  │                  │
└──────────────────────┴────────────────────────────┴──────────────────┘
```

### Left — the agents

The column opens with a summary — `6 lanes · 2 busy` — and then `main`, the
coordinator, followed by one row per lane: a status glyph, the task it is on
(truncated), and how long the current step has taken. A lane that is down shows
the reason in place of its task; hovering any row gives the whole story. While a
lane is working its row carries a small **Stop** — the one thing a person may do
to a lane (`run.interrupt`, scope `lane`); the coordinator is told, and steering
the lane stays its job.

| glyph | meaning |
|---|---|
| `●` | working |
| `◐` | compacting |
| `○` | idle |
| `◌` | starting |
| `✗` | down — the row says why |
| `◦` | stopped |

Clicking a row selects that agent: it changes what the centre column shows and
nothing else. The keyboard walks the rows with `↓`/`↑` and jumps to the ends
with `Home`/`End`. Input always goes to the coordinator. The folder the swarm
runs in is pinned at the bottom of the column, `~`-shortened, elided in the
middle, with the full path on hover.

### Centre — the transcript, and the todos

A slim header names the agent being shown (`main`, `Lane 1`) with the same status
glyph the left column uses, the task that agent was given, and — when that
agent's transcript carries thinking text — a **Show thinking** button that
reveals it (**Hide thinking** puts it back). The toggle is per agent: switching
agents shows each transcript the way you left it.

Below it, the agent's items. Every item is keyed by the id the server minted, so a
streaming row keeps its identity when a reconnect or a re-snapshot arrives: your
turns; the assistant's text rendered as markdown *while it streams*, so a
half-finished message already reads as the finished one will; tool calls and
results as one-line `name — ok/error` rows that open onto their arguments and
result (a whole result the snapshot truncated comes back from `GET /items/<id>`
when you open it); a lane's report as a `Lane 1 report` card; notices, run
outcomes and command notes as their own quiet lines. Rows are a reading surface —
drag to select, `⌘C` to copy — and an assistant message carries a copy icon for
its markdown source (it appears on hover, at the top of the message, and answers
`Copied` when pressed), with a separate **Copy** on every fenced code block for
the code alone.

Scrolling back pages older items in from the server, across compactions: the
scrollback is the whole session, not the model's current context.

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

Some rows in a transcript are not yours even though evo sent them in your voice,
and each is drawn as what it is — a quiet line that opens on click, never a turn
of its own. Their kind comes from the server, not from their words:

- **Context** — what an extension injected into the session (your global and
  project memory, IDE context): `Context · global memory`;
- **Goal** — evo keeping a goal going: `Goal · continue — <objective> · <budget>`,
  `Goal · objective updated — <objective>`, `Goal · budget exhausted — wrap up`;
- **Command** — an extension answering a command you ran mid-run (`/memory`,
  `/global-memory`, `/lore`, `/notify doctor`): `Command · /global-memory`. The
  swarm's own notices arrive the same way today, because they go out through the
  serve's command channel and carry `source: "command"`; a lane's report shows once
  as this line and once as its own card, below.
- **the swarm about a lane** — a lane's report is a `Lane 1 report` card, and
  `run ended`, `failed to start`, `is down` and the like are `Lane 1 · …` lines,
  red when something failed;
- **a person stopped something** — `Stopped Lane 1` / `Stopped the run`, said by
  the coordinator's own `human_action` item, so nothing is stopped behind its
  back.

A turn you send while the agent is working is drawn as a held-back card: it says
`queued · sent at the next step`, and it carries a **Cancel** until the
coordinator takes it. Cancelling removes the row (the input was never journaled);
once it is taken, the card becomes the turn it is.

When you scroll away from the bottom the view stops following and offers a
**Jump to latest** button. An agent with nothing to show says so in its own words:
`Ask the coordinator to get started` for `main`, and
`Lane 1 hasn't been given work yet.` for a lane.

Under the transcript, the todos of the selected agent (`☑` done, `◐` in
progress, `☐` pending), hidden when that agent has none.

### Right — the input, and one button

The input grows from two to eight rows. **Enter** sends, **Shift+Enter** is a
newline, **Esc** interrupts the coordinator's turn. An empty input's **↑** walks
back through the prompts this tab has sent (**↓** walks forward again), so a
prompt can be sent twice without retyping it; typing anything makes the recalled
text a draft like any other. **⌘C** with nothing selected in the input copies the
window's own selection instead, so a passage picked in the transcript can be
copied while the caret sits in the input.

Sending while the agent is working is normal: the text is queued and lands at the
running turn's next boundary, exactly as typing into the TUI does — the queued
row is in the transcript, cancellable, until then. The draft is cleared only when
the server accepted the send, and **Esc never touches it**.

The single button's face is what it does: **Send** while nothing is going on
(enabled once there is text), **■ Stop swarm** while anything is — the coordinator
running its own turn, held while its lanes work, or a lane still working
(`run.interrupt`, scope `swarm`: it stops everyone, and the coordinator is told).
There is never a Send and a Stop side by side; `Esc` is the same interrupt with the
keyboard, and **Enter** still sends while the swarm is busy (that is what the queue
is for). A lane can be stopped from its own row in the left column.

The status line sits at the foot of the centre column: the `segments` the server
publishes for the agent being shown, in its order, so the same session reads the
same here and in the TUI. A core registry builds them — model, thinking, context,
goal — and an extension's own segment (a project's `cache-stats`, say) arrives the
same way. The line is truncated with an ellipsis when the column is too narrow;
hovering shows the whole of it.

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
- New tabs use the paths you saved, and the catalog is read again with them; a
  tab that is already running keeps the binaries it started with, as the panel's
  note says.

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
  swarm restarts it (`lane_event: restarted` on the coordinator's topic) and the
  row comes back.
- **The connection drops.** The tab reconnects with a backoff and shows a
  reconnecting badge on the `main` row and above the transcript. A server that
  restarted answers with another epoch, which is a `stream.reset`: the tab reads
  everything again and keeps working — and because it re-reads, a restarted
  swarm cannot leave half of two conversations on screen.
- **The server says no.** An op that refuses (`busy`, `model_not_ready`,
  `already_sent`, …) is a dim notice above the composer, never a modal, carrying
  the server's own sentence. Notices clear themselves after a few seconds.
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
| `model-cache.json` | the last `catalog --json` body, for the empty tab's choosers |
| `tabs/<id>/ready.json` | where the server publishes its port, URL and bearer token (0600) |
| `tabs/<id>/swarm.log` | the swarm's stdout and stderr |
| `app.log` | the app's own log, one line per event, each with a UTC timestamp |

The token in `ready.json` is never logged and never shown. Nothing else is
written: the app reads no journal and edits no project file. What it shows comes
from evo itself — `evo-swarm catalog --json` for the choosers (at startup, and
again whenever the binaries change), `evo-agent sessions --json` for the history
list, `evo-swarm check --json` for the problem lines, and the running swarm for
everything on the tab page. Model names are never hardcoded.

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
checkout is somewhere else. The same script is what `crates/proofs` uses in
Rust (`docs/proofs.md`).

## Not here yet

- A command surface (slash commands): everything else is here, but a slash
  command typed into the input is sent to the coordinator as text, exactly as
  evo's TUI would receive it, rather than offered as a palette.
- Steering a lane. A person can stop a lane or the whole swarm, and the
  coordinator is told; redirecting a lane stays the coordinator's job
  (CONTRACT §7.4).
- Windows and Linux packaging: the bundle target is macOS.
- Settings covers the binaries and the theme and nothing else; there is no other
  preference to change.
