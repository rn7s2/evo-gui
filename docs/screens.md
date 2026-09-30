# Screens

Twenty pictures of the real window, taken by a real run: the app's own `Shell`,
the real `evo-swarm` / `evo-agent` binaries, and the scripted model — nothing drawn
by hand, nothing mocked.

```sh
EVO_SWARM_BIN=…/evo-swarm EVO_AGENT_BIN=…/evo-agent \
  cargo run -p evo-desktop --example screens -- --capture /tmp/screens
```

They are not kept in the repository: every branch regenerates them, and a merge of
two branches that have both captured is a merge of twenty binary conflicts. The
names below are the files a run leaves in its directory.

`crates/app/examples/screens.rs` drives the app the way a person does: it opens
the window the app opens, calls the app's own launch-time loads, launches a tab
through `TabContentEvent::Launch`, types with `window.input` / `window.press`,
clicks the composer's own button, a transcript row's own header and a row in the
agents column. Each state is taken twice, in the light theme and the dark one, at
the window's own 1440 × 900 (the file is 2× and is halved with `sips
--resampleWidth 1440` on the way in).

The world is `crates/proofs`' fixture: a throwaway `HOME` whose `init.lisp`
registers the scripted model, a folder for the swarm to run in, and a tab
directory for the server's ready file. The real `~/.evo` is never touched, and the
run leaves no process behind — every server it starts is in its own process group,
and the fixture stops exactly those when it goes.

The prompts are the scripted model's own vocabulary: `CALL <tool> <json>` makes it
call one tool, `SLOW …` makes it answer in sixty deltas over six seconds.

## What each picture is

| | |
|---|---|
| `01-empty-*` | The window as the app opens it: the New Swarm page (`design/doc28/NewSwarm.tsx`) — the Coordinator card (model and effort), the Workers card (count, model, effort) and the folder card beside them, with the history under them, empty and saying so. Every control opens on what evo resolved, so the two model fields read `stub · stub-a` (the one registration the stub home has) and the effort sliders sit on the catalog's middle rung. |
| `02-lanes-working-*` | Six lanes, one of them given work: the left column reads `6 lanes · 1 busy`, `main` is waiting on its lanes, lane 1 is working with its step clock running and its own **Stop**, and the coordinator's turn — the `delegate` call and the tool's answer — is in the transcript. The button reads **■ Stop swarm**. |
| `03-lane-transcript-*` | Lane 1 selected: the centre column is *that lane's* transcript — the `todo` call it was told to make, its result, and the lane's own checklist at the foot. Input still goes to the coordinator. |
| `04-tool-*` | A tool row, opened: `bash`'s arguments and its result as rows, with the exit status. The row is opened by clicking its own header, and a truncated result is fetched whole with `GET /items/<id>`. |
| `05-queued-*` | A prompt typed while the coordinator works: the card says `queued · sent at the next step` and carries a **Cancel**, the answer above it is still being written, and the button is still **Stop swarm** — the face follows the swarm being busy, not the composer's draft. |
| `06-report-*` | What a lane's report looks like when it arrives: the `Lane 1 report` card with the fields the lane sent (`done`, `evidence`, `next`), which is the item a client renders instead of reading `[lane 1 report]` out of a sentence. Above it, the notice the swarm published for the same report — today labelled `command`, because the swarm's own notice goes out through the serve's command channel (`source: "command"`), which is what a client is told. |
| `07-history-*` | A second tab in the same window, after that session: the New Swarm page again, with the resumable swarm in the history list under the cards — its glyph, its title, its `~`-shortened folder, how long ago, and the `›` that says a click resumes it. |
| `08-check-problem-*` | The New Swarm page when the binary `app.json` names cannot be run: both model fields say `No models are registered` in evo's own words, and under the cards are the check's own two lines — `/nonexistent/evo-swarm could not be run — fix it in Settings…` and the catalog's — each one line, calm, and a click on the first opens Settings. This is also the window that shows a history row with a real title. |
| `10-tabs-*` | The tab strip with four tabs, the first of them working and the second under the pointer: the design's own subject (`design/doc28/TabStrip.tsx`). The shown tab carries the page's surface and the outward corner at each end, the pointed-at one its own fill, the last two keep the strip's own colour — and the divider rule is visible: none beside the shown tab or the pointed-at one, one between the last two. Taken where the swarm is at work, so the working tab's dot is mid-breath. |
| `09-boot-failure-*` | The same binary, launched in a folder: *Could not start a swarm*, the folder it could not start in, the engine's one-line reason, the empty log box, and **Retry** / **Close** (§9.7). |

## What is not here yet

- **The lanes chooser's filter, with something greyed out.** The choosers show
  what `evo-swarm catalog --json` returned, and the filter greys out a model no
  lane could register; the stub home registers exactly one model, which every lane
  can run, so there is nothing to grey. A home with a second, lane-incapable model
  is what that picture needs.
- **A compaction divider**, which needs a session long enough for evo to compact.
- **A dropped connection** and the reconnecting badge: the app reconnects with a
  backoff, and the picture needs a server to die under a live tab.
- **An operator's stop** (`■ Stop swarm`, or a lane's own **Stop**): the pictures
  above show the button armed, not the item a stop leaves behind — that is
  `t05_interrupt_swarm`'s subject, and a capture of it would be one more frame two
  seconds later.
