//! The scrollback: how far back from the live screen the pane is looking, in
//! pixels, and how it gets there smoothly.
//!
//! The grid keeps its history in whole lines (`alacritty_terminal`'s
//! `display_offset`); a trackpad moves in pixels. The position is both: the
//! grid's offset, in lines, plus `frac` pixels into the line above it. The pane
//! draws one extra row on top while `frac` is not zero and shifts the rows down
//! by it, so a scroll moves the text by exactly what the fingers moved.
//!
//! A trackpad's deltas are applied as they come (the OS adds the momentum); a
//! notched wheel's lines and the paging keys are animated toward their target
//! instead of jumping, one frame at a time.

use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions as _, Scroll as GridScroll};
use alacritty_terminal::term::{Config, Term};

use crate::SCROLLBACK;

/// How quickly an animated scroll closes on its target: the time constant of
/// the exponential ease, short enough to feel immediate and long enough to be
/// seen to travel.
const EASE: f32 = 0.06;

/// Closer than this to the target, an animation is done.
const SETTLE_PX: f32 = 0.5;

/// The parser's configuration: the scrollback the pane keeps.
pub(crate) fn config() -> Config {
    Config {
        scrolling_history: SCROLLBACK,
        ..Config::default()
    }
}

/// Where the pane is looking, beyond what the grid itself records.
#[derive(Debug, Default)]
pub(crate) struct Scrollback {
    /// Pixels into the line above the grid's own offset: the sub-line part of
    /// the position. Always `0 <= frac < line`.
    frac: f32,
    /// Where an animated scroll is headed, in pixels back from the live screen.
    target: Option<f32>,
    /// When the animation last stepped.
    last: Option<Instant>,
    /// Lines of wheel travel not yet sent as arrow keys, on the alternate
    /// screen.
    alt_rest: f32,
}

impl Scrollback {
    /// Pixels back from the live screen.
    pub(crate) fn position<T>(&self, term: &Term<T>, line: f32) -> f32 {
        term.grid().display_offset() as f32 * line + self.frac
    }

    /// The farthest back the pane can look: the whole history.
    pub(crate) fn limit<T>(term: &Term<T>, line: f32) -> f32 {
        term.grid().history_size() as f32 * line
    }

    /// The sub-line part of the position, for drawing.
    pub(crate) fn frac(&self) -> f32 {
        self.frac
    }

    /// Whether the pane is showing anything but the live screen.
    pub(crate) fn is_scrolled<T>(&self, term: &Term<T>) -> bool {
        term.grid().display_offset() > 0 || self.frac > 0.
    }

    /// Whether an animated scroll is still travelling.
    pub(crate) fn is_animating(&self) -> bool {
        self.target.is_some()
    }

    /// Look `px` pixels back from the live screen, clamped to the history.
    pub(crate) fn set<T: EventListener>(&mut self, term: &mut Term<T>, line: f32, px: f32) {
        let history = term.grid().history_size();
        let px = px.clamp(0., Self::limit(term, line));
        let offset = ((px / line).floor() as usize).min(history);
        self.frac = if offset >= history {
            0.
        } else {
            (px - offset as f32 * line).clamp(0., line)
        };
        if self.frac >= line {
            self.frac = 0.;
        }
        let old = term.grid().display_offset();
        if offset != old {
            term.scroll_display(GridScroll::Delta(offset as i32 - old as i32));
        }
    }

    /// A trackpad's (or a precise wheel's) pixels: applied at once.
    pub(crate) fn by_pixels<T: EventListener>(&mut self, term: &mut Term<T>, line: f32, dy: f32) {
        self.target = None;
        let to = self.position(term, line) + dy;
        self.set(term, line, to);
    }

    /// A jump — a notched wheel's lines, a page key — eased toward rather than
    /// taken at once. A second jump while the first is travelling adds to where
    /// it was going.
    pub(crate) fn glide_by<T>(&mut self, term: &Term<T>, line: f32, dy: f32) {
        let from = self.target.unwrap_or_else(|| self.position(term, line));
        self.glide_to(term, line, from + dy);
    }

    /// Ease toward `px` pixels back from the live screen.
    pub(crate) fn glide_to<T>(&mut self, term: &Term<T>, line: f32, px: f32) {
        self.target = Some(px.clamp(0., Self::limit(term, line)));
        self.last = None;
    }

    /// One frame of an animated scroll. Returns whether another is needed.
    pub(crate) fn step<T: EventListener>(
        &mut self,
        term: &mut Term<T>,
        line: f32,
        now: Instant,
    ) -> bool {
        let Some(target) = self.target else {
            return false;
        };
        let elapsed = self.last.map_or(Duration::from_millis(16), |last| {
            now.saturating_duration_since(last)
        });
        self.last = Some(now);
        let from = self.position(term, line);
        let k = 1. - (-elapsed.as_secs_f32() / EASE).exp();
        let to = from + (target - from) * k;
        if (target - to).abs() < SETTLE_PX {
            self.set(term, line, target);
            self.target = None;
            self.last = None;
            return false;
        }
        self.set(term, line, to);
        true
    }

    /// Back to the live screen, at once: what typing does.
    pub(crate) fn snap_to_bottom<T: EventListener>(&mut self, term: &mut Term<T>) {
        self.target = None;
        self.last = None;
        self.frac = 0.;
        if term.grid().display_offset() != 0 {
            term.scroll_display(GridScroll::Bottom);
        }
    }

    /// Keep the position drawable after something outside moved under it: a
    /// resize, a cleared history, a different line height.
    pub(crate) fn settle<T>(&mut self, term: &Term<T>, line: f32) {
        let offset = term.grid().display_offset();
        if offset >= term.grid().history_size() || self.frac >= line || self.frac < 0. {
            self.frac = 0.;
        }
        if let Some(target) = self.target {
            self.target = Some(target.clamp(0., Self::limit(term, line)));
        }
    }

    /// Wheel travel on the alternate screen (a pager, an editor): whole lines
    /// of it, as the count of arrow presses to send — positive is up.
    pub(crate) fn alternate_lines(&mut self, lines: f32) -> i32 {
        self.alt_rest += lines;
        let whole = self.alt_rest.trunc();
        self.alt_rest -= whole;
        whole as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{quiet, Cells, PtyEvents};
    use alacritty_terminal::vte::ansi::Processor;

    const LINE: f32 = 20.;

    fn term(rows: u16) -> Term<PtyEvents> {
        Term::new(config(), &Cells::new(80, rows), PtyEvents::new(quiet()))
    }

    fn print(term: &mut Term<PtyEvents>, lines: std::ops::Range<usize>) {
        let mut text = String::new();
        for n in lines {
            text.push_str(&format!("{n}\r\n"));
        }
        let mut processor: Processor = Processor::new();
        processor.advance(term, text.as_bytes());
    }

    /// The history keeps at most 65535 lines: printing more drops the oldest.
    #[test]
    fn the_history_holds_65535_lines_and_no_more() {
        assert_eq!(SCROLLBACK, 65_535);
        let mut term = term(24);
        print(&mut term, 0..70_000);
        assert_eq!(term.grid().history_size(), 65_535);

        // Scrolled all the way back, the top row is the oldest line still kept.
        let mut scroll = Scrollback::default();
        scroll.set(&mut term, LINE, f32::MAX);
        assert_eq!(term.grid().display_offset(), 65_535);
        let top = &term.grid()[alacritty_terminal::index::Line(-65_535)];
        let first: String = (0..5)
            .map(|c| top[alacritty_terminal::index::Column(c)].c)
            .collect();
        // 70000 printed + the cursor's empty line, 24 on screen, 65535 behind.
        let oldest = 70_000 + 1 - 24 - 65_535;
        assert_eq!(first.trim(), oldest.to_string());
    }

    /// A trackpad moves the text by its own pixels: whole lines go to the
    /// grid's offset, and the rest is the part-line drawn above it.
    #[test]
    fn pixels_split_into_lines_and_a_part_line() {
        let mut term = term(10);
        print(&mut term, 0..100);
        let mut scroll = Scrollback::default();
        scroll.by_pixels(&mut term, LINE, 65.);
        assert_eq!(term.grid().display_offset(), 3);
        assert_eq!(scroll.frac(), 5.);
        assert!(scroll.is_scrolled(&term));
        scroll.by_pixels(&mut term, LINE, -65.);
        assert_eq!(term.grid().display_offset(), 0);
        assert_eq!(scroll.frac(), 0.);
        assert!(!scroll.is_scrolled(&term));
    }

    /// Neither end can be overshot: not past the live screen, not past the
    /// oldest line — and at the oldest line there is no part-line above it.
    #[test]
    fn the_position_is_clamped_to_the_history() {
        let mut term = term(10);
        print(&mut term, 0..100);
        let history = term.grid().history_size();
        let mut scroll = Scrollback::default();
        scroll.by_pixels(&mut term, LINE, -500.);
        assert_eq!(term.grid().display_offset(), 0);
        scroll.by_pixels(&mut term, LINE, 1e9);
        assert_eq!(term.grid().display_offset(), history);
        assert_eq!(scroll.frac(), 0.);
    }

    /// A glide travels over several frames and ends exactly on its target.
    #[test]
    fn a_glide_eases_to_its_target() {
        let mut term = term(10);
        print(&mut term, 0..100);
        let mut scroll = Scrollback::default();
        scroll.glide_by(&term, LINE, 9. * LINE);
        let mut now = Instant::now();
        let mut frames = 0;
        let mut last = 0.;
        while scroll.step(&mut term, LINE, now) {
            let at = scroll.position(&term, LINE);
            assert!(at > last, "it only moves toward the target");
            last = at;
            frames += 1;
            now += Duration::from_millis(16);
            assert!(frames < 200);
        }
        assert!(
            frames > 3,
            "it travelled rather than jumped: {frames} frames"
        );
        assert_eq!(scroll.position(&term, LINE), 9. * LINE);
        assert!(!scroll.is_animating());
    }

    /// New output while scrolled back leaves the view where it is, and typing
    /// brings it back to the live screen.
    #[test]
    fn output_keeps_the_view_and_typing_returns_to_the_bottom() {
        let mut term = term(10);
        print(&mut term, 0..100);
        let mut scroll = Scrollback::default();
        scroll.by_pixels(&mut term, LINE, 5. * LINE);
        print(&mut term, 100..103);
        assert_eq!(term.grid().display_offset(), 8);
        scroll.snap_to_bottom(&mut term);
        assert_eq!(term.grid().display_offset(), 0);
        assert!(!scroll.is_scrolled(&term));
    }

    /// On the alternate screen the wheel is arrow keys, sent a whole line at a
    /// time; the remainder waits for the next event.
    #[test]
    fn alternate_screen_wheel_counts_whole_lines() {
        let mut scroll = Scrollback::default();
        assert_eq!(scroll.alternate_lines(0.6), 0);
        assert_eq!(scroll.alternate_lines(0.6), 1);
        assert_eq!(scroll.alternate_lines(-2.3), -2);
    }
}
