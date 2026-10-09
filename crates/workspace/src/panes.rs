//! The tab page's splits (§7.3): the agent column, the conversation, and the
//! terminal on the right.
//!
//! The page is the agent list, everything the conversation needs beside it, and —
//! when it is open — the terminal at the other edge. The width does not belong to
//! a tab: the columns are the same in every tab, so a window has one of each of
//! them, the way a browser has one sidebar (§7.3, §14.4).
//!
//! What the widths may be lives in [`store::app_state`], with the schema that
//! remembers them; what a *window* may show lives here, because only this side
//! knows how wide the window is.

use store::app_state::{Panes, CENTER_MIN, LEFT_MAX, LEFT_MIN, RIGHT_MIN};

/// The widths fitted to a window: in their ranges, and then small enough that the
/// conversation keeps [`CENTER_MIN`] (§7.3).
///
/// A window narrowed under wide side columns would otherwise leave the transcript
/// with nothing, so the columns come in to make room — left first, then right. A
/// window wide again leaves them where they were — a resize is not a reason to grow
/// a column the reader sized by hand.
pub fn fit(panes: Panes, window_width: f32) -> Panes {
    let mut panes = Panes {
        left: panes.left.clamp(LEFT_MIN, LEFT_MAX),
        right: if panes.right <= 0.0 {
            0.0
        } else {
            panes.right.clamp(RIGHT_MIN, store::app_state::RIGHT_MAX)
        },
    };
    if !window_width.is_finite() {
        return panes;
    }
    // The conversation is what the page is for: the side columns give up room
    // down to their own minimums. Left shrinks first.
    let room = window_width - panes.right - CENTER_MIN;
    panes.left = panes.left.min(room.max(LEFT_MIN));
    // If even after shrinking left, center is still too small, shrink right too.
    let remaining = window_width - panes.left - CENTER_MIN;
    if panes.right > 0.0 && panes.right > remaining {
        panes.right = remaining.max(0.0);
        if panes.right > 0.0 && panes.right < RIGHT_MIN {
            panes.right = 0.0; // collapse if too small
        }
    }
    panes
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::app_state::{LEFT_DEFAULT, RIGHT_MAX, RIGHT_MIN};

    /// §7.3: a window wide enough shows the width as it is.
    #[test]
    fn a_wide_window_keeps_the_width_it_was_given() {
        let dragged = Panes {
            left: 300.0,
            right: 0.0,
        };
        assert_eq!(fit(dragged, 1600.0), dragged);
        assert_eq!(fit(dragged, 300.0 + CENTER_MIN), dragged);
    }

    /// The width is clamped to its range whatever the window says: a file that
    /// says 9000 is a 480-point column, not a page with no transcript.
    #[test]
    fn the_range_holds_whatever_is_asked_for() {
        assert_eq!(
            fit(Panes { left: 9000.0, right: 0.0 }, 4000.0),
            Panes {
                left: LEFT_MAX,
                right: 0.0
            }
        );
        assert_eq!(
            fit(Panes { left: -20.0, right: 0.0 }, 4000.0),
            Panes {
                left: LEFT_MIN,
                right: 0.0
            }
        );
    }

    /// A window too narrow to hold the column and the conversation takes from the
    /// column, and leaves the conversation its minimum.
    #[test]
    fn a_narrow_window_takes_from_the_column() {
        let fitted = fit(
            Panes {
                left: 480.0,
                right: 0.0,
            },
            700.0,
        );
        assert!(
            (fitted.left + CENTER_MIN - 700.0).abs() < 0.001,
            "the conversation is exactly its minimum: {fitted:?}"
        );
        assert!(fitted.left < 480.0 && fitted.left >= LEFT_MIN);
    }

    /// The default page in the app's smallest window (§7.1).
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

    /// A collapsed terminal pane does not affect the left column at all.
    #[test]
    fn a_collapsed_right_pane_does_not_affect_the_left() {
        let panes = Panes {
            left: 300.0,
            right: 0.0,
        };
        assert_eq!(fit(panes, 1600.0).right, 0.0);
        assert_eq!(fit(panes, 1600.0).left, 300.0);
    }

    /// An open terminal fits alongside the left column and the conversation.
    #[test]
    fn an_open_right_pane_fits_with_center_min() {
        let panes = Panes {
            left: 260.0,
            right: 400.0,
        };
        let fitted = fit(panes, 1200.0);
        assert!(fitted.left + CENTER_MIN + fitted.right <= 1200.0 + 0.001);
        assert!(fitted.right >= RIGHT_MIN || fitted.right == 0.0);
    }

    /// The open terminal holds its range: a value inside is kept, one outside
    /// is clamped.
    #[test]
    fn the_open_terminal_holds_its_range() {
        let panes = Panes {
            left: 260.0,
            right: 300.0,
        };
        let fitted = fit(panes, 2000.0);
        assert_eq!(fitted.right, 300.0);
        assert_eq!(fitted.left, 260.0);
        // Above max is clamped.
        let panes = Panes {
            left: 260.0,
            right: 9000.0,
        };
        let fitted = fit(panes, 2000.0);
        assert_eq!(fitted.right, RIGHT_MAX);
    }

    /// A narrow window collapses the terminal rather than leaving it below its
    /// minimum.
    #[test]
    fn a_narrow_window_collapses_the_terminal() {
        let panes = Panes {
            left: 260.0,
            right: 400.0,
        };
        // Window so narrow that even after shrinking left, there is not enough
        // room for CENTER_MIN + RIGHT_MIN.
        let fitted = fit(panes, 600.0);
        assert_eq!(fitted.right, 0.0, "the terminal is collapsed");
        assert!(fitted.left >= LEFT_MIN);
    }
}
