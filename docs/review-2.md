# Review 2 — against design/doc28, widget by widget (2026-10-01)

Source of truth: `~/coding/evo/design/doc28` (React/CSS) and its live page
`https://vibe-wonders.up.railway.app/r/EmSTIJulTq3jEc-Zm3ucEs_R` (vibe-wonders doc 28;
`vibe-wonders__see doc=28` renders any state with a pointer script).
Evidence is taken with `cargo run -p evo-desktop --example probe -- --world real|stub SCRIPT`
(see the file's header for the step language).

Every item below is a defect until its fix is proven by a probe picture (both themes) or a
test, and is ticked here with that evidence.

## Closeout scope

The requested source fixes and app-widget interaction review are complete in the working
tree on GUI HEAD `198ee72`, paired with agent source `0c4c1ec`. No new commit or system
installation was made. Final rerun: 525 GUI tests and 9 temporary-prefix runtime proofs,
clippy, formatting and diff checks pass; logs are `/tmp/evo-gui-closeout-{unit,proofs,clippy}.log`.
The earlier installation gate was an extra deployment requirement, not part of the user's
source-fix request. The installed-runtime follow-up below remains explicitly unperformed.
Native Open-button navigation is also unverified; app control dispatch, cancellation,
launch-plan propagation and the injected native completion path are covered separately.
Neither limitation is represented as a passing interaction test.

## Tab strip

- [x] T1 `+` sat at the strip's right edge; it now follows the last tab, as in the design
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

- [x] N1 Defaults were wrong. Root causes, now fixed:
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
- [x] N2 Count box: the digits sat low-left (Input kept its own padding and height inside
  a 28px box); now centred both ways, 12px. Typing, select-all, backspace and steppers
  work, clamping 1–64. The box paints the card's surface so GPUI's focus shadow cannot
  fill its transparent interior; the primary border and external muted ring remain.
- [x] N3 Model field: summary text is vertically centred in the 34px box, with
  `<b>provider</b> · id` and a `⌄` chevron at right 11px.
- [x] N4 The coordinator model field has no focus ring at rest; it appears on interaction.
- [x] N5 The folder hint not rendered by the design has been removed.

  Final controls evidence: `/tmp/probe/final-controls-current/01-defaults-{light,dark}.png`,
  `02-focused`, `03-stepped`, `04-menu`, `05-dismissed`, `06-scroll`, `07-rest` and
  `08-tooltip`. These show actual
  HOME defaults: `anthropic-oauth · claude-opus-5-5`, `aiden · ark-deepseek-v4.1-flash`,
  high/high and 9 workers, no false credential warning. Current agent HEAD `0c4c1ec`
  `check --json` agrees, with `ok:true` and `problems:[]`. Only by-hand choices enter
  `Launcher::plan()`; the probe's `launch` convenience step does not exercise those
  fields, so launch-choice propagation is covered by the workspace tests, not claimed
  from that step. Count ring crops: `/tmp/lane4/ring/count-focus-light.png` and
  `count-focus-dark-dark.png`; scene regression test
  `the_count_box_keeps_the_cards_surface_when_it_takes_the_keyboard` covers fill/border.
  The launch control is **Select folder…**, not a separate Start button. The probe
  cannot answer the native dialog and refuses that click. Workspace tests substitute
  only the folder answer, click the actual control, and assert untouched defaults or
  the exact manually changed plan; `a_new_swarm_gets_the_flags_the_choosers_chose`
  checks plan-to-argv propagation. The native dialog itself is not covered by this run.
  A read-only macOS capability check returned false for `AXIsProcessTrusted()`,
  `CGPreflightPostEventAccess()` and `CGPreflightScreenCaptureAccess()`; the
  System Events query also timed out. No permissions were requested or changed.
  Additional real-click evidence for count, both models and both effort rails is in
  `/tmp/lane4/launch-final/{controls,workers-menu}/`.

  Additional native-process integration evidence: the temporary harness at
  `/tmp/lane4/picker-harness` opens the real `FolderPicker::Dialog` on a shown app
  window, without AX/CG automation or permission changes. Its native `cancel:` action
  dismisses the panel, leaving the tab Empty with no engine launch. For acceptance,
  it **injects** `NSModalResponseOK` by ending the sheet: this exercises rfd's completion
  and path extraction, not the Open button or AppKit's validation. Both model choices
  are explicitly `stub-b`, both effort rails are `max`, and count is 8 after in-process
  GUI clicks. The exact plan, sole swarm argv, Running tab, process cwd and lane-start
  records agree. Coordinator checked `out/{harness.log,argv.log,serve-argv.json,exit}`;
  the argv log contains exactly lanes 1–8. These supplement, not replace, the manual
  native navigation/Open-button check. `NSSavePanel::ok:` raised a not-implemented
  exception on this OS, so that route was not used as acceptance evidence.
  The workers field already displayed `stub-b` before the menu scan; the exact
  final explicit plan and argv are proven, not a clean before/after change of that
  field. Native AppKit quit bypassed the Rust fixture's post-run Drop, leaving stub
  listeners behind despite the initial cleanup report. Coordinator attributed them
  through their open fixture `home/stub.log` descriptors and terminated only those
  confirmed test PIDs; listener/process checks then passed. The temporary driver's
  subsequent cleanup changes have not been exercised.

## Swarm tab (composer)

- [x] C1 The model drawer was empty because the workspace never called
  `Composer::set_catalog`. It now supplies the catalog and effort levels; model
  selection and effort changes are exercised in
  `/tmp/probe/final-integrated/02-drawer-{light,dark}.png` and `03-max-{light,dark}.png`.
- [x] C2 The composer used the kit's `input_background()` rather than the design's
  `--input`, obscuring its chips. The box now uses `palette.input`, with chips
  mixing 6% ink over it, in both themes.
- [x] C3 Cache totals are now a core journal-folded status segment, including lanes
  started without userspace extensions. **Accepted behavior:** no cache chip before
  the first request; afterwards display the server's percentage, including 0%.
  `/tmp/probe/final-composer/J01-idle-{light,dark}.png` has no chip, `J05-cache` has
  `0% cached` after a stub request. `/tmp/lane6/final/92-lane-after-resume-patched-*`
  shows the lane's chip restored with its earlier work.
- [x] C4 Selected input text used `muted` on `--input` and was indistinguishable.
  It now uses the design theme's selection: light `#0069CC40` (rl-vscode-dimmed
  `editor.selectionBackground`), dark a primary-blue selection of the same weight.
  Evidence: `/tmp/probe/final-integrated/05-selection-{light,dark}.png`.
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
Evidence includes `cargo test -p transcript -p workspace` and the stub-world probe
(`/tmp/lane8/`, `run.sh` + a script); final integrated results are recorded below.

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
  header row in `--muted-fg`, 12.5px medium (inheritance fix in R12); a fence is `--muted`,
  radius 6, `10px 12px`; lists are indented 22px. Evidence: `/tmp/lane8/45-md-table-light.png`,
  `/tmp/lane8/44-md-bottom-light.png`, `/tmp/lane8/50-jump-light.png`.
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
  Collapsed/open endpoint evidence: `/tmp/lane8/42-tools-{light,dark}.png` and
  `/tmp/lane8/43-tool-open-{light,dark}.png`.
- [x] R12 The table cell's explicit 13.5px/NORMAL shadowed the head row's
  12.5px/medium. Moved body type to the table ancestor and left the cell's text
  refinement unset, matching CSS inheritance. The non-shadowing regression test
  fails when those cell declarations return. Rebuilt probe A/B in both themes:
  `/tmp/lane5/header-final/{before,after}/10-table-{light,dark}.png`, with
  `compare-{light,dark}.png` showing smaller, heavier header glyphs; the header row
  shrinks from 36px to 34px while body rows stay 36px. Cell padding remains `7px 12px`
  and is applied once. The coordinator inspected the comparisons and final window.
- [x] R13 The cell refinement also reintroduced a right border on the last column,
  making the table's right edge twice as wide as its left. Removed the width override,
  preserving the kit's positional border and changing only its `--rule-soft` colour.
  `the_soft_right_rule_stays_off_the_last_column` checks actual painted border quads;
  restoring the width makes both its style and scene assertions fail. Coordinator
  inspected `/tmp/lane5/table-edge-final/edge-before-after-{light,dark}.png`: the extra
  inside rule is gone, with the outer frame, interior divider and header type preserved.

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
- [x] H4 The toggle deliberately uses the app's pointer cursor rather than the
  design's default (`TabContent::thinking_toggle`, `.cursor_pointer()`). The bare
  `.ghost` has no hover treatment. Actual click behavior is shown by
  `/tmp/probe/final-table-controls/02-thinking-open-{light,dark}.png` →
  `03-thinking-closed-{light,dark}.png`; cursor choice is source-verified.

## Final follow-up

- [x] Markdown row rules: transcript markdown now uses the public Base text style,
  with `with_border(palette.rule())`; frame/cell rules retain their separate colours.
  The before/after markdown fixture differs only at the horizontal row rules (and
  the blinking composer caret), both themes. Evidence: `/tmp/lane8/90-tablerule-*`
  and `the_text_style_draws_rules_in_the_designs_ink_in_both_themes`.
- [x] Scrollbar geometry rechecked: the earlier claimed mismatch mixed a unit test's
  content height with a different probe's pane. The kit uses the actual scroll
  extent, proportional thumb length, minimum size and inset; measurements match.
  No geometry override is needed. For the 694px pane and 2074px content in
  `/tmp/lane8/70-pill-scrolling-light.png`, the painted length is
  `694² / 2074 − 2×4 = 224.23px`; after scrolling 600px up from the tail, the
  predicted top is 345.00px, versus 345.5px in the rounded-cap pixels. Only
  visibility and colours are app overrides.
- [x] `render_failed` has seven arguments; the former clippy complaint is resolved.
- [x] Hovered busy dots now blend into the hovered row's own fill (5% ink mix), with
  selected fill (9%) taking precedence. `/tmp/lane7/dot-hovered-quiet-end-{light,dark}.png`
  shows the before/after quiet endpoint. Pointer-crossing tests cover a departing
  row's late leave event so it cannot clear the next row's hover.
- [x] Jump-to-latest regression: `ff5bbee` overwrote `c6176e1`'s negative-offset fix
  and deleted its regression test. Restored the negative target, per-frame repaint
  and final tail snap. `jump_to_latest_lands_on_the_tail` checks the visible pill,
  exact `-max_offset`, final row and dismissed pill; reverting the fix makes it fail
  at offset 0 instead of the tail. The rebuilt probe was independently clicked in
  both themes: `/tmp/probe/final-jump-dark/01-head-dark.png` → `02-tail-dark.png`,
  `04-head-light.png` → `05-tail-light.png`; the fourth markdown response is visible
  at the tail and the pill disappears. `03-rest-dark.png` / `06-rest-light.png`
  show the scrollbar gone after settling. The script moves the pointer into the
  transcript before each wheel event so a preceding click's hover cannot misroute it.
- [x] Font-weight consistency: `widgets::text::MEDIUM` centralizes the measured
  system-font convention (SEMIBOLD because 500 rasterizes like 400 here). Applied
  to selected agent names, header names, todo counts, drawer titles, tool targets
  and table heads. Task/state labels and table body cells stay
  normal. `/tmp/lane4/proof/ba-*.png`, `drawer-{light,dark}-before-after.png`,
  `/tmp/lane5/header-final/compare-{light,dark}.png` and the final integrated
  composer/resume captures show the distinction. Settings deliberately keep their
  existing kit `font_medium()` call (which draws regular here): doc28 has no
  Settings design, so this change does not restyle it.

## Resume and host behavior

- [x] Resuming replaces the requesting New Swarm tab. History titles use the folder,
  not injected scaffolding from the session index; the protocol limitation remains
  documented in `api-gaps.md`. `/tmp/probe/final-table-controls/08-history-*` →
  `09-resumed-*` shows the one requesting tab becoming the resumed project tab.
- [x] Current-source agent restores initial lane state/chips and prior lane items.
  Agent HEAD `0c4c1ec`, rebuilt `build/evo-swarm` SHA-256
  `a8ef641717e2ef49943b271b66a28bfd4306f889cf214bbaff04d56d9e023a63`.
  Evidence: `/tmp/lane6/final/90-lane-work-patched-{light,dark}.png`,
  `91-history-patched-*`, `92-lane-after-resume-patched-*`; final image has one
  project tab and the lane's prior report/tool rows and chips. Coordinator inspected
  both resumed images. Independently repeated with the integrated GUI in
  `/tmp/probe/final-rich/03-lane-{light,dark}.png` → `04-history-*` → `05-resumed-*`:
  the single project tab, lane's earlier report/tool rows and model/context/cache
  chips survive. No real provider calls; these runs used the fixture stub.
- [x] Isolated installation check: the existing Makefile installed the verified build
  into `/tmp/evo-install-check.Lzoej8` with `-o build`, `PREFIX` and a temporary
  `EVO_HOME`. Both installed hashes match the build and `bin/evo` links to `evo-swarm`.
  Explicit binary overrides ran all 9 proofs and the rich interaction sequence;
  `screens/09-resumed-{light,dark}.png` preserves lane work and chips after history
  resume. Coordinator inspected both images. Logs are `install.log`, `proofs.log`
  and `probe.log` under that prefix. No system binary or real user configuration
  changed. This proves the temporary installation, not default `/usr/local/bin` discovery.
- [x] Tracked fixture screenshots refreshed with the current GUI and temporary-prefix
  binaries. `screens.rs` now clicks `+` twice and asserts one New Swarm tab instead of
  constructing extra empty tabs directly; the tool capture waits for the completed
  `bash` row rather than opening an earlier delegate. All existing `docs/screens/*.png`
  were replaced at their documented resolution after privacy review; descriptions in
  `docs/screens.md` now match the actual states. Run and clippy log:
  `/tmp/evo-install-check.Lzoej8/screens-final.log`. The fixture and its listener exited.
- [ ] Installed-runtime recheck: installed `/usr/local/bin/evo-swarm` still has the
  older SHA-256 `bb90362fddf5f06499c3db4d7a294000124b1578ffe6363087dd9f2f47571c84`.
  It reproduces missing post-resume lane chips. Awaiting the user's reinstall.
- [x] GUI-created swarms receive `EVO_BABY_EVO=0` from `HOST_ENV`; ordinary terminal
  sessions do not. Agent's environment gate is merged in PR #104. The live ops probe
  returned disabled with 0 and enabled with 1; no global user configuration changed.

## Final integrated verification

After the table-header inheritance and last-column border fixes, the current tree passes:

- `cargo test --workspace --all-targets --exclude proofs`: 525 passed.
- `cargo test -p proofs` against rebuilt agent HEAD `0c4c1ec`: 9 passed.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo fmt --all -- --check` and `git diff --check`.

Logs: `/tmp/evo-gui-final-{unit,proofs,clippy,probe-build}.log`. The ordinary GUI
suite uses its fake control server without binary overrides; real-agent proofs
run separately with explicit binaries after the environment scrub.
The same full GUI suite (525 passed), temporary-prefix proofs (9 passed), clippy,
formatting and diff checks passed again after the capture-script changes; logs:
`/tmp/evo-gui-handoff-{unit,proofs,clippy}.log`.

The rebuilt integrated probe was exercised again after both table fixes:
`/tmp/probe/final-controls-verified/01-table-*`, `02-thinking-open-*`,
`03-thinking-closed-*`, `04-todo-open-*`, `05-todo-closed-*`, `06-drawer-*`,
`07-lane-*`, `08-history-*`, `09-resumed-*`. The script clicks the actual
`transcript-thinking` and `todo-strip-row` controls; the screenshots show the
thinking text and todo rows opening and closing. Table header type and rules,
model drawer and context/cache chips remain intact. Restoring composer focus
after dismissing the drawer let the delegate step run: lane 1 completed its
report, then its rows and chips survived close → history → resume in both themes.
The coordinator inspected both resumed images. The earlier `final-table-controls`
run lacked that focus step and is not lane-history proof. These captures and
the earlier final Jump/controls/selection runs are private
`/tmp` evidence, not new tracked screenshots. Installed-runtime verification
above remains pending; source-build proofs do not substitute for it.

## Verification method

- Keep real-HOME captures under `/tmp`, not committed: history and folders are private.
- Scrub inherited `EVO_*` before hermetic runs, then set explicit binary/stub overrides
  **after** the scrub. Confirm `catalog: reading with <path>` in the app log. A first
  resume attempt scrubbed its own overrides and accidentally exercised the installed
  old binary; those images are separated in `/tmp/lane6/final-attempt1-installed-bin`.
- Do not reuse stale Rust artifacts from removed worktrees as current proof. Rebuild
  affected crates and check the executable's provenance.
- Track only processes started by a probe when cleaning up; broad process-name kills
  can stop unrelated user swarms.
