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
