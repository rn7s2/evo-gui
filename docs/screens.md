# Screens

Twenty headless captures of the **assembled app window** — `Shell` + `WorkspaceView`
+ real `tab_engine` + real `evo-swarm` — taken by

```sh
cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens --scale 1
```

Every state is captured twice, light and dark. The pictures are 1600×1000 *points*
(§7.1's window size); `--scale 1` renders them at 1×, so a file is 1600×1000
pixels and the twenty files in this directory are 1.2 MB together. The example's
default is 2× (3200×2000 pixels, ~9 MB for the set), which is what to use when
reading small type; `--scale 1` is what is committed, then quantised to 192
colours.

The pictures are a *golden* set — nothing re-checks them — so take them again
after a change to the chrome, the tab page or the transcript and compare by eye.
`--only 08,09` draws only the states whose names it lists, and takes the earlier
ones without saving them, which is the cheap way to re-take the last few.

| capture | what it shows |
|---|---|
| `01-launch-light.png`, `01-launch-dark.png` | The app at launch (§7.2): one empty tab whose three choosers stand on `Default`, and the six resumable swarms the scan found — the newest of them `~/coding/evo-gui`, `6 lanes`, `11m ago`, `coordinator: claude-opus-5-5`. |
| `01b-lanes-chooser-light.png`, `01b-lanes-chooser-dark.png` | The lanes chooser open (§9.4), its menu filled from the catalog: five models a quarantined lane may be given, and `claude-opus-5-5` greyed with *needs an extension API — set it in swarm.lisp*. |
| `02-three-tabs-light.png`, `02-three-tabs-dark.png` | Three tabs — two running swarms and an empty one — with the first mid-stream: a heading, a list, a table and a code fence, all rendered by the transcript's markdown (§2.8). |
| `03-lane-todos-light.png`, `03-lane-todos-dark.png` | Lane 1 selected (§7.3, §9.3): the task it was delegated, the `todo` call it made, the write-up it streamed, and the checklist panel — `Todos 1/3`, one item in each state. |
| `04-tool-expanded-light.png`, `04-tool-expanded-dark.png` | The coordinator's `delegate` call opened onto its arguments and its result (§2.8). |
| `05-boot-failure-light.png`, `05-boot-failure-dark.png` | A swarm that could not start (§9.7): *Could not start a swarm*, the folder, the server's log tail, `Retry` and `Close`. |
| `06-reconnecting-light.png`, `06-reconnecting-dark.png` | The coordinator's stream gone quiet (§9.7): the amber `reconnecting` badge on the `main` row, the spinner row in the centre, and a disabled `Send`. |
| `07-tab-strip-light.png`, `07-tab-strip-dark.png` | The tab strip past its width (§7.1): fourteen tabs, ten of them folders whose names the strip truncates, and the last one a live swarm. The strip shows the first twelve tabs and keeps its highlight on the first of them, so the tab being shown — the last one — is the one that is off screen; the `+` is past the right edge with it. |
| `08-narrow-1000x700-light.png`, `08-narrow-1000x700-dark.png` | The tab page at §7.1's smallest window, 1000×700: three columns side by side, the coordinator's answer and the `delegate` row it made, lane 1's task ellipsized to `SLOW: walk the narrow layo…` in the lane list with its step clock beside it, the readout ellipsized behind `ctx 7…`, the composer's card, and nothing overlapping. |
| `09-bad-run-light.png`, `09-bad-run-dark.png` | A run that ended badly (§5, §9.5): the compaction a full context forced (`Compacting context…`, `Context compacted`), the provider dying mid-stream four times (`Retrying provider (n/4)`), and the run's last word — `Run failed: Provider request failed after 4 attempts: Stream ended without a terminal event (truncated response)`. |

## What is real, and what is scripted

Real: the window and its chrome, the tab strip, the empty tab and its choosers,
the tab page's three columns, the transcript (markdown, tool rows, reports, dim
rows), the composer and the §7.3 readout, the lane list with its glyphs, step
clock and badge, `tab_engine` driving everything, and `evo-swarm`/`evo-agent`
spawned as real processes with real SSE streams, journals and shutdown ladders.

Scripted, because a picture is not worth a model call:

* **the catalog** — a `ModelCache` written into the example (`CATALOG`), with the
  models this machine's evo really registers, one of them speaking an API the
  kernel does not have, which is what makes the lanes chooser mark it
  unavailable;
* **the history** — six journals the example writes into its temp home, in the
  shape evo writes them, then a real `store::history::scan` over them;
* **the model** — `scripted-model.py`, written into the temp directory by the
  example and started on a free port, with a temp `EVO_HOME` whose `init.lisp`
  registers it as provider `:stub` and model `stub-a-preview-2026-09`. It speaks
  the same minimal Anthropic SSE as `evo-agent/tests/stub-messages.py`, reports
  usage in the same measure the kernel estimates context in (chars/4, which is
  what makes `09`'s compaction happen at all), and adds the rules the pictures
  need: `CALL <tool> {json}` for a tool call, `TODOS` for the checklist,
  `SHOW`/`SLOW` for the markdown document (streamed, with a pause before the
  message ends so a capture can catch it whole while the row is still
  streaming), `BIG` for the same document eight times over, to cross a context
  window on purpose, and `FAIL` for a provider that dies mid-stream without a
  terminal event;
* **the second registration** — the same `init.lisp` registers
  `stub-small-window`, whose 3000-token window (with `:compact-reserve 2000`
  and `:compact-keep-recent 200`) is small enough that one streamed document
  crosses the compaction line. `09` launches its tab with `--model
  stub-small-window`; the others stay on the 200000-token registration;
* **the bad binary** of `05-boot-failure` — a shell script that prints three
  lines and exits 1, so the failure is a real boot failure with a real log tail;
* **the dropped stream** of `06-reconnecting` — the two swarm processes are
  `SIGSTOP`ped, not killed (a killed one fails its tab instead), the stream
  times out, and `SIGCONT` starts them again after the pictures.

## What the run leaves behind

Nothing: the fixture, the journals and the scripted model live under one temp
directory, which the example removes. The tabs' engines are stopped through
`workspace::stop_in_background` — the same path the quit sequence uses — and the
last lines of the run say how many swarm processes were still alive (none) and
where the pictures went. `EVO_DESKTOP_KEEP_SCREENS_TMP=1` keeps the temp
directory, for reading a journal after a failed run.

The tabs `07` opens for the strip are waited for — `/health` first, then stopped.
A swarm handed to the shutdown ladder while it is still booting is not reaped:
there is no port to post `/shutdown` to yet, and the processes the server had
already started outlive the engine and the tab (ten of them, when `07` opened its
tabs and stopped them at once).
