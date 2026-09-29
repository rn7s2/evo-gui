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

### m2_resume — a swarm that ran once is found, offered first, and comes back

A swarm runs one prompt in the temp project and is shut down the ladder's way; a
*second*, newer swarm runs one prompt after it; then the app's own path is followed —
the history list the empty tab shows, and the tab a row opens.

Proven automatically:

- `store::history::scan` over the fixture's sessions directory lists the journal as a
  **scanned** (not remembered) resumable swarm with `lanes == 2`, `workers == 2`, the
  coordinator's model (`stub-a`) from its journal, the project folder (compared
  canonically — macOS reports the temp path resolved) and its swarm id;
- `resume_args()` hands back the folder, the session path and the lane count a tab
  starts from;
- **the app's own recents win over recency** (§9.5): with two journals on disk — the
  one we care about and a newer one, so the scan alone shows the newer first — an
  `app.json` whose single `Recent` flags the first as *open at the last quit* puts it
  at the top of `store::history::load_history`'s list, **older mtime and all**; it is
  the scan's own row (source `Scanned` — the app adds the flag, not the row), the
  newer one is second and unflagged, and the flag is what moved it (the scan's own
  order, newest first, is asserted beside it). The same flag is read back off
  `app.json` through `AppState::load`. This is the empty tab's list: flagged row
  first, then everything the walk found;
- a new tab with `TabSpec::with_resume(entry.session)` and **no `--workers`** rebuilds
  the earlier conversation (the first prompt is in the coordinator's rows again),
  brings back two lanes, keeps writing to **the same session file**, and runs a new
  prompt whose answer streams in and lands in the transcript.

### The left column's header and its rows

The capture `docs/screens/03-lane-todos-dark.png` shows the lane list saying
`2 lanes · 1 busy` over two idle rows: the header's count came from
`GET /lanes`' `swarm.busy` as of the last read (taken while lane 1 was working),
while the rows had followed the `lane-state` events on to idle. Both halves of that
are fixed, and each has its own test:

- **the header counts the rows it labels** (`crates/session`): `LaneList::busy`
  counts the rows rather than repeating the snapshot's number, so the summary can
  never contradict the column under it; and a row that is not working shows no step
  clock, which is the swarm's own rule (`swarm/state.lisp`'s `lane-snapshot` reports
  a step age only for `working`/`compacting`). Proven by
  `crates/session/tests/lanes.rs`'s `the_header_counts_the_rows_it_labels`, over the
  real captures: `lanes-lane1-working.json` (`swarm.busy` 1) plus the captured
  `lane-state` for lane 1 going idle ⇒ the header says 0, which is what a fresh read
  (`lanes-final.json`) says too. Before the change that test fails on the captured
  data.
- **a lane's state moving reads the list again** (`crates/tab_engine`): every
  `lane-state` event asks for `/lanes` (debounced, as the launch announcement already
  did), and so does the watched lane's own `settled` — the coordinator's run ending
  is not the only moment the clocks and the count move. Unit tests in
  `crates/tab_engine/src/engine.rs` pin the decisions
  (`a_lane_state_that_moves_asks_for_the_list`, `one_read_per_debounce_window`,
  `a_launch_announcement_starts_the_follow_ups`).
- `crates/proofs/tests/m1_delegation.rs`'s `m1_the_lane_list_follows_a_lane_to_idle`
  proves the pair end to end on a real swarm: a delegated `SLOW` task puts lane 1's
  row in `Working`, the header counts it busy, and after the lane's own `settled` the
  row is idle and the header reads nothing busy again.

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

Four things came out of the proofs (and the captures) and are fixed in the app now:

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
   asked forever), and reads the list whenever a lane's state moves or a run settles.
   §2.5 is kept: every read has an event behind it, and none of it is a loop.
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
3. **The lane list's header counts its own rows, and the list follows a lane's
   state** — the `2 lanes · 1 busy` over two idle rows in
   `docs/screens/03-lane-todos-dark.png`: see "The left column's header and its rows"
   above.
4. **A resync replaces event-only rows** — documented behaviour, not a bug: a lane's
   `report` event row becomes the transcript's `report` tool row, exactly as `output`
   lines go. A proof must assert the durable (tool) row.

### Cleanup contract

Every proof collects each pid the swarm reported — the swarm process it started, the
coordinator under its supervisor, and one per lane — and, after the shutdown ladder and
`EngineHandle::join`, asserts `!swarm_client::process_alive(pid)` for every one of them
inside the deadline (`Drive::join_and_assert_gone`). A proof that leaks a swarm, a
coordinator or a lane fails; the fixtures' temp `HOME`s are removed by their `TempDir`
on the way out.

## M4 — appearance, window geometry, and the tab directories

The other three things this milestone owns, each checked in a real launch with a
throwaway `HOME` (`/tmp/evo-app-check/home`, deleted afterwards) and the log lines
that say which way it went.

### Light or dark (§7.1)

`app.json`'s `theme` decides, and `system` follows the system — live, while the
app runs:

```
# theme: "system", on a Mac in dark mode
2026-09-29T13:16:57Z info  theme: dark (the system appearance)

# the system appearance switched to light, then back, with the app running
2026-09-29T13:17:28Z info  theme: light (the system appearance)
2026-09-29T13:17:32Z info  theme: dark (the system appearance)

# theme: "light" in app.json, on the same dark-mode Mac
2026-09-29T13:16:21Z info  theme: light (app.json)
```

Screen captures of the same window confirm it: dark with `theme: "system"`, light
with `theme: "light"`. `crates/app/tests/appearance.rs` covers the choice (a
`Dark` window on the test platform's light one) and the log line; the live switch
itself is gpui's appearance observer, which the test platform offers no lever for.

### The window's geometry (§2 rule 1)

`app.json`'s bounds are a **request**, clamped to the display's work area — a
window last used on a display that is gone must still open somewhere visible:

```
# app.json asked for {"x": 9000, "y": -5000, "width": 99999, "height": 99999}
2026-09-29T13:15:53Z info  app.json: window 99999x99999 at Some(9000.0),Some(-5000.0), …
2026-09-29T13:15:54Z info  window open
2026-09-29T13:15:54Z info  window bounds 1728x992 at 0,33        # the work area

# and that is what the quit wrote back
{"window": {"x": 0.0, "y": 33.0, "width": 1728.0, "height": 992.0}}
```

### Tab directories (§6)

Every launch mints `tabs/<id>/` for a swarm's `tab.json`, token and `swarm.log`.
Those are evidence — §9.7 shows the log's tail when a boot fails — so the only
ones removed are the ones nobody has touched for a week and that `app.json` does
not have open, at startup, off the UI thread:

```
# tabs/ancient-tab/ (two months old) beside tabs/recent-tab/
2026-09-29T13:16:21Z info  tab dirs: pruned ["ancient-tab"], kept 1
$ ls ~/.evo/desktop/tabs/
recent-tab
```

`crates/app/src/housekeeping.rs` holds the rule with its own tests: the newest of
a directory's times decides its age (a swarm appends to its log without changing
the directory's own time), an id `app.json` has open is kept however old it is,
and anything the app cannot date it leaves alone.

## M4 — the About dialog, and the bundle after the day's changes

The same release path as "M4 — bundle" above, re-run once the day's changes were
all in, plus §7.1's ninth piece of polish: the app's own About.

### The About dialog

`Evo Desktop ▸ About Evo Desktop`. It shows the app's icon — `assets/icon/icon-1024.png`,
`include_bytes!`d into the binary so the dialog does not depend on where the bundle
was put — the build, both binaries it spawns, and where the app's state and log live:

```
# /tmp/evo-app-check/r4/r4b-about.png, the dialog over the real window
Evo Desktop          [the app icon]
Version 0.1.0
evo-swarm  0.1.0
evo-agent  0.1.0
State      ~/.evo/desktop
Log        ~/.evo/desktop/app.log
                                        [ Close ]
```

The two versions are read **once**, at startup, on a thread of its own: `--version`
is process work, and this dialog is opened from a menu action, on the UI thread.
What it found is kept on the `Shell`, so opening the dialog is a read:

```
2026-09-29T13:41:24Z info  versions: evo-swarm 0.1.0, evo-agent 0.1.0
2026-09-29T13:41:36Z info  about: evo-desktop 0.1.0
```

`crates/app/src/about.rs` holds the probe and the dialog, with its tests: no
`--version` to ask is `no --version` rather than a blank, `$HOME` is shortened to
`~`, the version line drops the binary's own name (the row already names it), and
the log line names both binaries. `crates/app/examples/about_snapshot.rs --capture
<dir>` renders it headless in both themes (`about-light.png`, `about-dark.png`) —
the picture the polish was judged on, with no screen involved.

### Real history, R1 redacted, dark, and ⌘Q — in the bundle

`scripts/bundle.sh` (exit 0) after the last change of the day, launched as its own
binary with the **real** `$HOME`, so the state and the history under test are the
user's. The launch, whole:

```
2026-09-29T13:41:23Z info  evo-desktop 0.1.0 starting (pid 14227, root /Users/bytedance/.evo/desktop)
2026-09-29T13:41:23Z info  app.json: window 1600x992 at Some(64.0),Some(33.0), 0 recent(s), binaries /usr/local/bin/evo-swarm / /usr/local/bin/evo-agent
2026-09-29T13:41:24Z info  window open
2026-09-29T13:41:24Z info  window bounds 1600x992 at 64,33
2026-09-29T13:41:24Z info  theme: dark (the system appearance)
2026-09-29T13:41:24Z info  startup: cached catalog has 0 model(s), fetched never
2026-09-29T13:41:24Z info  startup: 1 tab(s) will show it
2026-09-29T13:41:24Z info  catalog: probing with /usr/local/bin/evo-agent (cache older than 86400s or missing)
2026-09-29T13:41:24Z info  tab dirs: 0 kept, none old enough to prune
2026-09-29T13:41:24Z info  versions: evo-swarm 0.1.0, evo-agent 0.1.0
2026-09-29T13:41:26Z info  history: 16 row(s) (500 files read of 697 seen, stopped early)
2026-09-29T13:41:26Z error catalog probe failed: http 500: The value
  "Bearer <redacted>"
is not of type
  LIST
2026-09-29T13:41:38Z info  quitting: stopping every tab
2026-09-29T13:41:38Z info  saved /Users/bytedance/.evo/desktop/app.json: 1 tab(s), 0 of them resumable
2026-09-29T13:41:38Z info  shutdown: all 0 tab(s) exited
2026-09-29T13:41:38Z info  stopped; exiting
$ pgrep -f 'MacOS/evo-desktop' | wc -l
0
```

Four things this shows at once, each of them in the *release* build:

* **the real history** — sixteen rows, the same ones the launcher drew
  (`/tmp/evo-app-check/r4/r4b-app.png`): `work-harness · 6 lanes · 5h ago ·
  coordinator: ark-glm-5.2`, `clog`, the `evo-agent` and `evo-gui` worktrees, and
  so on, out of the user's own `~/.evo/sessions`;
* **R1, redacted** — `GET /registry` answering 500 with the token in the message,
  written as `Bearer <redacted>`, in the log *and* on the screen, with no raw
  token anywhere (the screen capture shows the same redaction in the red error
  line beside the model choosers);
* **dark** — `theme: dark (the system appearance)`: the Mac is in dark mode and
  `app.json` says `system`, so the window follows it;
* **⌘Q** — the ladder, then `app.json`, then `stopped; exiting`, and no process
  left behind.

### One instance, on today's build

`scripts/single_instance_check.sh` — the audit against two real launches, in a
throwaway `$HOME`, 11 checks, 0 failures:

```
1. first launch takes the lock
  ok   lock written by pid 12678
  ok   the window opened
  ok   the lock names the process we started
2. second launch asks the first to come forward
  ok   the second launch exited 0
  ok   …within 0s
  ok   it says why (another instance is running)
  ok   the primary was activated by the knock
  ok   the second launch did not disturb the primary
3. a killed primary leaves no stale lock
  ok   the primary is gone
  ok   the next launch became the primary
  ok   the new launch found no other instance
single instance: all checks passed
```

### The stored tab set now names the directories

`app.json`'s `tabs` are the ids the `tabs/<id>/` directories are named by, for
every tab that has started a swarm: `TabRecord::store_id` is what the workspace
hands over now, and it is what the recorded set keeps, so the tab-directory prune
recognises the directories of the tabs that are open instead of only their age. A
tab that has never started one names no directory and is stored under its window
handle, which keeps the set ordered and unique. `crates/app/src/quit.rs` covers
both, and the tab that started a swarm keeps its directory id through the write.

## M2 — relaunch, through the app

The milestone's own proof, with the app's machinery rather than a model of it: two
real `evo-swarm serve` processes started from the empty tab's `Launch` event (the
installed binaries, into a temp `EVO_HOME` whose `init.lisp` registers a stub
provider), the real composer, the real quit sequence, and a **second app instance**
on the same data root (`TestAppContext::new_app`).

```
$ cargo test -p evo-desktop --test m2_relaunch -- --nocapture
[proof] A's swarm: 414ms  [proof] A's reply: 61ms
[proof] A's journal: <EVO_HOME>/sessions/-private-var-…-A/20260929T152146Z_5af3ece8….sexp
[proof] B's swarm: 317ms  [proof] B's reply: 74ms   [proof] C's swarm: 404ms   [proof] quit: 3.748s
[proof] after the first quit: nothing left running (53ms)
[proof] the resumed swarm: 320ms  [proof] the earlier turn: 50µs  [proof] the new reply: 59ms
[proof] quit: 12ms                        [proof] after the second quit: nothing left running (3.38s)
[proof] the whole run: 10.28s
test result: ok. 1 passed; 0 failed; finished in 10.31s
```

What it asserts, in order:

* the first launch holds **four tabs** — three swarms, A, B and C, and one left
  empty — and `app.json` is written as `4 tab(s), 2 of them resumable`;
* exactly **two recents** carry `open_at_quit`, A and B, and each names the journal a
  resume needs. A and B were each given a turn; **C was not**, and a swarm writes its
  journal with its first *reply* — so C names a session, has no file behind it, and is
  not offered: the app does not record it as a recent (`crates/app/src/quit.rs`), the
  history merge does not list a recent whose session is missing
  (`crates/store/src/history.rs`), and the log line counts the same way
  (`4 tab(s), 2 of them resumable`);
* the second instance opens **one empty tab** (§14.6), whatever the stored strip
  says, and the launch-time history — the store's own `load_history`: the scan of the
  temp `EVO_HOME` plus `app.json`'s recents — puts **A and B first**, both wearing the
  badge on screen;
* **clicking A's row** boots a swarm with `--resume <A's journal>` (asserted from the
  process's own `ps` line), on A's own journal;
* the transcript it restores still holds the earlier prompt *and* its reply, a turn
  sent after the resume gets its reply, and nothing the test started — the swarms and
  the lanes they spawned — outlives it.

## M3 — a lane dies, at the UI level

The same app, one swarm with two lanes. The pid is the one *this* swarm's `/lanes`
reports for lane 1, so nothing else on the machine is touched:

```
$ cargo test -p evo-desktop --test m3_lane_ui -- --nocapture
[proof] the swarm: 410ms
[proof] killed lane 1 (80877)
[proof] lane 1 down: Some("✗ lane 1 down, down")
[proof] lane 1 walked through: [("idle", Idle, 0, Some(80877)), ("down", Down, 0, Some(80877)), ("idle", Idle, 1, Some(81077))]
[proof] the reply after the restart: 75ms     [proof] quit: 2.687s
[proof] after the quit: nothing left running (51ms)   [proof] the whole run: 8.53s
test result: ok. 1 passed; 0 failed; finished in 8.55s
```

The row on screen reads `✗ lane 1 down, down` while it is down — the left column's
own accessibility label — the model walks idle → down → idle with `restarts = 1`
behind a **fresh pid**, and the tab keeps working: a turn sent after the restart gets
its reply.

### What these two changed

`swarm_client::server::process_alive` asks a child of ours for its status
(`waitpid(WNOHANG)`) instead of `kill(pid, 0)`: a swarm the app spawned that has
exited but not been reaped is a zombie, and `kill(pid, 0)` called it alive — which is
how "the ladder finished" was being read. A process that is not our child is still
asked the other way.

And a tab whose swarm never answered used to be offered as "open at the last quit"
with nothing behind it (its journal is written with the first reply): the app no
longer records it, and `store::history::merge` no longer lists a recent whose session
file is missing.
