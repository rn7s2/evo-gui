//! The window this app opens and the chrome it opens with (§7.1).

use gpui_kit::component::TitleBar;
use gpui_kit::{point, px, size, App, Bounds, Pixels, Size, WindowBounds, WindowOptions};

/// The size the window opens at when the display has room for it (§7.1).
pub const DEFAULT_WINDOW_SIZE: Size<Pixels> = size(px(1600.), px(1000.));

/// The smallest window that still fits the lane column, a transcript and the
/// composer side by side (§7.3).
pub const MIN_WINDOW_SIZE: Size<Pixels> = size(px(1000.), px(700.));

/// The bounds the window opens at: [`DEFAULT_WINDOW_SIZE`], clamped to the
/// display's work area — the visible area, without the dock or the menu bar —
/// and centered in it.
pub fn initial_window_bounds(cx: &App) -> WindowBounds {
    match cx.primary_display().map(|display| display.visible_bounds()) {
        Some(work_area) => WindowBounds::Windowed(fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area)),
        // Headless or display-less: take the default size rather than fail.
        None => WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), DEFAULT_WINDOW_SIZE)),
    }
}

/// `requested` centered in `work_area`, shrunk when the work area is smaller.
///
/// Never returns bounds that stick out of the work area, so a small display
/// still shows the whole window.
pub fn fit_to_work_area(requested: Size<Pixels>, work_area: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = requested.min(&work_area.size);
    let center = work_area.center();
    let offset = size / 2.0;
    Bounds::new(
        point(center.x - offset.width, center.y - offset.height),
        size,
    )
}

/// The options the app's window opens with.
///
/// `TitleBar::window_options()` is the base because the title bar is the tab
/// strip: it lets the title bar own dragging and double-clicking instead of the
/// system (§7.1).
pub fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(initial_window_bounds(cx)),
        window_min_size: Some(MIN_WINDOW_SIZE),
        ..TitleBar::window_options()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_roomy_work_area_keeps_the_default_size_centered() {
        let work_area = Bounds::new(point(px(0.), px(25.)), size(px(2560.), px(1400.)));
        let bounds = fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area);

        assert_eq!(bounds.size, DEFAULT_WINDOW_SIZE);
        assert_eq!(bounds.center(), work_area.center());
    }

    #[test]
    fn a_small_work_area_shrinks_the_window_inside_it() {
        // A 13" display: smaller than the size we ask for.
        let work_area = Bounds::new(point(px(0.), px(25.)), size(px(1440.), px(875.)));
        let bounds = fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area);

        assert_eq!(bounds.size, work_area.size);
        assert!(work_area.contains(&bounds.origin));
        assert!(bounds.right() <= work_area.right());
        assert!(bounds.bottom() <= work_area.bottom());
    }
}
