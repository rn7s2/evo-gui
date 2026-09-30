# The real-binary proofs

`crates/proofs` proves the app's half of [`CONTRACT.md`](../CONTRACT.md) against
**real** `evo-swarm serve` / `evo-agent serve` processes: no fake client, no
recorded fixtures, no UI in the way. One proof per file, one test per proof.

```sh
EVO_SWARM_BIN=…/build/evo-swarm EVO_AGENT_BIN=…/build/evo-agent \
  EVO_STUB_MESSAGES=…/evo-agent/tests/stub-messages.py \
  CARGO_TARGET_DIR=target/proofs cargo test -p proofs -- --nocapture
```

The binaries come from the environment (`EVO_SWARM_BIN` / `EVO_AGENT_BIN`, else
`/usr/local/bin`), and the scripted model is `evo-agent/tests/stub-messages.py`,
found through `EVO_STUB_MESSAGES`, then beside this checkout's `evo-agent`
sibling, then in this workspace's own `evo-agent`. `--nocapture` prints each
proof's timings and what it asserted — the interesting half of a proof is the
line it prints before it fails.

## The world each proof runs in

`Fixture` (in `crates/proofs/src/fixture.rs`) builds one temp directory holding a
**stub home** — `$HOME/.evo/init.lisp` registering the stub provider and the
`stub-a` model, the recipe `scripts/stub_home.sh` uses, with the same scrub list
— a project folder, and one `tabs/<id>/` directory whose `ready.json` and
`swarm.log` the server writes. `spawn` runs `store::launch`'s argv through
`swarm_client` with the child's stdin held as a pipe, and readiness is the ready
file: nothing here polls a health endpoint, picks a port, waits for a token file
or compares pids.

A fixture is also what guarantees the servers are *gone* when it is: a proof's own
`Server` stops itself (it holds the child's pipe, and EOF is the whole signal), but
a server the app started — the captures, a UI test — belongs to nobody's `Drop` and
would go on running after the process that drove it ends. So the fixture's `Drop`
finds every process whose command line names its own temp directory (the ready file
publishes the session's pid and leaves `supervisor_pid` null, so a pid is not
enough), and stops them: `SIGTERM`, then `SIGKILL`.

`Watcher` (in `watch.rs`) is how a proof reads a server: one snapshot seeds a
topic, items and state mirror, and every op of §5.3 keeps it current
(`item.add`/`append`/`patch`/`remove` with the contract's merge rules,
`state.patch`, the two resets). Asserting on the mirror is asserting on what a
client would be holding.

## What each proof proves

| proof | subject | proves |
|---|---|---|
| `t01_launch_ready` | swarm | The ready file names the program, the port the child chose, a 64-hex token and the session under this fixture's home; `/health` and the snapshot agree on the epoch; the stream's `hello` frame carries it; and the shutdown ladder leaves nothing running. |
| `t02_prompt_streams` | agent | `input.send` answers with the **pre-minted item id** and that id is the item the stream adds; the answer is visible while its status is `streaming` and grows where it stands; the topic says `running`, then `idle`; a long answer arrives as `item.append` ops on one item, in seq order, never re-sent whole. |
| `t03_delegate_and_lanes` | swarm | The `swarm` topic publishes every lane transition (including a lane coming up idle) and its `lanes[]`; a delegated task reaches `lane:1`'s own topic as items; the lane's todos are in its own state; and the report comes back to the coordinator as a `lane_report` **item with fields**, not as a sentence. |
| `t04_restart_resumes` | agent, two servers in **one folder** | §1's E1: kill one child, and the one that comes back is the one that died — same journal path in the rewritten ready file, `--resume <that exact path>` on its command line, its earlier turn still there, and the other tab untouched. |
| `t05_interrupt_swarm` | swarm | `run.interrupt` with scope `swarm` stops the work, the lanes settle, and the coordinator is told: a `human_action` item naming the lanes (§7.4 — stopped, never redirected behind its back). |
| `t06_history_resume` | agent | `evo-agent sessions --json` lists the session (one process, one document, no journal parsed here) and `--resume <that exact path>` brings the turn back. |
| `t07_catalog_choosers` | no server at all | `evo-swarm catalog --json` fills the choosers — models, `lanes.models`, a registration with its provider — and `evo-swarm check --json` judges the launch the choosers describe, including refusing a model nothing registered, in evo's own words. |
| `t08_quit_on_eof` | agent | §8's first rung, and the whole of the app's quit: closing the pipe the tab was started with ends the child — a tab that has served nothing **and** a tab that has answered a turn. |

`t03` and `t05` are the swarm's: lanes are their subject. `t02`, `t04` and `t06`
name `Program::Agent` because what they prove — the prompt path, the restart, the
index — is the same in the coordinator a swarm runs; each is one line from
`Program::Swarm` (`const PROGRAM`), which is how they run once evo-swarm's lane
work is in the integration build.

## Findings the proofs are carrying

Each of these is a proof failing with its evidence in the message, against the
builds as they were when it was written. They are the reason the proofs exist;
when they pass, the finding is fixed.

- **`t02`** — the assistant item's id is not the one it streamed under. The view
  mints `a_…` at `message-start`, streams into it, then emits `item.remove a_…`
  and `item.add <the journaled id>` (CONTRACT §3's pre-minted id does not reach
  the frontend yet; `evo-agent --events` on that build printed
  `(:type :message-start :run-id … :turn 0)` with no `:entry-id`).
- **`t04`** — a supervisor restart is started as
  `serve --ready-file … --watch-stdin --port 0 --resume`: a **bare** `--resume`
  (the newest journal in the folder, which with two tabs in one folder can be the
  other tab's — E1) and `--port 0` again (a port no client knows — E2). The same
  restart reported `restarts: 0` and `supervisor_pid: null`, because the
  supervisor sets neither `EVO_RESTARTS` nor `EVO_SUPERVISOR_PID`.
- **`t04`** — a restart is still `--resume` bare and `--port 0` (see above), and
  the rewritten ready file still says `restarts: 0`, `supervisor_pid: null`.
- **`t08`** — the pipe stops meaning anything once a run has happened: an idle
  child exits 0 within two seconds of EOF (`--no-userspace`, a tenth of a
  second), but a child that has answered one prompt was still running 45 seconds
  after its pipe closed. That is the state a quit is most often made in, and the
  app's quit is only the pipe, so such a server leaks — nothing left alive knows
  its pid.
- **`t07`** — `evo-swarm catalog --json` and `check --json` answer
  `Unknown argument` in a build whose `evo-swarm serve` is still the old one
  (`--token-file`), so the empty tab's choosers and its problem lines have
  nothing to read yet. (This one is the swarm branch, not the agent's.)
- **`t01`/`t03`/`t05`** — `evo-swarm serve --ready-file` does not exist yet in the
  same build (exit 64, `Unknown argument: --ready-file`), so the swarm's launch,
  its lanes and its interrupt cannot be reached at all.

Fixed and green: **`t02`** — the pre-minted item id arrives in `input.send`'s own
reply and is the id the answer streams under, 60 appends on one item; **`t06`** —
`sessions --json` lists the session with its title, and `--resume <that exact
path>` brings the turn back.

## The app's own tests

What the proofs deliberately do not cover is the shell: single instance, the
window's bounds, the launch-time loads reaching the empty tab, Settings, the quit
sequence, and a boot failure's own screen. Those are `crates/app/tests/`
(`appearance`, `settings`, `quit`, `boot_failure`), plus each UI crate's own
tests. The bundle is checked by hand: `scripts/bundle.sh`, then
`scripts/single_instance_check.sh`.
