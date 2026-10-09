# Using evo-desktop

One window, one session per tab. A **tab** is one evo `serve` process —
`evo-swarm`'s coordinator plus a pool of worker lanes, or `evo-agent`'s single
agent — running in a folder you pick, spawned by the app and driven over one
loopback protocol: the ready file the child writes, one snapshot, one stream of
ops, and `POST /ops`
([`CONTRACT.md`](../CONTRACT.md)). Lanes are not addressed by the app at all:
they are the coordinator's own children, and their topics arrive mirrored in the
coordinator's stream; a single agent has none, and the page says so instead of
showing a count of nothing.

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
| `⌘V`, image on the clipboard | attaches it to the message instead of pasting (see the composer) |

The File, Edit and Window items are also in the menu bar. The Window menu carries
the three tab actions — Select Next Tab, Select Previous Tab, Select Last Tab —
which are the same actions as `⌃⇥`, `⌃⇧⇥` and `⌘9`; `⌘1`…`⌘8` exist only as
shortcuts.

Switching tabs takes the keyboard with it: the composer of the tab you land on
gets the caret, and an empty tab hands it to its first control — a keystroke
nobody hears is worse than one the window handles.

## Tabs

The title bar **is** the tab strip: one tab per session, labelled with the folder's
name — a tab nothing has been launched from says `New Swarm`, or `New Session`
while the workers switch is off — and hovering one shows the whole path plus what
the tab is doing — `no folder chosen`, `starting the session…`, `session: running`,
`session: reconnecting…`, `terminating the session…`, `failed to start`, or
`session gone: the server exited`. The words are the session's rather than a
swarm's because a tab may hold either program.

A middle click closes a tab, the way a browser's does. The `+` after the last tab
opens an empty tab — and when the strip already has one, it shows that one
instead: there is one empty tab at a time, and ⌘T is the same button. When the
tabs need more room than there is, they scroll inside the strip and the `+` stays
at the strip's right edge. Closing a tab stops its server; the session stays on
disk and comes back in the history list.

Two small marks answer questions a label cannot:

- The **dot** before a tab's name breathes while that tab's session is working —
  any lane at work, or the agent's own run (a single agent has no lanes), which is
  the same rule the composer's button follows. When a background tab's run *ends*,
  its dot turns to a full ring — "something happened here" — and looking at that
  tab is what clears it. A tab with nothing going on wears the ring quietly.
- The **window's own title** — what Mission Control and the app switcher show —
  names the tab being shown: `evo-gui — Evo Desktop`, or `Evo Desktop` alone on
  an empty tab.

## The empty tab: choosing a session

Every new tab opens on the empty page (`design/doc28/NewSwarm.tsx`): what the
launch will be, then the folder to run it in, then what can be resumed. Its
headline says which program the switch has chosen — `New Swarm`, or `New Session`
with **Use swarm** off, when the first card is titled `Main` rather than
`Coordinator` — and everything else on the page is true of either.

```
New Swarm
Choose how it runs, then select a project folder.

┌─ Coordinator ──────────────────────────┐   ┌────────────────────────┐
│ Model                                  │   │   [folder icon]        │
│ [ acme · deepseek-v4.1-flash        ▾ ]│   │   Select folder…       │
│ Effort                          medium │   │   The session starts   │
│ ──●─────────────────────────────────── │   │   in the folder you    │
└────────────────────────────────────────┘   │   pick                 │
                                             └────────────────────────┘
┌─ Workers ── Use swarm [●] ── Count ── 6 ─+ ─┐
│ Model                                  │
│ [ anthropic · claude-opus-4.5       ▾ ]│
│ Effort                          medium │
│ ──●─────────────────────────────────── │
└────────────────────────────────────────┘
  ⚠ config.file: no registration for … — fix it in Settings…

History  6 resumable
┌───────────────────────────────────────────────────────────────────────┐
│ 📁 evo-gui  [open at last quit]                                    ›  │
│    ~/coding/evo-gui · 11m ago                                         │
└───────────────────────────────────────────────────────────────────────┘
```

**Nothing here is called "Default".** Every control opens on what evo would run
*now*, and what it shows is exactly what the launch passes — so a page that shows
a model is a launch that runs it:

- **Coordinator — Model** is `--model ID@PROVIDER`: what `evo-swarm check --json`
  resolved, else `/catalog.default_model`, else the first registration the
  catalog says is `ready`. The menu lists every registration, `provider · id`,
  each with a detail line — `200k ctx · vision · effort low, medium, high, xhigh,
  max` — listing the levels **that** model takes, in the catalog's own order and
  in full (a provider's ladder need not be a run of levels, so `low, high, max`
  is three rungs and never a range); a model with no effort parameter simply ends
  the line at `vision`. A registration evo cannot reach is greyed with evo's own
  reason.
- **Coordinator — Effort** is `--thinking`, and **Workers — Effort** is
  `--lane-thinking`: the rungs `/catalog.thinking_levels` lists (the session's
  ladder; the drawer inside the box offers the chosen **model's** own, from its
  `effort_levels`, and says `not offered by this model` for one that takes no
  effort setting at all). Each opens on
  the effort `check` resolved — evo's own chain, journal and settings included,
  so a resumed swarm opens on the level it was running at; click, drag or use the
  arrows, and the level's name is beside the label.
- **Workers — Use swarm** is a switch, not a flag: on, the launch is
  `evo-swarm serve` — a coordinator and its pool of lanes; off, it is
  `evo-agent serve`, one agent, addressed as `Main` on the page that opens, and
  launched with the coordinator's `--model` and `--thinking` and nothing a lane
  would take. It is **this page's own**, for the launch this page makes: every
  empty tab opens with it on, and flipping it changes nothing outside the page in
  front of you — not the tab beside it, not a tab opened afterwards, and not the
  file the app remembers things in. Start the session and the tab leaves the page;
  come back to the page — a launch that failed, a session that ended — and it is a
  fresh page with the switch on again. Off, the Workers card's model, its effort and the count grey **in
  place** — the same boxes in the same places, taking no click, no keystroke and
  no tab stop, so nothing moves under the pointer that flipped it — and those are
  no longer the launch's own questions: with no `check` to ask (`evo-agent` has no
  such subcommand), the page reads `evo-agent catalog --json` for the list it
  shows them from.
- **Workers — Count** is `--workers`, 1–64, opening on the count `check`
  resolved — `--workers`, then the `swarm-workers` setting, then 6. Typing clamps
  to the range — `0`, `99` or a word lands on the nearest count — and `−`/`+`
  step one at a time.

- **Workers — Model** is `--lane-model`: what `check` resolved for a lane, else
  the default when a lane can register it, else the first registration a lane
  can. The list is the same catalog judged by `/catalog.lanes.models` — a model a
  lane could not register is greyed with evo's reason beside it. The app writes
  no project file: the lanes' model is a launch flag, and
  `<folder>/.evo/swarm.lisp` stays yours.
- **Select folder…** — the native folder dialog. Picking a folder starts the
  launch there and turns the tab into a tab page; cancelling leaves the tab empty.

A control the launcher resolved follows `check`: when the answer changes, the
page moves the control onto it. A control **you** moved is yours, and stays where
you put it. The one exception is the frame before the first `check` comes back,
where the sliders sit on the middle rung and the count on evo-swarm's own 6.

With **Use swarm** off there is nothing to follow — `evo-agent` has no `check` —
so the fields stand on one agent's own catalog instead: its default registration,
or the first one it says is ready.

Under the cards, one calm line each: what `evo-swarm check --json` found wrong
with the launch the controls describe — a model evo cannot reach, a lane that
cannot register one, a missing key — and, when the catalog itself could not be
read, what the page does without it. A line is clickable: it puts the keyboard on
the control it is about, or opens Settings when it is about the machine (the
binary that is not there). The check runs again on every model change, so these
lines describe what **Select folder…** would actually start.

With **Use swarm** off there is no check to run, so the one line is evo's own
`reason` for the registration the launch resolved — a model it cannot reach — and
nothing is said about lanes, the count or the effort: those are `check`'s answers,
and a single agent is never asked for them. Each read's trouble keeps its own
words, so what is under the cards is always about the list this launch would
really run on.

The tab's models are fixed when the session starts — the tab page has no model
selector, only the readout that shows what is running.

### Keyboard on the empty tab

Tab walks the page in the order it is read: the Coordinator's model field and
effort slider, the Workers' **Use swarm** switch and — while it is on — their
count, model and slider, the folder card, then the history rows. The two model
fields open with `Enter`, `Space` or an arrow, the arrows walk the options,
`Enter` picks, `Esc` closes the menu without leaving the
tab; the count box takes digits; a slider takes the arrows, `Home` and `End`; the
switch takes `Enter` or `Space`, and a Workers control it turned off is not a tab
stop at all; the folder card and every history row are buttons, so `Enter` is
what a click is.
A focused model field or count box draws the design's ring — a primary border
with a 2px muted halo — and a focused slider rings its thumb.

### History

The history lists every resumable session `evo-agent sessions --json` reports —
any folder, not only the ones this app started, and either program: a swarm's
coordinator or a single agent. A lane's own journal is not a row: a lane belongs
to the swarm that started it, and the swarm's session is the row for it. The list
is merged with the app's own recent sessions, newest first. The app reads no
journal: evo keeps an index and prints it. A row leads with the session's own
title (its first user text) and, under it,
the `~`-shortened folder with how long ago it ran, joined by `·`; the kind it is
wears its own mark — `Agent session` or `Swarm session` — and a
session the app had open when it last quit wears an `open at last quit` pill
beside its title, and the `›` at the row's end says what a click does. Hovering shows the
full path, the journal's name, the absolute time, the models and the lane count.

Click a row (or press `Enter` while it has the keyboard) to open it as a new tab:
it resumes that session (`--resume <that exact journal>`) in the folder it
ran in and **with the program that wrote it** — an agent session opens as one
agent, a swarm's as a swarm, because only the program that wrote a journal can
open it — so two tabs in one folder can never cross sessions. The list scrolls
inside the height that is left, under the fixed head and cards; and it has three
quiet states of its own: `Looking for sessions…` while the index is read, `No
sessions to resume yet.`, and the read's error in words. The calm one is what a
first run shows: there is nothing to resume yet, which is not a failure.


## The tab page

```
┌──────────────────────┬────────────────────────────────────────────┐
│ Lanes 2 of 6 busy    │ Coordinator                                │
├──────────────────────┼────────────────────────────────────────────┤
│ ● Coordinator        │        the agent's transcript              │
│ ◐ Lane 1 the view …  │                                            │
│ ○ Lane 2        idle │  ┌──────────────────────────────────────┐  │
│ ✗ Lane 3        down │  │ Todos 1/3         ⌃                  │  │
│                      │  │ Message the coordinator…             │  │
│ ~/coding/evo-gui     │  │ (Enter to send, Shift+Enter)         │  │
│                      │  │ [stub-a medium] [ctx 48k/936k]       │  │
│                      │  │                     [ ↑ Send ]       │  │
│                      │  └──────────────────────────────────────┘  │
└──────────────────────┴────────────────────────────────────────────┘
```

### Left — the agents

The column opens with its own band — `Lanes`, and `2 of 6 busy` at the other
end — and then the `Coordinator`, followed by one row per lane: a status
glyph, the task it is on (truncated), and the state — `idle`, or how long the
current step has taken while it works. The `Coordinator` row says `waiting on
lanes` while it is held for them. A lane that is down shows the reason in place
of its task; hovering any row gives the whole story. While a lane is working its
row carries a small **Stop** — the one thing a person may do
to a lane (`run.interrupt`, scope `lane`); the coordinator is told, and steering
the lane stays its job.

A **single-agent session** has no lanes, so the band keeps its `Lanes` title and
carries no count at all — a count of lanes a single agent does not have is not a
thing to read — and the column is its
one row, `Main`: no lane rows, whatever a swarm's snapshot left behind. The row's
state is the session's own, `idle`/`running`/`compacting`, and the transcript
beside it is that one agent's.

| glyph | meaning |
|---|---|
| `●` | working |
| `◐` | compacting |
| `○` | idle |
| `◌` | starting |
| `✗` | down — the row says why |
| `◦` | stopped |

Clicking a row selects that agent: it changes what the conversation shows and
nothing else. The keyboard walks the rows with `↓`/`↑` and jumps to the ends
with `Home`/`End`. Input always goes to the session's own agent — the coordinator
in a swarm, the one agent in an `evo-agent` session, which is the only row there
is. The folder the session runs in is pinned at the bottom of the column, one
line, `~`-shortened where it
is under the home directory and trimmed from the front — whole directories at a
time, so a name is never cut in half: `…/project`, or
`~/…/gui-model/crates/workspace/src`. Hovering shows the whole path.

### The transcript

A slim header names the agent being shown (`Coordinator`, `Main` in a
single-agent session, or `Lane 1`) with the same status glyph the left column
uses, the task that agent was given, and — when that
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

**Your turn's pictures** are drawn under its words as thumbnails, and a click
opens one at full size (a second click shrinks it back). The bytes come from the
session itself (`GET /media/<id>/<n>`), fetched while the row is on screen, so a
picture sent in a session you resume is there again; one still on its way says
`name — loading…` where the thumbnail will be, and one the session cannot produce
says `name — could not be shown` rather than leaving a hole.

**Your turn's files** are drawn under its words as chips, one per file: the file's
own name, the whole path a hover away, and a click opens the file where it is. A
file that has since moved or gone is still named, dimmed, and opens nothing. The
message itself carries the paths — one `- /abs/path` line per file under
`Attached files:`, after your words — and the transcript reads that block back
instead of showing you the line written for the agent.

Scrolling back reaches the whole session, not the model's current context: the transcript holds
every item the session has and draws it with a virtual list, so only the rows you can see are
built (a journal of thousands of items scrolls like a short one). Older items behind the oldest
one it holds are fetched from the server by the list itself — one page at a time, while the
session still has more — and a quiet `Loading earlier items…` line at the head says so while a
page is on its way; there is nothing to press, and a page that arrives is spliced in above you
without moving what you are reading.

The markdown is the subset a model actually writes — headings, lists, task lists,
tables, quotes, rules, inline code — with four decisions worth knowing:

- a **fence** is coloured for the grammars the app ships (`rust`, `python`,
  `javascript`, `typescript`, `bash`, `json`, `toml`) and plain otherwise;
- an **image reference** can show a local file: `file://`, absolute, `~/` or
  project-relative paths. Reads and decoding run off the UI thread; decoded frames
  share a bounded, view-owned memory cache. Closing the view releases its cache.
  Remote or unreadable references keep the `[image: alt]` fallback. No remote
  images are fetched and no disk cache is written;
- **raw HTML** is shown as its source, mono and muted — markup reads as markup;
- a **link** opens only `http`, `https`, `mailto` or a named path that is really there;
  `/` itself is never opened even though it exists.
  a `file:`, `javascript:` or bare-fragment target is left alone.

The same is true of the rows that say something in a person's or an agent's own words —
your turns, an assistant's prose, a notice, a lane's line, a report's field, the body of
a quiet row. An address or a path in them is underlined and pressable, including a path
written as `inline code` and including the `:42` (or `:42:7`) line at the end of one.
Pressing an address opens it in its own application — the browser, the mail client —
and pressing a path opens the file, or the folder in the Finder. A path is read the way
the tab is: `~/…` from your home, and a relative path from the folder the tab runs in,
so `crates/transcript/src/lib.rs` in a tab on `~/coding/evo-gui` opens that file, and a
tab that moves re-reads its rows. A path that is not there stays words: the transcript
asks the disk once per path and remembers the answer for a few seconds, so scrolling
never stats the world.

What a **tool call** says is not prose: its arguments and its result are drawn exactly as
they came, and no address or path in them is a link. They are data a model chose, not the
app's own words. A tool call's payload is expanded as rows: a nested object is indented
under its key, a long list gets a row per item, and past four levels (or for an array of
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
`Ask the coordinator to get started` — it plans the work and hands tasks to its
lanes — for a swarm's coordinator, `Ask the agent to get started` for the one
agent of a single-agent session, which does the work itself, and
`Lane 1 hasn't been given work yet.` for a lane.

### The composer, at the foot

The composer sits under the transcript, on the same reading measure, and it is
the selected agent's: a box with the goal strip across its top (a click on the
row folds the goal's objective out under it — never the goal's own id, which is
evo's handle on it — and the strip is hidden when that agent has no goal), the
todo strip under it (`Todos 1/3`, and a click on the row folds the list out under
it — `☑` done, `◐` in progress, `☐` pending — hidden when that agent has no
todos), the attachments strip under that while your draft carries anything
(`Attachments 2`, and a click folds the tiles out: the picture, the file's name
under it, and a `×` that takes that one back off the message), then the drawer a
chip folds out, then the input, then one row of chips, the `+` and the button.

The chips are the server's own status line for that agent, one chip per segment
and in its order — the model with its effort beside it, the context, and how much
of the input was read from cache; a segment the server does not publish has no
chip. The server's `goal` segment is the one segment the box does not chip: the
goal's own strip states it, and the id that segment spells out is not drawn
anywhere. The model chip folds out the model and effort drawer inside the box; a
click outside the box, or selecting another agent, folds it back — while the goal
and todo strips stay where their own rows put them. The model and effort are the
coordinator's own to change — a lane's chip states what the swarm runs, and the
drawer says so.

**Attachments ride with the message**, and there are three ways to add one: the
**`+`** beside the Send button opens the platform's file dialog (several files at
a time; never a folder), **⌘V** with an image on the clipboard attaches that
image instead of pasting it — text on the clipboard still pastes as text — and
files dragged onto the box are attached the same way. The first one unfolds the
strip; after that it opens and closes as you leave it. Each tile is the picture
(a file glyph when there is no picture to draw), the file's name under it,
truncated when it is long with the whole of it on hover, and a `×` at its
top-right that takes it off the message — the one way back from a file picked by
mistake. Adding one sends nothing, and a message carrying one is never a slash
command, however it begins: a command has nowhere to put a file.

What the agent gets depends on what the file is:

- An **image** — a file whose name says image, or whose own first bytes do, so a
  screenshot with a name of its own is one — rides with the turn. evo reads it,
  sniffs its type and keeps its bytes in the session, so it is in the transcript
  under your words and it is there again when you resume, even if the file has
  moved or gone by then. A pasted image has no file name to offer, so it is named
  `pasted image.png`.
- Anything **else** — a `.csv`, a PDF, a file of source — is not sent as bytes
  at all: its absolute path goes into your message, one line per file under
  `Attached files:`, after your own words. The agent reads it with its tools if it
  wants to, so a file it cannot read is a file it says it cannot read rather than
  an attachment that vanished. A file needs no words: a draft with an attachment
  and nothing typed sends the file block alone.

An image evo cannot read — a path that is not there, or bytes that are not a
picture — is refused: nothing is sent, the refusal is a dim notice above the box
in evo's own words, and the draft and its tiles are still there, so you can take
the picture out or attach another and send the same message again.

The input grows from two rows to half the pane, and scrolls after that.
**Enter** sends, **Shift+Enter** is a newline, **Esc** interrupts the
coordinator's turn. An empty box says what it is addressed to: `Message the
coordinator…` in a swarm, and `Message the agent…` in a single-agent session,
where there is no coordinator to name. An empty input's **↑** walks
back through the prompts this tab has sent (**↓** walks forward again), so a
prompt can be sent twice without retyping it; typing anything makes the recalled
text a draft like any other. **⌘C** with nothing selected in the input copies the
window's own selection instead, so a passage picked in the transcript can be
copied while the caret sits in the input.

**A word completes while it is typed**, as it does in the TUI and in an editor. What
the caret is on — whether it is in a `/command` word, or inside `/eval`'s content —
is evo's own answer (`complete`): the box asks with its text and its caret and the
server says what a candidate would replace, which is why `/usr/local/bin` offers
nothing and a `/word` inside a string in `/eval` is a Lisp token and never a
command. That question is a read, and a round trip away, so the box asks once per
rest of the caret and holds the answer against the text it was asked about: the
list is drawn over the word's own line, with every row's label beginning where the
word does and every description whole (the list is as wide as its widest row, and
only past 560 px does a description give). The characters the word matched are
drawn heavier, so it is clear why a row is there: `/re` offers `/reload` as a
prefix match and `/lore` as a subsequence one. `↑`/`↓` walk the rows and wrap,
**Tab** or **Enter** takes the highlighted one — **Enter takes the row instead of
sending the message** — and **Esc** puts the list away until the word changes. A
word started near the right edge of the box pulls the list back inside the window
rather than off it. A command is taken with a space after it
when it is at the message's own start, so its arguments can be typed straight on.
The list is the server's registry, so extension commands and skills
(`skill:lark-doc`, say) are in it exactly as evo has them, and a command evo drops
stops being offered.

Inside `/eval <form>` the word completes against the **live image** instead:
`/eval (evo.eval:` lists that package's own names, each with what it is
(`function`, `variable`, `macro`) — the running image is the only thing that knows
what its own packages hold, and the same `complete` op is what asks it. Taking a
row replaces just the token, leaving the rest of the form — closing parens and all
— where it is. A list is never drawn when there is nothing to choose: one
candidate that is the word already typed would only show the reader their own
input back, and would capture the `↑`/`↓` the history browses with. Recalled
history raises nothing either, until it is edited.

A message that **is** one slash command from its start goes to the command layer
(`command.run`), not to the coordinator — `/help`, `/compact`, `/eval (+ 1 2)`, an
extension's own command — which is exactly what the TUI does with the same line,
and the server says `not found` for a word it does not know. A message that merely
mentions one (`run /help now`) is the reader's words, as is one that begins `//`.
The command's own output (`/eval`'s `⇒ 3`, a command's notes) arrives as the
session's `notice` items in the transcript, which is where the TUI prints it too —
the reply to `command.run` carries the same lines beside them, and the app draws
the transcript alone rather than saying everything twice.
A command that hands a message back — `/rewind`, `/tree <id>` on a user message —
puts that message in the input, as the TUI does: it is the draft again, ready to be
edited and resubmitted.

Sending while the agent is working is normal: the text is queued and lands at the
running turn's next boundary, exactly as typing into the TUI does — the queued
row is in the transcript, cancellable, until then. The draft is cleared only when
the server accepted the send — its attachments go with it — and **Esc never
touches it**.

The single button's face is what it does: **Send** while nothing is going on, **■
Stop swarm** while anything is — the coordinator running its own turn, held while
its lanes work, or a lane still working (`run.interrupt`, scope `swarm`: it stops
everyone, and the coordinator is told). Send is drawn in the design's primary face
whatever the draft, so an empty input is a button that looks ready and has nothing
to send: clicking it (or pressing **Enter**) does nothing. A message that is still
on its way — `input.send` posted, the server not yet answered, which for a turn
carrying a screenshot is half a minute — reads **Sending…** with the kit's spinner
on that same button, and the draft stays in the box where the reader left it:
visible, selectable, copyable, and read-only, so what is already being sent cannot
be edited under it. That state never takes the run away from the reader: `Esc`, and
the Stop face, still stop the agent while the message is in flight. There is never a
Send and a Stop side by side; `Esc` is the
same interrupt with the keyboard, and **Enter** still sends while the swarm is busy
(that is what the queue is for). A lane can be stopped from its own row in the left column.

A **single-agent session** has no swarm to stop, so there the same button reads **■
Stop** while its run is going on, and clicking it (or `Esc`) posts
`run.interrupt` with scope `session` — one agent's own run is the only run there
is, and the only scope its server knows.

The chips row is that agent's status line, chipped: the `segments` the server
publishes for it, in its order, so the same session reads the same here and in the
TUI. A core registry builds them — model, thinking, context, goal — and an
extension's own segment (a project's `cache-stats`, say) arrives the same way, as
a chip of its own. A segment the server does not publish has no chip, so a session
with no cache activity shows no cache chip, which is the whole of that agent's
status line and not a row with holes in it. The `goal` segment is the exception:
the goal has the strip of its own, so its segment is not chipped.

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

- **Nothing boots.** The tab shows *Could not start a session* (either program), the
  folder, the engine's one-line reason, the server's log tail in a monospace box,
  and **Retry** and **Close**. That log is
  `~/.evo/desktop/tabs/<id>/swarm.log`.
- **A lane is down.** Its row shows `✗` and the reason in place of its task; the
  swarm restarts it (`lane_event: restarted` on the coordinator's topic) and the
  row comes back.
- **The connection drops.** The tab reconnects with a backoff and shows a
  reconnecting badge on the `Coordinator` row and above the transcript. A server that
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
| `app.json` | window bounds, the recorded tab set, binary paths, the workers switch, recent sessions, theme |
| `lock`, `activate.sock` | the single-instance lock and its activation socket |
| `model-cache.json` | the last `catalog --json` body, for the empty page's controls |
| `tabs/<id>/ready.json` | where the server publishes its port, URL and bearer token (0600) |
| `tabs/<id>/swarm.log` | the server's stdout and stderr (either program's) |
| `app.log` | the app's own log, one line per event, each with a UTC timestamp |

The token in `ready.json` is never logged and never shown. Nothing else is
written: the app reads no journal and edits no project file. What it shows comes
from evo itself — `evo-swarm catalog --json` for the controls (at startup, and
again whenever the binaries change), `evo-agent catalog --json` for them instead
while **Use swarm** is off, `evo-agent sessions --json` for the history
list, `evo-swarm check --json` for the problem lines, and the running server for
everything on the tab page. Model names are never hardcoded.

## Running it against a scripted model

`scripts/stub_home.sh` builds a throwaway `HOME` whose evo home registers the
stub provider — no API key, no network, nothing written to your real `~/.evo`:

```sh
scripts/stub_home.sh run -- "./dist/Evo Desktop.app/Contents/MacOS/evo-desktop"
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
