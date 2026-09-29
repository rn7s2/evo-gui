# Requirement audit — docs/PROMPT.md against the tree

Every requirement in `docs/PROMPT.md` §2–§14 (the §1 reading list aside), with the evidence that
exists **now**. Lane 6, 2026-09-29, ~15:20Z.

- **Snapshot.** HEAD `a8d36ea`; the working tree had 40-odd modified files from the other lanes at
  the time of writing, and the three Settings files are still untracked.
- **How it was checked.** Each row was verified by reading the code (`rg`/`read`) and, for every
  cited test, by locating its name in the source — cited tests were **not executed**: the long
  real-swarm suites (`crates/proofs`, `crates/workspace/tests/tab_swarm.rs`,
  `crates/tab_engine/tests/tab_e2e.rs`, `crates/swarm_client/tests/swarm_e2e.rs`,
  `crates/app/tests/m*`) are the crate owners' to run. The one exception is the two new
  `empty_tab::tests::focus_primary_*` tests, which were run (see §7.1).
- **Statuses.** *met* — implemented, with evidence present now. *partial* — some part of the
  requirement has no implementation or no evidence. *pending commit* — the evidence exists only in
  uncommitted files (paths given).
- **Line numbers** are from the working tree at audit time; lane edits move them.

## Gaps first

| # | gap | status | owner |
|---|---|---|---|
| 1 | `cargo fmt --check` is not clean (§11, M4). In the recorded HEAD run (fbbd021) 7 crates had diffs; the fmt-only re-check at audit time leaves `session` (`tests/model.rs:1010`) and `tab_engine` (`tests/tab_e2e.rs:1181`, `:1214`) red — one hunk and two | partial | `session`, `tab_engine` |
| 2 | clippy is not warning-free (§11): 41 warning lines in the recorded run (`unused variable`, redundant closures, useless conversions, `too many arguments`) | partial | per-crate lanes; gate = `scripts/check.sh` |
| 3 | M4's "cargo test green": the recorded HEAD run was green, and `workspace` is green again after lane 1's tab-shortcut commit (`cargo test -p workspace --lib`: 38 passed at audit time). The gate's last full run (`target/check/evo-desktop.test.log`, 22:55 local) still shows two `evo-desktop` lib failures in `quit.rs` from lane 2's in-flight edit — re-run `scripts/check.sh` once that lands | partial | `app` |
| 4 | The Settings panel (§13) exists and is tested, but only in uncommitted files: `crates/app/src/settings.rs`, `crates/app/tests/settings.rs`, `crates/app/examples/settings_snapshot.rs`, plus `crates/app/src/menus.rs` / `lib.rs` / `Cargo.toml` | pending commit | `app` |
| 5 | §2 rule 7: a tool argument nested deeper than one object/array level is rendered as compact JSON on one line rather than as rows (`crates/transcript/src/rows.rs:779`) — a deliberate, documented compromise, but it is the one place a JSON blob reaches the UI | partial | `transcript` |
| 6 | §9.7's "a lane that cannot register its model says so": the app surfaces the lane's own bring-up line (`session::TabModel::lane_down_reason`, tested), but no test pins the model-registration case specifically (only `failed to start`) | partial | `session`, `tab_engine` |
| 7 | §11's "verify whether this gpui snapshot exposes `cx.http_client()`; if not, say so": the repo hand-rolls HTTP deliberately (`crates/swarm_client/src/http.rs:1-6`) but no document records the verification result | partial | `docs` |
| 8 | M0's and M4's *pictures* live in `/tmp` (`docs/proofs.md:67,370,441,500`) and are gone; the committed pictures are the `docs/screens/` golden set. The milestone transcripts are committed | partial | `docs` |

Nothing in §2–§14 is **missing** outright.

One review finding against the real bundle is already fixed: the empty tab used to render the
catalog probe's raw error as its caption. It now reads as one warning line with the server's
own words in the tooltip — recorded in §7.2, with the captures it was fixed against.

---

## §2 Rules

| # | requirement | status | evidence |
|---|---|---|---|
| 1 | One instance: a second launch activates the running window and exits 0; no stale lock | met | `crates/store/src/single.rs:101` (`try_lock`), `crates/app/src/lib.rs:150` (secondary → return); `store::tests/single_instance.rs::a_second_process_activates_the_first_and_the_lock_dies_with_it`, `::a_killed_process_leaves_no_stale_lock`; `docs/proofs.md` "A second launch activates instead of duplicating" |
| 2 | Spawn the binaries as-is; everything shown comes from HTTP | met | `crates/swarm_client/src/server.rs:280` builds the argv, `:750` sets the env; no wrapper/patched binary anywhere in `scripts/` or the crates; the UI's data comes from `crates/tab_engine/src/engine.rs:483` (`assemble`) |
| 3 | Own only `~/.evo/desktop/`; journals and swarm dirs read-only | met | `crates/store/src/paths.rs:1-12` (the layout, one exception named), `crates/store/src/swarm_config.rs:43`; `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` (the real file's mtime is asserted unchanged) |
| 4 | Secrets stay in files: 0600 token, never logged/shown, never in `app.json` | met | `crates/store/src/paths.rs` `TOKEN_FILE`/`FILE_MODE`, `crates/swarm_client/src/http.rs:48` (`Token`'s `Debug`), `crates/swarm_client/src/redact.rs:98`; `swarm_client`'s `redact.rs::the_real_500_body_loses_its_token`, `server.rs::a_log_tail_is_redacted` |
| 5 | No second protocol: no polling where an event exists, no re-deriving state, no lane side channels | met | `crates/swarm_client/src/stream.rs` (SSE, `Last-Event-ID`), `crates/tab_engine/src/engine.rs:43` (the only timed read is the 500 ms lanes follow-up), `:484` (registry read once at assembly), `:831` (`/state` only on resync) |
| 6 | Never block the UI thread; results land via weak-entity updates with a revision check | met | `crates/tab_engine/src/engine.rs` runs on its own thread; `crates/app/src/startup.rs:74` (the scan on its own thread, `store::history::scan_in_background`); `crates/workspace/src/bridge.rs`; revision guards in `session::TabModel`; `workspace::tests/tab_swarm.rs::two_tabs_stream_at_once_and_the_ui_stays_responsive`, `session::tests/tab.rs::a_stale_transcript_is_dropped` |
| 7 | No raw sexprs and no raw JSON blobs in the UI | partial | journals are parsed in `store::sexp` and reduced to domain rows (`session::AgentModel`); tool arguments render as key/value rows (`transcript::tests.rs::tool_arguments_read_as_a_key_value_list`, `::a_long_value_is_elided_with_the_whole_text_kept_for_the_tooltip`) — the exception is gap 5 |
| 8 | Markdown rendered live while it streams, one retained document per message, faded appends | met | `crates/transcript/src/lib.rs:143` (`set_text(source so far)`), `:146` (`TextViewState::markdown`), `crates/transcript/src/rows.rs:85` (`TextViewMotion::with_stream_fade`), `:452`; `transcript::tests.rs::an_assistant_document_is_created_when_the_row_is_shown_and_then_retained`; capture `docs/screens/02-three-tabs-light.png` (heading, list, table, code fence mid-stream) |

## §3 Starting a tab's swarm

| requirement | status | evidence |
|---|---|---|
| cwd, argv (`serve --port --token-file [--workers] [--model] [--resume]`), `EVO_SERVE_WATCH_PID`, stdout+stderr → `swarm.log` | met | `crates/swarm_client/src/server.rs:280-302`, `:750`; `crates/store/src/paths.rs` `LOG_FILE`; `swarm_client::tests/swarm_e2e.rs::boot_reads_and_the_shutdown_ladder` |
| Pick the port by binding `127.0.0.1:0`; also read the port the server prints for `--port 0` | met | `crates/swarm_client/src/server.rs:699` (`free_port`), `:430` (`port_from_log`); `swarm_client::tests/swarm_e2e.rs::port_zero_reads_the_port_the_server_printed` |
| `--workers` only when asked; never `--allow-remote` | met | `crates/swarm_client/src/server.rs:287` (only `Some`), `:864` (asserts the flag is never passed) |
| Ready = token non-empty **and** `/health` 200 with `name`/`features`; 100 ms poll, 90 s deadline; early exit detected; last ~40 log lines on failure | met | `crates/swarm_client/src/server.rs:204-205` (90 s/100 ms), `:52-88` (`Readiness`), `:411`/`:473` (`log_tail(…, 40)`); `swarm_client::tests/swarm_e2e.rs::boot_failure_carries_the_log_tail`; `evo_desktop::tests/boot_failure.rs::a_swarm_binary_that_is_not_there_shows_the_reason_and_retries` |
| Shutdown ladder `POST /shutdown` ≤10 s → `SIGTERM` 5 s → `SIGKILL`, never kill first, tabs in parallel | met | `crates/swarm_client/src/server.rs:179-182`, `:208-209`; `swarm_client::tests/swarm_e2e.rs::two_swarms_shut_down_in_parallel`; `tab_engine::tests/tab_e2e.rs::shutdown_all_stops_every_tab`, `::shutdown_leaves_no_process` |
| A restarted coordinator is a new event log (ids from 1, `hello`): notice, refetch state, keep the view | met | `crates/swarm_client/src/stream.rs:99` (`ResetReason::Restarted`), `crates/session/src/model.rs:469` (`hello`/`session-switched` → `RESYNC`), `crates/tab_engine/src/engine.rs:1015`; `tab_engine::tests/tab_e2e.rs::a_restarted_coordinator_resyncs`, `session::tests/tab.rs::hello_after_a_restart_keeps_one_set_of_rows`, `session::tests/model.rs::event_ids_restarting_at_one_do_not_disturb_the_rows`, `crates/proofs/tests/m4_coordinator_restart.rs::m4_coordinator_restart` |

## §4 Endpoints and statuses

| requirement | status | evidence |
|---|---|---|
| Bearer on every request, `Connection: close`, loopback only | met | `crates/swarm_client/src/http.rs:177` (the request line), `:102` (host fixed to `127.0.0.1`) |
| POST replies are one envelope (`ok`/`status`/`error`/`output`/`data`/`choices`/`cursor`/`task`) | met | `crates/swarm_client/src/api.rs:548-566` |
| Coordinator endpoints: `/health`, `/state`, `/transcript`, `/registry`, `/events`, `/prompt`, `/interrupt`, `/shutdown` | met | `crates/swarm_client/src/api.rs:661-737`; callers in `crates/tab_engine/src/engine.rs:483` (`/registry`, `/lanes`, `/transcript`, `/state`, cache seed) and `crates/workspace/src/tab.rs:928` (`/prompt`, `/interrupt`) |
| `/command` present but **not used in v1** | met | `crates/swarm_client/src/api.rs:732` exists; no caller outside the client library (v1 ships no command surface, §13) |
| Swarm endpoints: `/lanes`, `/lanes/N/transcript`, `/lanes/N/events` | met | `crates/swarm_client/src/api.rs:686-705`; `crates/tab_engine/src/engine.rs:936` (lane transcript + todo seed), `stream.rs:429` (`?since=`) |
| Statuses 400/401/404/409/422/503 mapped: 409 dim, 422 the server's `error`, 503 shutting down | met | `crates/workspace/src/tab.rs:95` (`notice_tone`), `:117` (`notice_words`), `crates/tab_engine/src/types.rs:300-310` (every code typed); `tab_engine::tests/tab_e2e.rs::refusals_come_back_typed`, `workspace::tests/tab_swarm.rs::a_swarm_that_stops_answering_reconnects_and_refuses_quietly` |

Note: the envelope's `output` lines are folded into the error text for a 2xx-but-`ok:false` reply
(`crates/swarm_client/src/api.rs:654`); a *successful* reply's `output` is unused because nothing in
v1 posts a command that produces any.

## §5 Events

| event | status | evidence (all in `crates/session/src/model.rs` unless noted) |
|---|---|---|
| `run-start`, `turn-start` → activity, step clock | met | `:450`; `session::tests/model.rs::the_step_clock_follows_the_step_boundaries` |
| `message-start`, `text-delta` → the streaming markdown row | met | `:303`, `:307`; `session::tests/model.rs::message_start_and_deltas_build_one_streaming_row`, `::the_event_stream_and_the_transcript_agree` |
| `thinking-delta` → hidden by default, per-tab toggle shows it dim | met | `:315`; `transcript::tests.rs::thinking_is_hidden_until_the_view_reveals_it`; toggle `crates/workspace/src/tab_page.rs:281` (`Show thinking`/`Hide thinking`) |
| `tool-call-start` → a tool row | met | `:323`; `transcript::tests.rs::a_tool_row_opens_on_click` |
| `tool-result` → completes that row | met | `:327`; `session::tests/model.rs::rebuild_pairs_tool_calls_with_their_results`, `::an_orphan_tool_result_still_gets_a_row` |
| `message-end` → closes the row; `usage` feeds the cache/context segments | met | `:341`, `crates/session/src/readout.rs` `fold_message_end`; `session::tests/readout.rs::a_message_end_moves_the_line_with_the_run` |
| `run-end` → run state | met | `:440`; `session::tests/model.rs::a_run_that_ended_badly_gets_an_outcome_row`, `transcript::tests.rs::a_run_that_ended_badly_renders_as_its_own_notice_row` |
| `steering`, `user-input` → a user row | met | `:342` |
| `compaction-start`/`-end`, `provider-retry` → dim rows | met | `:377`, `:385`, `:395`; `session::tests/model.rs::provider_retry_is_a_dim_notice`, `::a_manual_compaction_drives_activity_and_dim_rows` |
| `todo-changed` → the todo panel of the agent that emitted it | met | `:421`; `session::tests/model.rs::a_todo_change_replaces_the_whole_list`, `session::tests/tab.rs::the_todo_panel_follows_the_selected_agent` |
| `task-start`/`task-end`, `settled` → activity; `settled` is a resync point | met | `:424`, `:432`, `:461`; `tab_engine::tests/tab_e2e.rs::prompt_streams_and_settles_with_a_resync` |
| `output` → dim/notice/error line in that agent's transcript | met | `:360`; `session::tests/model.rs::output_lines_take_the_style_the_server_sent` |
| `session-switched` → refetch everything | met | `:469` (same arm as `hello`); `session::tests/tab.rs::a_reset_refetches_every_agent_that_is_loaded` |
| `lane-state` → the left lane list (coordinator stream) | met | `:482`; `session::tests/lanes.rs::the_coordinator_stream_drives_the_whole_lane_list`, `::a_lane_state_event_updates_its_row` |
| `report` → a report row | met | `:350`; `session::tests/model.rs::a_report_event_becomes_a_report_row` |
| `hello`, `gap` → refetch (and a notice for `gap`) | met | `:469`, `:473`; `session::tests/model.rs::resync_events_and_lifecycle` |
| `ready`, `shutdown`, `bye` → lifecycle only | met | `:487` — the catch-all arm renders nothing (documented as lifecycle) |
| `unprintable-event` → ignore, log only | met | `:485`; `session::tests/tab.rs::a_tui_internal_event_is_not_a_server_event` |
| SSE rules: consecutive ids, `Last-Event-ID`/`?since=`, `:` keepalives, one stream per server, one per watched lane | met | `crates/swarm_client/src/sse.rs:48` (keepalives dropped), `stream.rs:221-255` (cursor + restart detection), `crates/tab_engine/src/engine.rs` (one coordinator stream, one lane stream); `tab_engine::tests/tab_e2e.rs::watching_a_lane_switches_the_single_stream`, `swarm_client::tests/swarm_e2e.rs::prompt_streams_events_and_the_stream_resumes` |

## §6 App data

| path | status | evidence |
|---|---|---|
| `app.json` (bounds, tab set, binaries, schema version) | met | `crates/store/src/app_state.rs:198` (`AppState`), `crates/app/src/bounds.rs:20`, `crates/app/src/quit.rs:150` (`remember_tab_set`); `evo_desktop::tests/quit.rs::quitting_starts_once_and_runs_to_the_end` |
| `lock` (flock, pid inside) | met | `crates/store/src/single.rs`; `store::tests/single_instance.rs::a_killed_process_leaves_no_stale_lock` |
| `model-cache.json` | met | `crates/store/src/model_cache.rs`; `empty_tab::tests::the_cached_catalog_fills_the_choosers_and_the_lanes_availability` |
| `probe/` (scratch cwd, catalog only) | met | `crates/store/src/paths.rs:162`, `crates/tab_engine/src/catalog.rs:9` (two throwaway probes) |
| `tabs/<uuid>/` `token` (0600), `swarm.log`, `tab.json` `{folder, session, swarm_id, models, workers}` | met | `crates/store/src/tab.rs:33-43` (`TabState`), `crates/store/src/paths.rs` (`TOKEN_FILE`, `LOG_FILE`, `TAB_FILE`); `tab_engine::tests/tab_e2e.rs::shutdown_leaves_no_process` writes and reads them |

## §7.1 Window and chrome

| requirement | status | evidence |
|---|---|---|
| 1600×1000 clamped to the work area, min ~1000×700, bounds restored from `app.json` | met | `crates/workspace/src/chrome.rs:36,40` (sizes), `crates/app/src/bounds.rs:20,45` (clamp); `bounds.rs::a_window_bigger_than_the_display_fills_it`, `::an_offscreen_window_is_pulled_back_inside`, `::a_window_smaller_than_the_minimum_is_grown_and_centered`, `chrome.rs::a_small_work_area_shrinks_the_window_inside_it`; `docs/proofs.md` "The window's geometry" |
| Custom title bar (`TitleBar::window_options`), title bar = the strip | met | `crates/workspace/src/chrome.rs:149` (`window_options`), `crates/app/src/lib.rs:201`; `docs/screens/01-launch-light.png` |
| One `Tab` per swarm: folder name, tooltip path + state, close button, macOS prefix left for the traffic lights | met | `crates/workspace/src/chrome.rs:55` (`TITLE_BAR_LEFT_INSET`), `render_tab`/`render_tab_close` below `render_tab_strip` (`:644`), `crates/workspace/src/tab.rs:449` (`title`), `:457` (`tooltip`); `workspace::tests/workspace_ui.rs::a_tab_shows_its_close_button_when_the_pointer_is_on_it`, `::clicking_a_tab_selects_it` |
| `+` always present at the end of the bar, appends and selects an empty tab | met | `crates/workspace/src/chrome.rs` `render_add_tab_button` (below `render_tab_strip` at `:644`); `workspace_ui.rs::the_add_button_appends_and_selects_a_new_tab` |
| Overflow scrolls, tab width capped | met | `crates/workspace/src/chrome.rs:644` (`render_tab_strip`, `max_width`/`max_w`), `:48` (`TAB_MAX_WIDTH`); `workspace_ui.rs::an_overflowing_strip_keeps_the_add_button_and_shows_the_selected_tab`; capture `docs/screens/07-tab-strip-light.png` |
| One window only | met | `crates/app/src/lib.rs:202` (one `open_window`), no New Window item in `crates/app/src/menus.rs:111` |
| Selecting an empty tab puts the keyboard on the first chooser (§7.1 polish, this lane) | met | `crates/workspace/src/empty_tab.rs:262` (`Choosers::focus_primary`), falling back to the folder card; the caller is lane 1's `crates/workspace/src/tab.rs:399` (`TabContent::focus_primary`), reached from `crates/workspace/src/chrome.rs:483` when a tab is selected — **committed**, so the seam is wired and the method needs no `allow(dead_code)`; `empty_tab::tests::focus_primary_lands_the_keyboard_on_the_first_chooser`, `::focus_primary_answers_false_when_the_window_refuses_the_keyboard` (both green; `cargo test -p workspace --lib` 40 passed at `6c9a0d0`) |

## §7.2 Empty tab

| requirement | status | evidence |
|---|---|---|
| Rows 1–3: coordinator model, lanes model, worker count; each defaults to **Default** | met | `crates/session/src/launcher.rs:31,275-307,506`; `crates/workspace/src/empty_tab.rs:612` (`render_choosers`); `empty_tab::tests::a_fresh_tab_is_default_everywhere_and_says_it_is_loading`, `workspace_ui.rs::the_empty_tab_defaults_every_chooser` |
| Coordinator model passed only when chosen (`--model`) | met | `crates/session/src/launcher.rs:1074` (`plan`); `empty_tab::tests::choosing_a_lanes_model_shows_the_file_it_is_written_to_and_lands_in_the_plan` |
| Lanes model written to the project, not passed | met | `crates/store/src/swarm_config.rs:16` (the block), `crates/session/src/launcher.rs:530` (the note); `workspace::tests/tab_swarm.rs::a_lanes_model_is_written_to_the_folders_swarm_lisp_and_default_takes_it_away`, `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` |
| Workers only when chosen (else evo's `:swarm-workers`, else 6) | met | `crates/session/src/launcher.rs:278-292`, `:315`; `session::tests/launcher.rs::the_workers_chooser_spans_one_to_sixty_four`, `::the_configured_swarm_workers_comes_from_the_registry_settings` |
| Models fixed when the tab is created; no selector on the tab page | met | `crates/workspace/src/tab_page.rs:337` (composer column only); `docs/usage.md` "the tab page has no model selector" |
| `Select folder…` spanning the three rows → native dialog (`rfd`), pick starts the swarm, cancel leaves the tab | met | `crates/workspace/src/empty_tab.rs:684` (`render_folder_button`), `:469` (`pick_folder`, `rfd`); `empty_tab::tests::a_chosen_folder_launches_with_the_plan`, `::a_cancelled_folder_pick_leaves_the_tab_where_it_was`; `crates/workspace/Cargo.toml` (`rfd`) |
| History: resumable swarms; a row opens a new tab with `--resume` and that cwd; the journal's lane count shown read-only | met | `crates/workspace/src/empty_tab.rs:872` (`render_history`), `:821` (`history_key`), which emits `TabContentEvent::Resume` at `:862`, `crates/session/src/launcher.rs:711` (the meta line with the lane count); `empty_tab::tests::a_history_row_resumes_its_session_in_its_folder`, `evo_desktop::tests/m2_relaunch.rs::a_swarm_that_ran_once_is_offered_first_and_comes_back` |
| The empty tab is a real tab; the app always keeps one | met | `crates/workspace/src/chrome.rs:509` (`close_tab`); `workspace_ui.rs::closing_the_last_tab_leaves_a_fresh_empty_tab`, `::closing_a_tab_selects_its_neighbour` |
| Botanic (beyond spec, this lane): option detail lines, greyed lanes models with the reason, the swarm.lisp caption, the `open at last quit` pill, empty/scanning/error states | met | `crates/workspace/src/empty_tab.rs:131-195` (detail + disabled), `:503` (caption), `:129` (`open at last quit`), `:1277` (the list's empty states); the row itself is `:1127` (`render_item`); `empty_tab::tests::the_cached_catalog_fills_the_choosers_and_the_lanes_availability`, `::only_a_row_the_app_had_open_wears_the_badge`, `::the_history_list_says_which_nothing_it_is`; captures `01-launch-light.png`, `01b-lanes-chooser-dark.png` |
| A catalog that could not be probed reads as **one** warning line, with the server's own error in its tooltip, and every chooser still usable on Default | met | found in review of the real bundle (`docs/screens/real/bundle-launch.png`), which showed the raw `http 500: The value "Bearer …"` as the caption, wrapped over two lines in the danger tone. Now: `crates/workspace/src/empty_tab.rs:74,76` (the two sentences — nothing loaded, or the last catalog still in use), `:84` (`warning_ink`: the light theme darkens the kit's amber to 4.9:1, the dark theme keeps `warning` at 12.9:1), `:503` (the line and its detail), `:641` (the tooltip); `empty_tab::tests::a_catalog_failure_reads_as_one_sentence_and_keeps_the_servers_words_for_the_hover`, `::a_failed_refresh_says_the_last_catalog_is_still_in_use`, `evo_desktop::tests/first_run.rs::a_catalog_that_cannot_be_probed_says_so_on_the_tab`; the error itself is unchanged in `app.log` and in `launch.catalog_error` (`crates/app/src/startup.rs:164`) |

## §7.3 Tab page

| requirement | status | evidence |
|---|---|---|
| Left ~260 px: `main` + one row per lane, status glyph `●◐○◌✗`, task truncated, step clock while working, driven by `/lanes` + `lane-state`, selection drives the centre | met | `crates/agent_list/src/lib.rs:34` (`COLUMN_WIDTH`), `:237` (header), `:558` (`status_color`), `crates/session/src/lib.rs:153` (glyphs); `agent_list::tests::rows_run_main_then_one_per_lane`, `::every_status_gets_its_glyph_and_its_word`, `::the_header_counts_the_lanes_and_the_busy_ones`, `::a_long_task_ellipsizes_and_never_pushes_the_clock_out`, `::clicking_a_row_asks_the_owner_to_select_that_agent` |
| Left: the keyboard walks the rows (arrows, Home/End) | met | `crates/agent_list/src/lib.rs:208`; `agent_list::tests::the_arrows_walk_the_rows_and_home_and_end_go_to_the_ends`, `::a_focused_list_is_the_only_thing_the_arrows_move` |
| Centre: the selected agent's transcript (coordinator `/transcript`, lane `/lanes/N/transcript`) | met | `crates/workspace/src/tab_page.rs:307` (`render_center_column`), `crates/tab_engine/src/engine.rs:936`; `tab_engine::tests/tab_e2e.rs::boot_assembles_the_view`, `workspace::tests/tab_swarm.rs::a_delegated_lane_shows_its_own_transcript` |
| Centre: assistant markdown rendered live, one document per message, `MessageScroller` tail-follow + jump affordance | met | `crates/transcript/src/lib.rs:143,146,521`; `transcript::tests.rs::an_assistant_document_is_created_when_the_row_is_shown_and_then_retained`, `::documents_stay_bounded_as_the_reader_scrolls_away`, `::a_resync_of_the_same_rows_changes_nothing` |
| Centre rows: user turn; assistant markdown; tool call/result one-liners expanding to the content; `report` row; dim lines | met | `crates/transcript/src/rows.rs` (`user_row` `:414`, `assistant_row` `:439`, `report_row`, `dim_row`, `run_outcome_row`); `transcript::tests.rs::a_tool_row_opens_on_click`, `::a_run_that_ended_badly_renders_as_its_own_notice_row`; the report row's own renderer is `crates/transcript/src/rows.rs:941`, its model arm `session::tests/model.rs::a_report_event_becomes_a_report_row` |
| Centre bottom: the selected agent's todos, hidden when none; coordinator from `/state.todos` + `todo-changed`; lane from its own stream | met | `crates/workspace/src/tab_page.rs:329`, `crates/transcript/src/todo.rs`; `transcript::tests.rs::the_todo_panel_is_hidden_while_the_agent_has_no_todos`, `::the_todo_panel_counts_its_items_caps_its_height_and_aligns_its_glyphs`, `::a_long_todo_list_scrolls_inside_the_panel`; `session::tests/tab.rs::the_todo_panel_follows_the_selected_agent` |
| Right ~360 px: `Textarea` auto-grow 2→8, Enter sends, Shift+Enter newline | met | `crates/workspace/src/tab_page.rs:27` (`COMPOSER_COLUMN_WIDTH`), `crates/composer/src/lib.rs:29-30` (2→8 rows), `:184,187`; `composer::tests::shift_enter_inserts_a_newline_instead_of_sending` |
| Right: one status row — readout left, button right, one line high, ellipsized with the whole line in a tooltip, never wrapping the button | met | `crates/composer/src/lib.rs:456` (`readout_element`), `:480` (`action_button`); `composer::tests::a_long_readout_is_ellipsized_and_keeps_the_button_on_its_line`; capture `docs/screens/08-narrow-1000x700-light.png` |
| Readout mirrors the TUI segment for segment, in order, dim | met | `crates/session/src/readout.rs:290` (`segments`), `:310` (`text`), `crates/composer/src/lib.rs:456` (the readout renders in `muted_foreground`); `session::tests/readout.rs::segments_are_the_five_the_spec_names_in_order`, `::the_captured_session_reads_as_the_tui_would_draw_it`; `docs/proofs-real.md` §"The §7.3 readout line" |
| Readout: **model** — id, or `id (provider)` when ambiguous; hidden when none | met | `crates/session/src/readout.rs:243`; `session::tests/readout.rs::a_model_id_under_two_providers_names_the_live_one`, `::a_missing_model_or_thinking_is_hidden` |
| Readout: **thinking** — effort lower-cased; hidden when null | met | `crates/session/src/readout.rs:295`; `session::tests/readout.rs::a_missing_model_or_thinking_is_hidden` |
| Readout: **context** — `ctx 48k/936k (5%)`, k-formatting, `min(100, round(100·used/window))`, `ctx 48k` without a window | met | `crates/session/src/readout.rs:256`; `session::tests/readout.rs::the_context_segment_shows_the_spec_line`, `::k_figures_round_half_to_even` |
| Readout: **cache** — `round(100·read/(input+read+write))`, hidden while read+write is zero | met | `crates/session/src/readout.rs:274`, `CacheTotals::cached_percent`; `session::tests/readout.rs::the_cache_segment_needs_cache_activity` |
| Readout: **goal** — `goal <id> (<status>) <tokens>[/<budget>]` | met | `crates/session/src/readout.rs:108`; `session::tests/readout.rs::the_goal_segment_follows_the_tui_rule` |
| Readout re-anchors on each `message-end` (`context = input+output+read+write`, cache folded) | met | `crates/session/src/readout.rs` `fold_message_end`; `session::tests/readout.rs::a_message_end_moves_the_line_with_the_run`, `::folding_the_capture_agrees_with_the_state_the_server_reports` |
| Cache seeded once from the newest `:custom` `cache-stats` journal entry, walking `20 → 100 → 400` | met | `crates/tab_engine/src/engine.rs:28` (`CACHE_LIMITS`), `:873` (the seed); `session::tests/readout.rs::the_cache_seed_comes_from_the_newest_journal_entry`, `::a_seeded_cache_figure_shows_up_in_the_line` |
| Button: Send (primary) while idle, enabled with text → `/prompt` | met | `crates/composer/src/lib.rs:480`, `crates/workspace/src/tab.rs:928`; `composer::tests::send_is_enabled_by_a_non_blank_draft_and_by_nothing_else`, `::enter_sends_the_draft_and_the_owner_clears_it_only_on_ok` |
| Button: Stop (secondary, square) while running/compacting → `/interrupt`, draft untouched, never sends | met | `crates/composer/src/lib.rs:84` (`■ Stop`), `:480`; `composer::tests::the_one_button_follows_the_activity_and_is_never_send_and_stop`, `::stop_interrupts_and_never_sends_or_clears`, `::escape_interrupts_and_leaves_the_draft_alone` |
| Enter queues while running; the button is disabled only while its own request is in flight | met | `crates/workspace/src/tab.rs:928` (`engine.prompt`), `crates/composer/src/lib.rs:275`; `composer::tests::enter_sends_while_running_so_text_can_be_queued`, `::the_button_is_disabled_only_while_its_own_request_is_in_flight` |
| Input and button always target the coordinator; selecting a lane changes only the centre | met | `crates/workspace/src/tab.rs:928` (the composer's own engine), `session::tests/tab.rs::selecting_a_lane_switches_the_center_column` |

## §8 Act, don't reimplement

| requirement | status | evidence |
|---|---|---|
| `409`/`400`/`404`/`422` shown from the reply's `error`/`output`, never re-validated locally | met | `crates/workspace/src/tab.rs:90-126` (tone + words, with the raw text kept for the hover), `crates/swarm_client/src/api.rs:632-655` (a 2xx-but-`ok:false` reply is typed by its own status and `output`); `tab_engine::tests/tab_e2e.rs::refusals_come_back_typed`; the only local checks are affordances (§7.2's empty-draft Send) |

## §9 Frontend behaviour

| # | requirement | status | evidence |
|---|---|---|---|
| 1 | Assembly `/health` cursor → `/transcript` → `/events?since=`; refetch on `settled`/`gap`/`hello`/reconnect; per-agent revision counters dropping late work | met | `crates/tab_engine/src/engine.rs:483` (`assemble`), `crates/session/src/model.rs:461-479`; `tab_engine::tests/tab_e2e.rs::boot_assembles_the_view`, `::prompt_streams_and_settles_with_a_resync`; `session::tests/tab.rs::a_stale_transcript_is_dropped`, `::the_two_assembly_paths_agree_through_the_tab` |
| 2 | Enter posts `/prompt` (idle starts a run, running queues); clear on `ok`; disable only in flight; the face matches the action | met | `crates/composer/src/lib.rs:275-303,371-396`, `crates/workspace/src/tab.rs:928`; `composer::tests::enter_sends_the_draft_and_the_owner_clears_it_only_on_ok`, `::a_refused_send_keeps_the_draft_and_the_caret_at_its_end`, `::each_face_has_a_label_and_its_own_glyph` |
| 3 | Subscribe to `/lanes/N/events` only for the shown lane; every lane's `lane-state` from the coordinator; never fetch a lane's token or URL | met | `crates/tab_engine/src/engine.rs` (single lane stream, switched on selection; lane reads go through the coordinator's `Client`); `tab_engine::tests/tab_e2e.rs::watching_a_lane_switches_the_single_stream`, `::a_lane_shown_while_it_is_down_fills_its_rows_when_it_returns` |
| 4 | Cache the last `/registry`; on first run learn it with a throwaway `evo-agent serve` in `probe/`; refresh from every live server; never hardcode model names; the lanes chooser offers only kernel-registerable models | met | `crates/app/src/startup.rs:35,51` (24 h staleness), `crates/tab_engine/src/catalog.rs` (two probes, one `--no-userspace`), `crates/store/src/model_cache.rs:131` (`kernel_apis_known`), `crates/session/src/launcher.rs:246` (the lanes chooser) and `:410` (the reason); `tab_engine::tests/tab_e2e.rs::the_catalog_probe_learns_registry_and_kernel_apis`, `swarm_client::tests/swarm_e2e.rs::the_probe_learns_the_registry`, `session::tests/launcher.rs::the_lanes_chooser_measures_models_against_the_kernel_api_set`; a probe that fails leaves one warning line, not the raw error (§7.2); `docs/proofs-real.md` §2 |
| 5 | History scan on a background thread, `~500 files/5 s`, header + the **last** `:custom` `swarm` record (`:id`, `:workers`, lane cwd), merged with the app's recents, deduped by session, newest first; row = folder, lane count, when, models | met | `crates/store/src/history.rs:31,44-45` (budget), `:156` (`scan`), `:368` (last record wins), `crates/session/src/launcher.rs:565` (merge/order), `:711` (meta line); `store::tests/real_data.rs::real_journals_parse_into_resumable_swarms`, `::the_swarm_record_fields_the_scan_depends_on`, `session::tests/launcher.rs::history_rows_merge_by_session_newest_first`; `docs/proofs-real.md` §3 |
| 6 | Lanes model → `<folder>/.evo/swarm.lisp`, managed block at the top, both settings, idempotent, creates `.evo/`, Default removes block and empty file, shared by the folder, the path surfaced by the chooser | met | `crates/store/src/swarm_config.rs:16,33,43` (block, markers, path), `crates/session/src/launcher.rs:530` (the note next to the chooser); `store::swarm_config` tests `::default_removes_the_block_and_keeps_the_rest`, `::default_deletes_a_file_that_was_only_our_block`, `::a_repeated_write_from_scratch_is_byte_stable`, `::hand_edited_and_half_written_blocks_are_cleaned_up`, `store::tests/real_data.rs::the_real_swarm_lisp_is_only_ever_touched_by_copy` |
| 7 | Failure surfaces: boot failure → log tail + Retry; a lane that cannot register its model says so in its row; lane down → `✗` + the reason from `output`; server gone → reconnect with `Last-Event-ID`, 0.5→10 s backoff, a reconnecting badge; a restarted coordinator keeps the tab working | partial | `crates/workspace/src/tab_page.rs:83` (failed screen with Retry/Close), `crates/agent_list` down reason in the row, `crates/swarm_client/src/stream.rs:72-85` (backoff); `tab_engine::tests/tab_e2e.rs::boot_failure_carries_the_log_tail`, `::a_dead_server_is_reported`, `::a_restarted_coordinator_resyncs`, `session::tests/tab.rs::lane_down_reason_comes_from_the_lanes_own_output_rows`, `::a_lane_that_failed_to_start_says_so`, `workspace::tests/tab_swarm.rs::a_swarm_that_stops_answering_reconnects_and_refuses_quietly`; the model-registration wording itself is gap 6 |
| 8 | Quit stops every tab's swarm in parallel, persists `app.json`, exits; a crash leaves nothing running | met | `crates/app/src/quit.rs:38,52,60` (deadline, once-only), `crates/swarm_client/src/server.rs:750` (`EVO_SERVE_WATCH_PID`); `evo_desktop::tests/quit.rs::quitting_starts_once_and_runs_to_the_end`, `::the_sequence_survives_being_started_without_a_window`, `swarm_client::tests/swarm_e2e.rs::two_swarms_shut_down_in_parallel`; `docs/proofs.md` "⌘Q" |

## §10 `../evo-agent` is read-only

| requirement | status | evidence |
|---|---|---|
| Never modify evo; work through the API; report gaps with evidence instead of patching | met | `git -C ../evo-agent status --porcelain` is empty at audit time; the client-side workarounds are written up in `docs/api-gaps.md` (7 entries: lane cursor, lane state, `409` only from `/steer`, tail-silence, snake-cased keys, restarted-server visibility, abortable readiness) |

## §11 Stack and conventions

| requirement | status | evidence |
|---|---|---|
| Rust, `gpui-kit` pinned 0.7.x, `serde`/`serde_json`, `rfd`, HTTP/SSE stack of choice, lock-file single instance | met | `Cargo.toml:11` (`gpui-kit = "=0.7.0"`), `crates/workspace/Cargo.toml` (`rfd`), `crates/swarm_client/src/http.rs` (blocking `std::net`, no tokio) |
| M0 proves the streaming path keeps the UI responsive with two tabs streaming | met | `workspace::tests/tab_swarm.rs::two_tabs_stream_at_once_and_the_ui_stays_responsive` |
| Verify whether `cx.http_client()` exists; if not, use your own client and say so | partial | the decision is made and documented in `crates/swarm_client/src/http.rs:1-6`; the *verification* is nowhere recorded (gap 7) |
| Coding guides: feature crates by capability, `Entity<T>`, `RenderOnce` for value-like pieces, stable domain ids, theme tokens, no feedback loops, weak entities in async work | met | the 11 crates in `docs/architecture.md`; `RenderOnce` in `crates/transcript/src/todo.rs`; named `ElementId`s throughout (`empty_tab::tests` address controls by id); no literal colours in the UI crates (`rg 'rgb\(|0x[0-9a-f]{6}'` finds none); weak entities in `crates/workspace/src/bridge.rs`, `crates/transcript/src/rows.rs`; revision guards in `session` |
| Tests: `cargo test` against a **real** `evo-swarm serve` from a temp `HOME` with a stub provider, two lanes, a scripted model; `TestAppContext` UI tests for tab strip, empty tab, transcript rows, todo panel | met | `crates/swarm_client/src/harness.rs` (temp `EVO_HOME`, stub provider, scripted lanes) used by `crates/proofs/tests/*`, `crates/tab_engine/tests/tab_e2e.rs`, `crates/workspace/tests/tab_swarm.rs`, `crates/app/tests/m2_relaunch.rs`/`m3_lane_ui.rs`; UI tests in `crates/workspace/tests/workspace_ui.rs`, `crates/workspace/src/empty_tab.rs` (tests), `crates/transcript/src/tests.rs`, `crates/composer/src/lib.rs` (tests) |
| `scripts/check.sh` green (fmt, clippy, every crate's tests) | partial | the recorded run (fbbd021, 2026-09-29 22:41–22:55 local, logs in `target/check/`) had all test binaries green at HEAD but fmt red for 7 crates and 41 clippy warning lines; the working tree was red in `evo-desktop` (2 lib tests) — gaps 1–3 |

## §12 Milestones

| milestone | status | evidence |
|---|---|---|
| M0 — window with a custom title bar, a spawned swarm, `/health` + `/events` streaming, `POST /prompt` deltas | met | `crates/swarm_client/examples/m0_real.rs` (installed binaries, real `HOME`) and `m0_cli.rs`; `docs/proofs-real.md` §1 (timeline, `/health`, 37 events, the route set, shutdown + process check); the window part is the app itself — `docs/screens/01-launch-light.png`, `02-three-tabs-light.png` (capture recipe in `docs/screens.md`); the M0-era picture is gap 8 |
| M1 — one tab, real: layout, live markdown, tail following, Send/Stop, resync on `settled` | met | `crates/proofs/tests/m1_delegation.rs::m1_delegation`, `::m1_the_lane_list_follows_a_lane_to_idle`; `docs/proofs.md` §"m1_delegation"; captures `docs/screens/03-lane-todos-*.png`, `04-tool-expanded-*.png`; `workspace::tests/tab_swarm.rs::a_tab_boots_a_real_swarm_and_streams_a_turn` |
| M2 — tab strip with Add/Close/Select, tab persistence, empty tab with choosers + folder button + history, resume from history | met | `crates/proofs/tests/m2_resume.rs::m2_resume`; `evo_desktop::tests/m2_relaunch.rs::a_swarm_that_ran_once_is_offered_first_and_comes_back`; `docs/proofs.md` §"m2_resume"; `workspace::tests/workspace_ui.rs` (strip: add/close/select/overflow/middle-click); capture `02-three-tabs-*.png`. "Persistence" is §14.6's recorded set + history, not a restored strip — pinned by `m2_relaunch.rs` ("a launch opens one empty tab, whatever `app.json` said") |
| M3 — lane list, lane transcript, lane todos, `lane-state` live updates, lane failure states | met | `crates/proofs/tests/m3_lane_down_up.rs::m3_lane_down_up`; `evo_desktop::tests/m3_lane_ui.rs::a_killed_lane_goes_down_and_the_swarm_brings_it_back`; `tab_engine::tests/tab_e2e.rs::a_lane_shown_while_it_is_down_fills_its_rows_when_it_returns`; `docs/proofs.md` §"m3_lane_down_up" |
| M4 — single instance, coordinator crash/restart, log tails, `~/.evo/desktop` state, quit, tests green, release build + a bundle launchable from Finder | partial | `docs/proofs.md` §"M4 — bundle" (Finder launch, icon, menu bar, second launch, ⌘Q), §"M4 — appearance, window geometry, and the tab directories", §"M4 — the About dialog"; `dist/evo-desktop.app` is on disk now (ad-hoc signed, `CFBundleIdentifier=com.evo.desktop`, `LSMinimumSystemVersion=15.0`, a 650 KiB `AppIcon.icns`); `scripts/bundle.sh`; the coordinator-crash half is `m4_coordinator_restart` + `tab_e2e.rs::a_restarted_coordinator_resyncs`. Partial because of gaps 1–3 (fmt/clippy, and the working tree's red `evo-desktop` tests) and gap 8 (the bundle pictures are `/tmp`) |

## §13 Non-goals (v1)

| non-goal | status (not present) | evidence |
|---|---|---|
| Rich-text composer, image paste, runtime model switching, command surface, lane control, remote/TLS servers, multiple windows, Windows/Linux packaging, embedded browser, journal editing, settings beyond binaries + theme | met | no `/command` caller (`api.rs:732` only); no model selector on the tab page (`tab_page.rs:337`); loopback-only client (`http.rs:102`); one window (`crates/app/src/lib.rs:202`); macOS bundle only (`scripts/bundle.sh`); the Settings panel edits exactly the two paths and the theme (`crates/settings/src/panel.rs:90-98`) |

## §14 Settled decisions

| # | decision | status | evidence |
|---|---|---|---|
| 1 | Lanes model goes into the project's `swarm.lisp` as a managed block, no flag, no evo change | met | `crates/store/src/swarm_config.rs`; `workspace::tests/tab_swarm.rs::a_lanes_model_is_written_to_the_folders_swarm_lisp_and_default_takes_it_away`; §9.6 row above |
| 2 | Workers chosen in the empty tab (1–64, Default = evo's own); passed for folder-created swarms only; a resumed swarm keeps its journal count | met | `crates/session/src/launcher.rs:129` (`LaunchPlan`), `:1074`; `session::tests/launcher.rs::the_plan_passes_only_what_was_chosen`; the resume path passes no plan (`crates/workspace/src/tab.rs`, `crates/proofs/tests/m2_resume.rs`) |
| 3 | History lists every resumable swarm on disk, merged with the app's recents | met | `crates/store/src/history.rs:213`, `crates/session/src/launcher.rs:565`; `store::tests/real_data.rs::real_journals_parse_into_resumable_swarms` |
| 4 | Input and its button always target the coordinator; lanes are watched, never typed to | met | `crates/workspace/src/tab.rs:928`; no lane POST endpoint exists in `crates/swarm_client/src/api.rs` |
| 5 | Closing a tab shuts its swarm down; the session stays on disk and returns to history | met | `crates/workspace/src/chrome.rs:509` (`stop_in_background`), `crates/store/src/history.rs` (a closed tab's journal is a normal row); `workspace_ui.rs::closing_a_tab_selects_its_neighbour` |
| 6 | The app never auto-starts swarms on launch: one empty tab | met | `crates/workspace/src/chrome.rs:288` (`open_empty_tab` in `with_config`); `workspace_ui.rs::the_app_opens_with_one_empty_tab`, `evo_desktop::tests/m2_relaunch.rs` ("…and only one tab: nothing restores a strip by itself") |
| 7 | No model selector in a tab; one action button, Send when idle / Stop while running, Enter always sends | met | `crates/workspace/src/tab_page.rs:337`; `crates/composer/src/lib.rs:84,275-303`; `composer::tests::the_one_button_follows_the_activity_and_is_never_send_and_stop` |

---

## Re-checking this audit

```sh
scripts/check.sh                  # the gate: fmt, clippy, every crate's tests
scripts/check.sh --head           # the same against HEAD
cargo test -p proofs              # M1–M4 against a real evo-swarm (slow)
cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens --scale 1
```

A row that cites a test means *its name was found in the source at the snapshot above*; re-running
the suite is what turns the row into proof.
