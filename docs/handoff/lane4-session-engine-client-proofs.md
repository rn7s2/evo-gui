# Handoff — crates/session, tab_engine, swarm_client, proofs, scripts/check.sh (lane 4)

## tab_engine
One Engine per tab on its own thread, driven by `Inbound`, emitting `Update`. Rules (each a fixed bug):
1. No HTTP on the loop: POSTs run on `tab-engine-post` threads (`Inbound::Posted`), lane-list reads on
   `tab-engine-lanes-fetch` (one in flight, `Inbound::LanesFetched`), resyncs (`/transcript` + `/state`,
   on the `tab-engine-fetch` thread) as `Inbound::Resynced`, a lane's two seed reads (its rows and the
   bounded todo replay, `tab-engine-lane-seed`) as `Inbound::LaneSeeded`, and assembly's view + journal
   walk (`tab-engine-assemble`) as `Inbound::Assembled`. A resync on the loop held the stream's own
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
   (its stream would belong to a lane the tab is no longer showing).
4. A stop during boot sets `want_shutdown` + `cancel.cancel()`; the `Booted(Ok)` arm must honour it
   (ladder + exit, no Ready, no assemble) — losing that race hung the tab and leaked the swarm.
5. The lane list is re-read on a `lane-state` event, a watched lane's first connect, and after
   `settled` (500 ms debounce; follow-ups only after a read that answered) — the swarm never
   announces a lane coming up idle.
Tests: `CARGO_TARGET_DIR=target/tab_engine cargo test -p tab_engine` (real swarms, serialized).

## session
Pure model (`TabModel`/`AgentModel`/`LaneList`), no I/O. `lane_down_reason` says nothing unless the
lane is Down and prefers the swarm's own account (kept in `lane_announcements`, since a settled
resync rebuilds rows without output lines); `LaneList::busy()` counts rows; `step_clock()` only for
working/compacting; a run outcome folds the identical error output row before it.
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
- The lane step clock ages only when the list is read.
- `assemble` still reads `/registry` and `/lanes` on the loop (before the stream opens). Only a
  server that stops answering *between* the readiness check and those two reads could hold the tab
  there, and the first frame's ordering is what buys it; the rest of the assembly is off the loop.
