# Handoff — crates/session, tab_engine, swarm_client, proofs, scripts/check.sh (lane 4)

## tab_engine
One Engine per tab on its own thread, driven by `Inbound`, emitting `Update`. Rules (each a fixed bug):
1. No HTTP on the loop: POSTs run on `tab-engine-post` threads (`Inbound::Posted`), lane-list reads on
   `tab-engine-lanes-fetch` (one in flight, `Inbound::LanesFetched`); only `assemble` reads
   synchronously (`fetch_lanes_now`) so the first frame has its lane list.
2. A stop during boot sets `want_shutdown` + `cancel.cancel()`; the `Booted(Ok)` arm must honour it
   (ladder + exit, no Ready, no assemble) — losing that race hung the tab and leaked the swarm.
3. The lane list is re-read on a `lane-state` event, a watched lane's first connect, and after
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
- Resyncs, cache seed, Refetch and WatchLane still do HTTP on the engine loop (a frozen server
  stalls those up to the request timeout; POSTs and lane reads are off it).
- The lane step clock ages only when the list is read.
