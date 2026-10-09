//! The tab page's one split (§7.3): the agent column, and the conversation.
//!
//! The page is two columns — the agent list, and everything the conversation
//! needs beside it — and the split between them is draggable. The width does not
//! belong to a tab: the two columns are the same two in every tab, so a window has
//! one of them, the way a browser has one sidebar (§7.3, §14.4).
//!
//! What the width may be lives in [`store::app_state`], with the schema that
//! remembers it; what a *window* may show lives here, because only this side knows
//! how wide the window is.

use store::app_state::{Panes, CENTER_MIN, LEFT_MAX, LEFT_MIN};

/// The width fitted to a window: in its range, and then small enough that the
/// conversation keeps [`CENTER_MIN`] (§7.3).
///
/// A window narrowed under a wide column would otherwise leave the transcript with
/// nothing, so the column comes in to make room. A window wide again leaves it
/// where it was — a resize is not a reason to grow a column the reader sized by
/// hand.
pub fn fit(panes: Panes, window_width: f32) -> Panes {
    let panes = Panes {
        left: panes.left.clamp(LEFT_MIN, LEFT_MAX),
    };
    if !window_width.is_finite() {
        return panes;
    }
    // The conversation is what the page is for: the column gives up its room down
    // to its own minimum, and past that it is the panels' own minimums that hold.
    let room = window_width - CENTER_MIN;
    Panes {
        left: panes.left.min(room.max(LEFT_MIN)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::app_state::LEFT_DEFAULT;

    /// §7.3: a window wide enough shows the width as it is.
    #[test]
    fn a_wide_window_keeps_the_width_it_was_given() {
        let dragged = Panes { left: 300.0 };
        assert_eq!(fit(dragged, 1600.0), dragged);
        assert_eq!(fit(dragged, 300.0 + CENTER_MIN), dragged);
    }

    /// The width is clamped to its range whatever the window says: a file that
    /// says 9000 is a 480-point column, not a page with no transcript.
    #[test]
    fn the_range_holds_whatever_is_asked_for() {
        assert_eq!(
            fit(Panes { left: 9000.0 }, 4000.0),
            Panes { left: LEFT_MAX }
        );
        assert_eq!(fit(Panes { left: -20.0 }, 4000.0), Panes { left: LEFT_MIN });
    }

    /// A window too narrow to hold the column and the conversation takes from the
    /// column, and leaves the conversation its minimum. The app's smallest window
    /// is wide enough that this never comes up — the widest column and the
    /// conversation's minimum fit it (§7.1) — but a window narrower than that is a
    /// window that still gets a page.
    #[test]
    fn a_narrow_window_takes_from_the_column() {
        let fitted = fit(Panes { left: 480.0 }, 700.0);
        assert!(
            (fitted.left + CENTER_MIN - 700.0).abs() < 0.001,
            "the conversation is exactly its minimum: {fitted:?}"
        );
        assert!(fitted.left < 480.0 && fitted.left >= LEFT_MIN);
    }

    /// The default page in the app's smallest window (§7.1): the column may come
    /// in a little, and never below its own minimum — a window narrower than even
    /// that is not a window the app opens, and the answer is still a page.
    #[test]
    fn the_smallest_window_still_fits_a_page() {
        let smallest = store::app_state::MIN_SIZE.0;
        let fitted = fit(Panes::default(), smallest);
        assert_eq!(
            fitted,
            Panes::default(),
            "the column opens where it always opens"
        );
        assert!(
            fitted.left + CENTER_MIN <= smallest,
            "{fitted:?} leaves the conversation less than {CENTER_MIN}"
        );
        assert!(fitted.left <= LEFT_DEFAULT && fitted.left >= LEFT_MIN);
        let tiny = fit(Panes::default(), 300.0);
        assert_eq!(tiny.left, LEFT_MIN);
    }
}
