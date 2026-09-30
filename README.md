# evo-desktop

A native macOS app for the evo agent runtime (sibling repo `../evo-agent`): one window whose
title bar is a browser-like tab strip, named after the folder of the tab you are looking at;
one tab per `evo-swarm serve` process — a coordinator agent plus a pool of worker lanes. The
app is a *client*: it drives evo over one loopback protocol (a ready file, a snapshot, one
stream of ops, and `POST /ops` — `CONTRACT.md` at the workspace root) and reimplements none of
it.

![evo-desktop icon](assets/icon/icon-1024.png)

Ten states of the real window — the empty tab, six lanes with one of them working,
a lane's transcript, a tool row opened, a prompt queued, a lane's report, the
history, the two ways a swarm fails to start, and the tab strip itself — are
captured in both themes by `crates/app/examples/screens.rs` and described in
[`docs/screens.md`](docs/screens.md), which is also the recipe for taking them.
The pictures are not kept in the repository: every branch regenerates them, and
they only conflict. Take them into a directory of your own:

```sh
cargo run -p evo-desktop --example screens -- --capture /tmp/screens
```

## Requirements

- macOS 15 or newer.
- A recent stable Rust toolchain. The `store` crate uses `File::try_lock`, so
  1.89 or newer.
- The evo binaries — `/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent`
  by default. Point the app elsewhere under **Settings… (⌘,)**, or by editing
  `binaries` in `~/.evo/desktop/app.json`. If the swarm binary is not one that
  runs, the empty tab says so in its own line and that line opens Settings.
- A model provider evo can reach. No provider? Use the stub home below.

## Build, run, test, bundle

```sh
cargo run -p evo-desktop              # debug build, one window
cargo build --release -p evo-desktop

cargo test                            # every crate
cargo test -p store                   # one crate

scripts/check.sh                      # the gate: fmt --check, clippy, every crate's tests
scripts/check.sh --head               # the same, against HEAD in a worktree of its own

scripts/bundle.sh                     # → dist/evo-desktop.app (ad-hoc signed)
scripts/bundle.sh --no-build          # bundle the binary already built
```

`scripts/check.sh` is the whole gate: `cargo fmt --check` per crate, then
`clippy --workspace --all-targets`, then `cargo test -p <crate>` crate by crate,
one at a time (each crate gets its own target directory, so the steps do not
contend for one lock). It prints a table at the end and keeps every step's log
under `target/check/`. `--head` checks what is committed rather than the working
tree. It starts real servers and swarms, so run it on an idle machine.

`scripts/bundle.sh` writes the icon into the bundle from
`assets/icon/AppIcon.icns`; regenerate it with
`python3 assets/icon/make_icon.py` (needs Pillow and NumPy).

## Try it without a provider

`scripts/stub_home.sh` builds a throwaway `HOME` whose evo home registers a
scripted model (`../evo-agent/tests/stub-messages.py`) — no API key, no
network, and nothing written to your real `~/.evo`:

```sh
scripts/stub_home.sh run -- ./dist/evo-desktop.app/Contents/MacOS/evo-desktop
scripts/stub_home.sh start          # for a long session; prints the exports
scripts/stub_home.sh stop <dir>
```

The model answers `ok: <your text>` unless you script it: `CALL bash {…}` makes
it call that tool, `SLOW ` streams for six seconds, `DELAY2 ` waits. See that
script's docstring for the full vocabulary.

## What it keeps, and where

Everything the app owns lives in `~/.evo/desktop/` (§6 of the spec):

| path | what |
|---|---|
| `app.json` | window bounds, the recorded tab set, the binary paths, the recent sessions, the theme |
| `lock`, `activate.sock` | the single-instance lock (`flock`, dies with the process) and its activation socket |
| `model-cache.json` | the last `catalog --json` body, for the empty tab's choosers |
| `tabs/<id>/ready.json` | where the server publishes its port, URL and bearer token (0600) once it is listening — never logged, never shown |
| `tabs/<id>/swarm.log` | that swarm's stdout and stderr (the log a tab shows when boot fails) |
| `app.log` | the app's own log (RFC 3339 UTC timestamps) |

Nothing else is written anywhere: the app reads no journal, writes no project
file, and asks evo for what it shows — `evo-swarm catalog --json` for the
choosers, `evo-agent sessions --json` for the history list, and one running
swarm for everything a tab page draws. The lanes' model is a launch flag
(`--lane-model`), so `<folder>/.evo/swarm.lisp` stays the project's own.

A launch always opens **one empty tab**, whatever `app.json` said (§14.6): the
tab set is recorded for the app's own bookkeeping, not reopened. The sessions
that were open at the last quit come back at the top of the empty tab's history
wearing an `open at last quit` badge.

## Architecture

- [`docs/architecture.md`](docs/architecture.md) — which crate owns what.
- [`docs/PROMPT.md`](docs/PROMPT.md) — the build spec; §-references in the code
  point here.
- [`docs/usage.md`](docs/usage.md) — how to drive the app: tabs and their
  shortcuts, the model choosers, history, the tab page's three columns, Settings,
  failures, and where the logs are.
- [`docs/screens.md`](docs/screens.md) — the twenty states the capture run takes
  (it does not keep the pictures), and what is real versus scripted in them.

## Status

The tree holds the whole app, and every part of it is wired: the client
(`swarm_client`), the data layout (`store`), the per-agent view model
(`session`), one tab's I/O (`tab_engine`), the UI crates (`transcript`,
`composer`, `agent_list`, `workspace`) and the app shell (`crates/app`: single
instance, remembered bounds, startup loading, quit, the menu bar, About, and the
Settings panel).

What is genuinely not here:

- A command surface (slash commands) and lane *steering*: a person can stop work
  (`run.interrupt`, scope `swarm` or `lane`) and the coordinator is told, but
  redirecting a lane stays the coordinator's job.
- Windows and Linux packaging: the bundle target is macOS.
- Settings covers the two binaries and the theme and nothing else (§13); there
  is no other preference to change.
- The transcript is a reading surface, not a browser: an image reference is
  drawn as `[image: alt]` and nothing is fetched, raw HTML shows as its source,
  and a link opens only `http(s)` or `mailto`.

Two things worth knowing when you run it:

- The app icon lives in the bundle, not in the binary: launch
  `dist/evo-desktop.app` (after `scripts/bundle.sh`) to see it in the Dock.
- The `evo-agent` sources this app is built against are strictly read-only; see
  `docs/PROMPT.md` §10.
