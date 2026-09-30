//! The transcript's scroll: when it follows the tail, and when it lets go.
//!
//! Ported from `Transcript.tsx`, whose three constants and one rule are the whole
//! of it (`design/doc28/Transcript.tsx`):
//!
//! * `PIN_SLACK = 24` — within 24px of the bottom counts as being *at* the
//!   latest;
//! * `JUMP_THRESHOLD = 240` — "↓ Jump to latest" appears once the reader is
//!   further away than that;
//! * `USER_WINDOW = 500` — a scroll within 500ms of a wheel, touch, key or
//!   pointer press is the reader's own.
//!
//! The list opens at its latest item and stays there while the pane changes
//! height — the composer growing, a drawer opening, a window resize. Only the
//! reader's own scroll unpins it; and a scroll that is *not* the reader's — the
//! list laying out again, an item growing as its text arrives — is pulled back to
//! the bottom rather than allowed to drift.
//!
//! This is the state machine on its own, with no window in it, so the rule can be
//! held to the design without rendering anything: [`Pin::on_scroll`] takes the
//! distance from the bottom and says what the view should do.

use std::time::{Duration, Instant};

/// Within this distance of the bottom counts as being at the latest.
pub const SLACK: f32 = 24.;

/// How far away the reader has to be before "Jump to latest" appears.
pub const JUMP_AT: f32 = 240.;

/// A scroll this soon after a wheel, touch, key or pointer press is the reader's
/// own.
pub const USER_WINDOW: Duration = Duration::from_millis(500);

/// What the view should do after a scroll, given the distance from the bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// The layout moved under a reader who is following the tail: pull it back.
    SnapToBottom,
    /// Leave the offset alone.
    Leave,
}

/// Whether the transcript is following its tail, and whether the reader has
/// scrolled away from it.
#[derive(Clone, Debug)]
pub struct Pin {
    pinned: bool,
    away: bool,
    /// When the reader last did something that scrolls: a wheel, a touch, a key, a
    /// pointer press. `None` until they do.
    last_user: Option<Instant>,
}

impl Default for Pin {
    fn default() -> Self {
        Pin::new()
    }
}

impl Pin {
    /// A transcript opens at its latest item, with nothing to jump back to.
    pub fn new() -> Self {
        Pin {
            pinned: true,
            away: false,
            last_user: None,
        }
    }

    /// Whether the list is following its tail.
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Whether "↓ Jump to latest" should be showing.
    pub fn is_away(&self) -> bool {
        self.away
    }

    /// The reader did something that scrolls. Nothing moves here: it only opens
    /// the window in which a scroll counts as theirs.
    pub fn touched(&mut self) {
        self.touched_at(Instant::now());
    }

    /// The same, at a stated instant — which is how the window is tested.
    pub fn touched_at(&mut self, now: Instant) {
        self.last_user = Some(now);
    }

    /// A scroll happened, `gap` pixels from the bottom. What to do about it.
    ///
    /// The design's `onScroll`: a scroll that is not the reader's, while the list
    /// is following its tail, is the layout moving — snap back. Anything else
    /// decides the two flags from where the reader now is.
    pub fn on_scroll(&mut self, gap: f32) -> Action {
        self.on_scroll_at(gap, Instant::now())
    }

    /// The same, at a stated instant.
    pub fn on_scroll_at(&mut self, gap: f32, now: Instant) -> Action {
        let by_user = self
            .last_user
            .is_some_and(|last| now.saturating_duration_since(last) < USER_WINDOW);
        if self.pinned && !by_user {
            if gap > SLACK {
                return Action::SnapToBottom;
            }
            return Action::Leave;
        }
        self.pinned = gap <= SLACK;
        self.away = gap > JUMP_AT;
        Action::Leave
    }

    /// The reader pressed "↓ Jump to latest": follow the tail again from here,
    /// with nothing to jump back to. The scroll itself is the view's.
    pub fn jumped(&mut self) {
        self.pinned = true;
        self.away = false;
    }

    /// The same, without the row that was asked for.
    ///
    /// A reader who scrolls away, then switches agent, then comes back is looking
    /// at a different transcript: the new one opens at its latest item.
    pub fn reset(&mut self) {
        self.pinned = true;
        self.away = false;
        self.last_user = None;
    }

    /// How far the reader is from the bottom, as the design computes it.
    ///
    /// `content` is the scrollable height, `viewport` the visible one, and
    /// `offset` how far down the list has been scrolled — the design's
    /// `scrollHeight - scrollTop - clientHeight`, which is negative when the
    /// content is shorter than the pane (never away, never snapping).
    pub fn gap(content: f32, viewport: f32, offset: f32) -> f32 {
        content - offset - viewport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    /// The list opens at its latest item: pinned, nothing to jump back to.
    #[test]
    fn a_transcript_opens_at_its_latest() {
        let pin = Pin::new();
        assert!(pin.is_pinned());
        assert!(!pin.is_away());
    }

    /// The reader's own scroll unpins: they scroll far enough up and the list
    /// stops following, and "Jump to latest" shows once they are 240px away.
    #[test]
    fn the_readers_own_scroll_unpins_and_the_pill_appears_at_240px() {
        let mut pin = Pin::new();
        pin.touched_at(at(0));
        assert_eq!(pin.on_scroll_at(300., at(10)), Action::Leave);
        assert!(!pin.is_pinned(), "300px up is not the latest");
        assert!(pin.is_away(), "and it is past the 240px threshold");

        // Back within the slack: pinned again, and the pill goes.
        pin.touched_at(at(20));
        assert_eq!(pin.on_scroll_at(20., at(30)), Action::Leave);
        assert!(pin.is_pinned());
        assert!(!pin.is_away());
    }

    /// A scroll that is *not* the reader's, while the list is following, is the
    /// layout moving — the design pulls it back rather than letting the reader's
    /// place drift.
    #[test]
    fn a_layout_scroll_under_a_follower_is_pulled_back() {
        let mut pin = Pin::new();
        // Nobody touched anything: this is the composer growing, or an item
        // arriving.
        assert_eq!(pin.on_scroll(120.), Action::SnapToBottom);
        assert!(pin.is_pinned(), "still following");
        assert!(!pin.is_away(), "and no pill: the reader has not moved");

        // Within the slack there is nothing to fix.
        assert_eq!(pin.on_scroll(4.), Action::Leave);
    }

    /// Once the reader is away, a layout scroll leaves them where they are: they
    /// are reading something, and the list must not yank them back.
    #[test]
    fn a_layout_scroll_leaves_a_reader_where_they_are() {
        let mut pin = Pin::new();
        pin.touched_at(at(0));
        pin.on_scroll_at(600., at(0));
        assert!(!pin.is_pinned());
        // Long after the reader's own scroll, the list is still theirs.
        assert_eq!(pin.on_scroll_at(900., at(5_000)), Action::Leave);
        assert!(!pin.is_pinned());
        assert!(pin.is_away());
    }

    /// The window is 500ms: a scroll inside it is the reader's even while the
    /// list is following, and one after it is the layout's.
    #[test]
    fn the_user_window_is_500ms() {
        let mut pin = Pin::new();
        pin.touched_at(at(0));
        assert_eq!(pin.on_scroll_at(300., at(499)), Action::Leave);
        assert!(!pin.is_pinned(), "inside the window: theirs");

        let mut pin = Pin::new();
        pin.touched_at(at(0));
        assert_eq!(pin.on_scroll_at(300., at(500)), Action::SnapToBottom);
        assert!(pin.is_pinned(), "outside it: the layout's");
    }

    /// A reader who jumps back is following again, at once.
    #[test]
    fn jumping_back_follows_the_tail_again() {
        let mut pin = Pin::new();
        pin.touched_at(at(0));
        pin.on_scroll_at(600., at(1));
        assert!(pin.is_away());

        pin.jumped();
        assert!(pin.is_pinned());
        assert!(!pin.is_away());
        // And the next scroll from the layout — long after the reader's own — is
        // pulled back as before.
        assert_eq!(pin.on_scroll_at(400., at(10_000)), Action::SnapToBottom);
    }

    /// Switching agent resets the reader's place: another agent's transcript
    /// opens at its latest, whatever the last one's reader was doing.
    #[test]
    fn switching_agent_starts_at_the_latest_again() {
        let mut pin = Pin::new();
        pin.touched_at(at(0));
        pin.on_scroll_at(900., at(1));
        assert!(!pin.is_pinned() && pin.is_away());

        pin.reset();
        assert!(pin.is_pinned());
        assert!(!pin.is_away());
        assert_eq!(pin.on_scroll(400.), Action::SnapToBottom);
    }

    /// The distance from the bottom, as the design computes it — including the
    /// case that matters for a short transcript: content shorter than the pane
    /// has a negative gap, which is "at the latest" and never a pill.
    #[test]
    fn the_gap_is_the_designs() {
        assert_eq!(Pin::gap(1000., 400., 600.), 0.);
        assert_eq!(Pin::gap(1000., 400., 300.), 300.);
        assert_eq!(Pin::gap(1000., 400., 0.), 600.);
        assert!(Pin::gap(200., 400., 0.) < 0., "shorter than the pane");

        let mut pin = Pin::new();
        pin.touched_at(at(0));
        assert_eq!(
            pin.on_scroll_at(Pin::gap(200., 400., 0.), at(0)),
            Action::Leave
        );
        assert!(pin.is_pinned());
        assert!(!pin.is_away());
    }

    /// The three numbers are the design's.
    #[test]
    fn the_numbers_are_the_designs() {
        assert_eq!(SLACK, 24.);
        assert_eq!(JUMP_AT, 240.);
        assert_eq!(USER_WINDOW, Duration::from_millis(500));
    }
}
