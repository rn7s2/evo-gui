//! The window's remembered bounds (§2 rule 1, §7.1).
//!
//! `app.json` remembers where the window was. Those numbers are only a
//! *request*: they are clamped to the display's work area, so a window last used
//! on a display that is gone (a bigger one, a second monitor) still opens
//! somewhere visible and at a usable size.
//!
//! The other half of the same job lives here too: the window is gone by the time
//! `on_window_closed` runs, so [`Tracker`] watches its bounds as they change and
//! keeps the last ones for the quit path to persist.

use gpui_kit::{px, point, size, App, Bounds, Context, Pixels, Subscription, Window, WindowOptions};
use gpui_kit::WindowBounds as GpuiWindowBounds;

use store::app_state::{WindowBounds as StoredBounds, DEFAULT_SIZE, MIN_SIZE};

/// The options the app opens its window with: the stored bounds, clamped.
pub fn window_options(cx: &App, stored: StoredBounds) -> WindowOptions {
    let mut options = workspace::window_options(cx);
    let work_area = cx.primary_display().map(|display| display.visible_bounds());
    options.window_bounds = Some(window_bounds(stored, work_area));
    options
}

/// The stored bounds as a window request. Without a display (a headless test, a
/// session with no screen) they are taken as they are.
pub fn window_bounds(stored: StoredBounds, work_area: Option<Bounds<Pixels>>) -> GpuiWindowBounds {
    match work_area {
        Some(work_area) => GpuiWindowBounds::Windowed(clamp(stored, work_area)),
        None => GpuiWindowBounds::Windowed(Bounds::new(
            point(px(stored.x.unwrap_or(0.0)), px(stored.y.unwrap_or(0.0))),
            size(
                px(finite(stored.width, DEFAULT_SIZE.0)),
                px(finite(stored.height, DEFAULT_SIZE.1)),
            ),
        )),
    }
}

/// `stored` clamped into `work_area`: never larger than the area, never smaller
/// than the app's minimum (unless the area itself is), and never sticking out of
/// it. A window with no remembered position is centered.
pub fn clamp(stored: StoredBounds, work_area: Bounds<Pixels>) -> Bounds<Pixels> {
    let available_width = work_area.size.width.as_f32().max(1.0);
    let available_height = work_area.size.height.as_f32().max(1.0);
    let offset_x = work_area.origin.x.as_f32();
    let offset_y = work_area.origin.y.as_f32();

    let width = axis_size(stored.width, DEFAULT_SIZE.0, MIN_SIZE.0, available_width);
    let height = axis_size(stored.height, DEFAULT_SIZE.1, MIN_SIZE.1, available_height);
    // `app.json` remembers screen coordinates; the clamp works in the work
    // area's own frame, so a window last seen on a display that is gone (or on
    // another one) is measured from this display's corner.
    let x = axis_offset(stored.x.map(|x| x - offset_x), width, available_width);
    let y = axis_offset(stored.y.map(|y| y - offset_y), height, available_height);

    Bounds::new(point(px(offset_x + x), px(offset_y + y)), size(px(width), px(height)))
}

/// How wide the window may be: `wanted`, or `fallback` when that is not a
/// usable number; at least `min`, never more than the work area. A work area
/// narrower than `min` wins — filling the screen beats overflowing it.
fn axis_size(wanted: f32, fallback: f32, min: f32, available: f32) -> f32 {
    let floor = min.min(available);
    finite(wanted, fallback).clamp(floor, available)
}

/// Where the window goes along one axis, in work-area coordinates: the remembered
/// position pulled back inside, or centered when there is none.
fn axis_offset(origin: Option<f32>, size: f32, available: f32) -> f32 {
    let slack = (available - size).max(0.0);
    match origin {
        Some(origin) if origin.is_finite() => origin.clamp(0.0, slack),
        _ => slack / 2.0,
    }
}

fn finite(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// Keeps the window's last bounds, for the quit path to persist.
///
/// `on_window_closed` runs after the window is inaccessible, so the bounds have
/// to be watched while it lives rather than read at the end.
pub struct Tracker {
    bounds: StoredBounds,
    _subscription: Subscription,
}

impl Tracker {
    pub fn new(window: &mut Window, cx: &mut Context<Tracker>) -> Tracker {
        let bounds = stored_from_window(window);
        let subscription = cx.observe_window_bounds(window, |tracker, window, _cx| {
            tracker.bounds = stored_from_window(window);
        });
        Tracker { bounds, _subscription: subscription }
    }

    /// The last bounds the window had.
    pub fn bounds(&self) -> StoredBounds {
        self.bounds
    }
}

/// A window's live bounds in `app.json`'s shape. A maximized or fullscreen
/// window reports its restore bounds, which is what reopening should use.
pub fn stored_from_window(window: &Window) -> StoredBounds {
    let (GpuiWindowBounds::Windowed(bounds)
    | GpuiWindowBounds::Maximized(bounds)
    | GpuiWindowBounds::Fullscreen(bounds)) = window.window_bounds();
    StoredBounds {
        x: Some(bounds.origin.x.as_f32()),
        y: Some(bounds.origin.y.as_f32()),
        width: bounds.size.width.as_f32(),
        height: bounds.size.height.as_f32(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    fn stored(x: Option<f32>, y: Option<f32>, w: f32, h: f32) -> StoredBounds {
        StoredBounds { x, y, width: w, height: h }
    }

    fn parts(bounds: Bounds<Pixels>) -> (f32, f32, f32, f32) {
        (
            bounds.origin.x.as_f32(),
            bounds.origin.y.as_f32(),
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
        )
    }

    #[test]
    fn a_window_bigger_than_the_display_fills_it() {
        // Remembered on a 1600x1000 display, opened on a 1000x700 one.
        let got = clamp(stored(Some(0.0), Some(0.0), 1600.0, 1000.0), area(0.0, 0.0, 1000.0, 700.0));
        assert_eq!(parts(got), (0.0, 0.0, 1000.0, 700.0));
    }

    #[test]
    fn an_offscreen_window_is_pulled_back_inside() {
        let got = clamp(stored(Some(5000.0), Some(5000.0), 1200.0, 800.0), area(0.0, 0.0, 2000.0, 1400.0));
        assert_eq!(parts(got), (800.0, 600.0, 1200.0, 800.0));
    }

    #[test]
    fn a_negative_position_is_pulled_back_inside() {
        let got = clamp(stored(Some(-400.0), Some(-300.0), 1200.0, 800.0), area(0.0, 0.0, 2000.0, 1400.0));
        assert_eq!(parts(got), (0.0, 0.0, 1200.0, 800.0));
    }

    #[test]
    fn a_window_smaller_than_the_minimum_is_grown_and_centered() {
        let got = clamp(stored(None, None, 200.0, 150.0), area(0.0, 0.0, 2000.0, 1400.0));
        assert_eq!(parts(got), (500.0, 350.0, 1000.0, 700.0));
    }

    #[test]
    fn a_display_smaller_than_the_minimum_wins() {
        let got = clamp(stored(Some(0.0), Some(0.0), 1000.0, 700.0), area(0.0, 0.0, 800.0, 600.0));
        assert_eq!(parts(got), (0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn a_secondary_display_keeps_its_origin() {
        // A display whose work area starts at (1600, 0): the window stays in it.
        let got = clamp(stored(Some(1700.0), Some(100.0), 1200.0, 800.0), area(1600.0, 0.0, 1920.0, 1080.0));
        assert_eq!(parts(got), (1700.0, 100.0, 1200.0, 800.0));
    }

    #[test]
    fn nonsense_dimensions_fall_back_to_the_defaults() {
        let got = clamp(stored(Some(f32::NAN), Some(0.0), f32::NAN, -5.0), area(0.0, 0.0, 2000.0, 1400.0));
        let (x, y, w, h) = parts(got);
        assert_eq!((w, h), (1600.0, 1000.0), "the default size, not the nonsense");
        assert_eq!(y, 0.0);
        assert_eq!(x, 200.0, "centered, since the remembered x was not a number");
    }

    #[test]
    fn without_a_display_the_stored_bounds_are_taken_as_they_are() {
        let bounds = window_bounds(stored(Some(40.0), Some(20.0), 1200.0, 800.0), None);
        let GpuiWindowBounds::Windowed(bounds) = bounds else { panic!("windowed") };
        assert_eq!(parts(bounds), (40.0, 20.0, 1200.0, 800.0));
    }
}
