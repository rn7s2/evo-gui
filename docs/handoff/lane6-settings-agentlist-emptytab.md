# Handoff — crates/settings, crates/agent_list, the empty tab (lane 6)

## crates/settings (§13: binary paths and theme; nothing else is configurable)
- `panel.rs` — `SettingsPanel::new(values, window, cx)`; `values()`; `focus_into(window, cx)`;
  `set_verdicts(..)` for captures/tests only (the default prober runs the real binaries on the app
  executor). Events: `SettingsEvent::Saved(values)` then `DismissEvent`; Cancel/Escape emit only the
  dismissal. Theme applies live; Cancel restores the opening mode; Save is off while a field is empty.
  Public: `PANEL_SIZE` (560x360), an id per control, `SettingsValues`, `Check`, `probe`.
- `probe.rs` — `probe(path, name) -> Check {Checking, Ready(line), Missing, NotExecutable, NotEvo,
  Empty}`; the name is read in the output, never the file name.
- `appearance.rs` — `mode_for(theme, appearance)` (a copy of app::theme's mapping).
- Run: `cargo test -p settings`; `cargo run -p settings --example settings_demo -- --capture <dir>`.
- Trap: the panel is exactly 560x360 and a test opens it at that size — trim spacing, not the test.
  Field focus edge = composer's (1px theme.ring + 12% wash; kit ring off).

## crates/agent_list (§7.3 left column)
- Holds its own focus (track_focus + tab_stop; click takes it): Down/Up, Home/End emit
  `AgentListEvent::Select`; the owner's `set_selected` alone moves the highlight. Rows are a named
  list box. Glyphs from `session::LaneStatus::glyph()`, rendered in the mono family at 16 px.
- Run: `cargo test -p agent_list`; `cargo run -p agent_list --example agent_list_demo -- --capture <dir>`.

## Empty tab (crates/workspace/src/empty_tab.rs, history.rs; rest of the crate is lane 1's)
- TabContent owns it: set_registry / set_model_cache / set_catalog_error / set_scanning /
  set_history_entries / set_history_error / set_home; emits `TabContentEvent::Launch{folder, plan}`
  and `Resume{session_path, folder}`. Choosers are the kit Select (Space dispatched as Confirm);
  history frame is a named group with Home/End. Ids: FOLDER_ID, HISTORY_ID, HISTORY_ROW_ID,
  CAPTION_ID. Text carrying facts uses tab_foreground / secondary_foreground, not muted_foreground.
- Run: `cargo test -p workspace --lib`; `cargo run -p workspace --example empty_tab_demo -- --capture <dir>`.
