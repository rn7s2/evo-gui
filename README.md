# evo-desktop

A native macOS app for the evo agent runtime (sibling repo `../evo-agent`): one window whose
title bar is a browser-like tab strip, one tab per `evo-swarm serve` process —
a coordinator agent plus a pool of worker lanes. The app is a *client*: it
drives evo over its HTTP API and reimplements none of it.

![evo-desktop icon](assets/icon/icon-1024.png)

## Requirements

- macOS 15 or newer.
- A recent stable Rust toolchain. The `store` crate uses `File::try_lock`, so
  1.89 or newer.
- The evo binaries — `/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent`
  by default. Point the app elsewhere by editing `binaries` in
  `~/.evo/desktop/app.json`.
- A model provider evo can reach. No provider? Use the stub home below.

## Build, run, test, bundle

```sh
cargo run -p evo-desktop              # debug build, one window
cargo build --release -p evo-desktop
cargo test                            # every crate
cargo test -p store                   # one crate

scripts/bundle.sh                     # → dist/evo-desktop.app (ad-hoc signed)
scripts/bundle.sh --no-build          # bundle the binary already built
```

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
| `app.json` | window bounds, the open tabs, the binary paths, the recent sessions, theme |
| `lock`, `activate.sock` | the single-instance lock (`flock`, dies with the process) and its activation socket |
| `model-cache.json` | the last `/registry` catalog, for the empty tab's choosers |
| `probe/` | scratch directory for the catalog probe |
| `tabs/<id>/tab.json` | one tab: folder, resumed session, swarm id, chosen models, workers |
| `tabs/<id>/token` | that swarm's bearer token, written by the server (0600) — never logged, never shown |
| `tabs/<id>/swarm.log` | that swarm's stdout and stderr (the log a tab shows when boot fails) |
| `app.log` | the app's own log |

The one file it writes outside that directory is the managed block at the top
of a project's `<folder>/.evo/swarm.lisp`, which is what gives that project's
lanes their model (§9.6). Everything else it reads — journals under
`~/.evo/sessions`, a swarm's directory under `~/.evo/swarm/<id>` — is
read-only.

## Architecture

- [`docs/architecture.md`](docs/architecture.md) — which crate owns what.
- [`docs/PROMPT.md`](docs/PROMPT.md) — the build spec; §-references in the code
  point here.
- [`docs/usage.md`](docs/usage.md) — how to drive the app: tabs, the model
  choosers, history, the Send/Stop button, lane glyphs, logs.

## Status

The tree holds the whole app: the client (`swarm_client`), the data layout
(`store`), the per-agent view model (`session`), one tab's I/O (`tab_engine`),
the UI crates (`transcript`, `composer`, `agent_list`, `workspace`), and the app
shell (`crates/app`: single instance, remembered bounds, startup loading, quit).
One wiring step is still open:

- `agent_list` exists, is tested, and has its own example, but `workspace` does
  not depend on it yet — the tab page still draws the lane column itself, and
  the swap is the next change. `crates/app/src/quit.rs` carries the tree's only
  `TODO`.

Two things worth knowing when you run it:

- The app icon lives in the bundle, not in the binary: launch
  `dist/evo-desktop.app` (after `scripts/bundle.sh`) to see it in the Dock.
- The `evo-agent` sources this app is built against are strictly read-only; see
  `docs/PROMPT.md` §10.
