# Screens

Fourteen pictures of the real window, taken by a real run: the app's own `Shell`,
the binaries the environment names, and the scripted model — nothing drawn by
hand, nothing mocked.

```sh
EVO_SWARM_BIN=…/evo-swarm EVO_AGENT_BIN=…/evo-agent \
  cargo run -p evo-desktop --example screens -- --capture docs/screens \
    [--via-agent …/evo-agent]
```

`crates/app/examples/screens.rs` drives the app the way a person does: it opens
the window the app opens, calls the app's own launch-time loads, launches a tab
through `TabContentEvent::Launch`, types with `window.input` / `window.press`, and
clicks the composer's own button and a transcript row's own header. Each picture
is taken twice, in the light theme and the dark one, at the window's own
1440 × 900 (the file is 2× and is resized on the way in, which is why the
screenshots in this directory are shared between the two themes: same window,
same state, two themes).

The world is `crates/proofs`' fixture: a throwaway `HOME` whose `init.lisp`
registers the scripted model, a folder for the swarm to run in, and a tab
directory for the server's ready file. The real `~/.evo` is never touched.

## What each picture is

| | |
|---|---|
| `01-empty-*` | The window as the app opens it: one empty tab, the four choosers, the folder card, and the history — empty, and saying so. The caption is the catalog read of this build, in evo's own words (`evo-swarm catalog --json` is not in it yet, so the choosers stay on Default). |
| `02-live-*` | A tab driving a swarm: the coordinator streaming an answer as it arrives, its step clock running, the tab's own green activity dot, and the button reading **■ Stop swarm** — the whole point of the face: what is going on is what the button will stop. |
| `03-tool-*` | A tool row, opened: the call's arguments and its result as rows, including the exit code. The row is opened by clicking its own header, and the full result was fetched with `GET /items/<id>`. |
| `04-queued-*` | A prompt typed while the coordinator works: the card says `queued · sent at the next step` and carries a **Cancel**. This is the same picture as `02` two seconds later, minus the streaming text and plus the queue — which is what a person looking at the screen sees. |
| `05-after-stop-*` | After `Esc`: the interrupt is evo's to take at a step boundary, so the picture is the state it left — nothing streaming, nothing queued, the queued prompt taken, the button back to **Send**. |
| `06-history-*` | A second tab, opened after that session: the empty tab again, with the app's history list under the choosers. |
| `07-boot-failure-*` | A swarm that cannot come up: the folder, the engine's own one-line reason, the server's log tail in a monospace box, and **Retry** / **Close**. This one is the real thing — the `evo-swarm` of this build exits 64 on the flags a tab passes it. |

## What is not here yet

These are states of a *swarm*, and the `evo-swarm` the app drives is not in the
build these pictures come from (`--via-agent` runs the agent's own `serve` in a
tab's place, which is the same server a swarm's coordinator is, so everything
above is real — it is the lanes that are missing):

- the agents column with lanes: their rows, a lane's own **Stop**, a lane down
  with its reason, and the `✗`/`◐`/`○` glyphs;
- a lane's report card, and a lane event (`restarted`, `is down`) in the
  coordinator's transcript;
- the `Lane 1` transcript a person gets by selecting a lane;
- a check's problem line under the choosers (the same missing CLI as `01`'s
  caption);
- the history list with rows in it: the app lists `--program evo-swarm` sessions,
  so a session run by a tab is in that list only when a swarm ran it;
- a refusal or a lane going down: the notice line above the composer needs a
  server that refuses, and a compaction divider needs a context long enough to
  compact.

`06-history-*` and `01-empty-*` are the same picture for the same reason; they
part company as soon as a tab's server is the swarm itself.
