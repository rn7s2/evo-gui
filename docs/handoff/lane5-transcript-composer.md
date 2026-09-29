# Handoff — crates/transcript + crates/composer (lane 5)

Owner: `crates/transcript/**`, `crates/composer/**`. Other crates belong to other lanes.

## transcript
- `lib.rs` — `TranscriptView`: `new(cx)`, `replace(revision, rows, cx) -> bool`, `upsert(revision, row, cx) -> bool`,
  `clear`, `rows(cx)`, `revision()`, `todos()`/`set_todos`, `set_expanded`/`toggle_expanded`, `is_following_tail`,
  `scroll_to_latest`, `set_agent(session::AgentKey)`/`agent()`, `has_thinking(cx)`, `is_showing_thinking(cx)`,
  `set_show_thinking`/`toggle_thinking`, `empty_note`/`empty_state` (ids `transcript-empty`, `("transcript-empty-lane", n)`), `KEPT_DOCUMENTS = 128`.
- `rows.rs` — `render_row(&mut TranscriptData, index, &WeakEntity<TranscriptView>, &mut Context<TranscriptData>)`, one
  function per `RowKind`, `waiting_dots`/`dot_ink` (pips), `json_fields`/`Field`/`FieldValue` (tool payloads),
  `key_cell`/`value_cell`, `NEST_INDENT`/`MAX_DEPTH`/`MAX_ARRAY`, `block_text`, `context_row`/`context_label`/`context_text`
  with `CONTEXT_BLOCK_LINES = 12`, `lane_notice_row` and `report_row` (headed with the lane when the row names one).
- `link.rs` — `openable(url)` (http/https/mailto only) and `on_click()`, the handler an assistant message's links are
  opened with; `rows::assistant_row` attaches it. Tests: `a_left_click_on_a_link_opens_it`,
  `a_click_on_plain_text_opens_nothing`, `a_right_click_on_a_link_opens_nothing`,
  `a_link_the_app_does_not_open_is_ignored`, `a_mailto_link_opens_a_mail_composer`,
  `the_app_opens_web_and_mail_links_and_nothing_else`.
- `markdown.rs` — `extensions()`, the `MarkdownExtensions` every assistant message is parsed with (built once):
  a plugin claiming `Node::Image` (drawn as `[image: alt]`, never fetched), one claiming inline `Node::Html` and one
  claiming block `Node::Html` (shown as the source, mono and muted).
- `style.rs` — `Palette`, `MEASURE = 800`, `text_style(cx)`; the table cell carries 4px of padding a side (see below).
- `todo.rs` — `TodoPanel::new(&[Todo])` (owns the list's ScrollHandle, capped at `MAX_LIST_HEIGHT` 132px).

Invariants
- **Documents are render-owned**: a row's `TextViewState` is created and handed its row's text — once per frame — by the
  row renderer, never by `upsert`/`replace`; a row nobody has rendered has no document. gpui-base's `set_text` is
  O(message) (full copy, synchronous parse below 4 KiB), so one call per row per frame is the contract, and only the
  `KEPT_DOCUMENTS` most recently rendered rows keep documents — scrolling back re-parses.
- Revisions: below the last accepted revision is dropped; a row is replaced only when its `version` is newer. A `replace`
  of equal ids/versions must not move the reader, the open tool rows or the documents (`a_resync_of_the_same_rows_changes_nothing`).
- Empty state: no rows → a centred invitation (coordinator, `IconName::Asterisk`) or "Lane N hasn't been given work yet.";
  the tab page must mount the view for it to show (§7.3). A streaming row with no text yet shows three pips
  (`with_animation`, so `App::reduce_motion` freezes them); the first delta retires them for the retained document.
- Rows are selectable: a row `track_focus`es the transcript's focus handle (`tab_stop(false)`) so a drag inside a row and
  ⌘C reach the window's own copy binding; assistant rows hover-reveal a message Copy and a per-code-block Copy.
- Injected context is a row of its own, not a turn. `evo:inject-context` journals a `:custom-message` with a `:key`; serve
  sends it back as `{"role": "user", "meta": {"key": "global-memory"}, "content": [{"type": "text", …}]}` (keys are
  snake_case on the wire, values keep their spelling: `global-memory`, `project-memory`, `recovery`). session's
  `rebuild_from_transcript` turns that into `RowKind::Context { key, text }` — additive, so every exhaustive `RowKind`
  match in the workspace has a `Context` arm — and the transcript draws it as one muted line, `Context · global memory`
  (`context_label`: the two memory keys in words, any other key as the extension named it) with a caret, closed until
  clicked, and never a `turn N` separator: turn numbers count `RowKind::User` only. Opened, it shows its text in a mono
  block capped at twelve lines with a scroll of its own. Spaces as `TIGHT_GAP` against another context row, `BLOCK_GAP`
  otherwise, plus the block's own `mb` so an opened one is not glued to the row below.
- The swarm's own words are not the reader's. `tell-coordinator` (`swarm/lanes.lisp:19`) steers the coordinator with
  `[lane N report] …` (`report-text`, :220) and `[lane N] …` (:237 run end, :193 failed to start, :202 initialization
  failed, :286 error, :295 output, :341 down, :359 crashed) — no `meta` key, no event of their own, so the prefix is the
  contract and it is evo's own format, not something a reader types. session's `lane_row`/`lane_prefix` turn them into
  `RowKind::Report { lane: Some(n), .. }` and `RowKind::LaneNotice { lane, text, tone }` on both paths (the `/transcript`
  rebuild and the `user-input`/`steering` events), and the transcript draws the report as the report card ("Lane 2 report")
  and the notice as one muted line, `Lane 1 · run ended (stop) — task: …`, ellipsized to the measure with the whole line
  in its tooltip and as its accessible name. Neither opens a turn: turn numbers count `RowKind::User` only. `tone` is
  `DimStyle::Error` when the body says `failed to start` / `initialization failed:` / `error:` / `is down:` /
  `crashed and was restarted` (case-insensitively, so a lane's own `Error:` output counts), `DimStyle::Notice` otherwise —
  the swarm's `:style :error` is the TUI's and does not cross the wire, so the wording is all there is.
  Cross-lane consequences: `crates/proofs/tests/m1_delegation.rs` now waits for `RowKind::Report { lane: Some(1), .. }`
  (the relay, not a user row); and `session/src/tab.rs`'s own `lane_of_line`
  reads the same prefix off the coordinator's `Dim` (`output`) rows for a lane's down reason — untouched by these rows,
  but it is a second parser of the same format and the two could become one.
- A goal nudge is evo talking, not the reader. When a run settles with the goal unfinished, `goal-settled-hook` steers a
  continuation into the agent (`goal-continuation-message`, `src/kernel/goal.lisp:69`, queued at :153): "You are idle but
  your goal is still active. Continue working toward it now." followed by the `<goal objective=…>` block (:73), a
  `Budget: …` line (`goal-budget-line`, :62), the agent's checklist (:77) and the rules. A spent budget steers the
  wrap-up instead (`goal-wrapup-message`, :106, queued at :149): "Your goal's token budget is exhausted (…). Do not start
  new work.". `queue-steering` marks neither, so the opening sentence is the contract, and session's `goal_nudge_row`
  reads it on both paths — the rows are the additive `RowKind::GoalNudge { kind: Continue|Wrapup, objective, budget, text }`
  (`text` is the whole message) and they open no turn. The transcript draws one quiet line: `Goal · continue — <objective>
  · <budget>` (the objective gives way first, the budget stays readable at the measure's right edge) and `Goal · budget
  exhausted — wrap up`, opened onto the same capped mono block a context row uses. A third kind, `Updated`, follows
  `/goal <new objective>` while a run is in flight (`src/command/command.lisp:227`): `Goal · objective updated —
  <objective>`. `budget` is evo's own words without the sentence's punctuation: `15 tokens used of 50,000 (49,985
  remaining)`, `0 tokens used (no limit)`, `45,001 used of 45,000` — and it sits right after the objective as a cell that
  never shrinks, while the objective is the cell that gives way (`flex_shrink(1.)` + `truncate()`), so a long objective
  is ellipsized and the budget is still on the line.
- A command the reader ran, answered with instructions for the agent, is one more quiet line: `Command · /global-memory`.
  Three formats exist in evo today — `scoped-memory-command` (`src/core-ext/memory.lisp:242`, "The user invoked
  `/<command>` …"), `/lore` (`src/command/command.lisp:397`, "The user added <label> (durable guidance, applies from now
  on): …") and any extension that hands work over the same way (`extensions/360-baby-evo.lisp:844`'s `/notify doctor`,
  "The user just ran `/<command>`") — and session's `command_note_row` claims exactly those phrasings, the whole of each,
  so a reader writing "The user added a column to the table" keeps their turn. The `command` in the row is the backticked
  token, or the lore label spelt as the command it was (`global-lore` → `/global-lore`). Every other place evo steers
  text at an agent is the reader's own words: `host-submit`/`POST /steer`/`POST /follow-up`/the TUI and CLI prompts
  (`:from-user t`), and a lane's own `/prompt` from `delegate`. The one unhandled case is
  `extensions/360-baby-evo.lisp:428`, where the idle-notification helper's own prose is steered in — arbitrary model
  text with no format to key on, and genuinely a message *to* the agent.
- The three "quiet line that opens onto the capped block" rows (context, goal nudge, command note) share `quiet_row`/
  `QuietRow` and `quiet_block`: `header` and `block` name the elements a test addresses, `head`/`trailing` are the line,
  and the header carries the whole line as its accessible name plus `aria_expanded`.
- Tool payloads: one `key  value` row per field; a container the top level holds directly flattens (`env.RUST_LOG`,
  `args.0`), anything deeper is drawn as rows indented `NEST_INDENT` (12px) per level under their own key, up to
  `MAX_DEPTH` (4) — past that, or for an array longer than `MAX_ARRAY` (20), the container is one muted `{…3 keys}` /
  `[…42 items]` row whose tooltip carries its compact JSON. Row ids nest: `((("transcript-tool-arguments", row), i), j)`,
  and each row's cells are `(row_id, "key")` / `(row_id, "value")`.
- An open row caps what it draws, by characters, not by lines: `ARGUMENTS_LIMIT` (1024) of a call's arguments — its
  `command` among them — and `RESULT_LIMIT` (2048) of what came back. The old `TOOL_TEXT_LIMIT` (4000 chars) and
  `BLOCK_LINES` (8 lines) are gone; `CONTEXT_BLOCK_LINES` (12) still caps the quiet rows, which are not tool panels.
  `Cap`/`cap_fields`/`cap_value` fit a panel's fields to its budget — a value the budget cannot reach is dropped, one it
  can only partly reach is cut, and `take_chars` cuts between characters, so a payload written in emoji or CJK is
  shortened, never torn — and `cap_text` does the same for a body with no keys of its own, counting from
  `result.content_chars` when the swarm said the copy that arrived was already shortened. What the limit cut is what the
  note under the panel counts (`cap_note_text`, `… (N more chars)`: one muted line, id `(panel, "note")`, the note as its
  accessible name, and not part of the selectable text). The note is about the limit, not about the panel's own
  elisions: a value elided to `VALUE_LIMIT` or a container collapsed to `{…3 keys}` keeps its text on hover and counts
  as drawn — otherwise every panel holding a long path would claim to have been cut. The drawn text is what the
  `SelectableText` carries, so a copy takes the shortened text; the row keeps the whole of the call, `arguments` and the
  result content unfiltered (`a_tool_call_keeps_its_whole_command_and_output` in session), which is what a later
  "show all" would be drawn from.
- A call's `command` is a block whether or not it breaks lines: it is what the call does, and the panel's budget is what
  caps it, not the one-line elision. The key is evo's own vocabulary (`*tool-key-args*`, `src/command/command.lisp:420`,
  names `command` for `bash`; `read` says `path`, `eval` says `code`).
- Panel captions are lower case — `arguments`, `result`, `error` — small and muted, each with id `(panel, "caption")`
  and its own text as its accessible name.

## composer
- `lib.rs` — `Composer::new(window, cx)`, `set_readout`, `set_activity(Activity)`, `request_finished(ok, window, cx)`,
  `face() -> ActionFace::{Send, Stop}`, `is_action_enabled(cx)`, `focus_input`; `ComposerEvent::{Send, Interrupt}`; ids
  `BUTTON_ID`/`READOUT_ID`; key context `Composer` (Esc interrupts). Rows 2→8; one status row under the card (readout
  left, 28px button right, 8px gap); focus = 1px `theme.ring` + a 12% wash in the 3px band, the input drawing no chrome.
- Per-tab prompt history (↑/↓) and the window's ⌘C live in the tab page, not here; Enter sends the draft the keypress
  found (`c1b901c`, `dfdb3d3`).

## Commands
- `cargo test -p transcript` (55) and `cargo test -p composer` (21), plus `cargo test -p session` (the context row is
  fixture-driven: `crates/session/tests/fixtures/context-transcript.json`, recorded by
  `crates/session/tests/capture_context_fixture.py`); `cargo fmt -p transcript -p composer`;
  `cargo clippy -p transcript --all-targets` is clean. Note `cargo fmt -p session` reaches `lanes.rs`/`tab.rs` through
  `lib.rs`'s `mod` list — format those files by path, not through the crate, while other lanes are in session.
- Captures: `cargo run -q -p transcript --example transcript_demo -- --capture crates/transcript/screenshots`, and the
  same for `-p composer`'s `composer_demo` (both dirs git-ignored). `34-lane-report-and-run-end.png` /
  `35-dark-lane-report-and-run-end.png` are the turn the swarm talks in (delegation, lane report, run end, the
  coordinator's answer), and `36-goal-nudges.png` / `37-dark-goal-nudges.png` the goal evo keeps going by itself (a
  continuation opened onto its message, and the wrap-up), and `38-command-notes.png` / `39-dark-command-notes.png` the
  commands the reader ran while the agent worked (`Command · /global-memory` closed, `/notify doctor` opened onto its
  capped block), and `40-long-call.png` / `41-dark-long-call.png` a call that ran the whole sweep — a command the
  arguments panel cuts at 1024 characters and a log the result panel cuts at 2048, each with the note that counts the
  rest. Cost harness: `cargo run -q -p transcript --example
  transcript_stress [-- rows stream delta message_chars]`.

## Kit facts worth not re-deriving (gpui-kit 0.7.0 / gpui-base 0.7.0)
- **Links**: the only hook is `TextView::on_link_click`; without one the kit opens *any* scheme through `App::open_url`.
  The pointer cursor is the kit's (`text/inline.rs:987`, `text/inline_object.rs:334`). There is **no per-link tooltip
  hook**: the inline flow paints links as glyph runs (`links: Vec<(Range<usize>, LinkMark)>`), `LinkMark.title` is parsed
  and never rendered, and the tooltip machinery needs an interactive element, which only inline *objects* have — so a
  plugin claiming `Node::Link` (the only route to an interactive link) turns every link into one atomic, unbreakable box.
- **Images are fetched**: `Node::Image` becomes an `<img>` whose `Resource::Uri` is loaded with `client.get(uri)`
  (`gpui::img::ImageAssetLoader::load`), so an unclaimed image reference makes the app issue a request. The tests prove
  the claim holds: `an_image_reference_is_drawn_as_its_alt_text_and_never_fetched` installs a recording `HttpClient` and
  shows zero requests while a control `img` element does record one.
- **Raw HTML**: the kit interprets `<b> <i> <u> <s> <del> <strike>` tag pairs into emphasis *before* plugins see the
  nodes (`format/markdown.rs::inline_groups`), and drops what its HTML reader cannot parse. Everything else is ours.
- **Inline plugin nodes are atomic**: `render_inline` output is one object measured at its intrinsic width
  (`WrapLineFragment::element`), it cannot wrap inside itself, and the object's own text is what a plugin must set.
- **Syntax highlighting is on**, and it is a workspace decision: the root `Cargo.toml` enables gpui-kit's `tree-sitter`
  (which brings the highlighter + the JSON grammar) plus `tree-sitter-{bash,javascript,python,rust,toml,typescript}`.
  The theme then installs the highlighter for every `TextView` (`install_text_view_defaults`), so a transcript only has
  to stay out of the way: an explicit component `TextViewStyle` left at its default `HighlightTheme::default_light()`
  is *not* an override (`text/compat.rs` ptr-compares it), and the theme's own light/dark highlight theme is used.
  Cost: release binary 27.88 MiB → 32.94 MiB (+5.06 MiB, +18%; 10 new lock packages). Aliases resolve through
  `highlighter/language_name.rs` (`sh`→bash, `js`→javascript, `py`/`pyi`→python, `rs`→rust, `ts`→typescript,
  `jsonc`→json); a tag with no grammar — or none at all — leaves the fence plain rather than failing, and
  `SyntaxHighlighter::styles` answers one unstyled range in that case. The kit ships **no lisp grammar**.
  `code_fences_are_highlighted_and_an_unknown_language_stays_plain` guards all of it (it fails on the stub the kit
  compiles when the feature is off). `tsx`, `yaml`, `go`, `c`/`cpp`, `sql`, `diff` and the rest are one feature name away.
- **Task-list markers are the kit's** (`text/node.rs::render_list_item_row`): unchecked is a 14px box bordered in
  `style.foreground()`, checked is the same box filled with it plus a 10px SVG check in the opposite colour. There is no
  list-marker field in `TextViewStyle` and no hook on the marker element, and the component adapter ignores a legacy
  style's colours — so the marker cannot be muted per view without recolouring all message text. The check **does**
  paint: it is an async asset (`ImageDecoder`), so a headless capture must let the loader park before the frame it
  reads — `shot()` in the demo calls `allow_parking()` + `run_until_parked()` between two frames, without which the box
  looks like a solid square in every PNG.
- **Table cell padding**: gpui-base measures a scroll column as `text + CELL_PAD_PX (16)`, so padding of 8px a side makes
  the text box exactly as wide as its text and words break ("call|s"); 4px a side leaves slack and keeps columns apart.
- **Nested ordered lists** number `1. 2. 3.` at the top level and `A. B. C.` one level in (`text/utils.rs::list_item_prefix`,
  `NUMBERED_PREFIXES_1/2`); it is the kit's own scheme, not a style hook.
- **A block can scroll without a `ScrollHandle`**: an element with `overflow_y_scroll()` and an id keeps its offset in
  its own element state (`elements/div.rs` prepaint, `element_state.scroll_offset`), which is what the context block uses
  — no state to own, no `track_scroll`. A visible thumb needs the `TodoPanel` arrangement instead
  (`Scrollbar::vertical(&handle)` in a `relative()` box beside a `track_scroll`ed list).
- **Reading rendered words in a test**: `ElementSnapshot::label()`/`expanded()` come from the element's accesskit node, so
  `div().aria_label(...).aria_expanded(...).test_support()` is the only way a test sees what a row *says*. `SelectableText`
  itself is never an observed element (it inserts a hitbox, not a `Registration`), so wrap the run in an observed div and
  find that; `find` matches on the last component of a global id.
- **Ellipsis vs selection**: only a plain string child of a `.truncate()` div is ellipsized — GPUI shapes that child with
  the div's own text style (which is where `text_overflow` lives). A `SelectableText` below the same div is clipped
  instead (it shapes itself); that is why a `key_cell`'s long key is cut and its tooltip carries it. A lane notice takes
  the ellipsis, and so gives up selection — the line is the swarm's, one line long, and the whole of it is on hover.
- **Text ranges**: `TextViewState::set_range_highlights` paints a background for a `Range<usize>` of the rendered text and
  carries no element or tooltip, so it is not a route to clickable file paths either.
