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

  Evidence (stub world, a running tab then `+` twice): `tab#0` 88..328, `tab#1` (New
  Swarm) 328..568, no `tab#2`, `tab-add` at x 576 = 568 + 8, y 8, 30×30 — the design's
  `.tab-add` measured on the live page is 808 + 8 in window terms, 30×30, 4px bottom
  margin. Strip crops with the first tab and the `+` hovered, light and dark, read the
  design's surfaces (strip #E6E0D5/#1F1F1F, active #F6F2EA/#141414, hover #EFE9DF/#2A2A2A).
  Follow-up (61c02ed): first tab at x 88 (design 88) with its corner whole, the shown
  tab's −0.5px hairline (pixel 216,211,200 vs the page's 216,211,201), × ink follows
  hover. Not drawn: `.tab-row`'s 8px padding after the `+` (nothing follows it).

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

- [x] C1 The model drawer lists no models and has no effort rail: `Composer::set_catalog`
  is never called by the workspace, so `models`/`levels` stay empty — nothing to select.
- [x] C2 The composer box is filled with the kit's `input_background()` (= the page `--bg`
  in light, a grey mix in dark) instead of the design's `--input`; chips are
  `fg 6% over --input`, so they vanish into the box in both themes.
- [ ] C3 No `N% cached` chip for a lane: lanes boot `--no-userspace`, so the user
  extension `340-cache-stats` never runs there and their `state.segments` carry no cache
  segment. The design shows it for every agent (`0% cached` on an idle lane).
- [x] C4 Selected text in the input is `muted` on `--input` — indistinguishable. Use the
  design theme's own selection: light `#0069CC40` (rl-vscode-dimmed
  `editor.selectionBackground`), dark a primary-blue selection of the same weight.
- [x] C5 The effort rail's move was never drawn: the widget was rebuilt every render with
  state of its own, so the thumb and the fill jumped from one level to the next (gpui's
  `with_animation` cannot carry it either — one id plays once, and it would need the "from"
  known at build time, which is the thing a transition is for). The motion is the caller's
  now (`widgets::effort::Motion`, one per rail), a change is drawn over the design's 140ms
  `cubic-bezier(.3,.7,.3,1)`, a change made mid-move starts from what is being drawn, and
  the press and the two rings fade over the design's 120ms `ease`.
  Evidence (real world, the coordinator rail x 677..841, thumb centre per frame against the
  same curve): 19ms 728.5 (731.5), 37ms 776 (779.5), 54ms 808 (809.0), 73ms 825.5 (826.4),
  90ms 834 (834.4), 108ms 838.5 (838.7), 143ms 841 (841.0) — every frame within 3.5px of
  the design's curve and none snapped; turning back 70ms into a move leaves the thumb at
  0.101 of the rail, the design's 0.899 of the way back, so it carries on from the drawn
  position rather than restarting. `/tmp/lane3c/real/midmove-light.png`, a crop 34ms in.

## Transcript column and the agent header band

Sources: `Rows.css`, `styles.css`, `Workspace.css`, `Transcript.tsx`, `transcript-data.tsx`.
Evidence: `cargo test -p transcript -p workspace` (39 + 71 + 2 + 19, 0 failed) and the stub
world through the probe (`/tmp/lane8/`, `run.sh` + a script).

- [x] R1 The space between rows is CSS margin collapse, not a fixed gap: each group carries
  the margin the design gives it (`User 0`, `Assistant`/`Tool 10px`, `Report 14px`,
  `Quiet`/`Context`/`Divider` `BLOCK_GAP`) and two neighbours get the larger of the two.
  Evidence: `the_room_between_rows_is_the_designs` (10px and 14px between rows, the first
  row at y 16 = `.transcript-scroll{padding:var(--inset) 0}`).
- [x] R2 The caption over a row is `.cap{font-size:11.5px;line-height:1.5;margin-bottom:4px}`
  — 17.25px line box, 4px below. Evidence: `the_report_card_is_the_designs`.
- [x] R3 The tool card: `.tc-head{height:34px}` inside the card's own 1px `--border`
  (= 36px), the head's pointer is the design's `cursor:default` (it was `cursor_pointer`),
  and the body starts 42px in and 11px below the head's foot
  (`.tc-body{padding:10px 12px 12px 42px;border-top:1px solid var(--rule-soft)}`).
  Evidence: `the_tool_card_is_the_designs`, `/tmp/lane8/43-tool-open-light.png`.
- [x] R4 A tool payload's key/value rows are one flex column with 3px between fields (was
  `gap_1()`), 19px rows, a 72px key column and 12px between key and value — the design's
  `.tc-kv{gap:3px}` grid. Evidence: `a_tool_payloads_rows_sit_three_pixels_apart`.
- [x] R5 `.thinking{margin:10px 0;padding-left:8px;border-left:2px solid var(--border)}` with
  `.thinking-label{font-size:12px}` and `.thinking-text{font-size:14px;font-style:italic}`,
  drawn **above** the message it belongs to (it was below it, in a 12px paragraph), the 2px
  rule outside the 8px padding, and 10px from the message. Evidence:
  `thinking_is_drawn_above_its_message`, `/tmp/lane8/46-think-off-{light,dark}.png`,
  `/tmp/lane8/47-think-on-{light,dark}.png`.
- [x] R6 The report card is the design's `.rp`: a 36px head carrying the lane's done pill on
  the success colour, and `.rp-row` rows 39px tall whose bodies are markdown documents
  (synced per field, like the assistant's message). Evidence: `the_report_card_is_the_designs`,
  `a_reports_field_is_rendered_as_markdown`, `/tmp/lane8/40-report-{light,dark}.png`.
- [x] R7 `.user-row{padding:8px 12px;border-left:3px solid var(--primary);font-size:14px}` on
  the page's 1.5 line box: a 37px row, 21px lines. Evidence: `the_user_row_is_the_designs`.
- [x] R8 A markdown table is the design's `.measure` table: `--rule` frame with radius 8,
  `--input` body, 13.5px cells at `7px 12px`, a `--rule-soft` rule to the right, a `--sidebar`
  header row in 12.5px `--muted-fg`; a fence is `--muted`, radius 6, `10px 12px`; lists are
  indented 22px. Evidence: `/tmp/lane8/45-md-table-light.png`, `/tmp/lane8/44-md-bottom-light.png`,
  `/tmp/lane8/50-jump-light.png`.
- [x] R9 `↓ Jump to latest` is `.ws-main .jump`: 28px tall, `0 12px`, radius 999,
  `background:var(--input)`, `color:var(--fg)`, `border:1px solid var(--border)`, 12px,
  `gap:4px`, `box-shadow:0 2px 8px rgba(60,40,10,.12)`, `cursor:default`, hover `--sidebar`,
  centred 12px above the foot, and shown only past `JUMP_THRESHOLD` 240 — its background was
  the kit theme's own `input` token (`#DDD7CB`/`#262626`, i.e. the widget border) and is now
  the design's `#FBF8F2`/`#171717`, the same surface as the composer box. The fade and the
  6px lift run on the design's `ease` (`cubic-bezier(.25,.1,.25,1)`, it was gpui's linear).
  Evidence: `a_wheel_takes_the_list_off_its_tail_and_the_pill_appears`,
  `/tmp/lane8/70-pill-scrolling-{light,dark}.png` (2x fill 251,248,242 / 23,23,23),
  `/tmp/lane8/71-pill-settled-{light,dark}.png` (y 734 = the transcript's foot − 12 − 28).
- [x] R10 The transcript's scrollbar (the reader's decision, not the design's): shown only
  while scrolling. The bar's overlay is on the `transcript` host — the box that does not
  scroll — driven by the scroller's own handle, so the thumb is not carried by the rows'
  translation. Evidence: `/tmp/lane8/70-pill-scrolling-light.png` (thumb rows 2x 691..1136)
  against `/tmp/lane8/71-pill-settled-light.png` (no thumb, 1.2s later).
- [x] R11 The disclosure chevron is the design's caret: `m9 6 6 6-6 6` at 12px in a 16px
  box, `stroke-width` 1.1 at that scale, `--muted-fg`, a quarter turn in 120ms on
  `cubic-bezier(.25,.1,.25,1)` — the numbers were already the design's (`rows.rs`).

## Agent header band (`.ws-head`, `Workspace.css:15-17,33-34`, `styles.css` `.ghost`)

- [x] H1 The band is `--header` 38px (37px + its own 1px `--border` rule), `gap:8px`,
  `padding:0 var(--inset)`, on `--sidebar`, over a 14px base. Evidence: `47-think-on-light.png`
  (band y 84..157 at 2x, rule 158..159, the first row 16px below).
- [x] H2 `.ws-agent-name{font-weight:500}` at the band's 14px, and
  `.ws-agent-task{flex:1;…;color:var(--muted-fg);font-size:13px}` elided to one line.
  Evidence: the same crop, ink height 10.5px (name, 14px) and 9.5px (task, 13px).
- [x] H3 `Show thinking` / `Hide thinking` is the design's `.ghost`: a bare 12px label in
  `--muted-fg`, no border, no surface, no padding, no hover — it was a `Button::ghost()`
  `.xsmall()` drawn in `--fg` with the kit's hover pill. Evidence: `46-think-off-light.png`
  (ink 96,96,96 = `--muted-fg`, ink height 11.5px, no hover surface when the pointer is away)
  and `c-toggle.png` from the same run.
- [x] H4 The toggle's cursor is the app's pointer rather than the design's default: `.ghost`
  has no cursor rule, and this control has no hover of its own to read as clickable.

### Not closed

- [ ] N1 A markdown table's **row** rules come from the node style's border token, not the
  design's `--rule` (frame and cell rules are ours and do match). `TextViewStyle` has no
  border field; painting over the row rule from the cell would move the cell rules off
  `--rule-soft` and double the table's bottom edge. Written up in `docs/api-gaps.md`.
- [ ] N2 The kit's scrollbar thumb is longer than the content warrants and travels less than
  the full track (223px thumb and 333px of travel for a 694px pane holding 3466px), so it is
  drawn by the kit's own arithmetic rather than ours.
- [ ] N3 `crates/workspace/src/tab_page.rs`'s `render_failed` takes 8 arguments (clippy
  `too_many_arguments`); it is another lane's signature (the `window` it now needs), left alone.
