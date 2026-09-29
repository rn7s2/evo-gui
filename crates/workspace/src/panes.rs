//! The tab page's two side columns (§7.3): one width each, for the whole app.
//!
//! The page is three columns — the agent list, the transcript, the composer — and
//! the splits between them are draggable. The widths do not belong to a tab: the
//! three columns are the same three columns in every tab, so a window has one pair
//! of them, the way a browser has one sidebar (§7.3, §14.4).
//!
//! What a width may be lives in [`store::app_state`], with the schema that
//! remembers it; what a *window* may show lives here, because only this side knows
//! how wide the window is.

use gpui_kit::Pixels;
use store::app_state::{Panes, CENTER_MIN, LEFT_MAX, LEFT_MIN, RIGHT_MAX, RIGHT_MIN};

/// Which of the page's two splits a divider is (§7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSide {
    Left,
    Right,
}

/// Which split a divider belongs to, from where the divider is drawn (§7.3).
///
/// The kit makes a handle per split but tells it nothing about itself — both are
/// the same element, drawn by the same appearance — so the divider's own place is
/// what says which side it is. The page begins at the window's left edge, so the
/// first split stands at the left column's width and the second at the width of
/// both columns before it; a divider is whichever of those it is nearer.
pub fn side_of(divider_x: f32, sizes: &[Pixels]) -> PaneSide {
    let (Some(left), Some(center)) = (sizes.first(), sizes.get(1)) else {
        return PaneSide::Left;
    };
    let first = left.as_f32();
    let second = first + center.as_f32();
    if (divider_x - first).abs() <= (divider_x - second).abs() {
        PaneSide::Left
    } else {
        PaneSide::Right
    }
}

/// The columns fitted to a window: the widths in their ranges, and then small
/// enough that the middle column keeps [`CENTER_MIN`] (§7.3).
///
/// The two side columns give up their room *together*, each in proportion to what
/// it has to give, so a window too narrow for the widths someone dragged neither
/// starves one column nor moves a split nobody touched. A window too narrow even
/// for the two minimums takes the rest from the middle: the app's smallest window
/// is wider than they need, but a layout rule that a resize can reach is worth
/// having an answer for.
pub fn fit(panes: Panes, window_width: f32) -> Panes {
    let panes = Panes {
        left: panes.left.clamp(LEFT_MIN, LEFT_MAX),
        right: panes.right.clamp(RIGHT_MIN, RIGHT_MAX),
    };
    if !window_width.is_finite() {
        return panes;
    }
    let over = panes.left + panes.right + CENTER_MIN - window_width;
    if over <= 0.0 {
        return panes;
    }
    let left_slack = panes.left - LEFT_MIN;
    let right_slack = panes.right - RIGHT_MIN;
    let slack = left_slack + right_slack;
    if slack <= 0.0 {
        // Nothing left to give: the middle column takes the rest, which is what
        // the panels do once their minimums are reached anyway.
        return panes;
    }
    let take = over.min(slack);
    let from_left = take * left_slack / slack;
    Panes {
        left: panes.left - from_left,
        right: panes.right - (take - from_left),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::app_state::{LEFT_DEFAULT, RIGHT_DEFAULT};

    /// §7.3: a window wide enough shows the widths as they are.
    #[test]
    fn a_wide_window_keeps_the_widths_it_was_given() {
        let dragged = Panes {
            left: 300.0,
            right: 500.0,
        };
        assert_eq!(fit(dragged, 1600.0), dragged);
        assert_eq!(fit(dragged, 300.0 + 500.0 + CENTER_MIN), dragged);
    }

    /// The widths are clamped to their ranges whatever the window says: a file
    /// that says 9000 is a 480-point column, not a page with no transcript.
    #[test]
    fn the_ranges_hold_whatever_is_asked_for() {
        assert_eq!(
            fit(
                Panes {
                    left: 9000.0,
                    right: -20.0
                },
                4000.0
            ),
            Panes {
                left: LEFT_MAX,
                right: RIGHT_MIN
            }
        );
    }

    /// A window too narrow for what was dragged takes from *both* sides, in
    /// proportion to what each has to give — and leaves the middle its minimum.
    #[test]
    fn a_narrow_window_takes_from_both_sides() {
        let dragged = Panes {
            left: 480.0,
            right: 640.0,
        };
        let fitted = fit(dragged, 1000.0);
        assert!(
            (fitted.left + fitted.right + CENTER_MIN - 1000.0).abs() < 0.001,
            "the middle column is exactly its minimum: {fitted:?}"
        );
        assert!(fitted.left < dragged.left && fitted.right < dragged.right);
        assert!(
            fitted.left >= LEFT_MIN && fitted.right >= RIGHT_MIN,
            "and neither went below its own minimum: {fitted:?}"
        );
        // What each gave up is proportional to what it had to give.
        let gave_left = dragged.left - fitted.left;
        let gave_right = dragged.right - fitted.right;
        let slack_left = dragged.left - LEFT_MIN;
        let slack_right = dragged.right - RIGHT_MIN;
        assert!(
            (gave_left / slack_left - gave_right / slack_right).abs() < 0.001,
            "{gave_left} of {slack_left} against {gave_right} of {slack_right}"
        );
    }

    /// The default page in the app's smallest window (§7.1): the two columns come
    /// in, and everything still fits.
    #[test]
    fn the_smallest_window_still_fits_everything() {
        let fitted = fit(Panes::default(), 1000.0);
        assert!(
            fitted.left + fitted.right + CENTER_MIN <= 1000.0,
            "{fitted:?} leaves the middle column less than {CENTER_MIN}"
        );
        assert!(fitted.left <= LEFT_DEFAULT && fitted.right <= RIGHT_DEFAULT);
        assert!(fitted.left >= LEFT_MIN && fitted.right >= RIGHT_MIN);
        // A window narrower than even the two minimums is not a window the app
        // opens, but the answer is the middle column giving way, not a panic.
        let tiny = fit(Panes::default(), 300.0);
        assert_eq!(tiny.left, LEFT_MIN);
        assert_eq!(tiny.right, RIGHT_MIN);
    }
}
