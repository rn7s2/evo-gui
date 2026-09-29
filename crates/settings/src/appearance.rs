//! What `System` means, on the window the panel is in.
//!
//! `gpui_component` has two modes, light and dark; `System` is this app's own third answer,
//! worked out from what the window reports — the same mapping the app makes for its own
//! choice (§7.1), so a preview inside the panel and the window behind it agree.

use gpui_kit::component::ThemeMode;
use gpui_kit::WindowAppearance;
use store::Theme;

/// The mode to draw with, given the stored choice and what the system says.
///
/// Pure, so the decision can be read without a window.
pub fn mode_for(theme: Theme, appearance: WindowAppearance) -> ThemeMode {
    match theme {
        Theme::Light => ThemeMode::Light,
        Theme::Dark => ThemeMode::Dark,
        Theme::System => match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_the_window_and_the_two_choices_do_not() {
        assert_eq!(
            mode_for(Theme::Light, WindowAppearance::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            mode_for(Theme::Dark, WindowAppearance::Light),
            ThemeMode::Dark
        );
        for appearance in [
            WindowAppearance::Light,
            WindowAppearance::VibrantLight,
            WindowAppearance::Dark,
            WindowAppearance::VibrantDark,
        ] {
            let expected = if appearance == WindowAppearance::Dark
                || appearance == WindowAppearance::VibrantDark
            {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            assert_eq!(mode_for(Theme::System, appearance), expected);
        }
    }
}
