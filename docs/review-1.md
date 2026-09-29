# Cross-crate review 1 — lane 4

**Reviewed at** `HEAD = 084e6d6` (`feat(tab_engine): abortable boot, catalog probe and parallel
shutdown`), the commit the coordinator named for this pass.

**Crates in scope** `swarm_client`, `tab_engine`, `transcript`, `composer`, `agent_list`,
`workspace`, `app` — plus the requirement's own reach into what a crate only *uses* (the
`session` view models, `store`).

**Against** `docs/PROMPT.md`: §2 rules 1–8, §3, §4, §5, §7.3, §9.1–§9.8.

**Method** read each crate's sources and its tests; greped for the failure shapes the rules are
about (token in a message, blocking call on a UI path, `sleep`/poll loops, raw JSON in a render
path, lane tokens/URLs, hardcoded model names); ran the checks named at the end of this file. Line
numbers are **at HEAD**, not the working tree.

**Severities** `bug` — wrong today, will be seen by a user. `spec gap` — a rule of the spec is not
implemented (some of these are openly WIP: the code's own comments say so, and where that is the
case it is quoted). `risk` — implemented in a way that will misbehave when the pieces are wired
together. `nit` — small, stylistic or unreachable.

## Findings at a glance

| id | severity | where | what | owner |
|---|---|---|---|---|
| F1 | spec gap | `workspace` | No tab ever starts or stops a swarm: the engine is never created, so §3, §9.1, §9.2, §9.7, §9.8 and §4 have no UI path | workspace |
| F2 | spec gap | `workspace` | The empty tab and the tab page render placeholders: choosers with only `Default`, a bare `swarm.lisp` path, a fake history list, a hardcoded lane column — §7.2, §9.4, §9.5, §9.6 | workspace |
| F3 | spec gap | `app` | No single instance, no activation, no quit-time shutdown, no persisted window bounds — §2.1, §9.8, §7.1 | app (+ store is ready) |
| F4 | risk | `tab_engine`, `workspace` | The coordinator model's **provider** is dropped: `--model` takes an id, and an id under two providers resolves first-wins, so §7.3's ambiguity case cannot be honoured | workspace / tab_engine |
| F5 | risk | `transcript` + `tab_engine` | One `TranscriptView` revision counter against per-agent engine revisions: switching to a lane can make the new agent's transcript a no-op | workspace (+ transcript) |
| F6 | nit | `tab_engine` | `Command::Steer` is routed but the UI may not send it; documented, keep as the 409 probe | tab_engine |
| F7 | nit | `session` | §5's `turn-start` step clock is not modelled for the coordinator (activity only) — no UI shows one | session |

## F1 — no tab ever starts a swarm (spec gap, `workspace`)

**Where.** `crates/workspace/Cargo.toml:6-12` (dependencies: `gpui-kit`, `anyhow`,
`async-channel`, `rfd`, `transcript`, `composer` — no `tab_engine`, no `swarm_client`, no
`store`); `crates/workspace/src/tab.rs:188-196`; `crates/workspace/src/chrome.rs:145-148`;
`crates/workspace/src/tab.rs:43-52` (`TabState`).

**Evidence.** The crate's own comments say it:
`tab.rs:190` — "Spawning the swarm comes with the client wiring; until then the tab shows the boot
placeholder and stops there"; `chrome.rs:145` — "Close `id`, which shuts its swarm down once the
client wiring lands (§3, §14.5)"; `chrome.rs:188` — "Resuming the recorded session comes with the
client wiring; opening the row's folder is what the tab means today (§9.5)".
`TabState::Running`/`Failed` are reachable only from a demo and a test: `mark_running`
(`tab.rs:198`) and `fail` (`tab.rs:209`) are called by
`crates/workspace/examples/workspace_snapshot.rs:125,132` and
`crates/workspace/tests/workspace_ui.rs:190` — nothing in a production path can reach them,
because nothing consumes an `Update`.

**Why it matters.** §3's lifecycle (spawn argv, readiness, the shutdown ladder, `swarm.log` on a
failed boot), §9.1's assembly (transcript → stream), §9.2's send/interrupt path, §9.7's
"reconnecting" badge and lane-down surface, §9.8's quit-all and §4's status handling are all
implemented in `swarm_client`/`tab_engine`/`composer` and reachable from nothing. A user today gets
a window, a tab strip and placeholders: no swarm is ever started.

**Suggested fix.** `workspace` takes `tab_engine` (and `session`); `begin_boot` calls
`TabEngine::start(TabSpec…)`, keeps the `EngineHandle` on the `TabContent`, and drives
`Update`s through `Bridge`/`cx.spawn` into `session::TabModel` + `transcript` + `composer` +
`agent_list`. `TabState` then follows `Update::{Booting,Ready,BootFailed,Exited,ServerGone}`.
`close_tab`/drop shuts down through `EngineHandle`; app quit calls
`tab_engine::shutdown_all` (§9.8). Note the work is in flight in the working tree
(`crates/workspace/src/launch.rs`, untracked at review time) — this finding is about HEAD.

## F2 — the empty tab and tab page are placeholders (spec gap, `workspace`)

**Where.**
- `crates/workspace/src/empty_tab.rs:205-207` — `model_chooser` offers **only** `Default`, with the
  comment "Model ids are never hardcoded (§9.4): the real catalog arrives with the `/registry`
  cache… Until that lands, the honest list is evo's own default." §7.2 wants the coordinator and
  lanes choosers, and §9.4's availability rule for the lanes chooser.
- `crates/workspace/src/empty_tab.rs:141-153` — the `swarm.lisp` note renders the bare path
  `"<folder>/.evo/swarm.lisp"`; §9.6 wants the note naming the file for the chosen folder (the
  wording and the `~` shortening now exist in `session::launcher::lanes_model_note`).
- `crates/workspace/src/history.rs:54-80` — `placeholder_history()` returns three invented rows with
  **hardcoded model names** (`ark-deepseek-v4.1-flash` at :60, `claude-sonnet-4` at :72) and fake
  folders (`/Users/bytedance/coding/evo-desktop`), and `empty_tab.rs:155` says "The background scan
  that fills this list is not wired yet". §9.4 ("Never hardcode model names") and §9.5 are both
  touched; a shipped build shows these names to the user.
- `crates/workspace/src/tab_page.rs:25-31` + `:111-127` — the agent column is `PLACEHOLDER_AGENTS`
  (`main`, `Lane 1`, `Lane 2`, `Lane 3`) with a hardcoded glyph column, though
  `crates/agent_list/src/lib.rs:99-160` already renders a real `LaneList` with
  `set_lanes/set_coordinator/set_selected/set_down_reason` and `agent_list` is not a dependency of
  `workspace`.

**Suggested fix.** Wire, in this order: the catalog (`tab_engine::catalog::learn` →
`session::Launcher::set_probe_registry`), the history scan (`store::history` →
`Launcher::set_history`), `agent_list` in `render_agent_column`, and the composer's readout from
`TabModel::readout_text()`. Delete `PLACEHOLDER_AGENTS` and `placeholder_history()` when they go —
`placeholder_history` exists only for the empty tab and would otherwise keep model names in the
binary.

## F3 — single instance, quit shutdown, window bounds (spec gap, `app`)

**Where.** `crates/app/src/main.rs:10-21` — the whole `run` closure opens one window; nothing else.
`crates/app/Cargo.toml:10-13` — dependencies are `gpui-kit`, `anyhow`, `workspace`: `store` is not
there, so `store::single::SingleInstance::acquire_default` (`crates/store/src/single.rs:98,111`)
and `store::app_state` cannot be called from the app.

**Evidence.** §2.1 "A second launch must not open a second window: it activates the running
instance's window and exits 0". Nothing in `app` or `workspace` takes a lock or calls an
activation: `rg -n 'SingleInstance|acquire_default|activate' crates/app crates/workspace` — no hits.
§9.8 "Shut every tab's swarm down … persist `app.json`, exit" — no `on_app_quit`/`on_window_closed`
handler exists: `rg -n 'on_app_quit' crates` — no hits (only `QuitMode::LastWindowClosed` in
`main.rs:12`). §7.1 "bounds restored from `app.json`" — `window_options` always uses
`initial_window_bounds` (`chrome.rs:38-66`), no restore.

**Suggested fix.** `app` depends on `store`; take the lock before opening the window, and on a
`Secondary` activate and exit 0 (the store's `Activation`/`Secondary` types are exactly for this);
restore/save bounds through `store::app_state`; register a quit handler that calls
`tab_engine::shutdown_all(handles, deadline)` and persists state before exiting.

## F4 — the coordinator's model provider is dropped (risk, `tab_engine` + `workspace`)

**Where.** `crates/tab_engine/src/types.rs:47-50` —
"The coordinator's model (`--model`). The swarm takes the model **id** alone — the provider comes
from how the model was registered — so there is no provider flag to carry."

**Evidence.** `evo-swarm --model <id>` (`../evo-agent/swarm/main.lisp:33`, `:69-70` — "`--model`
needs an id"), and a bare id resolves **first-wins**:
`../evo-agent/src/provider/registry.lisp:109-113` — "A bare id resolves to the FIRST registration
of that id: the same id under several providers is legal, so the default has to be a rule, and
registration order is the one the user wrote." `docs/PROMPT.md` §7.3 makes the ambiguous case
first-class (the readout prints `id (provider)`) and §9.6 spells out the consequence for the lanes
model ("a model id alone fails when that id is registered under another provider (`find-model`
refuses a model/provider mismatch)"). `session::launcher::LaunchPlan.model` is an
`Option<(id, provider)>` — the provider is known at the chooser and is discarded on the way to the
argv (`TabSpec::server_config` sets `config.model` and leaves `evo_bin`/nothing for the provider).

**Why it matters.** With the two-provider registry the GUI itself captured
(`crates/session/tests/fixtures/registry-two-providers.json`), a user who picks `stub-a (stub2)`
gets whichever entry was registered first, silently. This is the one §7.3 case the app cannot
honour, and nothing tells the user.

**Suggested fix.** One of: (a) `Launcher`'s coordinator chooser marks an ambiguous id
`available: false` with the reason "registered under several providers — pass it in
`swarm.lisp`", so the claim matches the argv; or (b) carry the provider through `TabSpec` and set
it in the coordinator's own image (there is no flag, so this means `POST /eval` /
`POST /command /model` — §13 keeps a command surface out of v1, so this is a bigger decision); or
(c) at minimum document the first-wins resolution in `TabSpec`'s doc and in the UI's tooltip.
Nothing here needs evo to change.

## F5 — one view revision vs per-agent engine revisions (risk, `transcript` + `tab_engine`)

**Where.** `crates/transcript/src/lib.rs:314-320` (`accept_revision`: `revision < self.revision` →
drop), `:208-227` (`replace`/`upsert` take the revision), `:282-291` (`clear` keeps the revision:
"The revision is kept: a rebuild decides it"); `crates/tab_engine/src/engine.rs:227-241`
(`Revisions` — "one counter per agent") and `:526-545` (`resync` stamps the update with the
agent's own counter).

**Why it matters.** The engine's revisions are **per agent** (coordinator 1, 2, 3…; lane 1 its own
1, 2, 3…). A tab page that shows one `TranscriptView` and feeds it both agents will drop the lane's
transcript whenever the lane's counter is behind the coordinator's — the ordinary case, since the
coordinator's transcript was fetched first. The center column then keeps rendering the coordinator's
rows after a lane is selected, and `clear()` does not help because it deliberately keeps the
revision. §7.3 ("Selection highlights and drives the center column") and §9.1 are both affected.

**Suggested fix.** Pick one and write it down: (a) one `TranscriptView` per agent (kept with the
`AgentModel`s — the crates are cheap to instantiate), (b) `replace` taking a *view* revision the
workspace owns (a monotone counter bumped on selection/resync), or (c) a small
`TranscriptView::reset_revision()` used on selection change. My `session::TabModel` already keeps
per-agent revisions and a per-agent row space, so (a) or (b) fits both.

## F6 — `Command::Steer` (nit, `tab_engine`)

`crates/tab_engine/src/types.rs:159-167` and `engine.rs:496-505` route `POST /steer`; the UI must not
send it (§14.7: every turn goes through `/prompt`, which queues). It is kept as the one command
serve answers `409 Not now` with, and `docs/api-gaps.md` §3 says so. No change wanted; recorded so
the next reader does not "clean it up".

## F7 — `turn-start`'s step clock (nit, `session`)

`docs/PROMPT.md` §5 lists `turn-start`/`run-start` as "activity state, step clock".
`crates/session/src/model.rs:323-341` folds `task-start`/`task-end`/`run-start`/`run-end` into
`Activity` only; `turn-start` is ignored. The coordinator has no step clock in the UI, and the §7.3
readout has no clock segment, so nothing is missing that the spec asks a *frontend* to show — the
lane step clock (which the left column does show) comes from `/lanes.task_age`/`step_age` and is
formatted by `session::short_duration`. Recorded as a deliberate reading, not a defect.

## Requirement checklist

| rule | status | where / why |
|---|---|---|
| §2.1 one instance, activation | **gap (F3)** | `app/main.rs` has no lock; `store::single` is ready but unused |
| §2.2 spawn binaries as-is, no wrappers | met | `swarm_client::server::spawn` (`server.rs:606-660`) runs `cfg.bin` with `argv()` and nothing else; `scripts/bundle.sh` copies only our own binary |
| §2.3 own only `~/.evo/desktop/` | met (in `store`) | `store::{paths,app_state,tab}`; the app does not write it yet (F1/F3). Nothing outside `store` touches `~/.evo/sessions` or a lane dir, and the store only reads the former (the §9.5 scan) |
| §2.4 secrets stay in files | met | `http::Token`'s `Debug`/`Display` are redacted (`http.rs:22-53`); the token is read from `--token-file` and never logged, never in `app.json`; grep for a token in a formatted message finds none |
| §2.5 no second protocol | met | event-driven engine (no polling loop: `engine.rs` has none); no `/lanes/N/health`, no lane token/URL (`stream.rs:52-70` builds lane paths only); journals are read only by the §9.5 scan the spec asks for |
| §2.6 UI thread never blocks | met | no blocking call in the UI crates (`rg 'recv_blocking\|block_on\|thread::sleep\|read_to_string\|TcpStream'` over `workspace/transcript/composer/agent_list/app` hits tests only); `workspace::bridge::Revision` drops stale values, with tests proving the cross-thread, parked-task behaviour |
| §2.7 no raw JSON/sexp in the UI | met | `transcript`, `composer`, `agent_list` depend only on `session`; `rg 'serde_json\|Value'` over those crates finds nothing |
| §2.8 markdown rendered live | met | one `TextViewState::markdown` per assistant row, `set_text` on `version` change, `stream_motion()` fade, `MessageScroller` tail-follow + "Jump to latest" (`transcript/lib.rs:54-110`, `:294-345`) |
| §3 spawn / ready / shutdown | met in `swarm_client`, **unwired (F1)** | `argv()` is byte-for-byte §3's (`server.rs:237-300`); `EVO_SERVE_WATCH_PID` set and the inherited one scrubbed (`server.rs:28-37`, `:621`); readiness = token + `/health` name+features, 100 ms poll, 90 s deadline, early exit detected, 40-line `swarm.log` tail (`:331-410`); ladder `POST /shutdown` → 10 s → `SIGTERM` → 5 s → `SIGKILL`, to the process group, never kill-first (`:540-590`); `--allow-remote` never passed (unit test) |
| §4 endpoints and statuses | met in `swarm_client`/`tab_engine`, **unwired (F1)** | every endpoint of §4 exists on `Client` (`api.rs:641-718`, plus `/follow-up` which serve does define); statuses typed one variant each (`error.rs:147-190`); `PostError::not_now` is the 409 soft refusal (`types.rs:189-210`) — but no UI shows it yet, so "a 409 is a dim notice, never a modal" is untested end to end |
| §5 events | met in `session` | every event of the table is handled by `session::model::apply_event` and covered by tests over the captured streams: message-*, text/thinking-delta, tool-call-start/tool-result, message-end, run-*/turn-*/task-*, steering/user-input, compaction-*, provider-retry, todo-changed, settled, output, session-switched, lane-state, report, hello/gap/ready/shutdown/bye, unprintable-event. **Wire spelling**: a plist key crosses as hyphen→underscore (`../evo-agent/src/serve/json.lisp:29`), so `is_error`, `content_chars`, `arguments_json`, `task_id`, `stop_reason`, `goal_id`, `tokens_used_live`; `session` had this wrong for `tool-result` and it was fixed in `a1da201`. `swarm_client`/`tab_engine` never parse event bodies (they pass `Value` through), so they are not exposed to it |
| §7.3 readout order and parity | met in `session`, **unwired (F1/F2)** | `session::Readout` builds `model · thinking · ctx · cache · goal` in the TUI's order (100/200/300/350/400) with the TUI's own rounding, verified value-by-value against Common Lisp. `composer::set_readout` is called by nobody but its own demo: `rg -n 'set_readout' crates` → `composer/examples/composer_demo.rs:41` only, so the status row ships empty |
| §9.1 assembly and resync | met in `tab_engine`, **unwired (F1)** | `assemble` = registry → lanes → transcript+state → cache seed → `/events?since=<health.cursor>` (`engine.rs:379-395`); resync on `settled`/`gap`/`hello`/`session-switched`/reset/reconnect (`:628-680`); revisions per agent; `transcript` drops older revisions. F5 is the wiring trap |
| §9.2 send, one button | met in `composer` | face = Send/Stop from `Activity`, Enter sends, `Esc` interrupts, draft cleared only on `ok`, disabled only in flight (`composer/lib.rs:129-215`; `face` :159, `request_finished` :149, `is_action_enabled` :168); unwired (F1) |
| §9.3 lane watching | met in `tab_engine` | at most one lane stream, closed before another opens (`engine.rs:291-330`, `:567-585`); a lane's token/URL is never fetched. The one heuristic here is the lane todo seed's 400 ms silence cut-off (`engine.rs:587-625`), already recorded as an API gap (`docs/api-gaps.md` §1) because the relay has no lane cursor |
| §9.4 model catalog | met in `tab_engine::catalog`, **unwired (F2)** | two throwaway probes (userspace + `--no-userspace`, kernel APIs), on their own thread, nothing written outside `probe_dir` (`catalog.rs`); the kernels' API set is what `session::lanes_chooser` measures availability against. Not persisted: `store::model_cache` is ready and unwired |
| §9.5 history scan | met in `store`, **unwired (F2)** | `store::history`; the empty tab shows `placeholder_history()` with hardcoded model names |
| §9.6 `swarm.lisp` managed block | met in `store`, **unwired (F2)** | `store::swarm_config::{set_lanes_model,…}` (`swarm_config.rs:33-109`) writes the block at the top, both `:model` and `:model-provider`, idempotently, and deletes the file when nothing is left; nothing calls it before a spawn yet |
| §9.7 failure surfaces | partly | boot failure log tail + Retry is built (`tab_page.rs:59-100`); lane-down reason and the reconnecting badge exist in `session`/`agent_list` (`set_down_reason`) and `tab_engine` (`StreamStatus::Reconnecting`) but are unwired (F1) |
| §9.8 quit | **gap (F3)** | `tab_engine::shutdown_all` exists and is tested; nothing calls it at quit |

## Fixes landed since this review (`crates/session`)

- **F4** — `session::launcher`'s **coordinator** chooser now offers exactly the registration
  `--model <id>` reaches and lists the other registrations of that id disabled with the reason
  "evo-swarm --model resolves this id to <provider>". The order evidence is in the code:
  `*models*` is "in registration order" (`src/provider/registry.lisp`), `/registry.models` is
  that list walked in order (`src/serve/routes.lisp`), and `find-model` for a bare id takes the
  first entry — which is why the two-provider capture resolves `stub-a` to `stub`. The lanes
  chooser is unaffected (§9.6 writes the provider). Tests:
  `crates/session/tests/launcher.rs::the_coordinator_offers_only_the_registration_a_bare_id_reaches`
  (capture order, a reversed registry, the one-provider capture, and the lanes chooser).
- **F6** — `session` now tracks the step: `StepClock { turn, event_id, started_at_millis }`,
  begun at `run-start`/`turn-start`/`compaction-start`/`compaction-end` (the TUI's own
  `begin-step` boundaries) and ended by `run-end`/`task-end`/`settled`/`hello`/
  `session-switched`. `Effect::STEP` and `Changes::step` say when it changed;
  `AgentModel::step_started()` and `TabModel::coordinator_step_started()` read it;
  `on_event_at(…, now_millis)` is how a frontend stamps the arrival time, while `on_event`
  leaves it unset for a frontend that counts on its own clock.
- **F1/F2/F3/F5** are not mine: F5 is directed to the workspace lane, F1–F3 remain open.

## What is verified, and how

Read-only checks run for this review (all from `/Users/bytedance/coding/evo-gui`):

```
git log --oneline | head -30                      # what is in, at which commit
git ls-files 'crates/**/*.rs' | xargs wc -l       # what exists, and how big
git status --short                                # what is in flight (excluded, see below)
rg -n 'serde_json|Value|read_to_string|std::fs|sleep|blocking|recv\(' \
   crates/transcript/src crates/composer/src crates/agent_list/src
rg -n 'recv_blocking|block_on|thread::sleep|read_to_string|TcpStream|reqwest|\.join\(\)' \
   crates/workspace/src crates/composer/src crates/transcript/src crates/agent_list/src crates/app/src
rg -n 'on_app_quit|QuitMode|shutdown_all|on_window_closed' crates --glob '*.rs'
rg -n 'set_readout|learn_registry|mark_running|SingleInstance|activate' crates --glob '*.rs'
rg -n 'run-start|run-end' crates --glob '*.rs'
```

Tests that back the "met" rows: `swarm_client/tests/swarm_e2e.rs` (boot + ladder, stream + resume,
boot-failure log tail, port 0, parallel shutdown), `tab_engine/tests/tab_e2e.rs` (boot assembly,
prompt → stream → settled resync, single lane stream, typed refusals, shutdown, restarted
coordinator, dead server, boot cancellation within a second, catalog probe with kernel APIs,
parallel shutdown), `session` (71 tests, most of them over the captured fixtures), `workspace/src/bridge.rs`
(worker values land on the UI thread; stale revisions dropped; awaiting parks the task),
`composer` (send/stop faces, draft rules), `agent_list` (`LaneList` rows, glyphs, down reason),
`transcript` (streaming rows, documents, todos).

## Excluded: in-flight work at review time

`git status --short` at `084e6d6` showed these dirty, so they are **not** part of this review:
`crates/workspace/src/launch.rs` (untracked — the F1/F2 wiring, it seems),
`crates/transcript/src/*`, `crates/agent_list/src/lib.rs`, `crates/store/src/*`
(the §9.4/§9.5/§9.6 polish). Anyone acting on F1/F2/F5 should read those first: some of these
findings may already be closed there.
