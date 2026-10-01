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
found through `EVO_STUB_MESSAGES`, then in `EVO_AGENT_REPO`, then in the
`evo-agent` checkout beside this one. `--nocapture` prints what each proof
asserted, and how long it took where the proof times itself — the interesting half
of a proof is the line it prints before it fails.

## The world each proof runs in

`Fixture` (in `crates/proofs/src/fixture.rs`) builds one temp directory holding a
**stub home** — `$HOME/.evo/init.lisp` registering the stub provider and the
`stub-a` model, the recipe `scripts/stub_home.sh` uses, with the same scrub list
— a project folder, and one `tabs/<id>/` directory whose `ready.json` and
`swarm.log` the server writes. `spawn` runs `store::launch`'s argv through
`swarm_client` with the child's stdin held as a pipe, and readiness is the ready
file: nothing here polls a health endpoint, picks a port, waits for a token file
or compares pids. The whole directory is removed when the process is done —
`EVO_PROOFS_KEEP=1` keeps it, which is where a failure is read from.

A fixture is also what guarantees the servers are *gone* when it is: a proof's own
`Server` stops itself (it holds the child's pipe, and EOF is the whole signal), but
a server the app started — the captures, a UI test, `crates/app`'s own end-to-end
test — belongs to nobody's `Drop` and would go on running after the process that
drove it ends. So the fixture's `Drop` stops the process **groups this process
started**, through `swarm_client::reap_spawned`: every server is spawned as its own
group leader (`process_group(0)`, so its pid is the handle), the group is
forgotten once nobody is left in it, and the signal is `SIGTERM` with `SIGKILL`
for whatever a three-second wait is not enough for. Nothing is found by name,
command line or pattern, and no other process's group is ever touched.

`Watcher` (in `watch.rs`) is how a proof reads a server: one snapshot seeds a
topic, items and state mirror, and every op of §5.3 keeps it current
(`item.add`/`append`/`patch`/`remove` with the contract's merge rules,
`state.patch`, the two resets). Asserting on the mirror is asserting on what a
client would be holding.

## What each proof proves

| proof | subject | proves |
|---|---|---|
| `t01_launch_ready` | swarm | The ready file names the program, the pid **and** its supervisor, the port the child chose, a 64-hex token and the session under this fixture's home; `/health` and the snapshot agree on the epoch; the stream connects in that epoch (a `hello` is the connection's cursor, not an op a client sees); and the shutdown ladder leaves nothing running. |
| `t02_prompt_streams` | swarm | `input.send` answers with the **pre-minted item id** and that id is the item the stream adds; the answer is visible while its status is `streaming` and grows where it stands; the topic says `running`, then `idle`; a long answer arrives as `item.append` ops on one item, in seq order, never re-sent whole. |
| `t03_delegate_and_lanes` | swarm | The `swarm` topic publishes every lane transition (including a lane coming up idle) and its `lanes[]`; a delegated task reaches `lane:1`'s own topic as items; the lane's todos are in its own state; and a lane that *calls the `report` tool* comes back to the coordinator as a `lane_report` **item with fields** — and the swarm's own `reports` count — not as a sentence parsed out of prose. |
| `t04_restart_resumes` | swarm, two servers in **one folder** | §1's E1: kill one child, and the one that comes back is the one that died — same journal path in the rewritten ready file, `--resume <that exact path>` on its command line, its earlier turn still there, and the other tab untouched. |
| `t05_interrupt_swarm` | swarm | `run.interrupt` with scope `swarm` stops the work, the lanes settle, and the coordinator is told: a `human_action` item naming the lanes (§7.4 — stopped, never redirected behind its back). |
| `t06_history_resume` | swarm | `evo-agent sessions --json` lists the session (one process, one document, no journal parsed here) and `--resume <that exact path>` brings the turn back. |
| `t07_catalog_choosers` | no server at all | `evo-swarm catalog --json` fills the choosers — models, `lanes.models`, a registration with its provider — and `evo-swarm check --json` judges the launch the choosers describe, including refusing a model nothing registered, in evo's own words. |
| `t08_quit_on_eof` | agent | §8's first rung, and the whole of the app's quit: closing the pipe the tab was started with ends the child — a tab that has served nothing **and** a tab that has answered a turn. |
| `t09_queued_turn_is_sent` | swarm | A turn queued while the coordinator works is **drawn as sent** once it is steered in: `input.send` answers with the id it will have, `queue: now` (what the composer sends) is drained at the run's next step and `after_run` when the whole run ends, and the stream shows both reaching `status: sent` with the session back to `idle` — the app's own fold (`session::TabModel`) reading each as a `user` item whose status is `Sent`. |
| `t10_stop_swarm_names_only_busy_lanes` | swarm | Two lanes, one working (`DELAY10`) and one idle: `run.interrupt` with scope `swarm` names `lane:1` **and never `lane:2`**, the note the coordinator reads is `[human] stopped lane 1 (interrupt)` — *lane*, singular — and the `human_action` item's own `lanes` is `[1]`. The bug it pins: a lane's own interrupt answers `{interrupted: []}` for an idle lane, and an empty array is non-NIL, so a swarm that read `(getf result :interrupted)` counted **every** lane as stopped. |
| `t11_completion_and_levels` | agent | Three reads, against a real server: `complete` **counts characters where a box works in bytes** (a caret past two `✓`), and what the caret is on is the server's answer — `/` a command word, `/comp` in `run /comp now` that word, `/eval`'s content a **symbol** whose range reaches past the caret, `/usr/local` nothing; every model carries the `effort_levels` it takes, live and offline (`catalog --json` / `check --json`); and a command's lines are published **twice** — the reply's `notices` and session `notice` items alike. |
| `t12_attachments` | agent | One turn carrying an image by path, pasted bytes and a table, where **only the images ride with the turn**: `images` gets one entry each (`{path}`, and `{name, media_type, data}` for the paste) and a file no payload at all — its **absolute** path is in the message's text, under `Attached files:`. The op is accepted, the reply's `item_id` is the row the snapshot publishes, both images name `/media/<id>/<n>` and fetch back the file as evo read it and the bytes as they were pasted. An image evo cannot read is refused `invalid_args` — the sentence the tab puts above the composer — and **adds no row**. |
| `t13_images_resume` | swarm, resumed | §1's resume with a picture in it: a turn carrying an image by path is sent, the server is stopped and **the picture deleted from disk**, and the resumed session publishes the same item back — same id, same words, the same `href`, `media_type` and byte count — with `GET /media/<id>/0` answering the **journaled** bytes, which is what a resumed transcript's image row is drawn from; `image 1` is still a 404 and `GET /items/<id>` names the same media, while `input.send` with `topic: lane:1` and an image is `invalid_args`. |

Every proof runs against `Program::Swarm` (`const PROGRAM`) — the server a tab
really starts — except `t08`, which is about the pipe and names the agent because
the coordinator a swarm is built on reads it the same way, and `t11`/`t12`, which
run a single `evo-agent` (`Program::Agent`, no lanes) to ask the server itself: a
completion, a catalog, one turn's attachments. `t07` starts no server at all — it
reads the offline CLIs — and `t13` is about what an **already-resumed** server
answers, where `t04` and `t06` are the other two that come back to a session on
disk.

## What the proofs found

Against the builds before the integration merge, and what happened to each:

- **Restart exactness (fixed).** A supervisor restart used to be started as
  `serve … --port 0 --resume`: a **bare** `--resume` (the newest journal in the
  folder, which with two tabs in one folder can be the other tab's) and a port no
  client knows. Now the restarted child is told `--resume <that exact journal>` and
  the rewritten ready file names it — pinned by `t04`.
- **`supervisor_pid` (fixed).** The ready file used to publish the session's pid
  and leave `supervisor_pid` null, so the process the app had spawned — the one that
  outlives its session — was named nowhere on disk. It is named now, and `t01`
  checks it.
- **EOF after a run (fixed).** A server that had answered a prompt used to keep
  running after its pipe closed — 45 seconds and counting, against 2 seconds for an
  idle one — which is a server the app's quit cannot stop, since the quit is only
  the pipe. `t08` pins both states now.
- **The pre-minted item id (fixed).** The view used to mint its own `a_…` id,
  stream into it, then remove it and add the journaled id. `t02` checks the id
  `input.send` answers with is the id the answer streams under.
- **`check --json` exits 1 with its document (not a server bug).** §2 says the
  offline CLIs exit 0/1, and `check` exits 1 when it found problems — with the
  problems on stdout. The app read that as a command that could not run, so the
  empty tab said "the check could not run" instead of evo's own lines. The client
  reads the document on that exit now (`store::cli::run_json_reporting`), and `t07`
  checks it.
- **A `hello` is a connection's cursor, not a frame (not a server bug).** The
  stream does send `hello` first, even with a `since` — verifiable by hand — and
  `swarm_client` takes it as the connection's cursor, which is what a client acts
  on. `t01` asked for the wrong thing and now asks for that cursor.
- **A lane reports by calling the `report` tool (not a server bug).** Nothing comes
  back to the coordinator for a lane that merely finishes; `t03` delegates the
  report call, which is how the channel works.

## The app's own tests

What the proofs deliberately do not cover is the shell: single instance, the
window's bounds, the launch-time loads reaching the empty tab, Settings, the quit
sequence, and a boot failure's own screen. Those are `crates/app/tests/`
(`appearance`, `zoom`, `settings`, `quit`, `quit_settings`, `quit_swarm`,
`boot_failure`), plus each UI crate's own tests. And one of them is not a shell but
an end-to-end turn: `attachments_e2e` drives a **real `evo-agent serve`** through
the app's own tab — the file dialog answered by an injected picker, one turn the
server takes and one it refuses — and reads what the window drew, down to the
transcript's image row. The bundle is checked by hand: `scripts/bundle.sh`, then
`scripts/single_instance_check.sh`.
