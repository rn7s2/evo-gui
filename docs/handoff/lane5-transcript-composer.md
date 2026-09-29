# Handoff — crates/transcript + crates/composer (lane 5)

Owner: `crates/transcript/**`, `crates/composer/**`. Other crates belong to other lanes.

## transcript
- `lib.rs` — `TranscriptView`: `new(cx)`, `replace(revision, rows, cx) -> bool`, `upsert(revision, row, cx) -> bool`,
  `clear`, `rows(cx)`, `revision()`, `todos()`/`set_todos`, `set_expanded`/`toggle_expanded`, `is_following_tail`,
  `scroll_to_latest`, `set_agent(session::AgentKey)`/`agent()`, `has_thinking(cx)`, `is_showing_thinking(cx)`,
  `set_show_thinking`/`toggle_thinking`, `empty_note`/`empty_state` (ids `transcript-empty`, `("transcript-empty-lane", n)`), `KEPT_DOCUMENTS = 128`.
- `rows.rs` — `render_row(&mut TranscriptData, index, &WeakEntity<TranscriptView>, &mut Context<TranscriptData>)`, one
  function per `RowKind`, `waiting_dots`/`dot_ink` (pips), `json_fields`/`Field`/`FieldValue` (tool payloads),
  `key_cell`/`value_cell`, `NEST_INDENT`/`MAX_DEPTH`/`MAX_ARRAY`, `block_text`.
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
- Tool payloads: one `key  value` row per field; a container the top level holds directly flattens (`env.RUST_LOG`,
  `args.0`), anything deeper is drawn as rows indented `NEST_INDENT` (12px) per level under their own key, up to
  `MAX_DEPTH` (4) — past that, or for an array longer than `MAX_ARRAY` (20), the container is one muted `{…3 keys}` /
  `[…42 items]` row whose tooltip carries its compact JSON. Row ids nest: `((("transcript-tool-arguments", row), i), j)`,
  and each row's cells are `(row_id, "key")` / `(row_id, "value")`.

## composer
- `lib.rs` — `Composer::new(window, cx)`, `set_readout`, `set_activity(Activity)`, `request_finished(ok, window, cx)`,
  `face() -> ActionFace::{Send, Stop}`, `is_action_enabled(cx)`, `focus_input`; `ComposerEvent::{Send, Interrupt}`; ids
  `BUTTON_ID`/`READOUT_ID`; key context `Composer` (Esc interrupts). Rows 2→8; one status row under the card (readout
  left, 28px button right, 8px gap); focus = 1px `theme.ring` + a 12% wash in the 3px band, the input drawing no chrome.
- Per-tab prompt history (↑/↓) and the window's ⌘C live in the tab page, not here; Enter sends the draft the keypress
  found (`c1b901c`, `dfdb3d3`).

## Commands
- `cargo test -p transcript` (39) and `cargo test -p composer` (21); `cargo fmt -p transcript -p composer`;
  `cargo clippy -p transcript --all-targets` is clean.
- Captures: `cargo run -q -p transcript --example transcript_demo -- --capture crates/transcript/screenshots`, and the
  same for `-p composer`'s `composer_demo` (both dirs git-ignored). Cost harness: `cargo run -q -p transcript --example
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
- **Syntax highlighting is off**: gpui-component installs a tree-sitter highlighter only under its `tree-sitter` feature,
  which `gpui-kit`'s default features do not enable. Fenced blocks render plain; turning it on is a workspace
  `Cargo.toml`/`Cargo.lock` change (big grammar dependency), not a transcript one.
- **Table cell padding**: gpui-base measures a scroll column as `text + CELL_PAD_PX (16)`, so padding of 8px a side makes
  the text box exactly as wide as its text and words break ("call|s"); 4px a side leaves slack and keeps columns apart.
- **Nested ordered lists** number `1. 2. 3.` at the top level and `A. B. C.` one level in (`text/utils.rs::list_item_prefix`,
  `NUMBERED_PREFIXES_1/2`); it is the kit's own scheme, not a style hook.
- **Text ranges**: `TextViewState::set_range_highlights` paints a background for a `Range<usize>` of the rendered text and
  carries no element or tooltip, so it is not a route to clickable file paths either.
