# `crates/app` — handoff (lane 2)
`evo-desktop` is the process around the window: one instance, the remembered
window, the launch-time loading, and the way the app ends. Everything the window
*shows* is `crates/workspace`.

## `crates/app/src/`

- `lib.rs` — `run(cx)`: lock → window → activation watcher → startup; re-exports.
- `startup.rs` — single instance, `app.json`, the session scan and the catalog
  probe, pushed to the empty tabs through a channel; `CACHE_MAX_AGE`.
- `quit.rs` — `begin(cx)`: each tab's ladder → `app.json` → exit. A `Recent` is
  recorded only when the session **is a file** (`SHUTDOWN_DEADLINE`).
- `menus.rs` — `menus()`, `install(cx)`: ⌘T/⌘W/⌘M/⌘,/About. The Window menu's tab
  items are `workspace::{SelectNextTab, SelectPreviousTab, SelectLastTab}`.
- `theme.rs` — `system|light|dark` → the gpui theme. `about.rs` — the About
  dialog; the two `--version`s are read once, on their own thread, and the same
  probe's answer is what `missing_swarm()` turns into the "evo-swarm not found at
  … — fix it in Settings…" line the empty tab shows (§9.7).
- `settings.rs` — `open()` → `Entity<SettingsPanel>` built `embedded(true)`; the
  dialog is `PANEL_SIZE.0 + 2*DIALOG_PADDING` wide, so its frame is the only one,
  and the panel's `DismissEvent` closes it (`DIALOG_CONTENT_ID` names its box).
  `apply()` re-runs the version probe, so the line above goes when the path is
  fixed.
- `housekeeping.rs` — the tab-dir prune (`TAB_DIR_TTL`). `launcher.rs` — the empty
  tab: history rows, model choosers, the live registry, and the swarm-binary
  problem line (`LauncherData::swarm_problem`). `logging.rs` — `app.log`.
  `bounds.rs` — window geometry, clamped to the display.
- Seams: `WorkspaceView::with_config(Arc<SwarmConfig>)` / `set_swarm_config` /
  `workspace::window_options(cx)`; `store::{app_state, history, single, paths}`.
  `start_background_loads` and `install_menus` are the two halves of `run` a test
  calls itself; `Launcher::data()` is the app's whole contract with the window.
  `tab_engine` drives a tab; `SettingsPanel` is lane 6's.

## Tests and scripts

`cargo test -p evo-desktop --all-targets` — 66, on `tests/common/mod.rs`: `m2_relaunch`
~10 s, `m3_lane_ui` ~9 s, `settings` ~1 s, `quit`, `boot_failure`, `appearance`
fast. `first_run.rs` — an empty `~/.evo/desktop` (a temp `EVO_HOME` whose stub
provider the real `evo-agent` probe answers): the loading hint, then the probed
catalog, then no sessions directory being a first run and not a failure.
`missing_swarm.rs` — `app.json` naming a binary that is not there: the line, the
click that opens Settings, and the same path fixed through the real Save.
`examples/{about,settings}_snapshot.rs --capture <dir>` renders the dialogs
headless in both themes; `first_run_snapshot.rs` renders the three states above.
`scripts/single_instance_check.sh` — two real launches, 11 checks. `scripts/bundle.sh`
— release build → `dist/evo-desktop.app`.

## Open

- The Window menu's tab items carry no key equivalent: the workspace binds
  ⌃⇥/⌃⇧⇥/⌘9 inside `WORKSPACE_CONTEXT`. The keys work; the menu stays silent.
- `DIALOG_PADDING` hard-codes the kit's 16 pt dialog padding;
  `examples/real_gui_run.rs` (lane 3's) is unformatted.
