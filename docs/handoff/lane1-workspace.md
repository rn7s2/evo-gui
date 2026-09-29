# Handoff — crates/workspace (lane 1)

Owner: everything in `crates/workspace` except `src/empty_tab.rs`, `src/history.rs`,
`examples/empty_tab_demo.rs` (empty tab owner).

## Structure
- `lib.rs` — modules + re-exports: Bridge/BridgeSender/Revision/Tagged/Worker; LauncherData/TabRecord/
  WorkspaceView/window_options; Launch/SwarmConfig/SHUTDOWN_DEADLINE/stop_in_background;
  RegistryHook/TabContent/TabContentEvent/TabId/TabState; placeholder_history/HistoryRow.
- `bridge.rs` — off-thread I/O (§2.6): `Bridge<T>::spawn(Revision, worker) -> (Bridge, Worker)`,
  `drive_into(cx, live, apply)`, `BridgeSender::{send, rebind, is_closed}`.
- `chrome.rs` — the window (§7.1, §9.8): window_options + WorkspaceView — tab strip in the TitleBar,
  `+` after the last tab, light × revealed by hover, select/close, take_engines, open_tab_count,
  add_tab/close_selected_tab, tab_records, set_launcher_data, on_registry, QuitHook/QuitRequest +
  the should_close ladder.
- `launch.rs` — §3: `start(&SwarmConfig, &Launch) -> Started{engine, updates, tab_dir, store_id}`
  (tab dir + the folder's swarm.lisp lanes model written before spawn); `stop_in_background`.
- `tab.rs` — §9.1/§7.3 state: TabState::{Empty, Booting, Running, Failed{folder,message,log_tail},
  Stopping}; TabContentEvent::{Launch, Resume, FolderPicked, RetryRequested, CloseRequested};
  TabContent{ live: Live{engine, model: TabModel, _pump, tab_dir, store_id, pid, in_flight,
  state_revision, recorded, ticker}, transcripts: BTreeMap<AgentKey, Entity<TranscriptView>>,
  agents: Entity<AgentList>, composer, notice }.
  Pump: spawn_pump batches try_recv (a delta burst = one poll + one render); apply(update) ->
  Live::absorb -> TabModel -> push(Changes) -> shown transcript, todos, composer readout/activity,
  sync_agents, notify.
- `tab_page.rs` — §7.3 frame: agent column (AgentList + one-line folder), center (header glyph/name/
  task + Show-thinking toggle; transcript; todos), composer column + §4 notice line;
  booting/stopping/failed screens; elide_middle/shorten_path.
- tests: `workspace_ui.rs` (headless, no swarm); `tab_swarm.rs` (real evo-swarm via swarm_client
  harness; ONE_AT_A_TIME lock). examples: `workspace_snapshot.rs --capture <dir>` (11 shots).

## Requirement map
§3 boot/fail: launch.rs + tab.rs(apply) + tab_page.rs · §4 notices: tab.rs notice_tone/show_notice
(409 dim, else error, 4 s) · §7.1 chrome.rs · §7.3 tab_page.rs · §9.1 tab.rs spawn_pump/apply/push ·
§9.2 on_composer_event/finish_post · §9.3 select_agent (watch_lane) + sync_agents · §9.4
Update::Registry -> on_registry · §9.5 record_recent · §9.6 launch.rs · §9.7 server_gone/
fail_with_log + badges · §9.8 take_engines/stop_in_background/should_close + QuitHook.

## Open issues (at handoff)
- Silent (SIGSTOPped) server never noticed: SSE 45 s / POST 30 s timeouts don't fire (lane 4).
- 409 unreachable via /prompt in v1; transport failures show raw io text.
- Delegation needs an idle lane (evo's contract).

## Commands
- `cargo test -p workspace` — lib + tab_swarm (real swarms, ~25 s) + workspace_ui
- `cargo test -p workspace --test tab_swarm -- --nocapture` — two-tab latency numbers
- `cargo run -p workspace --example workspace_snapshot -- --capture /tmp/shots` (SNAPSHOT_TRACE=1)

## Since the first handoff (to f00e250)
- Strip: pixel-capped from the window width each frame, scrolls the shown tab into view; `+` pinned.
- Actions `workspace::{SelectTab(usize), SelectNextTab, SelectPreviousTab, SelectLastTab}` bound in
  the "Workspace" context (⌘1–⌘8, ⌘9, ⌃⇥/⌃⇧⇥, ⌘⇧]/⌘⇧[); middle-click closes a tab.
- Activity dots: 6 px success while running (`tab-running-<id>`), muted for an unseen finish
  (`tab-finished-<id>`, cleared on select). `TabContentEvent::{RunFinished, ScreenChanged}`.
- Focus: `TabContent::focus_primary` (Empty → Choosers::focus_primary, Running → composer, else
  window root); every state change emits ScreenChanged and the shown tab retakes the keyboard.
- Window title "<folder> — Evo Desktop" via `sync_window_title`; `window_title()` for tests.
- Notices: transport failures in plain words with raw text in a tooltip (`notice_detail()`).
- Recents recorded only once the session file exists. `TabRecord{window_id, store_id, folder,
  session}`; `set_swarm_config` for Settings (new tabs only).
- Store crate is also yours when a workspace test needs it (swarm.lisp block fix 678a2c0).
