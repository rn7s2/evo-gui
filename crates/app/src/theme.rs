//! §7.1: light or dark.
//!
//! The app follows the system's appearance — the macOS setting, which is also the
//! one the Dock and every other app follow — unless `app.json`'s `theme` says
//! otherwise, and it follows it *live*: switching the system appearance switches
//! the window with it. macOS reports the change on the window, which is where the
//! app listens.
//!
//! `gpui_component`'s themes come in two, [`ThemeMode::Light`] and
//! [`ThemeMode::Dark`]; `System` is this app's own third answer, worked out from
//! the window's appearance.

use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
use gpui_kit::{App, Subscription, Window, WindowAppearance};

use store::app_state::Theme as StoredTheme;

use crate::Shell;

/// What to show, given `app.json`'s choice and what the system says.
///
/// Pure, so the decision can be read without a window: `System` is the only
/// choice that looks at the system at all.
pub fn mode_for(choice: StoredTheme, appearance: WindowAppearance) -> ThemeMode {
    match choice {
        StoredTheme::Light => ThemeMode::Light,
        StoredTheme::Dark => ThemeMode::Dark,
        StoredTheme::System => match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        },
    }
}

/// Apply the choice now, and keep following the system while it is `System`.
///
/// The returned subscription is what makes the following live: it has to be kept
/// for as long as the window lives (the app keeps it on the [`Shell`]).
pub fn follow_appearance(cx: &mut App, window: &mut Window) -> Subscription {
    apply(cx, window);
    window.observe_window_appearance(|window, cx| {
        // Only an appearance the system decided is worth reacting to: with Light
        // or Dark chosen, the system's opinion is not the app's.
        if cx.global::<Shell>().theme == StoredTheme::System {
            apply(cx, window);
        }
    })
}

/// Put the current answer on the window, and say in the log which one it is —
/// this line is how a launch's light/dark is checked from outside the UI.
fn apply(cx: &mut App, window: &mut Window) {
    let (choice, log) = {
        let shell = cx.global::<Shell>();
        (shell.theme, shell.log.clone())
    };
    let mode = mode_for(choice, window.appearance());
    ComponentTheme::change(mode, Some(window), cx);
    log.info(format!(
        "theme: {} ({})",
        if mode.is_dark() { "dark" } else { "light" },
        match choice {
            StoredTheme::System => "the system appearance",
            StoredTheme::Light | StoredTheme::Dark => "app.json",
        }
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_what_the_window_reports() {
        for (appearance, expected) in [
            (WindowAppearance::Light, ThemeMode::Light),
            (WindowAppearance::VibrantLight, ThemeMode::Light),
            (WindowAppearance::Dark, ThemeMode::Dark),
            (WindowAppearance::VibrantDark, ThemeMode::Dark),
        ] {
            assert_eq!(mode_for(StoredTheme::System, appearance), expected);
        }
    }

    #[test]
    fn an_explicit_choice_ignores_the_system() {
        for appearance in [
            WindowAppearance::Light,
            WindowAppearance::VibrantLight,
            WindowAppearance::Dark,
            WindowAppearance::VibrantDark,
        ] {
            assert_eq!(mode_for(StoredTheme::Light, appearance), ThemeMode::Light);
            assert_eq!(mode_for(StoredTheme::Dark, appearance), ThemeMode::Dark);
        }
    }

    #[test]
    fn the_default_choice_is_the_system_and_the_default_mode_is_light() {
        // A file with no `theme` at all: `System`, and on a light window that is
        // light. (`WindowAppearance`'s own default is light.)
        assert_eq!(StoredTheme::default(), StoredTheme::System);
        assert_eq!(
            mode_for(StoredTheme::default(), WindowAppearance::default()),
            ThemeMode::Light
        );
    }
}
