# Milestone proofs (M1–M4)

`crates/proofs` is a tests-only crate: each milestone is one test binary that drives
a **real** `evo-swarm serve` through the same three crates the tab page drives —
`tab_engine` for the I/O, `session::TabModel` for the view models, `store` for what
the app keeps on disk — with **no UI** and nothing faked but the model.

All times UTC. Latest run: **2026-09-29T12:43Z** on this Mac, against the installed
`/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent`.

```text
CARGO_TARGET_DIR=target/proofs cargo test -p proofs
```

| milestone | file | last result |
|---|---|---|
| M1 delegation | `crates/proofs/tests/m1_delegation.rs` | ok, 8.24 s |
| M2 resume | `crates/proofs/tests/m2_resume.rs` | ok, 8.35 s |
| M3 lane down → up | `crates/proofs/tests/m3_lane_down_up.rs` | ok, 14.23 s |
| M4 coordinator restart | `crates/proofs/tests/m4_coordinator_restart.rs` | ok, 12.46 s |

`4 passed; 0 failed`. One test at a time (`cargo` runs test binaries in sequence);
each binds real ports and starts a supervisor plus a process per lane, so a run is
a swarm, not a unit test. A single one re-runs with, say,
`CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test m3_lane_down_up -- --nocapture`.
The separate target directory keeps these off the UI crates' `target/` (and off
`target/session`, which `crates/session` owns).

## What is real, and what the model is

`swarm_client::harness`'s `Fixture` (feature `test-harness`) gives a hermetic
environment: a temp `EVO_HOME` whose `init.lisp` registers a stub provider, a temp
project, and the installed binaries. The stub model
(`/Users/bytedance/coding/evo-agent/tests/stub-messages.py`) answers from the last
user turn, so a test scripts "what the model does" by what it sends:
`CALL <tool> {json}` becomes that tool call, `DELAY<n> ` waits first, `SLOW`
streams 60 deltas. Nothing else is faked — HTTP, SSE, the swarm's supervisor, the
lanes' own `serve` processes, the journal on disk and `POST /prompt` are all the
real thing, and every request is recorded (`StubProvider::find`) so a proof can
also check what was really asked.

`crates/proofs/tests/common/mod.rs` folds a `tab_engine::Update` into a
`TabModel` line for line as `crates/workspace/src/tab.rs`'s `Live::absorb` does
(same `on_transcript`/`on_state`/`on_lanes`/`on_event_at`, same monotonic state
revision, same arrival stamp for the step clock). That is what lets a proof assert
on the model and mean **what the tab page renders** — the UI adds drawing, not
state.

One thing a proof must not assume: two streams interleave (the coordinator's and
the one lane being watched) in whatever order their threads deliver, so
`Drive::next` (cursor-based) walks past updates it is not looking for. Waits are
therefore written against the model (`wait_model`) or against the whole log
(`wait_for_update`), and only exclusions use the cursor.

## M1 — delegation

`m1_delegation.rs`. The coordinator's model is asked to
`CALL delegate {"lane":1,"task":"DELAY3 CALL todo {…}"}`; lane 1 is watched
(`Command::WatchLane`, as showing it does) before the delegation.

Proven automatically:

- the coordinator's transcript holds the **`delegate` tool row**, with the task it
  was given in its arguments and the swarm's own answer
  (`Delegated to lane 1…`, not an error) as its result;
- the left column follows lane 1 through `lane-state`: `LaneStatus::Working` and
  then `LaneStatus::Idle` folded into `TabModel::lanes()`, with the `working`
  event before the `idle` one in the stream;
- the lane's own stream reaches the lane's `AgentModel`: `tool-call-start` for
  `todo`, `todo-changed` carrying two items, and the checklist in
  `lane_model(1).todos()`;
- a second delegation whose task is `CALL report {…}` produces the report as
  **input** in the coordinator's transcript (`[lane 1 report] done: …`) — a lane
  is only ever heard from this way — and the lane's own model carries the report
  as its `report` tool row;
- exactly one `delegate` row per delegation (no double-run).

`DELAY3` is there on purpose: it keeps lane 1 working long enough for the
`working` → `idle` cycle to be observed on the coordinator's stream.

## M2 — resume

`m2_resume.rs`. A swarm runs one prompt in the temp project and is shut down the
ladder's way; then the app's own path is followed.

Proven automatically:

- `store::history::scan` over the fixture's sessions directory lists the journal as
  a **scanned** (not remembered) resumable swarm with `lanes == 2`,
  `workers == 2`, the coordinator's model (`stub-a`) from its journal, the project
  folder (compared canonically — macOS reports the temp path resolved) and its
  swarm id;
- `resume_args()` hands back the folder, the session path and the lane count a tab
  starts from;
- a new tab with `TabSpec::with_resume(entry.session)` and **no `--workers`**
  rebuilds the earlier conversation (the first prompt is in the coordinator's rows
  again), brings back two lanes, keeps writing to **the same session file**, and
  runs a new prompt whose answer streams in and lands in the transcript.

## M3 — a lane dies and comes back

`m3_lane_down_up.rs`. The lane's pid is read from `GET /lanes` (asked directly —
see the finding below) and only that pid is `SIGKILL`ed.

Proven automatically:

- the left column shows `LaneStatus::Down` for lane 1, and the `lane-state`
  event that said `down` is on the coordinator's stream;
- the lane comes back **as a new process** (`pid` changed) in state `idle` with
  `restarts >= 1`, and the model's row carries both (the tooltip says
  `1 restart`);
- the swarm's own account of it is on the coordinator's stream, error-styled:
  `[lane 1] crashed and was restarted by its supervisor (pid 40651 → 40697); its
  session was resumed and it was re-initialized.`
- the row's reason (`TabModel::lane_down_reason(1)`) names the lane from a
  coordinator `output` line;
- the restarted lane still takes work: a second delegation
  (`SLOW m3 lane one after the restart`) streams from the lane and the stub's
  request log proves that lane 1 really ran it.

**How the restart happens (checked in the source, confirmed on the wire):** no
`restart_lane` call is needed. `evo-swarm` launches each lane as `evo-agent serve`,
which is *itself* the supervisor (`src/cli/supervisor.lisp`: it re-spawns itself
as the supervised child and restarts it with `--resume` on a crash). So a killed
child is restarted by its own parent, and `swarm/lanes.lisp`'s `recover-lane`
notices the new pid, re-initializes the lane, counts the restart and tells the
coordinator. The `[lane N] is down: its process exited. restart_lane brings it
back.` message is the *other* branch — a lane whose supervisor is gone too — and
is not what a supervised crash produces.

## M4 — the coordinator dies

`m4_coordinator_restart.rs`. `/health`'s pid (the supervised coordinator, not the
swarm process the tab started — asserted distinct) is `SIGKILL`ed.

Proven automatically:

- the stream goes `Reconnecting` and comes back `Connected`;
- a process the tab has never seen announces itself: `hello` **at id 1** from a
  new pid;
- the view is refetched at a **higher revision**, and the model holds the
  conversation **once**: row ids strictly increasing and unique, no two rows with
  the same signature, the pre-crash turn exactly once, the assistant answer still
  there;
- the step the old process was in is cleared (`coordinator_step_started()` is
  `None` — the new process never began it);
- a prompt after the restart streams, and the transcript ends up with both turns,
  once each.

## Findings (not this crate's to fix)

1. **The left column shows every lane as `starting` until something publishes a
   change.** The swarm deliberately does not announce a lane going idle when it has
   never been given work (`swarm/lanes.lisp`'s `sync-lane-state :announce nil`, "so
   the machine-readable stream tells a lane's work cycle… rather than an idle event
   for a lane never given work"), and `tab_engine` fetches `/lanes` only at assembly
   and on a coordinator reset (`engine.rs`'s `assemble` and `resync(…, lanes)` —
   `lanes` is true only for `hello`/`Reset`). Evidence: m1's first version waited
   240 s for `LaneStatus::Idle` and the model still said `(1, "starting"),
   (2, "starting")` while `GET /lanes` reported both lanes idle. The proofs work
   around it with `Drive::wait_lanes_idle`, which asks the swarm's `/lanes`
   directly. A fix on our side would be to refetch `/lanes` when a `lane-state`
   event arrives (or once, after the lanes have had time to come up).
2. **The down row's reason after a supervised crash is the *newest* `[lane N]`
   error line**, which is often the transient `✗ No model is configured — set one
   in init.lisp…` the restarted lane emits before the swarm re-runs its baseline —
   not the crash announcement. m3 therefore asserts the announcement is on the
   stream and only that the reason *names the lane*; preferring the crash line (or
   the lane's own re-init over its boot error) is a `session` question.
3. **A resync replaces event-only rows.** A lane's `report` event row is replaced
   by the transcript's `report` tool row at the next rebuild, exactly as `output`
   lines are. Not a bug — it is the documented split between `/transcript` and the
   stream — but a proof must assert the durable (tool) row, as m1 does.

## Cleanup contract

Every proof collects each pid the swarm reported — the swarm process it started,
the coordinator under its supervisor, and one per lane — and, after the shutdown
ladder and `EngineHandle::join`, asserts `!swarm_client::process_alive(pid)` for
every one of them inside the deadline (`Drive::join_and_assert_gone`). A proof that
leaks a swarm, a coordinator or a lane fails; the fixtures' temp `HOME`s are
removed by their `TempDir` on the way out.
