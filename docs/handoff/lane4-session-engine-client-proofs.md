# Handoff — crates/session, tab_engine, swarm_client, proofs, scripts/check.sh (lane 4)

## tab_engine
One Engine per tab on its own thread, driven by `Inbound`, emitting `Update`. Rules (each a fixed bug):
1. No HTTP on the loop: POSTs run on `tab-engine-post` threads (`Inbound::Posted`), lane-list reads on
   `tab-engine-lanes-fetch` (one in flight, `Inbound::LanesFetched`), resyncs (`/transcript` + `/state`,
   on the `tab-engine-fetch` thread) as `Inbound::Resynced`, a lane's seed reads (`tab-engine-lane-seed`)
   as `Inbound::LaneSeeded` (its rows) and `Inbound::LaneTodo` (the bounded todo replay), and assembly's
   view + journal walk (`tab-engine-assemble`) as `Inbound::Assembled`. A resync on the loop held the stream's own
   `Disconnected`/`settled` behind a silent server for a whole request timeout.
   Two synchronous reads are left, both in `assemble` and both before the stream opens
   (`client.registry()` and `fetch_lanes_now`) so the first frame has its registry and lane list; the
   stream itself opens when `Inbound::Assembled` lands, so the frame is still assembled in order —
   rows, state, cache seed, then `/events?since=<health.cursor>`.
2. Read results carry a ticket (`Reads`): a view older than one already applied is dropped (a late
   answer must not replace a newer one), while a result that *failed* answers nothing and so spends no
   revision and stales nothing — `Revisions::get == 0` still means "no view of this lane was ever
   read", which is what tells a lane's first connect to read its rows.
3. `WatchLane` carries an episode number: a seed that comes back after a newer `WatchLane` is dropped
   (its stream would belong to a lane the tab is no longer showing). The lane's *rows* hold its stream
   back; the bounded todo replay is read afterwards, on the same seed thread, and reported apart
   (`Inbound::LaneTodo`) — the replay reads a live log, and a lane put to work while the watch was still
   reading had its first run swallowed by a replay the tab never sees the events of, while the stream
   then resumed *past* it. The lane's stream tails live (`since = None`).
4. A lane's event log belongs to its process: a *different* pid for the watched lane (`/lanes` or a
   `lane-state`) is a new log whose ids begin again at 1, so the stream is opened again from no cursor
   (`Engine::reopen_lane`) and the rows read again. Without it the stream sat silent until the new log
   grew past the dead process's cursor (proven in m3, whose lane runs *before* it dies).
5. A stop during boot sets `want_shutdown` + `cancel.cancel()`; the `Booted(Ok)` arm must honour it
   (ladder + exit, no Ready, no assemble) — losing that race hung the tab and leaked the swarm.
6. The lane list is re-read on a `lane-state` event, a watched lane's first connect, and after
   `settled` (500 ms debounce; follow-ups only after a read that answered) — the swarm never
   announces a lane coming up idle.
Tests: `CARGO_TARGET_DIR=target/tab_engine cargo test -p tab_engine` (real swarms, serialized).

## session
Pure model (`TabModel`/`AgentModel`/`LaneList`), no I/O. `lane_down_reason` says nothing unless the
lane is Down and prefers the swarm's own account (kept in `lane_announcements`, since a settled
resync rebuilds rows without output lines); `LaneList::busy()` counts rows; `step_clock()` only for
working/compacting, `step_clock_at(now)` for the ticking one; a run that failed is said once — the
failing message's own error is the row (`error: M`), and the run's outcome row is only for a failure
no message carries (`Run failed`, `aborted`, `length`).
The lane step clock: `/lanes` reports a step *age*, not a step start, so `apply_lanes_at(body, now)` /
`apply_lane_state_at(data, now)` stamp when the age was seen (`LaneRow::step_age_at_millis`) and
`LaneRow::step_clock_at(now_millis)` counts on from there (a `lane-state` that moves a lane *into*
working starts the clock at 0 from that stamp); the un-stamped `apply_lanes`/`apply_lane_state` and
`step_clock()` are the "as reported" pair a caller with no clock keeps. `TabModel::on_lanes_at` and
`on_event_at` (the lane-state branch) are the stamped paths.
`CARGO_TARGET_DIR=target/session cargo test -p session`.

## swarm_client
Process/HTTP/SSE client; patience from `ServerConfig`/`TabSpec` (request 30 s, stream 45 s);
`Server::start_cancellable` + `BootCancel`; a cancelled boot gives a reachable supervisor 5 s to take
its lanes down — lanes are their own process groups. `process_alive` reaps our own children.

## proofs
m1–m4 on real swarms via `Drive`, folding Updates as workspace's tab.rs does.
`CARGO_TARGET_DIR=target/proofs cargo test -p proofs`.

## scripts/check.sh
Per-crate fmt --check + tests (own target dirs), workspace clippy, table; `--head` runs a detached
worktree of HEAD with `CARGO_TARGET_DIR=target/head`. Logs in target/check/. Needs an idle machine.

## Open
- The lane step clock is wired in `session` but not yet in the UI: lane 1 passes `now` from the 1 s
  ticker (`TabModel::on_lanes_at` on the way in, `LaneRow::step_clock_at(now)` on the way out), and
  the ticker has to run while a *lane* is busy, not only while the coordinator has a step.
- `assemble` still reads `/registry` and `/lanes` on the loop (before the stream opens). Only a
  server that stops answering *between* the readiness check and those two reads could hold the tab
  there, and the first frame's ordering is what buys it; the rest of the assembly is off the loop.
- A lane stream carries no cursor of its own against a lane that restarts *and* whose new log grows
  past the old cursor before the pid change is seen: reopening on the pid is what covers it
  (`Engine::reopen_lane`), and the relay's `?since=N` is only ever read on a first connect.
- The proofs' `Drive` folds a thread's updates in arrival order; a test that delegates to a lane must
  wait for the lane list to say it is *idle* first — the swarm refuses a lane that is not, and a fast
  watch no longer gives a lane its boot or its run time for free.
