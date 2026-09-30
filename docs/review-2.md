# Review 2 — against design/doc28, widget by widget (2026-09-30)

Source of truth: `~/coding/evo/design/doc28` (React/CSS) and its live page
`https://vibe-wonders.up.railway.app/r/EmSTIJulTq3jEc-Zm3ucEs_R` (vibe-wonders doc 28;
`vibe-wonders__see doc=28` renders any state with a pointer script).
Evidence is taken with `cargo run -p evo-desktop --example probe -- --world real|stub SCRIPT`
(see the file's header for the step language).

Every item below is a defect until its fix is proven by a probe picture (both themes) or a
test, and is ticked here with that evidence.

## Tab strip

- [x] T1 `+` sits at the strip's right edge; the design puts it right after the last tab
  (`.tab-add` is a sibling in `.tab-row`, `margin-left:8px`, `flex:0 0 30px`).
- [x] T2 Every `+` / ⌘T opens another empty tab. There must be at most one New Swarm tab:
  a second `+` selects (and focuses) the existing one.
- [x] T3 An unused tab is titled `New tab`; it must read `New Swarm` (doc.tsx tabs).

  Evidence (stub world, a running tab then `+` twice): `tab#0` 92..332, `tab#1` (New
  Swarm) 332..572, no `tab#2`, `tab-add` at x 580 = 572 + 8, y 8, 30×30 — the design's
  `.tab-add` measured on the live page is 808 + 8 in window terms, 30×30, 4px bottom
  margin. Strip crops with the first tab and the `+` hovered, light and dark, read the
  design's surfaces (strip #E6E0D5/#1F1F1F, active #F6F2EA/#141414, hover #EFE9DF/#2A2A2A).
  Remaining, accepted: the first tab starts at window x 92 (design 88) because the clip
  box keeps the first tab's outward corner; the active tab's −0.5px hairline shadow is
  not drawn.

## New Swarm page

- [ ] N1 Defaults are wrong. Root causes:
  - the app asks `check` with its own guesses as flags (`--workers 6`, `--model <first
    ready>` …) before evo answered, so `check` echoes the guess back: real HOME shows
    `super_relay · seed-evolving` / 6 workers where evo resolves `claude-opus-5-5`, 9
    workers, lanes `ark-deepseek-v4.1-flash@aiden`. Only a control a person changed may
    become a flag (check and launch alike).
  - `evo-swarm check`/`catalog` judge a provider ready only by a literal key or an env var;
    the OAuth extension keeps its token in `~/.evo/claude-oauth/token.sexp`, so every
    anthropic_oauth model reads "no API key: set CLAUDE_OAUTH_ACCESS_TOKEN" — false.
  - `check`/`catalog` never evaluate swarm.lisp's `(evo.swarm:in-lanes …)`, so the lanes'
    model is reported as the coordinator's and "comes from an extension: load that
    extension in the lanes" — false: swarm.lisp loads it and sets the lanes' model.
  - a lane applies `--lane-model`/`--lane-thinking` *before* the in-lanes forms, so an
    explicit choice in the Workers card would be silently overridden.
- [ ] N2 Count box: the digits sit low-left (Input keeps its own padding and height inside
  a 28px box); design: centred both ways, 12px. Typing into it must work (select-all,
  type, backspace), steppers clamp 1–64.
- [ ] N3 Model field: summary text is not vertically centred in the 34px box; the design
  shows `<b>provider</b> · id` (provider weight 500) and a `⌄` chevron at right 11px.
- [ ] N4 The coordinator model field wears the focus ring at rest (first-control autofocus);
  the design shows no ring until the person focuses it.
- [ ] N5 Folder card draws a hint line (“The swarm starts in the folder you pick”) that the
  design's TSX does not render.

## Swarm tab (composer)

- [ ] C1 The model drawer lists no models and has no effort rail: `Composer::set_catalog`
  is never called by the workspace, so `models`/`levels` stay empty — nothing to select.
- [ ] C2 The composer box is filled with the kit's `input_background()` (= the page `--bg`
  in light, a grey mix in dark) instead of the design's `--input`; chips are
  `fg 6% over --input`, so they vanish into the box in both themes.
- [ ] C3 No `N% cached` chip for a lane: lanes boot `--no-userspace`, so the user
  extension `340-cache-stats` never runs there and their `state.segments` carry no cache
  segment. The design shows it for every agent (`0% cached` on an idle lane).
- [ ] C4 Selected text in the input is `muted` on `--input` — indistinguishable. Use the
  design theme's own selection: light `#0069CC40` (rl-vscode-dimmed
  `editor.selectionBackground`), dark a primary-blue selection of the same weight.
