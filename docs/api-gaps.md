# API gaps

What evo-desktop needed from the `evo-swarm` / `evo-agent` serve API and did not
find. Recorded per §10 of docs/PROMPT.md: `../evo-agent` is read-only, so when
something is genuinely missing the client works around it and the gap is written
down here with evidence, rather than patched into evo.

Line references are into `../evo-agent` (the installed binaries'
source). Each entry says what we wanted, what the API offers instead, and what
the client does about it.

## 1. There is no lane cursor, so a lane cannot be watched with zero loss

**Wanted.** A lane watched mid-run should be rendered exactly: open
`GET /lanes/N/events` from a cursor that lines up with the
`GET /lanes/N/transcript` we just folded, and fold everything after it.

**What exists.** `/lanes/N/events` takes the client's cursor and forwards it to
the lane's own serve (`swarm/routes.lisp:99-119`, `swarm/api.lisp:123-132`), and
`/lanes/N/transcript` returns the lane's folded messages — but nothing returns
the lane's *current* event id. The lane's own `GET /health` carries a cursor
(`src/serve/routes.lisp:340-345`, `handle-health`), but the relay exposes only
`/lanes`, `/lanes/N/transcript` and `/lanes/N/events` (`swarm/routes.lisp:123-131`)
— there is no `/lanes/N/health`, and the transcript reply has no cursor field
(`swarm/api.lisp:96-101`).

**Consequence.** With `?since=0` a lane's whole retained log is replayed (up to
20 000 events, `src/serve/events.lisp:21`), which duplicates every row the
transcript already folded; with a live tail, anything emitted between the
transcript fetch and the stream open is lost.

**What we do.** `tab_engine::engine::Engine::lane_todo_seed` opens `?since=0`
for a bounded pass whose only forwarded event is the newest `todo-changed` (a
state-only event that never creates a row), then resumes the live stream at the
id that pass reached. Rows come from the transcript alone. A lane watched
strictly mid-run can therefore miss row-building events emitted inside the
~0.4 s seed window; the lane's next `settled` resync repairs the view.

**What would close it.** A cursor on `/lanes/N/transcript` (the lane's
`last-event-id` at the moment the transcript was folded), or `/lanes/N/health`.

## 2. A lane has no state endpoint

**Wanted.** `GET /lanes/N/state` — a lane's activity, task, todos — so a watched
lane renders from one read plus deltas.

**What exists.** A lane's state reaches a client only as `lane-state` events on
the coordinator's stream, or as the snapshot rows of `GET /lanes`
(`swarm/routes.lisp:78-84`). The `/lanes` row carries state, task, ages, pid,
restarts and goal — but no todos.

**Consequence.** A lane's todo panel can only be built from its `todo-changed`
events, which is why watching a lane needs the replay pass of gap 1 at all.

**What we do.** `Update::Lanes` (the snapshot) plus `lane-state` events for the
list, and the bounded `todo-changed` replay for the panel.

The §7.3 status line is the same story one level down: for a lane it is built
from what the API does carry — the model and provider its transcript's assistant
messages name, the usage they report, and the window its model runs with in
`/registry` — because there is no lane `/state` to read a context window, a
thinking level or a goal from. A lane's goal is the one asymmetry that shows:
`/lanes` carries it, but as the lane's *cached* goal, refreshed only when the
lane's own `/state` is read (at bring-up, `swarm/lanes.lisp:212`), and updated
in place by the status alone afterwards (`note-lane-goal-status`,
`swarm/api.lisp:22-30`). A lane that acquires a goal after boot therefore
reports `{"status": …}` with no id and no token count — not enough for the goal
segment, which needs both — so the line leaves it out rather than write a
half-empty one.

## 3. `POST /steer` is the only command that answers `409 Not now`

**Wanted.** A reliable way to tell "the session was busy" from "the command
failed".

**What exists.** Only `/steer` refuses with `:conflict` → 409
(`src/serve/routes.lisp:240-252`). `POST /interrupt` with nothing running is a
200 with an in-band "nothing to interrupt" line (`src/serve/routes.lisp:264-271`),
and `POST /prompt` always succeeds (it queues).

**Consequence.** The UI's Stop button learns nothing from a status code; and the
one 409 path is a command the UI never sends (§14.7 sends every turn through
`/prompt`).

**What we do.** `Command::Steer` exists in `tab_engine` for tests and
diagnostics — it is what proves the `409 → PostResult::Err(not_now)` path — and
is documented as not-for-the-UI.

## 4. Tailing from "now" is silent about what was skipped

**Wanted.** If a client resumes without a cursor (or with one that has aged
out), say what it missed.

**What exists.** A cursor that fell out of the ring produces a `gap` event
naming the missed range (`src/serve/routes.lisp:515-521`). But with *no* cursor
at all the stream simply starts at the current tip
(`src/serve/routes.lisp:541-546`) — the client sees nothing about the events
between its last view and now.

**Consequence.** A client that fails to persist its cursor loses the interval
silently.

**What we do.** `EventStream` always sends a cursor: `?since=N` on the first
connection, `Last-Event-ID` on reconnects
(`crates/swarm_client/src/stream.rs`). A tab that has no cursor yet asks for the
transcript and the state first, which is the real view of that interval.

## 5. Wire keys are snake_cased from Lisp keywords

**Wanted.** To read the payload tables in the spec prose literally.

**What exists.** Every reply is built from Lisp keywords, and `keyword->json-key`
substitutes `_` for `-` (`src/serve/json.lisp:28-29`). So the wire says
`is_error`, `content_chars`, `stop_reason`, `cache_read`, `cache_write`,
`goal_id`, `tokens_used_live`, `task_age`, `step_age`, `api_key_env` — not the
hyphenated spellings that appear in prose tables.

**Consequence.** None if you read captures; a silent `null` if you trust the
prose.

**What we do.** Every raw-JSON read in `swarm_client` and `tab_engine` uses the
snake_case spelling, cross-checked against the real captures in
`crates/session/tests/fixtures/`.

## 6. A restarted server is only visible from the `hello` event

**Wanted.** To detect that the process behind an event stream was replaced.

**What exists.** Ids restart at 1 with the process and the new process announces
itself with a `hello` event (`src/serve/events.lisp:11-14`) — but a cursor that
is simply replayed may *skip* that `hello` (it is id 1; a client resuming at 40
never sees it), and the log's `gap` logic only fires when a cursor fell out of
the ring (`src/serve/events.lisp:53-58`), which a restarted process cannot
report.

**Consequence.** A client comparing only cursors misses a restart whose new log
has already grown past the old cursor, and then reads a completely different
session's events without noticing.

**What we do.** `crates/swarm_client/src/stream.rs` also records `/health.pid`
(`src/serve/routes.lisp:340-345`) and resets the replay to `?since=0` whenever
the pid changes (`ResetReason::Restarted`), which is what makes the `hello` of a
restarted coordinator observable — proven in
`crates/tab_engine/tests/tab_e2e.rs::a_restarted_coordinator_resyncs`.

## 7. Boot readiness cannot be aborted (resolved in this repo)

**Wanted.** A quit during a tab's boot must not sit out the wait for readiness.

**What existed.** `Server::start` polled until `/health` was ready or the 90 s
deadline passed (`crates/swarm_client/src/server.rs`); there was no way to say
"stop waiting". Since the app owns the child, a quit could have left it behind.

**What we do now.** `swarm_client::Server::start_cancellable(cfg, &BootCancel)`
checks the flag once per poll and, when raised, runs the shutdown ladder with
short graces — `POST /shutdown` when a token and port are known, otherwise
`SIGTERM` and then `SIGKILL` — and returns `Error::Cancelled(outcome)`.
`tab_engine` boots on its own thread for exactly this reason, so an engine
handle dropped during boot stops in about a second
(`crates/tab_engine/tests/tab_e2e.rs::a_handle_stops_within_a_second_while_booting`).

This is the only entry here that was a gap in *our* client rather than in the
serve API; it is recorded because it shaped the engine's threading.
