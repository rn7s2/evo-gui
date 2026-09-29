//! What a click on a link in an assistant message does.
//!
//! The kit hands a click to the view's own handler, and without one it opens
//! whatever URL a link carries through `App::open_url` (`gpui-base`
//! `text_view::handle_link_click`). A transcript is model output, so the app
//! opens only what it means to — a web page, or a mail composer — and ignores
//! every other scheme rather than handing `file:`, `javascript:` or an unknown
//! scheme to the OS.
//!
//! The cursor is the kit's own: a link under the pointer gets
//! `CursorStyle::PointingHand` (`text/inline.rs`, and `text/inline_object.rs`
//! for a linked object).

use gpui_kit::{App, ClickEvent, MouseButton, SharedString, Window};

/// Whether the app opens `url`.
///
/// Only `http`, `https` and `mailto`, case-insensitively. A relative path or a
/// bare fragment is not something a desktop app can open either.
pub(crate) fn openable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

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

/// The handler an assistant message's links are opened with.
pub(crate) fn on_click(
) -> impl Fn(&SharedString, &ClickEvent, &mut Window, &mut App) + Send + Sync + 'static {
    |url, event, _window, cx| {
        if opens_on(event) && openable(url) {
            cx.open_url(url);
        }
    }
}
