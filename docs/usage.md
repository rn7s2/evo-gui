# Using evo-desktop

One window, one tab per swarm. A **tab** is one `evo-swarm serve` process —
a coordinator agent plus a pool of worker lanes — running in a folder you pick,
spawned by the app and driven over evo's HTTP API. Lanes are never addressed
directly: their tokens and ports stay inside the coordinator.

## Starting, and the second launch

Launch `evo-desktop` (or `cargo run -p evo-desktop`). Only one instance ever
runs: launching it again raises the running window and exits 0, so a second
`open` from the Dock does not give you a second window. The app never starts a
swarm by itself — it opens one empty tab.

Closing the window (or `⌘Q`) is the whole quit sequence: every tab's
swarm is stopped (a clean `POST /shutdown`, then `SIGTERM`, then `SIGKILL`, on
threads of its own), `app.json` is written, and the process exits. If the app
crashes, nothing is left running — the swarms are told this app's pid at launch
and stop with it.

## Tabs

The title bar **is** the tab strip: one tab per swarm, labelled with the
folder's name (hover for the full path and the swarm's state), each with a close
button. The `+` at the end of the strip appends a new empty tab. Closing a tab
shuts its swarm down; the session stays on disk and comes back in the history
list. The app always keeps at least one tab: closing the last one leaves a fresh
empty tab.

## The empty tab: choosing a swarm

```
Coordinator model  [ Default ▾ ]
Lanes model        [ Default ▾ ]      [ Select folder… ]
Workers            [ Default ▾ ]
──────────────────────────────────────────────────────
History
  foo            4 lanes · 2h ago · coordinator: ark-deepseek-v4.1-flash
  bar            6 lanes · yesterday
```

- **Coordinator model** — passed to the swarm as `--model`. On **Default**
  nothing is passed and evo's own default applies.
- **Lanes model** — what a *lane* starts with. evo-swarm has no flag for this,
  so the choice is written into the project's `.evo/swarm.lisp` (below). On
  **Default** nothing is written and every lane inherits the coordinator's model.
- **Workers** — how many lanes, 1–64. On **Default** evo's own `:swarm-workers`
  setting decides. Only a swarm created from a folder takes this count: a
  resumed swarm keeps the lane count its journal records.
- **Select folder…** — the native folder dialog. Picking a folder starts the
  swarm there and turns the tab into a tab page. Cancelling leaves the tab empty.

The tab's models are fixed when the swarm starts — the tab page has no model
selector, only the readout that shows what is running.

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
decides what the next lane initialization gets. The note under the chooser names
the exact file. Choosing **Default** removes the block, and the file itself if
that was all it held.

The list below offers the models a lane can actually register. A lane that
cannot register its model fails to initialize, so a model whose API only a
userspace extension defines is marked unavailable there — reach it through your
own `swarm.lisp` if you want it in the lanes.

### History

The history lists every resumable swarm on disk — any folder, not only the ones
this app started — merged with the app's own recent sessions, newest first. A
row shows the folder, its lane count, when it was last used, and the coordinator
model when the journal says which one. Click a row to open it as a new tab: the
swarm resumes that session (`--resume`) in that folder, and the lane count comes
from the journal (the workers chooser does not apply to it).

## The tab page

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

### Left — the agents

`main` is the coordinator; then one row per lane, with a status glyph, the task
it is on (truncated) and how long the current step has taken.

| glyph | meaning |
|---|---|
| `●` | working |
| `◐` | compacting |
| `○` | idle |
| `◌` | starting |
| `✗` | down — hover for the reason (the lane's own `output` lines say why) |

Clicking a row selects that agent: it changes what the centre column shows and
nothing else. Input always goes to the coordinator.

### Centre — the transcript, and the todos

The selected agent's transcript: your turns; the assistant's text rendered as
markdown *while it streams*, so a half-finished message already reads as the
finished one will; tool calls and results as one-line `name — ok/error` rows;
a lane's `report` as a report row; `output` and status events as dim lines.
When you scroll away from the bottom the view stops following and offers a
**Jump to latest** button.

Under it, the todos of the selected agent (`☑` done, `◐` in progress, `☐`
pending), hidden when that agent has none.

Assistant *thinking* is hidden. There is no toggle for it in the UI yet.

### Right — the input, the readout, and one button

The input grows from two to eight rows. **Enter** sends, **Shift+Enter** is a
newline, **Esc** interrupts the turn. Sending while the agent is working is
normal: the text lands at the running turn's next boundary, exactly as typing
into the TUI does. The draft is cleared only when the send was accepted, and
**Esc never touches it**.

The single button follows the coordinator's status, and its face is what it
does: **Send** while idle (enabled once there is text), **Stop** while running
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
run rather than jumping when the turn settles.

## Failures

- **The swarm does not boot.** The tab shows the tail of its log and a **Retry**
  button. That log is `~/.evo/desktop/tabs/<id>/swarm.log`.
- **A lane is down.** Its row shows `✗` and the reason in a tooltip; the swarm
  restarts it, and the row comes back.
- **The connection drops.** The tab reconnects with `Last-Event-ID` and
  exponential backoff (0.5 s, up to 10 s) and shows a reconnecting badge. If
  the supervisor restarts the coordinator, the same tab keeps working: the app
  notices the new event log and refetches state and transcript.
- **The server says no.** A refusal (`409`, busy) is a dim notice, never a
  modal; a command that ran and failed (`422`) shows the server's own error
  text. The app does not re-validate what evo already decided.

## Where things are written

The app keeps its own data in `~/.evo/desktop/`:

| path | what |
|---|---|
| `app.json` | window bounds, open tabs, binary paths, recent sessions, theme |
| `lock`, `activate.sock` | the single-instance lock and its activation socket |
| `model-cache.json` | the last model catalog, for the empty tab's choosers |
| `probe/` | scratch folder used once to learn that catalog |
| `tabs/<id>/tab.json` | that tab's folder, resumed session, swarm id, models, workers |
| `tabs/<id>/token` | the swarm's bearer token, written by the server (0600) — never logged, never shown |
| `tabs/<id>/swarm.log` | the swarm's stdout and stderr |
| `app.log` | the app's own log |

Everything else it touches is read-only: the journals under `~/.evo/sessions`
(which is what the history list is), a swarm's own directory under
`~/.evo/swarm/<id>`, and the one managed block in a project's `.evo/swarm.lisp`
described above. Model names are never hardcoded: the catalog comes from a live
server, or from a one-off probe the first time the app runs.

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

- The thinking-text toggle (thinking is hidden; the transcript can show it, and
  nothing in the window toggles it).
- A command surface (slash commands) and lane control endpoints — deliberate
  v1 non-goals; lanes are watched, never typed to.
- Windows and Linux packaging: the bundle target is macOS.
