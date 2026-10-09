# Screens

A focused gallery of the app in light and dark themes. The README uses
`06-report-light.png`: a delegation followed by the lane's report.

## Capture

The example runs the app's real window and real `evo-swarm` / `evo-agent` binaries
against a scripted demo model. Prompts and replies are fixture text, not results
from a production model. The delegate, report and shell tool calls really run.

With a built `evo-agent` checkout beside this one:

```sh
EVO_SWARM_BIN="$PWD/../evo-agent/build/evo-swarm" \
EVO_AGENT_BIN="$PWD/../evo-agent/build/evo-agent" \
  cargo run -p evo-desktop --example screens -- --capture /tmp/evo-screens

# Inspect the captures before replacing the gallery.
for image in /tmp/evo-screens/*.png; do
  sips --resampleWidth 1440 "$image" \
    --out "docs/screens/$(basename "$image")"
done
```

Set `EVO_AGENT_REPO` if the source checkout is elsewhere, and adjust the binary
paths as needed. Captures are normalized to 1440 × 900.

`crates/app/examples/screens.rs` drives the launch controls, composer and tool
rows. `screens_model.py` adapts the upstream test model to natural prompts and
readable replies. The fixture copies the relevant public transcript sources into
its sample project so the shell listing and report links refer to real files.

The run uses a disposable `HOME`, never the user's real `~/.evo`. It stops only
its own servers through the app and fixture shutdown paths. To capture history,
it closes the session tab first: open sessions are deliberately excluded from
the resume list.

## Gallery

Each name has a `-light.png` and `-dark.png` version.

| Capture | Shows |
|---|---|
| `01-empty` | The launch page with model, effort, worker count and folder controls. |
| `01b-model-menu` | The model menu, including the catalog's context and effort details. |
| `02-lanes-working` | A real delegation, the worker's task and busy state, and Stop swarm. |
| `05-queued` | An expanded shell call with its source listing, a streaming answer and a queued prompt. |
| `06-report` | A completed lane report with Markdown, source links and next steps. |
| `07-history` | The completed session available to resume from the launch page. |

The gallery keeps distinct everyday states. Redundant transcript/tab captures
and deliberately broken launches belong in tests, not in the product gallery.
