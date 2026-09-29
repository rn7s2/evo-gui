//! The theme tokens the transcript draws with, resolved once per render.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, Pixels};

/// Theme colors and radii used by transcript rows and the todo panel.
///
/// Semantic tokens cover the surfaces and text roles; the status colors an ok /
/// warn / error row needs are still only on the legacy component palette, so
/// those come from there.
pub(crate) struct Palette {
    pub(crate) foreground: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) muted_foreground: Hsla,
    pub(crate) border: Hsla,
    pub(crate) primary: Hsla,
    pub(crate) destructive: Hsla,
    pub(crate) success: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) info: Hsla,
    pub(crate) radius: Pixels,
    pub(crate) radius_lg: Pixels,
}

impl Palette {
    pub(crate) fn from_app(cx: &App) -> Self {
        let theme = cx.theme();
        let colors = theme.semantic_tokens().colors;
        Self {
            foreground: colors.foreground,
            muted: colors.muted,
            muted_foreground: colors.muted_foreground,
            border: colors.border,
            primary: colors.primary,
            destructive: colors.destructive,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            radius: theme.radius,
            radius_lg: theme.radius_lg,
        }
    }
}
