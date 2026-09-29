# evo-desktop architecture (crate ownership)

Spec: docs/PROMPT.md (copy of the build prompt). Section refs like §7.3 point there.

| crate | kind | owns |
|---|---|---|
| `swarm_client` | pure Rust, no gpui | spawning `evo-swarm serve`/`evo-agent serve` (§3), readiness, shutdown ladder, blocking HTTP (Connection: close, bearer), SSE parser + resumable reconnecting stream (§5), typed payloads for /health /state /transcript /registry /lanes /journal, POST envelope + status mapping (§4). Test harness against a real evo-swarm with stub provider. |
| `store` | pure Rust | `~/.evo/desktop/` layout (§6): app.json, lock (flock + activation), model-cache.json, tab dirs + tab.json, history scan of `~/.evo/sessions` (§9.5), swarm.lisp managed block (§9.6). |
| `tab_engine` | pure Rust, no gpui | one per tab: owns the `Server` + streams, runs §9.1 assembly/resync, lane watching, POSTs, cache-stats seeding, registry refresh; commands in / updates out over async-channel. The UI applies updates to `session` models. |
| `session` | pure Rust | per-agent view model: transcript rows from /transcript + event reducer (§9.1), revision counters, status readout segments (§7.3, exact TUI format), cache-stats seeding/folding, todos, lane list model. |
| `transcript` | gpui | transcript view: MessageScroller, streaming markdown TextView per assistant message (§2.8), tool rows, report rows, dim rows, jump-to-bottom; todo panel. |
| `agent_list` | gpui | the tab page's left column (§7.3): `main` + one row per lane, status glyph, task, step clock, selection, reconnecting badge. |
| `composer` | gpui | Textarea input (2→8 rows, Enter/Shift+Enter/Esc), status readout row + single Send/Stop button. |
| `workspace` | gpui | window root, TitleBar+TabBar, empty tab (choosers, folder button, history), tab page layout (lane list / center / right), tab lifecycle wiring swarm_client↔session. |
| `proofs` | tests only | milestone proofs (M1–M4) that drive real `evo-swarm` through tab_engine + session + store without the UI; UI-level proofs live in workspace tests. |
| `settings` | gpui | the Settings panel (§13: binary paths and theme only): evo-swarm / evo-agent paths with a version check, theme System/Light/Dark; persisted via store::app_state. |
| `evo-desktop` (crates/app) | bin | main: single-instance, window options/bounds, quit handling. |

Rules: pure crates must not depend on gpui. UI thread never blocks: I/O on own threads, results delivered via channels → weak entity updates with revision checks.

## Commit convention

Conventional commits: `type(scope): subject` — type ∈ feat, fix, refactor, test, docs, chore, perf, build;
scope = crate name (`swarm_client`, `store`, `session`, `tab_engine`, `transcript`, `composer`, `workspace`,
`app`) or `repo`/`scripts`/`docs`. Subject imperative, lower-case, no trailing period, ≤ 72 chars. Then a blank line and a body
(wrapped at ~72) that says what changed and why — behaviour, notable decisions, how it was verified;
a title-only message is not acceptable.
Every message ends with the trailer `Co-authored-by: EvoAgent <evo@ruiqilei.com>`.
Lanes do not commit; they end each report with a proposed message in this format, and the coordinator
commits by explicit paths.
