# Handoff — full-app captures, real-env proofs, stub home (lane 3)

1. `crates/app/examples/app_snapshot.rs` — headless captures of the assembled app window
   (Shell + WorkspaceView + real tab_engine + real evo-swarm), light and dark.
   Run: `cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens --scale 1`
   (default scale 2 = 3200x2000); `--only 07,09` saves just those states (earlier ones run unsaved).
   States: 01 launch, 01b lanes chooser, 02 three tabs, 03 lane todos, 04 tool expanded,
   05 boot failure, 06 reconnecting, 07 overflowing strip, 08 narrow 1000x700, 09 bad run.
   Scripted (docs/screens.md is the contract): temp HOME whose init.lisp registers provider :stub,
   stub-a-preview-2026-09 (200k window) and stub-small-window (3k window, :compact-reserve 2000) so
   09 compacts; an embedded Python SSE stub (CALL/TODOS/SHOW/SLOW/BIG/FAIL rules, usage = chars/4);
   a bad shell script for 05; SIGSTOP/SIGCONT for 06. Env: EVO_SWARM_BIN, EVO_AGENT_BIN,
   EVO_DESKTOP_KEEP_SCREENS_TMP=1.
   Gotchas: a wait must render a frame or a pressed prompt is never posted; wait for /health
   before stopping a tab (a tab stopped while Booting leaked swarms); expect API churn in
   Shell::new, select_tab and TabContentEvent. The run must end with
   `[cleanup] swarm process(es) still alive: 0` and `[done] N picture(s)`.
2. `docs/screens` + `docs/screens.md` — 20 committed 1x pictures (192 colours, ~1.2 MB; 08 is
   1000x700) and the table of what each shows. Re-take after chrome/tab-page/transcript changes.
3. `docs/proofs-real.md` + `crates/swarm_client/examples/{m0_real,real_readout,real_catalog,real_history}.rs`
   — real-backend proofs and findings R1–R4.
4. `scripts/stub_home.sh` — stubbed EVO_HOME; exports `EVO_HOME="$home/.evo/"` (trailing slash matters).
Also: store::history::sessions_dir() ignores EVO_SESSIONS_DIR; swarm_client::redact is applied to
StatusError, Error and BootFailure.log_tail.
