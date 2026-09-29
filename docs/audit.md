# Requirement audit — docs/PROMPT.md against the tree

Every requirement in `docs/PROMPT.md` §2–§14 (the §1 reading list aside), with the evidence that
exists **now**. Lane 6, 2026-09-29, 18:25Z.

- **Snapshot.** HEAD `0ab1375`; the working tree is clean.
- **How it was checked.** Every row was re-verified against that commit: the code was read
  (`rg`/`read`) and every citation in this document — each `path:line`, each parenthetical symbol,
  each test name — was resolved against the tree with `scripts/audit_refs.sh`, which reports what no
  longer holds (0 broken at this snapshot). `cargo fmt --check` is clean for every crate;
  `cargo clippy --workspace --all-targets` prints one warning line, the third-party `block v0.1.6`
  future-incompatibility note, and no lint of ours.
- **What was *not* done.** Cited tests were **not executed**: the long real-swarm suites
  (`crates/proofs`, `tab_swarm.rs`, `tab_e2e.rs`, `swarm_e2e.rs`, `crates/app/tests/m*`) are the
  gate's to run, and `scripts/check.sh` is that run — see gap 1. A row that cites a test means its
  name is in the source at this snapshot; running it is what turns the row into proof.
- **Statuses.** *met* — implemented, with evidence present now. *partial* — some part of the
  requirement has no implementation, or no evidence at this snapshot.
- **Line numbers** are from `0ab1375`.

## Gaps first

| # | gap | status | owner |
|---|---|---|---|
| 1 | §11's `scripts/check.sh` and M4's "`cargo test` green": no test suite was run at this snapshot. What was checked here instead: `cargo fmt -p <crate> -- --check` clean for all eleven crates, and `cargo clippy --workspace --all-targets` with no lint of ours. The last full gate run is the `scripts/check.sh --head` of 2026-09-30 01:52–02:00 (`target/check/head-*.log`), whose worktree was `e152cc7`: every test binary green, 11 clippy warnings that are gone at this snapshot. Four commits landed after it (`6abe4db`, `8a83f5b`, `b33fce5`, `0ab1375`) | partial | the gate run, next |
| 2 | §12 M0's and M4's *supplementary* pictures (the Dock icon, the appearance/geometry frames, the About dialog) live only under `/tmp/evo-app-check` (`docs/proofs.md:67,370,441,500`), which macOS clears on its own schedule. The pictures the milestones actually ask for are committed: the twenty in `docs/screens/`, the four real-run frames and the bundle launch in `docs/screens/real/` | partial | `docs` |

Nothing in §2–§14 is **missing** outright: every requirement has an implementation and a test,
a capture, or a committed proof transcript behind it.

Two gaps that this audit used to carry are closed, and the rows are gone rather than reworded:
a tool payload nested deeper than one object level now renders as indented rows (gap 5, closed by
`a_nested_payload_renders_as_rows_indented_under_their_key`), and a lane that cannot register its
model now has a test of its own (`session::tests/tab.rs::a_lane_that_cannot_register_its_model_says_so`).

---

## §2 Rules

| # | requirement | status | evidence |
|---|---|---|---|
| 1 | One instance: a second launch activates the running window and exits 0; no stale lock | met | `crates/store/src/single.rs:101` (`try_lock`), `crates/app/src/lib.rs:150` (`SingleInstance::acquire`); `store::tests/single_instance.rs::a_second_process_activates_the_first_and_the_lock_dies_with_it`, `::a_killed_process_leaves_no_stale_lock`; `docs/proofs.md` "A second launch activates instead of duplicating" |
| 2 | Spawn the binaries as-is; everything shown comes from HTTP | met | `crates/swarm_client/src/server.rs:280` builds the argv, `:750` sets the env; no wrapper and no patched binary: `scripts/` only ever names the binaries as candidates (`scripts/stub_home.sh`), and the crates spawn the path `app.json` names; every byte the UI shows comes from a reply — `crates/tab_engine/src/engine.rs:619` (`assemble`) |
| 3 | Own only `~/.evo/desktop/`; journals and swarm dirs read-only | met | `crates/store/src/paths.rs:1-12` (the layout, the one exception named), `crates/store/src/swarm_config.rs:40` (`swarm_lisp_path`); `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` (the real file's mtime is asserted unchanged) |
| 4 | Secrets stay in files: 0600 token, never logged/shown, never in `app.json` | met | `crates/store/src/paths.rs:24` (`TOKEN_FILE`), `:33` (`FILE_MODE`), `crates/swarm_client/src/http.rs:48` (`Token`'s `Debug`), `crates/swarm_client/src/redact.rs:98` (`redact`); `swarm_client`'s `redact.rs::the_real_500_body_loses_its_token`, `server.rs::a_log_tail_is_redacted` |
| 5 | No second protocol: no polling where an event exists, no re-deriving state, no lane side channels | met | `crates/swarm_client/src/stream.rs` (SSE, `Last-Event-ID`), `crates/tab_engine/src/engine.rs:43` (the only timed read is the 500 ms lanes follow-up), `:619` (the registry is read once at assembly), `:1087` (`resync`, and it runs off the loop) |
| 6 | Never block the UI thread; results land via weak-entity updates with a revision check | met | `crates/tab_engine/src/engine.rs` runs on a thread of its own and hands every `POST` and every read off the loop (`:260`, `:311`); `crates/app/src/startup.rs:74` (the scan on its own thread, `store::history::scan_in_background`); `crates/workspace/src/bridge.rs`; revision guards in `session::TabModel`; `workspace::tests/tab_swarm.rs::two_tabs_stream_at_once_and_the_ui_stays_responsive`, `session::tests/tab.rs::a_stale_transcript_is_dropped` |
| 7 | No raw sexprs and no raw JSON blobs in the UI | met | journals are parsed in `store::sexp` and reduced to domain rows (`session::AgentModel`); a tool call's arguments render as key/value rows, a nested object as rows indented under their key, and past four levels (or for an array over twenty) a summarised `{…3 keys}` row — the compact JSON is in that row's tooltip, the one place it survives — `crates/transcript/src/rows.rs:892` (`tool_row`); `transcript::tests.rs::tool_arguments_read_as_a_key_value_list`, `::a_nested_payload_renders_as_rows_indented_under_their_key`, `::a_payload_nests_four_levels_and_summarises_the_rest` |
| 8 | Markdown rendered live while it streams, one retained document per message, faded appends | met | `crates/transcript/src/lib.rs:145` (`set_text(source so far)`), `:148` (`TextViewState::markdown`), `crates/transcript/src/rows.rs:102` (`TextViewMotion::with_stream_fade`); `transcript::tests.rs::an_assistant_document_is_created_when_the_row_is_shown_and_then_retained`, `::a_waiting_message_shows_pips_until_its_first_delta`; the four frames in `docs/screens/real/` are that path on a real provider, and `02-three-tabs-light.png` is mid-stream with a heading, a list, a table and a fence |

## §3 Starting a tab's swarm

| requirement | status | evidence |
|---|---|---|
| cwd, argv (`serve --port --token-file [--workers] [--model] [--resume]`), `EVO_SERVE_WATCH_PID`, stdout+stderr → `swarm.log` | met | `crates/swarm_client/src/server.rs:280-302` (argv), `:750` (the watch pid), `crates/store/src/paths.rs:26` (`LOG_FILE`); `swarm_client::tests/swarm_e2e.rs::boot_reads_and_the_shutdown_ladder` |
| Pick the port by binding `127.0.0.1:0`; also read the port the server prints for `--port 0` | met | `crates/swarm_client/src/server.rs:699` (`free_port`), `:755` (`port_from_log`); `swarm_client::tests/swarm_e2e.rs::port_zero_reads_the_port_the_server_printed` |
| `--workers` only when asked; never `--allow-remote` | met | `crates/swarm_client/src/server.rs:287` (only `Some`), `:865` (asserts the flag is never passed) |
| Ready = token non-empty **and** `/health` 200 with `name`/`features`; 100 ms poll, 90 s deadline; early exit detected; last ~40 log lines on failure | met | `crates/swarm_client/src/server.rs:55` (`Readiness`), `:204-205` (90 s / 100 ms), `:526` and `:770` (`log_tail`); `swarm_client::tests/swarm_e2e.rs::boot_failure_carries_the_log_tail`; `evo_desktop::tests/boot_failure.rs::a_swarm_binary_that_is_not_there_shows_the_reason_and_retries`, `::a_folder_that_cannot_be_written_fails_the_tab_before_anything_starts` |
| Shutdown ladder `POST /shutdown` ≤10 s → `SIGTERM` 5 s → `SIGKILL`, never kill first, tabs in parallel | met | `crates/swarm_client/src/server.rs:179-181` (the deadlines), `:552` (`shutdown`), `crates/tab_engine/src/engine.rs:201` (`shutdown_all`); `swarm_client::tests/swarm_e2e.rs::two_swarms_shut_down_in_parallel`, `tab_engine::tests/tab_e2e.rs::shutdown_all_stops_every_tab`, `::shutdown_leaves_no_process`; and the race a stop can lose — `::a_handle_stops_within_a_second_while_booting`, `::a_swarm_stopped_while_booting_leaves_nothing` |
| A restarted coordinator is a new event log (ids from 1, `hello`): notice, refetch state, keep the view | met | `crates/swarm_client/src/stream.rs:99` (`ResetReason::Restarted`), `crates/session/src/model.rs:494` (`hello`/`session-switched`), `crates/tab_engine/src/engine.rs:1087` (`resync`); `tab_engine::tests/tab_e2e.rs::a_restarted_coordinator_resyncs`, `session::tests/tab.rs::hello_after_a_restart_keeps_one_set_of_rows`, `session::tests/model.rs::event_ids_restarting_at_one_do_not_disturb_the_rows`, `crates/proofs/tests/m4_coordinator_restart.rs::m4_coordinator_restart` |

## §4 Endpoints and statuses

| requirement | status | evidence |
|---|---|---|
| Bearer on every request, `Connection: close`, loopback only | met | `crates/swarm_client/src/http.rs:177` (the request line), `:102` (host fixed to `127.0.0.1`) |
| POST replies are one envelope (`ok`/`status`/`error`/`output`/`data`/`choices`/`cursor`/`task`) | met | `crates/swarm_client/src/api.rs:548-566` (`Envelope`) |
| Coordinator endpoints: `/health`, `/state`, `/transcript`, `/registry`, `/events`, `/prompt`, `/interrupt`, `/shutdown` | met | `crates/swarm_client/src/api.rs:661-737`; callers in `crates/tab_engine/src/engine.rs:619` (`/registry`, `/lanes`, `/transcript`, `/state`, cache seed) and `crates/workspace/src/tab.rs:1013` (`/prompt`, `/interrupt`) |
| `/command` present but **not used in v1** | met | `crates/swarm_client/src/api.rs:732` exists; no caller outside the client library (v1 ships no command surface, §13) |
| Swarm endpoints: `/lanes`, `/lanes/N/transcript`, `/lanes/N/events` | met | `crates/swarm_client/src/api.rs:686-705`; `crates/tab_engine/src/engine.rs:386` and `:1171` (lane transcript, and the todo seed), `crates/swarm_client/src/stream.rs:58` (the lane stream target) |
| Statuses 400/401/404/409/422/503 mapped: 409 dim, 422 the server's `error`, 503 shutting down | met | `crates/workspace/src/tab.rs:95` (`notice_tone`), `:117` (`notice_words`), `crates/tab_engine/src/types.rs:140` (the request/stream patience); `tab_engine::tests/tab_e2e.rs::refusals_come_back_typed`, `workspace::tests/tab_swarm.rs::a_swarm_that_stops_answering_reconnects_and_refuses_quietly` |
| A server that is there but silent is noticed while a POST waits | met | `crates/swarm_client/src/api.rs:614` / `crates/swarm_client/src/http.rs:121` (`with_stream_timeout`), `crates/tab_engine/src/engine.rs:311` (a `POST` gives up off the loop and says so); `tab_engine::tests/tab_e2e.rs::a_silent_swarm_is_noticed_while_a_post_waits`, `::a_resync_against_a_silent_swarm_does_not_hold_the_stream` |

Note: the envelope's `output` lines are folded into the error text for a 2xx-but-`ok:false` reply
(`crates/swarm_client/src/api.rs:632`); a *successful* reply's `output` is unused because nothing in
v1 posts a command that produces any.

## §5 Events

| event | status | evidence (all in `crates/session/src/model.rs` unless noted) |
|---|---|---|
| `run-start`, `turn-start` → activity, step clock | met | `:475`; `session::tests/model.rs::the_step_clock_follows_the_step_boundaries`, `session::tests/tab.rs::the_step_clock_is_stamped_by_the_caller`, `session::tests/lanes.rs::a_step_clock_counts_on_from_the_moment_the_age_was_seen` |
| `message-start`, `text-delta` → the streaming markdown row | met | `:315`, `:319`; `session::tests/model.rs::message_start_and_deltas_build_one_streaming_row`, `::the_event_stream_and_the_transcript_agree` |
| `thinking-delta` → hidden by default, per-tab toggle shows it dim | met | `:327`; `transcript::tests.rs::thinking_is_hidden_until_the_view_reveals_it`; toggle `crates/workspace/src/tab_page.rs:321` (`Show thinking`/`Hide thinking`) |
| `tool-call-start` → a tool row | met | `:335`; `transcript::tests.rs::a_tool_row_opens_on_click` |
| `tool-result` → completes that row | met | `:339`; `session::tests/model.rs::rebuild_pairs_tool_calls_with_their_results`, `::an_orphan_tool_result_still_gets_a_row`, `::a_replayed_tool_call_updates_its_row_instead_of_stacking_a_copy` |
| `message-end` → closes the row; `usage` feeds the cache/context segments | met | `:353`; `crates/session/src/readout.rs:196` (`fold_message_end`); `session::tests/readout.rs::a_message_end_moves_the_line_with_the_run` |
| `run-end` → run state | met | `:465`; `session::tests/model.rs::a_run_that_ended_badly_gets_an_outcome_row`, `::a_failed_run_says_it_once`, `transcript::tests.rs::a_run_that_ended_badly_renders_as_its_own_notice_row` |
| `steering`, `user-input` → a user row | met | `:354` |
| `compaction-start`/`-end`, `provider-retry` → dim rows | met | `:402`, `:410`, `:420`; `session::tests/model.rs::provider_retry_is_a_dim_notice`, `::a_manual_compaction_drives_activity_and_dim_rows` |
| `todo-changed` → the todo panel of the agent that emitted it | met | `:446`; `session::tests/model.rs::a_todo_change_replaces_the_whole_list`, `session::tests/tab.rs::the_todo_panel_follows_the_selected_agent` |
| `task-start`/`task-end`, `settled` → activity; `settled` is a resync point | met | `:449`, `:457`, `:486`; `tab_engine::tests/tab_e2e.rs::prompt_streams_and_settles_with_a_resync` |
| `output` → dim/notice/error line in that agent's transcript | met | `:385`; `session::tests/model.rs::output_lines_take_the_style_the_server_sent`; `output` is also where evo's own words are recognised and given rows of their own (below) |
| — evo's goal nudges, command answers, and its words about a lane are their own quiet rows, not dim text | met | `:952` (`goal_nudge_row`), `:981` (`command_note_row`), `:1070` (`lane_row`), `:1151` (`lane_notice_tone`); `transcript/src/rows.rs:584` (`command_note_row`), `:627` (`goal_nudge_row`), `:1493` (`lane_notice_row`); `session::tests/model.rs::every_goal_nudge_evo_writes_becomes_its_own_row`, `::every_command_answer_becomes_its_own_row`, `::every_line_the_swarm_writes_becomes_a_lane_row`, `::the_swarms_words_about_a_lane_are_not_the_readers`, `::a_rebuilt_transcript_reads_a_goal_nudge_too`, `transcript::tests.rs::a_command_note_is_one_quiet_line_that_opens`, `::a_goal_nudge_is_one_quiet_line`, `::a_lane_notice_is_one_quiet_line` |
| — an injected message (an extension's context) is a collapsed `Context · <what it is>` row, never a user turn | met | `:1174` (`context_key`), `transcript/src/rows.rs:554` (`context_row`), `:610` (the words a key is named by); `session::tests/model.rs::an_injected_message_is_context_rather_than_a_user_turn`, `::a_message_without_a_context_key_is_still_a_user_turn`, `transcript::tests.rs::injected_context_is_one_quiet_line_that_opens_on_click`, `::injected_context_does_not_open_a_turn`, `::an_opened_context_is_capped_and_scrolls_on_its_own`; the four frames in `docs/screens/real/` show it |
| `session-switched` → refetch everything | met | `:494` (the same arm as `hello`); `session::tests/tab.rs::a_reset_refetches_every_agent_that_is_loaded` |
| `lane-state` → the left lane list (coordinator stream) | met | `:507`; `session::tests/lanes.rs::the_coordinator_stream_drives_the_whole_lane_list`, `::a_lane_state_event_updates_its_row` |
| `report` → a report row | met | `:372`; `session::tests/model.rs::a_report_event_becomes_a_report_row`, `transcript::tests.rs::a_lane_report_is_headed_with_its_lane` |
| `hello`, `gap` → refetch (and a notice for `gap`) | met | `:494`, `:498`; `session::tests/model.rs::resync_events_and_lifecycle` |
| `ready`, `shutdown`, `bye` → lifecycle only | met | `:513` — the catch-all arm renders nothing (documented as lifecycle) |
| `unprintable-event` → ignore, log only | met | `:510`; `session::tests/tab.rs::a_tui_internal_event_is_not_a_server_event` |
| SSE rules: consecutive ids, `Last-Event-ID`/`?since=`, `:` keepalives, one stream per server, one per watched lane | met | `crates/swarm_client/src/sse.rs:34` (`feed`), `crates/swarm_client/src/stream.rs:42` (`since`), `:48`/`:58` (the two targets), `crates/tab_engine/src/engine.rs` (one coordinator stream, one lane stream); `tab_engine::tests/tab_e2e.rs::watching_a_lane_switches_the_single_stream`, `swarm_client::tests/swarm_e2e.rs::prompt_streams_events_and_the_stream_resumes` |

## §6 App data

| path | status | evidence |
|---|---|---|
| `app.json` (bounds, tab set, binaries, schema version) | met | `crates/store/src/app_state.rs:198` (`AppState`), `crates/app/src/bounds.rs:20` (`window_options`), `crates/app/src/quit.rs:150` (`remember_tab_set`); `evo_desktop::tests/quit.rs::quitting_starts_once_and_runs_to_the_end` |
| `lock` (flock, pid inside) | met | `crates/store/src/single.rs:101`; `store::tests/single_instance.rs::a_killed_process_leaves_no_stale_lock` |
| `model-cache.json` | met | `crates/store/src/model_cache.rs`; `empty_tab::tests::the_cached_catalog_fills_the_choosers_and_the_lanes_availability` |
| `probe/` (scratch cwd, catalog only) | met | `crates/store/src/paths.rs:162` (`probe_dir`), `crates/tab_engine/src/catalog.rs:85` (the two throwaway probes) |
| `tabs/<uuid>/` `token` (0600), `swarm.log`, `tab.json` `{folder, session, swarm_id, models, workers}` | met | `crates/store/src/tab.rs:33-43` (`TabState`), `crates/store/src/paths.rs:24,26,28`; `tab_engine::tests/tab_e2e.rs::shutdown_leaves_no_process` writes and reads them |

## §7.1 Window and chrome

| requirement | status | evidence |
|---|---|---|
| 1600×1000 clamped to the work area, min ~1000×700, bounds restored from `app.json` | met | `crates/workspace/src/chrome.rs:41,45` (`DEFAULT_WINDOW_SIZE`, `MIN_WINDOW_SIZE`), `crates/app/src/bounds.rs:45` (`clamp`); `bounds.rs::a_window_bigger_than_the_display_fills_it`, `::an_offscreen_window_is_pulled_back_inside`, `::a_window_smaller_than_the_minimum_is_grown_and_centered`, `chrome.rs::a_small_work_area_shrinks_the_window_inside_it`; `docs/proofs.md` "The window's geometry" |
| Custom title bar (`TitleBar::window_options`), title bar = the strip | met | `crates/workspace/src/chrome.rs:159` (`window_options`), `crates/app/src/lib.rs:202` (one `open_window`); `docs/screens/01-launch-light.png` |
| One `Tab` per swarm: folder name, tooltip path + state, close button, macOS prefix left for the traffic lights | met | `crates/workspace/src/chrome.rs:55` (`TITLE_BAR_LEFT_INSET`), `:741` (`render_tab`), `crates/workspace/src/tab.rs:494` (`title`), `:502` (`tooltip`); `workspace::tests/workspace_ui.rs::a_tab_shows_its_close_button_when_the_pointer_is_on_it`, `::clicking_a_tab_selects_it` |
| `+` always present at the end of the bar, appends and selects an empty tab | met | `crates/workspace/src/chrome.rs:823` (`render_add_tab_button`); `workspace_ui.rs::the_add_button_appends_and_selects_a_new_tab` |
| Overflow scrolls, tab width capped | met | `crates/workspace/src/chrome.rs:674` (`render_tab_strip`), `:48` (`TAB_MAX_WIDTH`); `workspace_ui.rs::an_overflowing_strip_keeps_the_add_button_and_shows_the_selected_tab`; capture `docs/screens/07-tab-strip-light.png` |
| One window only | met | `crates/app/src/lib.rs:202` (one `open_window`); no New Window item in `crates/app/src/menus.rs` |
| (polish) a tab shows what happened while you were not looking, and the window is named for the tab you are on | met | `crates/workspace/src/chrome.rs:711` (`render_tab_activity`: a `success` dot while the coordinator has a run in flight, muted when a background tab's run ended), `:510` (`sync_window_title`), `crates/workspace/src/tab.rs:502` (the tooltip's `stopping the swarm…` / `swarm gone: the server exited`); `workspace::tests/tab_swarm.rs::the_strip_dots_a_run_and_the_finish_a_background_tab_kept`, `workspace_ui.rs::the_window_is_named_after_the_folder_of_the_tab_being_shown` |
| (polish) selecting an empty tab puts the keyboard on the first chooser | met | `crates/workspace/src/tab.rs:424` (`TabContent::focus_primary`), `crates/workspace/src/chrome.rs:492` (`focus_selected`), `crates/workspace/src/empty_tab.rs:282` (`Choosers::focus_primary`); `empty_tab::tests::focus_primary_lands_the_keyboard_on_the_first_chooser`, `::focus_primary_answers_false_when_the_window_refuses_the_keyboard` |
| The tab keys the menu bar shows are bound app-wide, not per window | met | `crates/workspace/src/chrome.rs:109` (`bind_tab_keys`), `crates/app/src/menus.rs:152-154` (Window ▸ Select Next/Previous/Last Tab); `workspace_ui.rs::the_tab_keys_are_bound_app_wide_so_the_menu_can_show_them` |

## §7.2 Empty tab

| requirement | status | evidence |
|---|---|---|
| Rows 1–3: coordinator model, lanes model, worker count; each defaults to **Default** | met | `crates/session/src/launcher.rs:31` (`DEFAULT_KEY`), `:278` (`workers_chooser`), `crates/workspace/src/empty_tab.rs:643` (`render_choosers`); `empty_tab::tests::a_fresh_tab_is_default_everywhere_and_says_it_is_loading`, `workspace_ui.rs::the_empty_tab_defaults_every_chooser` |
| Coordinator model passed only when chosen (`--model`) | met | `crates/session/src/launcher.rs:1074` (`plan`); `empty_tab::tests::choosing_a_lanes_model_shows_the_file_it_is_written_to_and_lands_in_the_plan` |
| Lanes model written to the project, not passed | met | `crates/store/src/swarm_config.rs:31,33` (the markers), `crates/session/src/launcher.rs:530` (the note); `workspace::tests/tab_swarm.rs::a_lanes_model_is_written_to_the_folders_swarm_lisp_and_default_takes_it_away`, `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` |
| Workers only when chosen (else evo's `:swarm-workers`, else 6) | met | `crates/session/src/launcher.rs:278-292`, `:1074`; `session::tests/launcher.rs::the_workers_chooser_spans_one_to_sixty_four`, `::the_configured_swarm_workers_comes_from_the_registry_settings` |
| Models fixed when the tab is created; no selector on the tab page | met | `crates/workspace/src/tab_page.rs:377` (the composer column, and nothing else in the page); `docs/usage.md` "the tab page has no model selector" |
| `Select folder…` spanning the three rows → native dialog (`rfd`), pick starts the swarm, cancel leaves the tab | met | `crates/workspace/src/empty_tab.rs:762` (`render_folder_button`), `:500` (`pick_folder`, `rfd`); `empty_tab::tests::a_chosen_folder_launches_with_the_plan`, `::a_cancelled_folder_pick_leaves_the_tab_where_it_was`; `crates/workspace/Cargo.toml` (`rfd`) |
| History: resumable swarms; a row opens a new tab with `--resume` and that cwd; the journal's lane count shown read-only | met | `crates/workspace/src/empty_tab.rs:952` (`render_history`), `:901` (`history_key`), `:1215` (`render_item`), `crates/session/src/launcher.rs:711` (`meta_line`); `empty_tab::tests::a_history_row_resumes_its_session_in_its_folder`, `evo_desktop::tests/m2_relaunch.rs::a_swarm_that_ran_once_is_offered_first_and_comes_back` |
| The empty tab is a real tab; the app always keeps one | met | `crates/workspace/src/chrome.rs:571` (`close_tab`); `workspace_ui.rs::closing_the_last_tab_leaves_a_fresh_empty_tab`, `::closing_a_tab_selects_its_neighbour` |
| (beyond spec) option detail lines, greyed lanes models with the reason, the `swarm.lisp` caption, the `open at last quit` pill, empty/scanning/error states | met | `crates/workspace/src/empty_tab.rs:149` (`OPEN_AT_QUIT_TEXT`), `:534` (`caption`), `:1365` (the list's empty states), `crates/session/src/launcher.rs:38` (`NEEDS_EXTENSION_API`); `empty_tab::tests::the_cached_catalog_fills_the_choosers_and_the_lanes_availability`, `::only_a_row_the_app_had_open_wears_the_badge`, `::the_history_list_says_which_nothing_it_is`; captures `01-launch-light.png`, `01b-lanes-chooser-dark.png` |
| A catalog that could not be probed reads as **one** warning line, with the server's own error in its tooltip, and every chooser still usable on Default | met | `crates/workspace/src/empty_tab.rs:87,89` (the two sentences), `:97` (`warning_ink`: the light theme darkens the kit's amber to 4.9:1, the dark theme keeps `warning` at 12.9:1), `:534` (the line and its detail), `:719` (the tooltip); `empty_tab::tests::a_catalog_failure_reads_as_one_sentence_and_keeps_the_servers_words_for_the_hover`, `::a_failed_refresh_says_the_last_catalog_is_still_in_use`, `evo_desktop::tests/first_run.rs::a_catalog_that_cannot_be_probed_says_so_on_the_tab` |
| A first run — no `~/.evo/sessions` yet — reads as the calm empty history, not a read failure | met | `crates/store/src/history.rs:207` (`unreadable`), and `crates/app/src/startup.rs:110` asks it instead of testing `is_dir()`; `history::tests::a_sessions_directory_that_is_not_there_yet_has_nothing_to_report`, `::a_file_where_the_sessions_directory_belongs_is_reported`; `evo_desktop::tests/first_run.rs::a_first_run_shows_the_loading_hint_then_the_probed_catalog` |
| A swarm binary that cannot run is said under the folder card, and that line opens Settings | met | `crates/workspace/src/empty_tab.rs:132` (`SWARM_MISSING_ID`), `:691` (`render_swarm_problem`), `crates/app/src/about.rs:90` (`missing_swarm`), `crates/app/src/launcher.rs:268` (`set_swarm_problem`); `evo_desktop::tests/missing_swarm.rs::a_swarm_binary_that_cannot_be_run_is_said_at_startup_and_fixed_in_settings` |

## §7.3 Tab page

| requirement | status | evidence |
|---|---|---|
| Left ~260 px: `main` + one row per lane, status glyph `●◐○◌✗`, task truncated, step clock while working, driven by `/lanes` + `lane-state`, selection drives the centre | met | `crates/agent_list/src/lib.rs:34` (`COLUMN_WIDTH`), `:258` (`header`), `:580` (`status_color`), `crates/session/src/lib.rs:197` (the glyph table); `agent_list::tests::rows_run_main_then_one_per_lane`, `::every_status_gets_its_glyph_and_its_word`, `::the_header_counts_the_lanes_and_the_busy_ones`, `::a_long_task_ellipsizes_and_never_pushes_the_clock_out`, `::clicking_a_row_asks_the_owner_to_select_that_agent` |
| Left: the step clock keeps counting between reads | met | `crates/agent_list/src/lib.rs:1174` (`a_lane_clock_counts_on_from_the_moment_its_age_was_read`), `session::tests/lanes.rs::a_step_clock_counts_on_from_the_moment_the_age_was_seen`, `session::tests/tab.rs::the_lane_step_clock_is_stamped_by_the_caller`, `workspace::tests/tab_swarm.rs::a_busy_lanes_clock_advances_with_nothing_asked_of_the_swarm` |
| Left: the keyboard walks the rows (arrows, Home/End) | met | `crates/agent_list/src/lib.rs:208` (`lanes`); `agent_list::tests::the_arrows_walk_the_rows_and_home_and_end_go_to_the_ends`, `::a_focused_list_is_the_only_thing_the_arrows_move` |
| Centre: the selected agent's transcript (coordinator `/transcript`, lane `/lanes/N/transcript`) | met | `crates/workspace/src/tab_page.rs:347` (`render_center_column`), `crates/tab_engine/src/engine.rs:386`; `tab_engine::tests/tab_e2e.rs::boot_assembles_the_view`, `workspace::tests/tab_swarm.rs::a_delegated_lane_shows_its_own_transcript`, `tab_e2e.rs::a_lane_watched_while_idle_streams_its_own_run` |
| Centre: assistant markdown rendered live, one document per message, tail-follow + jump affordance | met | `crates/transcript/src/lib.rs:145,148`; `transcript::tests.rs::an_assistant_document_is_created_when_the_row_is_shown_and_then_retained`, `::documents_stay_bounded_as_the_reader_scrolls_away`, `::a_resync_of_the_same_rows_changes_nothing`, `::markdown_tables_use_the_sideways_scrolling_layout` |
| Centre rows: user turn; assistant markdown; tool call/result one-liners expanding to the content; `report` row; dim lines | met | `crates/transcript/src/rows.rs:440` (`user_row`), `:726` (`assistant_row`), `:892` (`tool_row`), `:1397` (`report_row`), `:1532` (`dim_row`), `:1539` (`run_outcome_row`); `transcript::tests.rs::a_tool_row_opens_on_click`, `::an_open_tool_row_renders_its_arguments_as_key_value_rows`, `::a_run_that_ended_badly_renders_as_its_own_notice_row` |
| Centre rows: an injected context, a goal nudge, a command answer and the swarm's words about a lane each get their own collapsed row | met | `crates/transcript/src/rows.rs:554` (`context_row`), `:584` (`command_note_row`), `:627` (`goal_nudge_row`), `:1493` (`lane_notice_row`); `transcript::tests.rs::a_goal_nudge_is_one_quiet_line`, `::an_opened_goal_nudge_shows_the_message_evo_sent`, `::a_command_note_is_one_quiet_line_that_opens`, `::a_lane_notice_is_one_quiet_line`, `::injected_context_is_one_quiet_line_that_opens_on_click`; §5's rows above |
| Centre: rows are selectable, and a message's markdown or a fence's code can be copied | met | `crates/transcript/src/rows.rs:193` (`copy_button`); `transcript::tests.rs::dragging_over_a_row_selects_it_and_command_c_copies_it`, `::a_message_copies_its_markdown_source`, `::a_code_block_copies_its_own_text`, `::an_opened_context_is_selectable`, `::a_tool_payload_is_selectable` |
| Centre: the markdown a model actually writes is rendered, with four decisions — fences highlighted, images as text, raw HTML as source, links only `http(s)`/`mailto` | met | `crates/transcript/src/markdown.rs` (`ImageFallback`, `RawHtml`, `RawHtmlBlock`), `crates/transcript/src/link.rs:22` (`openable`), `Cargo.toml:11` (the tree-sitter features); `transcript::tests.rs::code_fences_are_highlighted_and_an_unknown_language_stays_plain`, `::an_image_reference_is_drawn_as_its_alt_text_and_never_fetched`, `::raw_html_is_shown_as_the_source_it_is`, `::the_app_opens_web_and_mail_links_and_nothing_else`, `::a_link_the_app_does_not_open_is_ignored`, `::a_mailto_link_opens_a_mail_composer` |
| Centre bottom: the selected agent's todos, hidden when none; coordinator from `/state.todos` + `todo-changed`; lane from its own stream | met | `crates/workspace/src/tab_page.rs:347` (the panel under the transcript), `crates/transcript/src/todo.rs`; `transcript::tests.rs::the_todo_panel_is_hidden_while_the_agent_has_no_todos`, `::the_todo_panel_counts_its_items_caps_its_height_and_aligns_its_glyphs`, `::a_long_todo_list_scrolls_inside_the_panel`; `session::tests/tab.rs::the_todo_panel_follows_the_selected_agent`, `workspace::tests/tab_swarm.rs::the_coordinators_goal_and_todos_reach_the_tab_page` |
| Right ~360 px: `Textarea` auto-grow 2→8, Enter sends, Shift+Enter newline | met | `crates/workspace/src/tab_page.rs:27` (`COMPOSER_COLUMN_WIDTH`), `crates/composer/src/lib.rs:29-30` (`MIN_ROWS`/`MAX_ROWS`), `:484` (`action_button`); `composer::tests::shift_enter_inserts_a_newline_instead_of_sending` |
| Right: one status row — readout left, button right, one line high, ellipsized with the whole line in a tooltip, never wrapping the button | met | `crates/composer/src/lib.rs:456` (`readout_element`); `composer::tests::a_long_readout_is_ellipsized_and_keeps_the_button_on_its_line`; capture `docs/screens/08-narrow-1000x700-light.png` |
| Readout mirrors the TUI segment for segment, in order, dim | met | `crates/session/src/readout.rs:290` (`segments`), `crates/composer/src/lib.rs:466` (`muted_foreground`); `session::tests/readout.rs::segments_are_the_five_the_spec_names_in_order`, `::the_captured_session_reads_as_the_tui_would_draw_it`; `docs/proofs-real.md` §"The §7.3 readout line" and §4's reading of a real tab |
| Readout: **model** — id, or `id (provider)` when ambiguous; hidden when none | met | `crates/session/src/readout.rs:243` (`model_label`); `session::tests/readout.rs::a_model_id_under_two_providers_names_the_live_one`, `::a_missing_model_or_thinking_is_hidden` |
| Readout: **thinking** — effort lower-cased; hidden when null | met | `crates/session/src/readout.rs:295`; `session::tests/readout.rs::a_missing_model_or_thinking_is_hidden` |
| Readout: **context** — `ctx 48k/936k (5%)`, k-formatting, `min(100, round(100·used/window))`, `ctx 48k` without a window | met | `crates/session/src/readout.rs:256` (`context_label`); `session::tests/readout.rs::the_context_segment_shows_the_spec_line`, `::k_figures_round_half_to_even` |
| Readout: **cache** — `round(100·read/(input+read+write))`, hidden while read+write is zero | met | `crates/session/src/readout.rs:274` (`cache_label`), `:65` (`CacheTotals::cached_percent`); `session::tests/readout.rs::the_cache_segment_needs_cache_activity` |
| Readout: **goal** — `goal <id> (<status>) <tokens>[/<budget>]` | met | `crates/session/src/readout.rs:282` (`goal_label`); `session::tests/readout.rs::the_goal_segment_follows_the_tui_rule` |
| Readout re-anchors on each `message-end` (`context = input+output+read+write`, cache folded) | met | `crates/session/src/readout.rs:196` (`fold_message_end`); `session::tests/readout.rs::a_message_end_moves_the_line_with_the_run`, `::folding_the_capture_agrees_with_the_state_the_server_reports`; `workspace::tests/tab_swarm.rs::the_readout_moves_with_the_run_not_only_at_settled` |
| Cache seeded once from the newest `:custom` `cache-stats` journal entry, walking `20 → 100 → 400` | met | `crates/tab_engine/src/engine.rs:28` (`CACHE_LIMITS`), `:1326` (the walk); `session::tests/readout.rs::the_cache_seed_comes_from_the_newest_journal_entry`, `::a_seeded_cache_figure_shows_up_in_the_line` |
| Button: Send (primary) while idle, enabled with text → `/prompt` | met | `crates/composer/src/lib.rs:484`, `crates/workspace/src/tab.rs:1013`; `composer::tests::send_is_enabled_by_a_non_blank_draft_and_by_nothing_else`, `::enter_sends_the_draft_and_the_owner_clears_it_only_on_ok` |
| Button: Stop (secondary, square) while running/compacting → `/interrupt`, draft untouched, never sends | met | `crates/composer/src/lib.rs:286` (`face`), `:484`; `composer::tests::the_one_button_follows_the_activity_and_is_never_send_and_stop`, `::stop_interrupts_and_never_sends_or_clears`, `::escape_interrupts_and_leaves_the_draft_alone`; `workspace::tests/tab_swarm.rs::stop_interrupts_the_run_and_keeps_the_draft`, `::escape_interrupts_the_run_and_keeps_the_draft` |
| Enter queues while running; the button is disabled only while its own request is in flight; the draft sent is the one the keypress found | met | `crates/workspace/src/tab.rs:1013`, `crates/composer/src/lib.rs:286`; `composer::tests::enter_sends_while_running_so_text_can_be_queued`, `::the_button_is_disabled_only_while_its_own_request_is_in_flight`, `::enter_sends_the_draft_as_the_keypress_found_it` |
| Input: ↑/↓ walk the prompts this tab has sent; typing ends the walk | met | `crates/composer/src/lib.rs:64` (`HISTORY_LIMIT`), `:326` (`remember`), `:342` (`recall`); `composer::tests::the_up_arrow_walks_the_prompts_this_tab_sent`, `::a_prompt_typed_over_ends_the_walk`, `::sending_the_same_prompt_again_is_one_entry`, `::a_draft_belongs_to_its_tab` |
| ⌘C with nothing selected in the input copies the window's own selection | met | `crates/composer/src/lib.rs:342` (the copy arm of the same key handler); `composer::tests::the_copy_shortcut_takes_the_windows_selection_when_the_input_has_none`, `::the_inputs_own_selection_wins_the_copy` |
| Input and button always target the coordinator; selecting a lane changes only the centre | met | `crates/workspace/src/tab.rs:1013` (the composer's own engine); `session::tests/tab.rs::selecting_a_lane_switches_the_center_column` |

## §8 Act, don't reimplement

| requirement | status | evidence |
|---|---|---|
| `409`/`400`/`404`/`422` shown from the reply's `error`/`output`, never re-validated locally | met | `crates/workspace/src/tab.rs:95` (`notice_tone`), `:117` (`notice_words`, with the raw text kept for the hover), `crates/swarm_client/src/api.rs:632` (a 2xx-but-`ok:false` reply is typed by its own status and `output`); `tab_engine::tests/tab_e2e.rs::refusals_come_back_typed`; the only local checks are affordances (an empty draft's Send) |

## §9 Frontend behaviour

| # | requirement | status | evidence |
|---|---|---|---|
| 1 | Assembly `/health` cursor → `/transcript` → `/events?since=`; refetch on `settled`/`gap`/`hello`/reconnect; per-agent revision counters dropping late work | met | `crates/tab_engine/src/engine.rs:619` (`assemble`), `:1087` (`resync`), `crates/session/src/model.rs:486` (`settled`); `tab_engine::tests/tab_e2e.rs::boot_assembles_the_view`, `::prompt_streams_and_settles_with_a_resync`; `session::tests/tab.rs::a_stale_transcript_is_dropped`, `::the_two_assembly_paths_agree_through_the_tab` |
| 2 | Enter posts `/prompt` (idle starts a run, running queues); clear on `ok`; disable only in flight; the face matches the action | met | `crates/composer/src/lib.rs:286,484`, `crates/workspace/src/tab.rs:1013`; `composer::tests::enter_sends_the_draft_and_the_owner_clears_it_only_on_ok`, `::a_refused_send_keeps_the_draft_and_the_caret_at_its_end`, `::each_face_has_a_label_and_its_own_glyph` |
| 3 | Subscribe to `/lanes/N/events` only for the shown lane; every lane's `lane-state` from the coordinator; never fetch a lane's token or URL | met | `crates/tab_engine/src/engine.rs` (a single lane stream, switched on selection; lane reads go through the coordinator's `Client`), `crates/swarm_client/src/stream.rs:58`; `tab_engine::tests/tab_e2e.rs::watching_a_lane_switches_the_single_stream`, `::a_lane_shown_while_it_is_down_fills_its_rows_when_it_returns` |
| 4 | Cache the last `/registry`; on first run learn it with a throwaway `evo-agent serve` in `probe/`; refresh from every live server; never hardcode model names; the lanes chooser offers only kernel-registerable models | met | `crates/app/src/startup.rs:35` (24 h staleness), `crates/tab_engine/src/catalog.rs:49,85` (two probes, one `--no-userspace`), `crates/store/src/model_cache.rs:131` (`kernel_apis_known`), `crates/session/src/launcher.rs:246` (the lanes chooser) and `:410` (the reason); `tab_engine::tests/tab_e2e.rs::the_catalog_probe_learns_registry_and_kernel_apis`, `swarm_client::tests/swarm_e2e.rs::the_probe_learns_the_registry`, `session::tests/launcher.rs::the_lanes_chooser_measures_models_against_the_kernel_api_set`; a probe that fails leaves one warning line, not the raw error (§7.2); `docs/proofs-real.md` §2 |
| 5 | History scan on a background thread, `~500 files/5 s`, header + the **last** `:custom` `swarm` record (`:id`, `:workers`, lane cwd), merged with the app's recents, deduped by session, newest first; row = folder, lane count, when, models | met | `crates/store/src/history.rs:31` (`ScanBudget`), `:44-45` (the budgets), `:156` (`scan`), `:217` (`scan_in_background`), `:232` (`merge`), `crates/session/src/launcher.rs:565` (`history_rows`), `:711` (`meta_line`); `store::tests/real_data.rs::real_journals_parse_into_resumable_swarms`, `::the_swarm_record_fields_the_scan_depends_on`, `session::tests/launcher.rs::history_rows_merge_by_session_newest_first`; `docs/proofs-real.md` §3 |
| 6 | Lanes model → `<folder>/.evo/swarm.lisp`, managed block at the top, both settings, idempotent, creates `.evo/`, Default removes block and empty file, shared by the folder, the path surfaced by the chooser | met | `crates/store/src/swarm_config.rs:31,33,40,82` (markers, path, the write), `crates/session/src/launcher.rs:530` (the note next to the chooser); `store::swarm_config` tests `::default_removes_the_block_and_keeps_the_rest`, `::default_deletes_a_file_that_was_only_our_block`, `::a_repeated_write_from_scratch_is_byte_stable`, `::hand_edited_and_half_written_blocks_are_cleaned_up`, `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` |
| 7 | Failure surfaces: boot failure → log tail + Retry; a lane that cannot register its model says so in its row; lane down → `✗` + the reason from `output`; server gone → reconnect with `Last-Event-ID`, 0.5→10 s backoff, a reconnecting badge; a restarted coordinator keeps the tab working | met | `crates/workspace/src/tab_page.rs:96` (`render_failed`, Retry/Close), `crates/agent_list/src/lib.rs` (the down reason in the row), `crates/swarm_client/src/stream.rs:72,85` (the backoff); `tab_engine::tests/tab_e2e.rs::boot_failure_carries_the_log_tail`, `::a_dead_server_is_reported`, `::a_restarted_coordinator_resyncs`, `::a_silent_swarm_is_noticed_while_a_post_waits`, `session::tests/tab.rs::lane_down_reason_comes_from_the_lanes_own_output_rows`, `::a_lane_that_failed_to_start_says_so`, `::a_lane_that_cannot_register_its_model_says_so`, `workspace::tests/tab_swarm.rs::a_swarm_that_stops_answering_reconnects_and_refuses_quietly`; and a swarm that is gone can be resumed: `tab_swarm.rs::a_swarm_that_is_gone_shows_its_log_and_retry_resumes_the_session` |
| 8 | Quit stops every tab's swarm in parallel, persists `app.json`, exits; a crash leaves nothing running | met | `crates/app/src/quit.rs:41,48` (`begin`, `begin_with`), `:196` (`save_state`), `crates/workspace/src/launch.rs:154` (`stop_in_background`), `crates/swarm_client/src/server.rs:750` (`EVO_SERVE_WATCH_PID`); `evo_desktop::tests/quit.rs::quitting_starts_once_and_runs_to_the_end`, `::the_sequence_survives_being_started_without_a_window`, `swarm_client::tests/swarm_e2e.rs::two_swarms_shut_down_in_parallel`; `docs/proofs.md` "⌘Q"; the bundle run in `docs/proofs-real.md` §4 ends in the app's own quit with a process check |

## §10 `../evo-agent` is read-only

| requirement | status | evidence |
|---|---|---|
| Never modify evo; work through the API; report gaps with evidence instead of patching | met | `git -C ../evo-agent status --porcelain` is empty at this snapshot; the client-side workarounds are written up in `docs/api-gaps.md` (7 entries: lane cursor, lane state, `409` only from `/steer`, tail-silence, snake-cased keys, restarted-server visibility, abortable readiness) |

## §11 Stack and conventions

| requirement | status | evidence |
|---|---|---|
| Rust, `gpui-kit` pinned 0.7.x, `serde`/`serde_json`, `rfd`, HTTP/SSE stack of choice, lock-file single instance | met | `Cargo.toml:11` (`gpui-kit = "=0.7.0"`, with the tree-sitter grammars behind it), `crates/workspace/Cargo.toml` (`rfd`), `crates/swarm_client/src/http.rs` (blocking `std::net`, no tokio) |
| M0 proves the streaming path keeps the UI responsive with two tabs streaming | met | `workspace::tests/tab_swarm.rs::two_tabs_stream_at_once_and_the_ui_stays_responsive`; `docs/proofs-real.md` §4 is the same path in the real window against the installed binaries |
| Verify whether `cx.http_client()` exists; if not, use your own client and say so | met | verified and written down in `docs/architecture.md:34`: gpui-pre 0.3.7 exposes `App::http_client()`, but every non-test app built through `gpui_kit::application()` gets a `NullHttpClient` whose `send` fails, so all I/O uses `swarm_client`'s blocking client on std threads. The decision itself is in `crates/swarm_client/src/http.rs:1-6` |
| Coding guides: feature crates by capability, `Entity<T>`, `RenderOnce` for value-like pieces, stable domain ids, theme tokens, no feedback loops, weak entities in async work | met | the 11 crates in `docs/architecture.md`; `RenderOnce` in `crates/transcript/src/todo.rs`; named `ElementId`s throughout (`empty_tab::tests` address controls by id); no literal colours in the UI crates (`rg 'rgb\(|0x[0-9a-f]{6}' crates/*/src` finds none); weak entities in `crates/workspace/src/bridge.rs`, `crates/transcript/src/rows.rs`; revision guards in `session` |
| Tests: `cargo test` against a **real** `evo-swarm serve` from a temp `HOME` with a stub provider, two lanes, a scripted model; `TestAppContext` UI tests for tab strip, empty tab, transcript rows, todo panel | met | `crates/swarm_client/src/harness.rs` (temp `EVO_HOME`, stub provider, scripted lanes) used by `crates/proofs/tests/*`, `crates/tab_engine/tests/tab_e2e.rs`, `crates/workspace/tests/tab_swarm.rs`, `crates/app/tests/m2_relaunch.rs`/`m3_lane_ui.rs`; UI tests in `crates/workspace/tests/workspace_ui.rs`, `crates/workspace/src/empty_tab.rs` (tests), `crates/transcript/src/tests.rs`, `crates/composer/src/lib.rs` (tests) |
| `scripts/check.sh` green (fmt, clippy, every crate's tests) | partial | fmt is clean for every crate and `cargo clippy --workspace --all-targets` prints no lint of ours, both checked at this snapshot; the per-crate test run is the gate's and has not been made at `0ab1375` — gap 1 |

## §12 Milestones

| milestone | status | evidence |
|---|---|---|
| M0 — window with a custom title bar, a spawned swarm, `/health` + `/events` streaming, `POST /prompt` deltas | met | `crates/swarm_client/examples/m0_real.rs` (installed binaries, real `HOME`) and `m0_cli.rs`; `docs/proofs-real.md` §1 (timeline, `/health`, 37 events, the route set, shutdown + process check); §4 is the same run *through the app's own window*, and its four frames are committed in `docs/screens/real/` — the screenshot §12 asks for |
| M1 — one tab, real: layout, live markdown, tail following, Send/Stop, resync on `settled` | met | `crates/proofs/tests/m1_delegation.rs::m1_delegation`, `::m1_the_lane_list_follows_a_lane_to_idle`; `docs/proofs.md` §"m1_delegation — the coordinator delegates, the lane works, the report comes back"; `docs/proofs-real.md` §4 — a lane is delegated work and the coordinator's transcript renders it while the lane works (`real-02-lane-working.png`); `workspace::tests/tab_swarm.rs::a_tab_boots_a_real_swarm_and_streams_a_turn`; captures `docs/screens/03-lane-todos-*.png`, `04-tool-expanded-*.png` |
| M2 — tab strip with Add/Close/Select, tab persistence, empty tab with choosers + folder button + history, resume from history | met | `crates/proofs/tests/m2_resume.rs::m2_resume`; `evo_desktop::tests/m2_relaunch.rs::a_swarm_that_ran_once_is_offered_first_and_comes_back`; `docs/proofs.md` §"m2_resume — a swarm that ran once is found, offered first, and comes back"; `workspace::tests/workspace_ui.rs` (strip: add/close/select/overflow/middle-click); capture `02-three-tabs-*.png`. "Persistence" is §14.6's recorded set + history, not a restored strip — pinned by `m2_relaunch.rs` ("a launch opens one empty tab, whatever `app.json` said") |
| M3 — lane list, lane transcript, lane todos, `lane-state` live updates, lane failure states | met | `crates/proofs/tests/m3_lane_down_up.rs::m3_lane_down_up`; `evo_desktop::tests/m3_lane_ui.rs::a_killed_lane_goes_down_and_the_swarm_brings_it_back`; `tab_engine::tests/tab_e2e.rs::a_lane_shown_while_it_is_down_fills_its_rows_when_it_returns`, `::a_lane_that_came_up_is_read_back_idle`; `docs/proofs.md` §"m3_lane_down_up — a lane dies and the swarm brings it back" |
| M4 — single instance, coordinator crash/restart, log tails, `~/.evo/desktop` state, quit, tests green, release build + a bundle launchable from Finder | partial | `docs/proofs.md` §"M4 — bundle" (Finder launch, icon, menu bar, second launch, ⌘Q), §"M4 — appearance, window geometry, and the tab directories", §"M4 — the About dialog"; the bundle picture is committed at `docs/screens/real/bundle-launch.png`; `dist/evo-desktop.app` is on disk (ad-hoc signed, `CFBundleIdentifier=com.evo.desktop`, `LSMinimumSystemVersion=15.0`); `scripts/bundle.sh`; the coordinator-crash half is `m4_coordinator_restart` + `tab_e2e.rs::a_restarted_coordinator_resyncs`. Partial for gap 1 alone: "`cargo test` green" has not been run at this snapshot |

## §13 Non-goals (v1)

| non-goal | status (not present) | evidence |
|---|---|---|
| Rich-text composer, image paste, runtime model switching, command surface, lane control, remote/TLS servers, multiple windows, Windows/Linux packaging, embedded browser, journal editing, settings beyond binaries + theme | met | no `/command` caller (`crates/swarm_client/src/api.rs:732` only); no model selector on the tab page (`crates/workspace/src/tab_page.rs:377`); loopback-only client (`crates/swarm_client/src/http.rs:102`); one window (`crates/app/src/lib.rs:202`); macOS bundle only (`scripts/bundle.sh`); the Settings panel edits exactly the two paths and the theme (`crates/settings/src/panel.rs:56-61`, `:51` for its size) |

## §14 Settled decisions

| # | decision | status | evidence |
|---|---|---|---|
| 1 | Lanes model goes into the project's `swarm.lisp` as a managed block, no flag, no evo change | met | `crates/store/src/swarm_config.rs:82` (`set_lanes_model`), `:130` (`render_block`); `workspace::tests/tab_swarm.rs::a_lanes_model_is_written_to_the_folders_swarm_lisp_and_default_takes_it_away`; §9.6 row above |
| 2 | Workers chosen in the empty tab (1–64, Default = evo's own); passed for folder-created swarms only; a resumed swarm keeps its journal count | met | `crates/session/src/launcher.rs:130` (`LaunchPlan`), `:1074` (`plan`); `session::tests/launcher.rs::the_plan_passes_only_what_was_chosen`; the resume path passes no plan (`crates/workspace/src/tab.rs`, `crates/proofs/tests/m2_resume.rs`) |
| 3 | History lists every resumable swarm on disk, merged with the app's recents | met | `crates/store/src/history.rs:232` (`merge`), `crates/session/src/launcher.rs:565` (`history_rows`); `store::tests/real_data.rs::real_journals_parse_into_resumable_swarms` |
| 4 | Input and its button always target the coordinator; lanes are watched, never typed to | met | `crates/workspace/src/tab.rs:1013`; no lane POST endpoint exists in `crates/swarm_client/src/api.rs` |
| 5 | Closing a tab shuts its swarm down; the session stays on disk and returns to history | met | `crates/workspace/src/chrome.rs:571` (`close_tab`), `crates/workspace/src/launch.rs:154` (`stop_in_background`), `crates/store/src/history.rs` (a closed tab's journal is a normal row); `workspace_ui.rs::closing_a_tab_selects_its_neighbour` |
| 6 | The app never auto-starts swarms on launch: one empty tab | met | `crates/workspace/src/chrome.rs:429` (`open_empty_tab`); `workspace_ui.rs::the_app_opens_with_one_empty_tab`, `evo_desktop::tests/m2_relaunch.rs` ("…and only one tab: nothing restores a strip by itself") |
| 7 | No model selector in a tab; one action button, Send when idle / Stop while running, Enter always sends | met | `crates/workspace/src/tab_page.rs:377`; `crates/composer/src/lib.rs:286,484`; `composer::tests::the_one_button_follows_the_activity_and_is_never_send_and_stop` |

---

## Re-checking this audit

```sh
scripts/audit_refs.sh             # every path:line and test name in this document, against the tree
scripts/check.sh                  # the gate: fmt, clippy, every crate's tests
scripts/check.sh --head           # the same against HEAD
cargo test -p proofs              # M1–M4 against a real evo-swarm (slow)
cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens --scale 1
cargo run -p evo-desktop --example real_gui_run -- --capture docs/screens/real --scale 1
```

`scripts/audit_refs.sh` exits 1 and prints what no longer holds — a file that moved, a line past
the end of the file, a name the cited line no longer carries, a test that was renamed. Run it first
when refreshing this document: it answers the mechanical half in seconds, and the rows it leaves
alone are the ones a person still has to read. A row that cites a test means its name was found in
the source at the snapshot above; re-running the suite is what turns the row into proof.
