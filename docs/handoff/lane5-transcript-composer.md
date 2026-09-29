# Handoff — crates/transcript + crates/composer (lane 5)

Owner: `crates/transcript/**`, `crates/composer/**`. Other crates belong to other lanes.

## transcript
- `lib.rs` — `TranscriptView`: `new(cx)`, `replace(revision, rows, cx) -> bool`, `upsert(revision, row, cx) -> bool`,
  `clear`, `rows(cx)`, `revision()`, `todos()`/`set_todos`, `set_expanded`/`toggle_expanded`, `is_following_tail`,
  `scroll_to_latest`, `set_agent(session::AgentKey)`/`agent()`, `has_thinking(cx)`, `is_showing_thinking(cx)`,
  `set_show_thinking`/`toggle_thinking`, `empty_note`/`empty_state` (ids `transcript-empty`, `("transcript-empty-lane", n)`), `KEPT_DOCUMENTS = 128`.
- `rows.rs` — `render_row(&mut TranscriptData, index, &WeakEntity<TranscriptView>, &mut Context<TranscriptData>)`, one
  function per `RowKind`, `waiting_dots`/`dot_ink` (pips), `json_fields`/`Field`/`block_text` (tool payloads).
- `todo.rs` / `style.rs` — `TodoPanel::new(&[Todo])` (owns the list's ScrollHandle, capped at `MAX_LIST_HEIGHT` 132px); `Palette`, `MEASURE = 800`, `text_style(cx)`.

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

## composer
- `lib.rs` — `Composer::new(window, cx)`, `set_readout`, `set_activity(Activity)`, `request_finished(ok, window, cx)`,
  `face() -> ActionFace::{Send, Stop}`, `is_action_enabled(cx)`, `focus_input`; `ComposerEvent::{Send, Interrupt}`; ids
  `BUTTON_ID`/`READOUT_ID`; key context `Composer` (Esc interrupts). Rows 2→8; one status row under the card (readout
  left, 28px button right, 8px gap); focus = 1px `theme.ring` + a 12% wash in the 3px band, the input drawing no chrome.

## Commands
- `cargo test -p transcript` (24) and `cargo test -p composer` (11); `cargo fmt -p transcript -p composer`.
- Captures: `cargo run -q -p transcript --example transcript_demo -- --capture crates/transcript/screenshots`, and the same
  for `-p composer`'s `composer_demo` (both dirs git-ignored); cost harness: `cargo run -q -p transcript --example
  transcript_stress [-- rows stream delta message_chars]`.

## Open issues
- Frames are dominated by `TextView` shaping of visible messages (~3 ms per 1.5 KiB at 1600×1000); the assembled window is
  unmeasured; `docs/screens/` goldens predate the composer card and the softer focus edge.
- In the headless test platform `notify` on a mounted view draws a frame synchronously, so per-delta timings include a frame.
