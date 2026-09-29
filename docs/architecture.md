# evo-desktop architecture (crate ownership)

Spec: docs/PROMPT.md (copy of the build prompt). Section refs like §7.3 point there.

| crate | kind | owns |
|---|---|---|
| `swarm_client` | pure Rust, no gpui | spawning `evo-swarm serve`/`evo-agent serve` (§3), readiness, shutdown ladder, blocking HTTP (Connection: close, bearer), SSE parser + resumable reconnecting stream (§5), typed payloads for /health /state /transcript /registry /lanes /journal, POST envelope + status mapping (§4). Test harness against a real evo-swarm with stub provider. |
| `store` | pure Rust | `~/.evo/desktop/` layout (§6): app.json, lock (flock + activation), model-cache.json, tab dirs + tab.json, history scan of `~/.evo/sessions` (§9.5), swarm.lisp managed block (§9.6). |
| `session` | pure Rust | per-agent view model: transcript rows from /transcript + event reducer (§9.1), revision counters, status readout segments (§7.3, exact TUI format), cache-stats seeding/folding, todos, lane list model. |
| `transcript` | gpui | transcript view: MessageScroller, streaming markdown TextView per assistant message (§2.8), tool rows, report rows, dim rows, jump-to-bottom; todo panel. |
| `composer` | gpui | Textarea input (2→8 rows, Enter/Shift+Enter/Esc), status readout row + single Send/Stop button. |
| `workspace` | gpui | window root, TitleBar+TabBar, empty tab (choosers, folder button, history), tab page layout (lane list / center / right), tab lifecycle wiring swarm_client↔session. |
| `evo-desktop` (crates/app) | bin | main: single-instance, window options/bounds, quit handling. |

Rules: pure crates must not depend on gpui. UI thread never blocks: I/O on own threads, results delivered via channels → weak entity updates with revision checks.
