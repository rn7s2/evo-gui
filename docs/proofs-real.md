# Real-environment proof (M0)

What the app's own crates do against the **installed** evo and the user's real
configuration — no stub provider, no temp `EVO_HOME`, no fixtures. Lane 3,
2026-09-29, all times **UTC**.

- machine: this Mac; the local zone is UTC+08:00 (`localtime_r` `tm_gmtoff`
  `+28800`), which is why the history rows' tooltips below say `+08:00`.
- binaries: `/usr/local/bin/evo-swarm`, `/usr/local/bin/evo-agent`, both
  reporting `0.1.0` (`GET /health`), `/usr/local/bin/evo` is a symlink to
  `evo-swarm`.
- `HOME=/Users/bytedance` (the real one), real `~/.evo/init.lisp`,
  `~/.evo/swarm.lisp`, `~/.evo/extensions/`, real credential files.
- one worker, no `--model`: the swarm takes the user's own default.

Three of the findings below were fixed on our side after this proof: R1 (a
`swarm_client::redact` applied where server text enters the crate), R3 (the
history scan follows the evo home, not a lane's `EVO_SESSIONS_DIR`) and R2's
trailing-separator care for the one place we build an `EVO_HOME`. R4 stands as
notes, and the `/registry` 500 itself is evo's to fix.

Four examples drive it, all new files under `crates/swarm_client/examples/`:

| example | what it does |
|---|---|
| `m0_real.rs` | the §12 M0 proof: start, stream, prompt, the route set, `POST /shutdown`, process check |
| `real_readout.rs` | `session::Readout` renders the §7.3 status line from the run's `/state` + `/registry` + `message-end` usages |
| `real_catalog.rs` | `tab_engine::catalog::learn` (§9.4) against the real `HOME`, then the numbers and the lanes chooser's own marks |
| `real_history.rs` | `store::history::scan` over `~/.evo/sessions` and `session::history_rows` (§9.5) |

`crates/swarm_client/Cargo.toml` gained three **dev**-dependencies (`session`,
`store`, `tab_engine`) so those examples can render what the app renders; the
library's own dependency graph is unchanged, so nothing in a production build
compiles them. (Needs the coordinator's ack — it is the one file I touched that
is not a new example.)

A fifth example drives the same real backend through **the app's own window** —
`crates/app/examples/real_gui_run.rs`, §4 below: the production `Shell` and
`WorkspaceView`, one tab launched by the empty tab's `Launch` event, a prompt sent
through the composer, four headless frames of the run, then the app's own quit.

```text
CARGO_TARGET_DIR=target/real cargo run -q -p swarm_client --example m0_real -- /tmp/evo-m0-real-20260929T121155Z
```

---

## 1. The M0 run

```text
started_utc:        2026-09-29T12:11:55Z
bin:                /usr/local/bin/evo-swarm
evo (lanes):        /usr/local/bin/evo-agent
folder:             /private/tmp/evo-m0-real-20260929T121155Z/project
results:            /tmp/evo-m0-real-20260929T121155Z
workers:            1
model:              (default: none passed)
prompt:             Delegate to a lane: create hello.txt containing 'hi' in this folder, then tell me when it's done. Keep it brief.
```

The example runs with the lane's own environment (an evo session's `EVO_*`
variables are set in it). `swarm_client`'s spawner scrubs `SCRUB_ENV`
(`EVO_SERVE_TOKEN`, `EVO_SESSIONS_DIR`, `EVO_IDE_CONTEXT`, `EVO_PID`,
`EVO_HEARTBEAT_FILE`, `EVO_SERVE_WATCH_PID`, …) and then sets
`EVO_SERVE_WATCH_PID` to the example's own pid, so the run had the real config
and journalled to the real `~/.evo/sessions` — not to the lane's private
sessions directory. That is visible in the history section below, where the row
this run produced is `…/sessions/-private-tmp-…-project/…`.

### Timeline, `/health`, `/state`

```text
ready_utc:          2026-09-29T12:11:56Z
ready:              evo-swarm 0.1.0 pid 8476 port 56622 features ["swarm"]
seeded_state_utc:   2026-09-29T12:11:56Z
seeded_state:       status=idle model=Some("claude-opus-5-5") provider=Some("anthropic-oauth") thinking=Some("high") context=699/Some(1000000)
prompt_utc:         2026-09-29T12:11:56Z
prompt_reply:       ok=true queued=Object {"queued": Bool(true)}
settled:            stop at 2026-09-29T12:12:05Z
lane1_state:        working at 2026-09-29T12:12:05Z
lane1_state:        idle at 2026-09-29T12:12:11Z
shutdown_utc:       2026-09-29T12:12:13Z
finished_utc:       2026-09-29T12:12:14Z
```

- **model/provider** (`/state`): `claude-opus-5-5` on provider
  `anthropic-oauth`, thinking `high`, context window 1 000 000. That is the
  user's own `~/.evo/swarm.lisp` (`:model "claude-opus-5-5"`, `:thinking
  :high`), not something the proof passed.
- the swarm's first `/state` is already idle and consistent with `/lanes`: one
  lane, `workers: 1`, swarm id `20260929T121156-f08c`.
- **`settled` is not the lane finishing.** `settled: stop` at 12:12:05Z is the
  coordinator idle again — it delegated and returned (`I've handed this to lane
  1.`), while lane 1 stayed `working` for another 6 s. §12's "settled" is the
  turn boundary, so the proof keeps reading the stream and polling `/lanes`
  until lane 1 has been busy and stopped being busy; a UI-only M0 would fetch
  lane 1's transcript mid-task.
- the coordinator **does** relay the lane's own words onto `/events`:
  `[lane 1 report] done: Created …/hello.txt …` and `[lane 1] run ended (stop)
  — task: …` both arrived while lane 1 was still finishing.

### The event stream (§5), 37 events

```text
hello 1 · lane-state 3 · message-end 2 · message-start 3 · output 2 · ready 1 ·
run-end 1 · run-start 2 · settled 1 · steering 2 · task-end 1 · task-start 2 ·
text-delta 11 · tool-call-start 1 · tool-result 1 · turn-start 3
```

`tool-call-start` carried `delegate {task: "In the current working directory,
create a file named hello.txt …"}` — the one tool call the coordinator made.
`ready`, `task-start`/`task-end`, `lane-state` and `run-end` are the swarm's own
events; `message-*`, `turn-start`, `text-delta`, `tool-*` and `settled` are the
kernel's. Text arrived as 11 `text-delta` events, i.e. the §2.8 streaming path
with a real provider, not a replayed fixture.

### The route set after the run

```text
route /transcript          200 (6113 bytes) → transcript.json       6 messages
route /state               200 (539 bytes)  → state.json
route /registry            FAILED: http 500 (see finding R1)
route /lanes               200 (571 bytes)  → lanes.json            1 lane
route /lanes/1/transcript  200 (7804 bytes) → lane1-transcript.json 9 messages
state:              status=running model=Some("claude-opus-5-5") provider=Some("anthropic-oauth") thinking=Some("high") context=151197/Some(1000000) turn=2 todos=0
```

`/lanes` said lane 1 was `idle`, `reports: 1`, `restarts: 0`, `pid: 8510`,
`task_age: 11` — every field the left column draws was there. Lane 1's
transcript shows it ran on `ark-deepseek-v4.1-flash`
(`api: anthropic-messages`) — the user's in-lanes model from `~/.evo/swarm.lisp`
— with four tool calls of its own (`thinking`, `tool-call`, `tool-result`
blocks), ending `stop_reason: stop`.

### The §7.3 readout line, from `session::Readout`

```text
model_label:        Some("claude-opus-5-5")
context_label:      ctx 151k/1000k (15%)
cache_label:        Some("0% cached")
segments:           ["claude-opus-5-5", "high", "ctx 151k/1000k (15%)", "0% cached"]
readout line:       claude-opus-5-5 · high · ctx 151k/1000k (15%) · 0% cached
```

Fed exactly what the tab feeds it: `/state` → `apply_state`, `/registry` →
`apply_registry`, then each `message-end`'s `usage` → `fold_message_end` (the
run's two usages, `{"cache_read":0,"cache_write":6929,"input":4,"output":124}`
and `{"cache_read":0,"cache_write":151095,"input":2,"output":39}`).

Three observations, all benign:

- `0% cached` is real, not a bug: both usages have `cache_read: 0` — the
  provider wrote ~158k to the cache and read nothing back inside one short run.
- the folded `context_tokens` is 151 136, while the `/state` fetched a moment
  later said 151 197 — 61 more. The readout mirrors the TUI's own accounting
  (§7.3: seed from `/state`, *then* fold each `usage`), so it shows the folded
  number; nothing else in the code reads `/state`'s `context_tokens`.
- `registry.json` was never written, so `apply_registry` had nothing to say —
  with a single registration for this id the segment is identical anyway. On a
  machine where an id is registered under two providers this same failure would
  silently cost the `id (provider)` annotation (finding R1).

### One thing the shutdown cut short

The run's own journal (`~/.evo/sessions/-private-tmp-evo-m0-real-20260929T121155Z-project/20260929T121155Z_5455e533887e7824.sexp`)
ends:

```lisp
(:type :message … :timestamp "2026-09-29T12:12:07Z" :message (:role :user :content ((:type :text :text "[lane 1 report] done: …"))))
(:type :message … :timestamp "2026-09-29T12:12:12Z" :message (:role :assistant :api :anthropic-oauth-messages :provider :anthropic-oauth
  :model "claude-opus-5-5" :stop-reason :aborted :usage (…) :content nil))
```

The lane's report is fed back to the coordinator as a new user turn — that is why
`/state` still said `running`, `turn=2`, when the routes were fetched — and the
proof's `POST /shutdown` two seconds later **aborted that turn before it wrote a
word**. So the last *complete* assistant message is the hand-off above, and the
coordinator's closing line about the report does not exist. Recorded as R4 below:
"settled" is a turn boundary, and a swarm shut down at the first `settled` loses
the coordinator's follow-up.

### The work itself

```text
--- final coordinator text ---
I've handed this to lane 1. I'll tell you once it confirms hello.txt exists with "hi" in it.
--- lane 1's final text ---
Done. `hello.txt` exists in the working directory containing exactly `hi` (2 bytes,
no trailing newline), verified with `cat` and `wc -c`.
lane 1 report: done: Created /private/tmp/evo-m0-real-20260929T121155Z/project/hello.txt with
content exactly `hi` (2 bytes, no trailing newline) and verified it.
evidence: `cat hello.txt` printed `hi` (exit 0); `wc -c < hello.txt` returned 2.
hello_txt:          true
hello_txt_content:  "hi"
```

### Shutdown (§3) and the process check

```text
processes_before:   coordinator 8475 descendants [8476, 8493, 8504, 8510]
shutdown:           Graceful exit Some(0) waited 2.072818333s
processes_after:    still alive []
```

- the spawner's pid is **not** the serving pid: `Server::pid()` was 8475 (what
  `cargo`/the spawner started) while `/health` reported 8476 — the installed
  binary forks a supervisor, and the lanes hang off it through `perl … setsid`
  wrappers (8493/8504) and their `evo-agent` processes (8510). Nothing is
  left behind: the ladder's `SIGTERM`-to-the-group path never had to be used,
  `POST /shutdown` alone ended the group in 2.07 s, and `kill(pid, 0)` said
  every pid was gone 500 ms later.
- the run's own artifacts are left for inspection under
  `/tmp/evo-m0-real-20260929T121155Z/` (`state.json`, `transcript.json`,
  `lanes.json`, `lane1-transcript.json`, `usage.jsonl`, `project/hello.txt`,
  `tab/swarm.log`) — no token, and no file there mentions one.

---

## 2. The catalog probe (§9.4)

Against the **real** `HOME`:

```text
agent_bin:          /usr/local/bin/evo-agent
probe_dir:          /var/folders/…/T/evo-desktop-real-catalog-…
home:               (the real one)
catalog:            FAILED after 2.153s: http 500: The value
  "Bearer <redacted>"
is not of type
  LIST
```

The probe's own server started fine (its log holds only the listening line);
the failure is `GET /registry`, i.e. **finding R1**. So on this machine
`tab_engine::catalog::learn` cannot learn anything, and the empty tab keeps
whatever the model cache already had and shows the error as its caption.

To still read the rest of the catalog, the probe was re-run with
`--home /tmp/evo-home-catalog-fixed/`, a temp `EVO_HOME` whose `init.lisp` is
the user's with the two `("Authorization" . "Bearer …")` header entries written
as 2-element lists (the difference that R1 turns on) and whose
`extensions/`, `docs/`, `prompts/`, `skills/`, `claude-oauth/` are symlinks to
the real ones:

```text
registry models:    10
registry providers: 5   anthropic (key-unknown), super-relay (has-key), aiden (has-key), anthropic-oauth (key-unknown), kimi (key-unknown)
kernel apis:        anthropic-messages
models:
  seed-evolving · super-relay · anthropic-messages
  ark-deepseek-v4.1-flash · aiden · anthropic-messages
  ark-glm-5.2 · aiden · anthropic-messages
  gpt-5.6-sol · aiden · anthropic-messages
  gpt-6-astra · aiden · anthropic-messages
  claude-sonnet-5 · anthropic-oauth · anthropic-oauth-messages
  claude-opus-5 · anthropic-oauth · anthropic-oauth-messages
  claude-fable-5 · anthropic-oauth · anthropic-oauth-messages
  claude-fable-5-1 · anthropic-oauth · anthropic-oauth-messages
  claude-opus-5-5 · anthropic-oauth · anthropic-oauth-messages
```

- `kernel_apis` came from the `--no-userspace` probe and is **one** API,
  `anthropic-messages`, while the userspace probe's `/registry.apis` holds two
  (`anthropic-messages`, `anthropic-oauth-messages`) — the extension registers
  its own wire API, the kernel does not. That is exactly the §9.4 distinction
  the lanes chooser is built on: the five `claude-*` models speak an extension
  API a quarantined lane cannot reach.
- **The lanes chooser marks exactly those five unavailable:**

  ```text
  lanes chooser: uncertain=false, 11 options (10 models + Default)
  lanes unavailable: 5
    claude-fable-5 (anthropic-oauth) — needs an extension API — set it in swarm.lisp
    claude-fable-5-1 (anthropic-oauth) — needs an extension API — set it in swarm.lisp
    claude-opus-5 (anthropic-oauth) — needs an extension API — set it in swarm.lisp
    claude-opus-5-5 (anthropic-oauth) — needs an extension API — set it in swarm.lisp
    claude-sonnet-5 (anthropic-oauth) — needs an extension API — set it in swarm.lisp
  lanes available: 5      (seed-evolving, ark-deepseek-v4.1-flash, ark-glm-5.2, gpt-5.6-sol, gpt-6-astra)
  coordinator chooser: 11 options, 0 unavailable
  ```

  Note the shape of these two real cases: the model the coordinator itself runs
  on (`claude-opus-5-5`) is one a lane may **not** be given, and the model the
  user's lanes actually run (`ark-deepseek-v4.1-flash`) is one they may.
- the same probe run with `--home /tmp/evo-home-catalog-fixed` (no trailing
  slash) reported only 5 models — the five `claude-*` ones were missing, because
  the vendored `020-claude-oauth-provider.lisp` looks for its token under
  `EVO_HOME` and mis-merges a path without a trailing separator (finding R2).
  With the slash, all ten appear. The temp home was deleted after the proof.

---

## 3. The history list (§9.5)

```text
sessions_dir:       /Users/bytedance/.evo/sessions
files_seen:         697
files_read:         405
stopped_early:      true
resumable swarms:   16 in 5.002s
scan:               now=1790684534 offset=+28800s home=/Users/bytedance
rows:               16
```

`ScanBudget::default()` is 500 files / 5 s / 8 MB per file; on this machine the
**time** cap binds first (405 files in 5 s ≈ 81 files/s) and the scan stopped
early. Files are walked newest first, so the rows are the newest resumable
swarms — older ones are simply not listed, which is what the budget means.

The first five rows, each paired with the scan entry it came from (folder,
lanes, when, coordinator model — `session::history_rows` formatting):

```text
1. folder=/private/tmp/evo-m0-real-20260929T121155Z/project/  lanes=1  when=epoch 1790683915  coordinator_model=claude-opus-5-5  lanes_model=None
   title: project   subtitle: /private/tmp/evo-m0-real-20260929T121155Z/project/
   meta:  1 lane · 10m ago · coordinator: claude-opus-5-5
   tooltip: /private/tmp/evo-m0-real-20260929T121155Z/project · 20260929T121155Z_5455e533887e7824.sexp ·
            2026-09-29 20:11:55 +08:00 · coordinator: claude-opus-5-5 · 1 lane
   session: /Users/bytedance/.evo/sessions/-private-tmp-evo-m0-real-20260929T121155Z-project/20260929T121155Z_5455e533887e7824.sexp
2. folder=/Users/bytedance/coding/evo-agent/  lanes=6  when=epoch 1790681545  coordinator_model=claude-opus-5-5  lanes_model=None
   title: evo-agent   subtitle: ~/coding/evo-agent/
   meta:  6 lanes · 49m ago · coordinator: claude-opus-5-5
   tooltip: /Users/bytedance/coding/evo-agent · 20260929T113225Z_9c8cd9db25f6b194.sexp ·
            2026-09-29 19:32:25 +08:00 · coordinator: claude-opus-5-5 · 6 lanes
3. folder=/private/tmp/evo-m0-real-20260929T111237Z/project/  lanes=1  when=epoch 1790680357  coordinator_model=claude-opus-5-5  lanes_model=None
   title: project   subtitle: /private/tmp/evo-m0-real-20260929T111237Z/project/
   meta:  1 lane · 1h ago · coordinator: claude-opus-5-5
   tooltip: /private/tmp/evo-m0-real-20260929T111237Z/project · 20260929T111237Z_c5decce8f09379ce.sexp ·
            2026-09-29 19:12:37 +08:00 · coordinator: claude-opus-5-5 · 1 lane
4. folder=/Users/bytedance/coding/evo-agent/  lanes=6  when=epoch 1790675011  coordinator_model=claude-opus-5-5  lanes_model=None
   meta:  6 lanes · 2h ago · coordinator: claude-opus-5-5
   tooltip: /Users/bytedance/coding/evo-agent · 20260929T094331Z_29fd83d952180aa1.sexp ·
            2026-09-29 17:43:31 +08:00 · coordinator: claude-opus-5-5 · 6 lanes
5. folder=/Users/bytedance/coding/evo-gui/  lanes=6  when=epoch 1790672996  coordinator_model=claude-opus-5-5  lanes_model=None
   title: evo-gui   subtitle: ~/coding/evo-gui/
   meta:  6 lanes · 3h ago · coordinator: claude-opus-5-5
   tooltip: /Users/bytedance/coding/evo-gui · 20260929T090956Z_ed99c60d1dee3c3f.sexp ·
            2026-09-29 17:09:56 +08:00 · coordinator: claude-opus-5-5 · 6 lanes
```

- row 1 is **this proof's own run** (§1, 12:11:55Z), journalled by the real evo
  at the real `~/.evo/sessions` path — so the spawner's environment scrub held,
  and the row is a genuinely resumable swarm (folder + `--resume` path + 1 lane).
  Row 3 is the aborted first attempt at the same proof (its temp folder was
  deleted afterwards; the journal stays until someone cleans `~/.evo/sessions`),
  which is why the list shows two one-lane `project` rows.
- `lanes_model` is `None` on every scanned row, as designed: the journal records
  the coordinator's model (last `:model-change`), the lanes model only the app
  itself remembers from a session it opened. A first-launch app therefore shows
  "coordinator: …" and no lanes model, and fills it in from then on.
- the first attempt at §3 ran inside a swarm lane, where `EVO_SESSIONS_DIR`
  points at that lane's own sessions directory: it scanned
  `/Users/bytedance/.evo/swarm/20260929T093455-90a1/lane-3/sessions/` and found
  0 files, 0 rows (finding R3 — since fixed: the variable is ignored, and the
  run above is the same directory with it set in the environment).

---

## 4. M0/M1 through the GUI: one real run in the app window

§12's M0 also asks for a screenshot of a real run, and M1 for a run where a lane
is delegated work and the coordinator's transcript renders it. This is that
proof with the app's own window in the loop: `crates/app/examples/real_gui_run.rs`
assembles the production `Shell` global and a real `WorkspaceView` (the app's
1600×1000 window, its title bar, its tab strip), one tab opened by the empty
tab's own `TabContentEvent::Launch` in a fresh temp folder with `--workers 1` and
**no** model override, and a real `tab_engine` driving the installed
`/usr/local/bin/evo-swarm` + `evo-agent`. The `HOME`, the evo home, the
credentials and the user's `~/.evo/init.lisp` / `~/.evo/swarm.lisp` are the real
ones, so the swarm ran the user's own models; the catalog probe and the session
scan are the only things that did not run (this proof is about the swarm, and on
this machine the probe is finding R1 anyway).

```sh
cargo run -p evo-desktop --example real_gui_run -- --capture docs/screens/real --scale 1
```

The one thing deliberately not the user's is the app's own root: `~/.evo/desktop`
is a temp directory for the run, because the quit sequence writes `app.json` and
this proof must not rewrite the tab set of a desktop app the user may have open.
The run below drove the working tree at 2026-09-29T16:57Z (HEAD `e8ecd5d` plus the
other lanes' in-flight work).

Two things about these frames are worth saying before they are read, because both
are visible in them. The session's injected context — the message `/transcript`
marks with `meta.key` — is **one collapsed line** in every frame (`Context ·
global memory`, `e8ecd5d`): drawn as a note, never as a user turn, and never open
here. (Before that change the earlier run's four frames drew the user's own memory
snapshot instead, which is why these pictures were taken again.) And this run met
a provider hiccup on its way — `Retrying provider (1/4)` sits in the first two
frames — which is why the coordinator's first word did not arrive until 76 s after
the prompt.

### The run, in UTC

```text
started_utc:        2026-09-29T16:57:22Z  temp project folder + temp app root; real HOME
window_utc:         2026-09-29T16:57:23Z  theme light (the system appearance); one empty tab
launch_utc:         2026-09-29T16:57:23Z  the empty tab's Launch event: workers 1, models Default
ready_utc:          2026-09-29T16:57:24Z  /health answered — swarm pid 41066, no session yet
prompt_utc:         2026-09-29T16:57:24Z  typed into the composer and Entered (§9.2's send path)
(b)_utc:            2026-09-29T16:58:37Z  delegate row with its result; lane list 1 busy, lane 1 ● working
(a)_utc:            2026-09-29T16:58:40Z  the first reply streaming (assistant row, 71 characters)
(c)_utc:            2026-09-29T16:58:42Z  lane 1 selected; its own transcript, 3371 characters
(d)_utc:            2026-09-29T16:58:55Z  report seen, follow-up turn over, coordinator idle, transcript still
quit_utc:           2026-09-29T16:58:57Z  evo_desktop::begin_quit
shutdown_utc:       2026-09-29T16:58:59Z  app.log: "shutdown: all 1 tab(s) exited"
process_check_utc:  2026-09-29T16:59:00Z  0 processes alive, by tab dir and by project folder
```

The coordinator's journal at the real `~/.evo/sessions` (the temp project's cwd)
carries the turns, and they line up with the frames — including the two `bash`
calls it made for itself, one before delegating and one to check the file:

```text
16:57:24Z injected context (the row the tab draws as one collapsed line)
16:57:24Z user      Delegate to a lane: create hello.txt containing 'hi' in this folder, …
16:57:33Z assistant (tool-use) bash {"command": "pwd"}
16:57:33Z tool-result …
16:58:37Z assistant (tool-use) delegate {"task": "Create a file named hello.txt in the directory …/project/ containing exactly the text 'hi' …"}
16:58:37Z tool-result Delegated to lane 1. It reports back here when it has something; end your turn to wait.
16:58:40Z assistant I've handed this to lane 1 and will tell you when hello.txt is created.
16:58:43Z user      [lane 1 report] done: Created hello.txt containing 'hi' and verified via cat. …
16:58:46Z assistant (tool-use) bash {"command": "cat hello.txt"}
16:58:47Z tool-result hi
16:58:47Z user      [lane 1] run ended (stop) — task: Create a file named hello.txt in the directory …
16:58:49Z assistant Done: hello.txt is in this folder and contains "hi", which I confirmed by reading it.
```

and the lane's own journal (`~/.evo/swarm/20260929T165724-f9fb/lane-1/sessions/…`)
is what it did with the delegation — eight messages, four of them the lane's own,
on **`ark-deepseek-v4.1-flash`** (provider `aiden`, api `anthropic-messages`),
three ending in a tool call and one `:stop`:

```text
16:58:37Z user      Create a file named hello.txt in the directory …/project/ containing exactly the text 'hi' …
16:58:39Z assistant I'll create the file and verify it.
                    (tool-call) write {"path": "…/project/hello.txt", "content": "hi"}
16:58:39Z tool-result Wrote 2 chars to /private/var/folders/…/project/hello.txt
16:58:41Z assistant (tool-call) bash {"command": "cat hello.txt"}
16:58:42Z tool-result hi
16:58:43Z assistant (tool-call) report :done "Created hello.txt containing 'hi' and verified via cat."
                                        :evidence "write wrote 2 chars to .../project/hello.txt; `cat hello.txt` printed `hi`."
                                        :goal "complete"
16:58:43Z tool-result Delivered to the coordinator. You have no goal to complete.
16:58:45Z assistant Created `hello.txt` with `hi`; `cat` confirms the content.
```

Note which model is which: the coordinator ran the user's `claude-opus-5-5` on
`anthropic-oauth`, and the lane ran `ark-deepseek-v4.1-flash` on `aiden` — the
`evo.swarm:in-lanes` model from `~/.evo/swarm.lisp`. That is the §9.4 rule
holding in a real run: the `claude-*` provider is an extension API a quarantined
lane cannot reach, so the lanes chooser marks it unavailable (finding §2 above)
and the lane uses the model the user gave the lanes.

### The §7.3 readout, from the tab's own model

```text
readout at boot:    ctx 0k                                        (nothing has answered /state yet)
readout at the end: claude-opus-5-5 · high · ctx 146k/1000k (15%) · 74% cached
segments:           ["claude-opus-5-5", "high", "ctx 146k/1000k (15%)", "74% cached"]
model:              Some("claude-opus-5-5")    provider: Some("anthropic-oauth")
```

`claude-opus-5-5` with thinking `high` is the user's own `~/.evo/swarm.lisp`
(`:model`, `:thinking`); nothing in the proof passed a model, and the launch's
`LaunchPlan` only carried `workers: Some(1)`. The 146k context is the session's
own injected context plus the run — the same snapshot the collapsed row stands
for, counted by the provider all the same.

### The four frames

All four are the app's own theme (light — the system appearance), 1600×1000
**points** rendered at 1× (the window's own size), saved as ≤192-colour PNGs —
0.15 MB together (the injected context used to be the bulk of the pixels):

```sh
magick real-0N.png -background white -alpha remove -alpha off -colors 192 PNG8:real-0N.png
```

| capture | what it shows |
|---|---|
| `docs/screens/real/real-02-lane-working.png` | **The delegation is out and a lane is working** (16:58:37Z). The center column shows turn 1 — the prompt — with the coordinator's `bash ● ok` and `delegate ● ok` rows, the `Retrying provider (1/4)` line the run met on the way, and the run still streaming; the left column reads `1 lane · 1 busy`, `main ●` mid-turn and lane 1 `●` with the task it was given; the composer is in its running state (`Stop`). This is M1's picture: the coordinator's transcript renders the delegation while the lane works. |
| `docs/screens/real/real-01-mid-stream.png` | **The coordinator's first reply, mid-stream** (16:58:40Z). The same rows plus the reply itself: the assistant row was still streaming when the frame was taken (71 characters of it), under the composer's `Stop` and a readout that has moved with the run (`ctx 146k/1000k`) — §2.8's streaming path, on a real provider, in the real transcript view. |
| `docs/screens/real/real-03-lane-transcript.png` | **Lane 1 selected, its own transcript** (16:58:42Z). The center column is the lane's — its task as the header and as its first turn, its own text ("I'll create the file and verify it."), and its `write ● ok` tool row — while the left column keeps following the swarm (`main ○ idle`, lane 1 `● 1s`) and the composer keeps the coordinator's readout and stays disabled: lanes are watched, never typed to (§14.4). |
| `docs/screens/real/real-04-follow-up.png` | **The final state** (16:58:55Z): the report has been fed back and answered, and the lane's run-end with it. Turn 1 the prompt with `bash ● ok`, `delegate ● ok` and "I've handed this to lane 1 and will tell you when hello.txt is created."; turn 2 the relayed `[lane 1 report]` with its evidence and the coordinator's own `bash ● ok` (it ran `cat hello.txt` itself); turn 3 `[lane 1] run ended (stop)` and "Done: hello.txt is in this folder and contains \"hi\", which I confirmed by reading it."; both rows `○ idle`, `1 lane · 0 busy`, and the composer back to `Send`. |

### The work, and the way the run ends

```text
hello.txt in the project folder:  true
hello.txt content:                "hi"        (2 bytes, no trailing newline)
app.log:  16:58:57Z quitting: stopping every tab
          16:58:57Z saved …/app/app.json: 1 tab(s), 1 of them resumable
          16:58:59Z shutdown: all 1 tab(s) exited
[cleanup] swarm process(es) still alive after the quit: 0 (quiet: true)
```

The quit is the app's own: `evo_desktop::begin_quit` takes the tab's engine out of
the workspace, runs §3's ladder on its own thread and writes `app.json` — the log
lines above are that thread's. The process check looks for what this run started
**twice**: by the tab directory (`--token-file <root>/tabs/<id>/…`, which is the
swarm and everything it forked) and by the project folder, which no unrelated
process names. Both said zero, ~4 s after the quit began.

### Notes on taking these frames

- **The lane is faster than the coordinator's thinking.** With the flash lane the
  whole delegation — write, verify, report, run-end — took 8 s (16:58:37 →
  16:58:45), while the coordinator's first text reached the screen at 16:58:40 and
  its follow-up turns ran to 16:58:49. So the delegation can be out and the lane
  working *before* the coordinator has written anything: the (a) and (b) waits have
  to run **together**, in one loop, or the frame of the lane working arrives after
  the lane has already stopped. (Taken one at a time and the lane's dot is missed:
  the first three attempts at this proof, on an earlier tree, did exactly that, and
  the (b) wait found `0 busy, lane 1 ○ idle` every time.)
- **The injected context is a row of the window's, and it is private.** What the
  session's `/transcript` marks with `meta.key` is drawn as one collapsed line and
  nothing else (`e8ecd5d`), but it is *still in the message list*: it is a
  user-role message in the journal, and it is what a proof prints if it dumps the
  rows verbatim. This proof's first set of pictures were taken before that row
  existed, and every one of them showed the user's memory snapshot on screen; the
  frames here were taken again after it. (A run's log still carries the row's text
  — the example prints the rows — so a log is not a file to paste or commit.)
- **A real run can meet a provider hiccup, and the picture will say so.** This run
  retried once (`Retrying provider (1/4) in 1.6 s — SSL-ERROR-SYSCALL: …`, a
  transient TLS failure) and the coordinator's first word took 76 s to arrive
  instead of the few seconds the earlier run took; the row is in the first two
  frames, and the transcript loses it on the next resync (it is an `output` line,
  which `/transcript` does not carry — the same reason a lane's "down" reason is
  remembered separately).
- **The lane's dot comes from the lane list, not from the lane's own model.** A
  lane is only *watched* while it is the one shown (§9.3), so
  `TabModel::lane_model(1).activity()` reads `Idle` while the lane works; what the
  left column draws — and what this proof waits on — is
  `TabModel::lanes().lane(1).is_busy()`, fed by `/lanes` and the coordinator
  stream's `lane-state` events. The proof records the sequence it saw:
  `["◌ starting", "○ idle", "● working"]`.
- **A `report` row does not survive a resync.** The report arrives as a
  `RowKind::Report` on the event stream, but `settled` rebuilds the coordinator's
  rows from `/transcript`, where the same news is the user message the swarm fed
  back (`[lane 1 report] done: …`). A proof that waits only for `RowKind::Report`
  waits forever once a turn has settled; both shapes are the report.
- **A real provider does not always stream.** This reply did (the frame caught it
  at 71 characters, still streaming), but the same prompt on other runs arrived in
  single deltas, which gives the UI nothing to render mid-word — so "mid-stream" is
  a frame the provider hands a proof when it streams, not one it can be forced to
  produce.
- **M1's "idle" is not "finished".** The coordinator went idle at 16:58:40 with the
  lane still working, and again at 16:58:47 with the lane's run-end still to come;
  the final frame waits for the report *and* a still transcript, per R4.
- **Nothing private in the frames, checked two ways.** `tesseract` over each of the
  four pictures finds no memory marker (`mem-`, `Asia/Singapore`, `OpenViking`,
  `192.168`, `dybench`, `guardrails`, `api_key`, `Bearer`, `sk-`, `/Users/`), and
  the only non-ASCII text any of them shows is the UI's own glyphs, this proof's
  prompt, the collapsed `Context · global memory` line and the temp folder's path.

---

## Findings

### R1 — `GET /registry` is 500 on the real config, and the error body is a secret

**Evidence.** Real run: `route /registry FAILED: http 500: The value "Bearer
<redacted>" is not of type LIST` (the value in the body is the user's own MCP
authorization header as written in `~/.evo/init.lisp`). Same for the catalog
probe (above) and for a bare `curl` with a valid token, on both `evo-swarm` and
`evo-agent`; an invalid token still gets a clean 401, so the 500 is in the
authenticated handler.

**Minimal repro** (no user data, no model tokens), `EVO_HOME` = a temp dir whose
`init.lisp` is one line:

```lisp
;; 500 — dotted pair, which is the form the docs and the user's file use
(evo:set-setting :mcp-servers (list (list :name "x" :url "https://example.invalid/mcp"
                                          :headers (list (cons "Authorization" "Bearer TESTVALUE")))))
;; 200 — the same header as a 2-element list
(evo:set-setting :mcp-servers (list (list :name "x" :url "https://example.invalid/mcp"
                                          :headers (list (list "Authorization" "Bearer TESTVALUE")))))
```

```text
dotted: /registry -> 500 | {"ok":false,"error":"The value\n  \"Bearer TESTVALUE\"\nis not of type\n  LIST"}
pair:   /registry -> 200 | {"models":[],"providers":[…],"apis":["anthropic-me…
```

**Cause.** `src/serve/json.lisp`'s `sexpr->json-value`: a dotted pair is not a
plist (`plist-p` requires keyword keys) and is not a vector, so it falls into
`((listp value) (map 'vector #'sexpr->json-value value))` — and `map` over the
dotted list `("Authorization" . "Bearer …")` is a type error, which names the
non-list tail, i.e. the credential. `/registry` serialises the whole
`*settings*` plist, so any setting that holds an alist of dotted pairs (the
documented `:headers` form of `extensions/500-mcp.lisp`) kills the route.
`event->json` catches encoding errors; `answer-read`/`handle-registry` does not.

**Impact on us.** `tab_engine::catalog::learn` (empty tab, §9.4) and
`tab_engine`'s post-boot registry refresh (`if let Ok(registry) =
client.registry()`) both lose the whole model/provider/API picture on a real
machine; the empty tab shows the raw 500 body as its caption, and anything that
displayed a model list would be empty. `swarm_client` is not at fault — the
server is.

**And we would print the secret.** `app/src/startup.rs:145` logs
`catalog probe failed: {message}` and passes the same string to
`launcher::set_catalog_error`, which `workspace/src/empty_tab.rs:417` renders as
the caption under the lanes chooser (danger colour at `:513`, and the same text
as the tooltip). With
the real config that caption is the user's MCP bearer token, on screen and in
the app log. Not a spec violation, but worth fixing on our side: redact
`Bearer`/`Basic`/`token=` runs in whatever we log or show.

**Fixed on our side.** `swarm_client::redact` / `redact_json`
(`crates/swarm_client/src/redact.rs`) masks an auth scheme's value, a named
key's value (`authorization:`, `x-api-key`, `api_key=`, `token=`, `apikey`, …),
`sk-…` keys and, as a backstop, a marker-free run of 32+ characters that mixes
cases and digits over at least 17 distinct bytes — while leaving prose, paths,
URLs, session ids and hex digests readable. It runs where server text enters the
crate, so everything that reads it is safe without changes elsewhere:

* `StatusError::from_reply` (`crates/swarm_client/src/error.rs`) — the refusal's
  `message`, its whole `raw` body, and the `Envelope` parsed out of it. That one
  constructor is every HTTP error path: `HttpResponse::error`, `open_sse`'s
  refusal, `Client::read` and `Client::envelope`;
* `Error`'s `Display`, which is what the app logs
  (`crates/app/src/startup.rs:145`) and shows as the empty tab's caption
  (`crates/workspace/src/empty_tab.rs:417`, danger colour at `:513`);
* `BootFailure.log_tail` and `log_tail()` (`crates/swarm_client/src/server.rs`),
  i.e. what `tab_engine` puts into `Update::BootFailed`
  (`crates/tab_engine/src/engine.rs:427-428`) and `crates/workspace/src/tab_page.rs:38`
  renders.

Verified: `cargo test -p swarm_client --lib` — 23 tests, of which seven are the
redactor's own (the real 500 body shape with its token replaced by a squib of
the same shape, a JSON body's strings, prose like "a basic idea" and "token: the
file", paths, URLs, a hex digest, a session id, two credentials in one line),
plus one that a refusal is redacted before it is stored (`error.rs`), one that a
non-JSON body is too, and one that a log tail is redacted on the way out
(`server.rs`). `cargo test -p swarm_client` (the e2e suite included) passes.
End-to-end, with `real_catalog` printing the *raw* `CatalogUpdate::Failed.message`
against the real `HOME`, the output is still
`http 500: The value "Bearer <redacted>" is not of type LIST` — the masking is in
the client, not in the example.

**Server text that is still unredacted**, and why: what the *session itself*
prints is not a refusal quoting configuration but the thing the user asked for,
and masking it would mangle real output. Named for whoever owns it:

* `crates/session/src/model.rs:228` — an assistant message's `error_message`
  becomes a row's error, and at `:606-608` the text `Run failed: {error}`;
* `crates/session/src/model.rs:611` — an unrecognised `run-end` outcome becomes
  `Run ended: {other}`;
* `crates/session/src/model.rs:385-397` — `output` events become dim rows (the
  `error` style included), and tool results (`:339`) become tool rows. All of it
  is server text rendered by `transcript`.

### R2 — `EVO_HOME` without a trailing separator breaks the claude-oauth extension

`~/.evo/extensions/020-claude-oauth-provider.lisp:216`:

```lisp
(merge-pathnames "claude-oauth/" (or (uiop:getenv "EVO_HOME") (merge-pathnames ".evo/" (user-homedir-pathname))))
```

`merge-pathnames` keeps the *name* of a directory path written without a
separator:

```text
(merge-pathnames "claude-oauth/" "/tmp/evo-home-catalog-fixed")  => #P"/tmp/claude-oauth/evo-home-catalog-fixed"
(merge-pathnames "claude-oauth/" "/tmp/evo-home-catalog-fixed/") => #P"/tmp/evo-home-catalog-fixed/claude-oauth/"
```

So with `EVO_HOME=/tmp/x` the token file is not found — `/claude-oauth:status`
answers `stored token file: none`, `has_api_key` is null, the five `claude-*`
models never register — while `EVO_HOME=/tmp/x/` finds it and registers all ten.
The real `HOME` is unaffected (the `user-homedir-pathname` fallback has the
separator). This bites any probe or harness that points `EVO_HOME` at a temp
directory, including `catalog::learn_with`'s own `EVO_HOME` switch and our
`tab_engine`/`swarm_client` tests that set it (they use stub providers, so it
is invisible there today). Cheap for us to respect: pass `EVO_HOME` with a
trailing `/`.

**Resolved for us.** Nothing in the app constructs `EVO_HOME`: neither
`crates/app` nor `crates/workspace` mentions it, so the app simply inherits
whatever the environment has. The two places *we* do make one now carry the
separator — `scripts/stub_home.sh` exports `EVO_HOME="$home/.evo/"` (both the
`run` environment and `print_exports`, `:178`/`:191`) — and
`store::history::sessions_dir` joins the value with `Path::join`, which is
path-aware: `/tmp/x` and `/tmp/x/` both give `/tmp/x/sessions` (tested in
`crates/store/src/history.rs`). The test harness that still passes it bare
(`crates/swarm_client/src/harness.rs:275`, `crates/swarm_client/tests/swarm_e2e.rs`)
points at homes with no extensions directory, so that extension never loads
there. The evo-side fix is `extensions/020-claude-oauth-provider.lisp:216`; the
coordinator reports it to the user, and it is not ours to change.

### R3 — `EVO_SESSIONS_DIR` is the app's history directory too

`store::history::sessions_dir()` prefers `$EVO_SESSIONS_DIR` over
`~/.evo/sessions` (deliberate, for tests). Launched from inside an evo session —
a lane's shell, an IDE integration, `scripts/stub_home.sh` — the desktop app
would scan that variable's directory instead of the user's sessions: the first
attempt at §3 above got 0 files / 0 rows from
`~/.evo/swarm/20260929T093455-90a1/lane-3/sessions/`. §9.5 says
`~/.evo/sessions`. The app already scrubs `EVO_SESSIONS_DIR` for the servers it
*spawns*; its own scan is the one that inherits it.

**Fixed.** `store::history::sessions_dir()` now ignores `EVO_SESSIONS_DIR` and
returns `<evo home>/sessions` — `$EVO_HOME` when a process sets one, `~/.evo`
otherwise (`crates/store/src/history.rs`). Two tests cover it: the home rule
with and without a trailing separator and with the variable empty, and that a
lane's `EVO_SESSIONS_DIR` does not steer the scan at all. Verified end-to-end:
run inside a swarm lane with
`EVO_SESSIONS_DIR=/Users/bytedance/.evo/swarm/20260929T093455-90a1/lane-3/sessions/`
in the environment, `real_history` reports
`sessions_dir: /Users/bytedance/.evo/sessions` and the same 16 rows as the unset
run, so `app/src/startup.rs:68`'s scan follows the swarms rather than the lane.

### R4 — notes, not defects

- **`settled` ≠ the lane is done** (§1): a delegating coordinator settles in
  seconds while lane 1 works on, so an M0 that stops at `settled` reads a
  half-finished lane transcript. The M0 proof waits for the lane; the UI is
  already right for this (the lane column draws `working`, and `/lanes.state`
  is what the glyph reads) — but a "run finished" affordance driven by `settled`
  would be telling the user something untrue.
- **the coordinator's follow-up turn is aborted by an early shutdown** (§1): the
  lane's report starts a new coordinator turn (12:12:07Z) and the proof's
  shutdown (12:12:13Z) ended it `:stop-reason :aborted` with empty content. Our
  session model turns a non-clean `run-end` into a visible
  `RowKind::RunOutcome` row, so an app that shut its swarm down at the first
  `settled` would show the user an "aborted" row instead of the coordinator's
  summary. The app itself only shuts a swarm down when its tab closes, so this
  is a note for the M0 proof (wait for the *second* `settled`), not a defect.
- **the spawned pid is not the serving pid** (§1): `/health.pid` (8476) ≠
  `Server::pid()` (8475); the shutdown ladder's group signal is what makes the
  difference harmless. Worth remembering in any code that decides "is my server
  alive" from the spawned pid rather than the child handle.
- **the readout's context number is the TUI's fold, not `/state`'s** (§1):
  151 136 vs 151 197 here. Per §7.3; recorded so nobody "fixes" it later.

---

## What this proves, and what it does not

Proven against the installed binaries and the real configuration: spawn +
readiness + `/health` (§3); the SSE stream with a real provider, counted by
event kind, text arriving as `text-delta`s (§5); `POST /prompt` → `queued`
(§4); `GET /state`, `/transcript`, `/lanes`, `/lanes/1/transcript` (§4); the
coordinator relaying a lane's report and run-end onto its own stream; the §7.3
readout line computed by `session::Readout` from that run; the §9.4 catalog
probe and the lanes chooser's availability rule, with real models; the §9.5
scan and row formatting over 697 real journals; `POST /shutdown` ending the
whole process group with exit 0 and nothing alive afterwards (§3).

Not proven here: a multi-lane swarm (this ran `--workers 1` on purpose),
`--resume`/history *launch*, the tab directory's own token/lock handling (§6), the
parts of the UI that need a manual hand (§7.3's tooltips, §9.5's *open at the last
quit* flow), and anything about a machine whose `init.lisp` does not hold the
dotted-pair `:headers` that R1 needs to fail.

§4 is the same backend **through the GUI**, so it adds what the examples above
cannot: the assembled window rendering a real run end to end — the empty tab's own
`Launch` event starting a real swarm, a prompt typed into the real composer, the
coordinator's reply streaming into the real transcript view while the left column
draws the lane's dot from `/lanes`, lane 1's own transcript on selection, and the
report relayed back and answered in the coordinator's transcript. What it does not
add: any of the UI the other lanes' work covers, a multi-lane swarm, and the one
picture a real provider refuses to give on demand — see the notes in §4.
