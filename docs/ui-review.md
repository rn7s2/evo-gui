# UI review notes (coordinator)

Findings from screenshots, to be addressed in the polish pass once the tab page is integrated.

## Transcript (crates/transcript, demo screenshots 04/08)
- Reading width: text spans the whole center column (~1600 px lines). Cap the row content at a
  readable measure (~760–820 px) and center it in the column; tables/code may use the same cap.
- Vertical rhythm: tool one-liners and dim rows have ~40 px gaps — too airy. Tool rows should be
  compact (≈24 px line, 2–4 px gap), grouped tightly when consecutive; tool name in the mono font,
  status as a small coloured dot + word.
- Heading scale is document-sized (H1 ≈ 26 pt). In a chat transcript cap H1 ≈ 1.25×, H2 ≈ 1.1× body.
- `run-start · turn 1` / `run-end · outcome ok` read as noise. Show a hairline turn separator with a
  small muted label instead, or drop run-start entirely; keep run-end only when the outcome is not ok.
- User row: good as a muted card; consider a left accent bar (theme accent) to separate turns.
- Report row: good. Keep label column width fixed; add next/blocked/requests only when non-empty.
- Todo panel: glyph column misaligned (☑ is smaller and green, ◐ bold black, ☐ tiny). Use one
  fixed-width glyph column, same size, muted colours except the in-progress one.

## Empty tab (workspace snapshot 01)
- Sprawl: choosers sit at the left edge and "Select folder..." floats ~1500 px away at the far right.
  Compose the launcher as one centred block (max ≈ 880 px): a header line, the three chooser rows on
  the left, and a tall primary "Select folder…" button (folder icon, real ellipsis char) that spans
  exactly the three rows, directly beside them.
- The swarm.lisp note breaks the rows' rhythm; render it as a muted caption under the lanes chooser
  aligned with the select, showing the real path once a folder is known, generic text before.
- Choosers need option details (context window, effort) and disabled/greyed lanes options with the
  reason; show "uncertain" hint when the kernel api set is unknown.
- History: same centred measure; rows as hoverable list items with folder name, `~` path, meta line
  (lanes · when · coordinator model) right-aligned or beneath; tooltip with full details; empty and
  scanning states ("Scanning sessions…").
## Tab strip
- `+` sits at the far right; browsers put it right after the last tab (pinned when overflowing).
- Close icon is a heavy circled ⊗; use a small × shown on hover and on the selected tab.
## Tab page (workspace snapshot 04)
- Centre column empty state missing.
- Right column is a tall blank area under the composer; fine per spec, but give the composer a
  subtle top padding aligned with the lane list's first row.

## Transcript, round 2 (captures 11/14 after f632f52)
- Table cells break words mid-token ("call|s", "cach|e", "320m|s", "write_fil|e"): give columns a
  min width of their longest word (no intra-word breaks) and let the table scroll sideways instead.
- Tool name in mono renders visibly larger than body text; match the body size (or 1px smaller).
- Todo panel is flush with the window edge while rows sit in the centred measure: align it with the
  measure. Glyphs still render at different visual weights (☑ tiny, ◐ bold): draw them as icons/shapes
  (check box, half disc, empty box) at one size instead of font glyphs.
- Status rows read like protocol ("provider-retry 1/3 in 500ms", "run-end · outcome aborted",
  "output · …"): humanize in session ("Retrying provider (1/3) in 500 ms", "Run aborted", plain
  output text) and give session a first-class run-outcome row so the view stops sniffing strings.

## Tab page, live (workspace captures /tmp/evo-live-shots2 04/05, after 68df775)
- Left column is still lane 1's placeholder list, not the polished `agent_list::AgentList` crate
  (glyph colours, tooltips, trailing clock cell, down reasons, header "N lanes · M busy"). Mount it.
- The raw folder path (`/var/folders/.../proj`) wraps over four lines at the bottom of the agent list.
  Show the folder as a single `~`-shortened line, ellipsized in the middle, with the full path in a
  tooltip — or drop it (the tab tooltip already carries it).
- Close icon on the tab is still the heavy circled ⊗; use a light `×` (IconName::Close, small, muted,
  accent on hover).
- The center column has no heading: when a lane is selected nothing says whose transcript it is.
  Add a slim sticky header ("main" / "Lane 1 · <task>" + status glyph) above the transcript.
- Idle lane glyphs (○) are near-invisible at this size — same fix as agent_list's starting glyph.
