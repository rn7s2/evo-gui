# Milestone proofs

One section per milestone in `docs/PROMPT.md` §12, as it is proven. The M0 record
is its own, longer document: `docs/proofs-real.md`.

Append your milestone's section; do not rewrite another lane's.

## M4 — bundle

The hardening milestone's Finder half (§12: "`cargo build --release` and a bundle
you can launch from Finder", single instance + activation, quit shutting
everything down). Run on macOS 27.0, arm64, `2026-09-29`.

### The bundle

```
$ scripts/bundle.sh
   Compiling evo-desktop v0.1.0 (/Users/bytedance/coding/evo-gui/crates/app)
/Users/bytedance/coding/evo-gui/dist/evo-desktop.app: replacing existing signature
bundle: /Users/bytedance/coding/evo-gui/dist/evo-desktop.app        # exit 0

$ codesign -dv dist/evo-desktop.app
Identifier=com.evo.desktop
Format=app bundle with Mach-O thin (arm64)
Signature=adhoc
```

`Contents/`: `MacOS/evo-desktop`, `Resources/AppIcon.icns`, `Info.plist` (linted
by `plutil` inside the script), `_CodeSignature`.

### Launched the Finder way

`HOME` is the real one — `open` cannot hand the app another, and §9.1 puts the
app's state in `~/.evo/desktop` anyway. No swarm was started (nothing to resume
was launched), so nothing else in `~/.evo/desktop` was touched: the run left
`app.log`, `app.json`, `lock`, `activate.sock` and `probe/`.

```
$ open dist/evo-desktop.app
$ tail ~/.evo/desktop/app.log
2026-09-29T12:44:00Z info  evo-desktop 0.1.0 starting (pid 44806, root /Users/bytedance/.evo/desktop)
2026-09-29T12:44:00Z info  app.json: window 1600x1000 at None,None, 0 recent(s), binaries /usr/local/bin/evo-swarm / /usr/local/bin/evo-agent
2026-09-29T12:44:00Z info  window open
2026-09-29T12:44:00Z info  startup: cached catalog has 0 model(s), fetched never
2026-09-29T12:44:00Z info  startup: 1 tab(s) will show it
2026-09-29T12:44:00Z info  catalog: probing with /usr/local/bin/evo-agent (cache older than 86400s or missing)
```

```
$ lsappinfo list | grep -A 6 'com.evo.desktop'
106) "Evo Desktop" ASN:0x0-0x1682681: (in front)
    bundleID="com.evo.desktop"
    bundle path="/Users/bytedance/coding/evo-gui/dist/evo-desktop.app"
    executable path="…/evo-desktop.app/Contents/MacOS/evo-desktop"
    pid = 44806 … type="Foreground" … Version="0.1.0" Arch=ARM64
```

`type="Foreground"` is the point: LaunchServices sees an app with a UI, not a
background process. The name it shows is the bundle's, `Evo Desktop`.

### The name, the icon, and the menu bar

The menu bar of the running app read ` Evo Desktop | File | Edit | Window` —
the app menu from the bundle's name, and the four menus this round added.

The Dock: an icon of the app's own is present while it runs and gone after it
quits. Screen captures taken for the check (`/tmp/evo-app-check/dock.png` before,
`dock_after.png` after) show the same Dock with one icon fewer: the app's, the
one between the Docker whale and the Downloads folder. A close-up of the surface
appears in the window too — the empty tab rendered the **real** history: sixteen
resumable sessions with folders, lane counts and coordinator models
(`~/coding/evo-agent/ · 12 lanes · coordinator: claude-opus-5-5`, and so on),
which is §9.5 against real data. It also showed `GET /registry` failing with the
500 `docs/proofs-real.md` finding R1 records, as the catalog's own error line.

### A second launch activates instead of duplicating

```
$ open dist/evo-desktop.app        # again, while it runs
$ pgrep -f 'evo-desktop.app/Contents/MacOS/evo-desktop' | wc -l
1
$ lsappinfo list | grep -c com.evo.desktop
1
# and nothing new in app.log: LaunchServices brought the running app forward
```

Finder's own second `open` therefore activates rather than duplicates. For the
stronger case — a second *process*, which is what the app's own lock and
activation socket are for — the bundle's binary was run directly:

```
$ ./dist/evo-desktop.app/Contents/MacOS/evo-desktop
2026-09-29T12:44:26Z info  evo-desktop 0.1.0 starting (pid 45302, root /Users/bytedance/.evo/desktop)
2026-09-29T12:44:26Z info  another instance is running (activation acknowledged: true); exiting
$ echo $?
0
# in the *first* process's log, the knock arrived:
2026-09-29T12:44:26Z info  activated by another launch (pid 45302)
$ pgrep … | wc -l
1
```

The second process left with success (the user asked for the app and it is up),
the first raised its window, and there is still one instance.

### ⌘Q

```
$ osascript -e 'tell application "System Events" to keystroke "q" using command down'
$ tail ~/.evo/desktop/app.log
2026-09-29T12:48:15Z info  quitting: stopping every tab
2026-09-29T12:48:15Z info  saved /Users/bytedance/.evo/desktop/app.json: 1 tab(s), 0 of them resumable
2026-09-29T12:48:15Z info  shutdown: all 0 tab(s) exited
2026-09-29T12:48:15Z info  stopped; exiting
$ pgrep -f 'Contents/MacOS/evo-desktop' | wc -l
0
```

`app.json` after that run (§6's tab set, and §9.5's recents):

```json
{"version": 1, "tabs": ["tab-0"], "selected": "tab-0"}
```

One tab, which is what an untouched launch has; no recents, because no tab ever
had a session.

### What this proves, and what it does not

Proven: the release build bundles into an app Finder opens; it comes up as a
foreground app named `Evo Desktop` with its own Dock icon; one instance, with the
second launch activating the first instead of duplicating it; ⌘Q runs §9.8's
ladder, writes `app.json` and leaves no process behind.

Not proven here: a tab whose *swarm* runs inside a bundle (that is M1/M2's proof,
and it needs a folder picked by hand — the failure half of §9.7 is proven
headless in `crates/app/tests/boot_failure.rs`); the icon's design beyond "the
bundle's own icon is what the Dock shows"; and a relaunch reading the tab set
back (nothing restores a tab set yet — the app opens one empty tab by design,
§14.6).

## The swarm milestones, headless (`crates/proofs`)

`crates/proofs` is a tests-only crate: one binary per swarm milestone, driving a
**real** `evo-swarm serve` through the same three crates the tab page drives —
`tab_engine` for the I/O, `session::TabModel` for the view models, `store` for what
the app keeps on disk — with **no UI** and nothing faked but the model.

```text
CARGO_TARGET_DIR=target/proofs cargo test -p proofs
```

| milestone | file | last result |
|---|---|---|
| m1_delegation | `crates/proofs/tests/m1_delegation.rs` | ok, 8.21 s |
| m2_resume | `crates/proofs/tests/m2_resume.rs` | ok, 8.38 s |
| m3_lane_down_up | `crates/proofs/tests/m3_lane_down_up.rs` | ok, 14.23 s |
| m4_coordinator_restart | `crates/proofs/tests/m4_coordinator_restart.rs` | ok, 10.44 s |

`4 passed; 0 failed`, all times UTC, 2026-09-29T13:12Z, on this Mac, against the
installed `/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent`. One test at a
time (`cargo` runs test binaries in sequence); each binds real ports and starts a
supervisor plus a process per lane. A single one re-runs with, say,
`CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m3_lane_down_up -- --nocapture`.
The separate target directory keeps these off the UI crates' `target/`.

### What is real, and what the model is

`swarm_client::harness`'s `Fixture` (feature `test-harness`) gives a hermetic
environment: a temp `EVO_HOME` whose `init.lisp` registers a stub provider, a temp
project, and the installed binaries. The stub model
(`/Users/bytedance/coding/evo-agent/tests/stub-messages.py`) answers from the last
user turn, so a test scripts "what the model does" by what it sends:
`CALL <tool> {json}` becomes that tool call, `DELAY<n> ` waits first, `SLOW` streams
60 deltas. Nothing else is faked — HTTP, SSE, the swarm's supervisor, the lanes' own
`serve` processes, the journal on disk and `POST /prompt` are the real thing, and
every request is recorded (`StubProvider::find`) so a proof can also check what was
really asked.

`crates/proofs/tests/common/mod.rs` folds a `tab_engine::Update` into a `TabModel`
line for line as `crates/workspace/src/tab.rs`'s `Live::absorb` does (same
`on_transcript`/`on_state`/`on_lanes`/`on_event_at`, same monotonic state revision,
same arrival stamp for the step clock). That is what lets a proof assert on the model
and mean **what the tab page renders** — the UI adds drawing, not state. One trap it
avoids: the coordinator's stream and the watched lane's interleave in whatever order
their threads deliver, so its waits are model-based or whole-log based, and only
exclusions use a cursor.

### m1_delegation — the coordinator delegates, the lane works, the report comes back

`CALL delegate {"lane":1,"task":"DELAY3 CALL todo {…}"}` from the coordinator's own
model, with lane 1 watched (`Command::WatchLane`, as showing it does) first.

Proven automatically:

- the coordinator's transcript holds the **`delegate` tool row**, with the task in
  its arguments and the swarm's own answer (`Delegated to lane 1…`, not an error) as
  its result;
- the left column follows lane 1 through `lane-state`: `LaneStatus::Working` and then
  `LaneStatus::Idle` folded into `TabModel::lanes()`, with `working` before `idle` in
  the stream;
- the lane's own stream reaches the lane's `AgentModel`: `tool-call-start` for `todo`,
  `todo-changed` carrying two items, and the checklist in `lane_model(1).todos()`;
- a second delegation whose task is `CALL report {…}` produces the report as **input**
  in the coordinator's transcript (`[lane 1 report] done: …`) — a lane is only ever
  heard from this way — and the lane's own model carries the report as its `report`
  tool row (the event-only row it also makes is replaced by the next resync, as
  `output` lines are);
- exactly one `delegate` row per delegation.

It reads the lanes' states out of the model, which is only possible because of the
tab_engine fix below: before that, a lane that came up idle without ever having
worked stayed `starting` in the list for the whole session.

### m2_resume — a swarm that ran once is found and comes back

A swarm runs one prompt in the temp project and is shut down the ladder's way; then
the app's own path is followed.

Proven automatically:

- `store::history::scan` over the fixture's sessions directory lists the journal as a
  **scanned** (not remembered) resumable swarm with `lanes == 2`, `workers == 2`, the
  coordinator's model (`stub-a`) from its journal, the project folder (compared
  canonically — macOS reports the temp path resolved) and its swarm id;
- `resume_args()` hands back the folder, the session path and the lane count a tab
  starts from;
- a new tab with `TabSpec::with_resume(entry.session)` and **no `--workers`** rebuilds
  the earlier conversation (the first prompt is in the coordinator's rows again),
  brings back two lanes, keeps writing to **the same session file**, and runs a new
  prompt whose answer streams in and lands in the transcript.

**To extend when lane 2's persistence lands** (its tab set + the "open at last quit"
history flag): add a case that writes an `app.json` with a `Recent` carrying
`open_at_quit`, runs `store::history::merge` over a scan of a directory holding that
session, and asserts the `Recent` is at the top of the merged list (and that the scan
alone would have placed it elsewhere) — the UI-level half of §9.5's "the last swarm
is offered first".

### m3_lane_down_up — a lane dies and the swarm brings it back

The lane's pid is read from the lane list the left column folded (originally
`GET /lanes`'s `pid`) and only that pid is `SIGKILL`ed.

Proven automatically:

- the left column shows `LaneStatus::Down` for lane 1, from the `lane-state` event the
  swarm publishes when its watcher sees the lane's stream end;
- the lane comes back **as a new process** (`pid` changed) in state `idle` with
  `restarts >= 1`, and the model's row carries both (the tooltip says `1 restart`);
- the swarm's own account is on the coordinator's stream, error-styled:
  `[lane 1] crashed and was restarted by its supervisor (pid 40651 → 40697); its
  session was resumed and it was re-initialized.` — and it comes **before** the
  restarted lane's own complaint about its fresh state (`✗ No model is configured…`,
  until the swarm re-runs its baseline), which is the ordering
  `session::TabModel::lane_down_reason` prefers the announcement for;
- a lane that is back up claims no reason at all;
- the restarted lane still takes work: a second delegation
  (`SLOW m3 lane one after the restart`) streams from the lane and the stub's request
  log proves that lane 1 really ran it.

**How the restart happens (checked in the source, confirmed on the wire):** no
`restart_lane` call is needed. `evo-swarm` launches each lane as `evo-agent serve`,
which is *itself* the supervisor (`src/cli/supervisor.lisp`: it re-spawns itself as
the supervised child and restarts it with `--resume` on a crash). So a killed child is
restarted by its own parent, and `swarm/lanes.lisp`'s `recover-lane` notices the new
pid, re-initializes the lane, counts the restart and tells the coordinator. The
`[lane N] is down: its process exited. restart_lane brings it back.` message is the
*other* branch — a lane whose supervisor is gone too — and is not what a supervised
crash produces.

### m4_coordinator_restart — the coordinator dies, the view resyncs once

`/health`'s pid (the supervised coordinator, not the swarm process the tab started —
asserted distinct) is `SIGKILL`ed.

Proven automatically:

- the stream goes `Reconnecting` and comes back `Connected`;
- a process the tab has never seen announces itself: `hello` **at id 1** from a new pid;
- the view is refetched at a **higher revision**, and the model holds the conversation
  **once**: row ids strictly increasing and unique, no two rows with the same
  signature, the pre-crash turn exactly once, the assistant answer still there;
- the step the old process was in is cleared (`coordinator_step_started()` is `None`);
- a prompt after the restart streams, and the transcript ends up with both turns, once
  each.

### What the proofs changed

Three things came out of the proofs and are fixed in the app now:

1. **The lane list is read back when a lane comes up** (`crates/tab_engine`). The swarm
   publishes `starting` when it launches a lane and never announces that the lane came
   up (`swarm/lanes.lisp`'s `sync-lane-state :announce nil` — deliberate, "so the
   machine-readable stream tells a lane's work cycle rather than an idle event for a
   lane never given work"), while the assembly snapshot is read *while* the lanes boot.
   A client that only folded events showed every lane as `starting` (◌) until the next
   coordinator restart. The engine now schedules one deferred `/lanes` read per
   "a lane is starting" episode (`LANES_DEBOUNCE` = 500 ms, coalescing a boot's
   per-lane announcements and a watched lane's first connect; `LANES_FOLLOWUPS` = 20
   looks while some lane is still starting, so a swarm that never finishes a boot is not
   asked forever), and reads the list on `settled` too. §2.5 is kept: the read is
   triggered by the event that exists, and it is not a loop.
   `crates/tab_engine/tests/tab_e2e.rs`'s `a_lane_that_came_up_is_read_back_idle`
   proves it headless (an all-idle list arrives, and it is not the assembly one);
   m1_delegation proves it in the app's own model by dropping the direct `/lanes`
   workaround the first version needed.
2. **A red lane row's reason is the swarm's account** (`crates/session`). A restarted
   lane complains about its own fresh state while it is being brought back, and those
   lines used to win by being newer. `lane_down_reason` now prefers the swarm's account
   of the lane going down (`crashed and was restarted`, `is down`,
   `failed to start`), keeps it across resyncs (a `settled` refetch rebuilds the
   coordinator's rows from `/transcript`, which carries no `output` lines at all), and
   shows nothing at all for a lane that is not down. Unit tests in
   `crates/session/tests/tab.rs` pin the precedence, the "nothing for a lane that is
   up" rule and the survival across a resync; m3 proves the app-level half.
3. **A resync replaces event-only rows** — documented behaviour, not a bug: a lane's
   `report` event row becomes the transcript's `report` tool row, exactly as `output`
   lines go. A proof must assert the durable (tool) row.

### Cleanup contract

Every proof collects each pid the swarm reported — the swarm process it started, the
coordinator under its supervisor, and one per lane — and, after the shutdown ladder and
`EngineHandle::join`, asserts `!swarm_client::process_alive(pid)` for every one of them
inside the deadline (`Drive::join_and_assert_gone`). A proof that leaks a swarm, a
coordinator or a lane fails; the fixtures' temp `HOME`s are removed by their `TempDir`
on the way out.
