//! What a click on a link in a row does.
//!
//! The kit hands a click to the view's own handler, and without one it opens
//! whatever URL a link carries through `App::open_url` (`gpui-base`
//! `text_view::handle_link_click`). A transcript is model output, so the app
//! opens only what it means to: a web page, a mail composer, or a path that is
//! really there — never `file:`, `javascript:` or a scheme nobody knows.
//!
//! The cursor is the kit's own: a link under the pointer gets
//! `CursorStyle::PointingHand` (`text/inline.rs`, and `text/inline_object.rs`
//! for a linked object), and a link is drawn in the style's link colour with the
//! kit's own underline.
//!
//! The handler is the owner's when there is one — a test says what was pressed
//! without opening anything — and the platform's otherwise.

use gpui_kit::{App, ClickEvent, MouseButton, SharedString, Window};

use crate::linkify::{self, Target};
use crate::LinkHandler;

/// The click a link opens on, matching what the kit does without a handler:
/// a left or middle mouse button, a keyboard activation, a tap.
fn opens_on(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
        }
        ClickEvent::Keyboard(_) => true,
        ClickEvent::Touch(touch) => !touch.long_press,
    }
}

/// What the app does with a link when nobody said otherwise: the browser for an
/// address, and the platform's own application — Finder, for a folder — for a path.
fn open_with_platform(href: &str, cx: &mut App) {
    match linkify::target(href) {
        Some(Target::Web(url)) => cx.open_url(&url),
        Some(Target::Path(path)) => cx.open_with_system(&path),
        None => {}
    }
}

/// The handler a row's links are opened with: the owner's, or the platform's.
pub(crate) fn on_click(
    handler: Option<LinkHandler>,
) -> impl Fn(&SharedString, &ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
    move |href, event, window, cx| {
        if !opens_on(event) {
            return;
        }
        match &handler {
            Some(handler) => handler(href, window, cx),
            None => open_with_platform(href, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only what the app means to open is opened: a scheme nobody knows stays text,
    /// and so does a path that is not there.
    #[test]
    fn a_link_is_openable_only_when_it_is_something_to_open() {
        assert!(linkify::web("https://evo.dev/x"));
        assert!(linkify::web("HTTP://EVO.DEV/x"));
        assert!(linkify::web("mailto:evo@ruiqilei.com"));
        assert!(!linkify::web("file:///etc/passwd"));
        assert!(!linkify::web("javascript:alert(1)"));
        assert!(!linkify::web("/tmp/x"));
        assert_eq!(linkify::target("javascript:alert(1)"), None);
        assert_eq!(linkify::target("/tmp/definitely-not-here-9876"), None);
    }
}
