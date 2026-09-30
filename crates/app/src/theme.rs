//! The design's palette, installed as this app's theme, and light-or-dark.
//!
//! The colours are the design's own (`store::design`, from `doc.tsx`'s `L` and
//! `DARK`): a warm "Dimmed Light 2026" ramp — the editor's deepest surface, the
//! chrome above it, inputs the lightest — and gpui-component's dark tokens beside
//! it. They are installed as the component theme's *light* and *dark* theme, so a
//! UI crate reads them through the theme it already reads (`cx.theme().border`),
//! and `Theme::change` keeps switching between them.
//!
//! What the kit has no token for — the tab strip's own surfaces, the table rules —
//! is `store::design` itself, next to the design's numbers.
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

use gpui_kit::component::{Theme as ComponentTheme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Subscription, Window, WindowAppearance};

use store::app_state::Theme as StoredTheme;
use store::design::{self, Palette, Rgb};

use crate::Shell;

/// The names the two palettes are registered under, and the theme file's own.
const LIGHT_THEME: &str = "Evo Light (Dimmed 2026)";
const DARK_THEME: &str = "Evo Dark";
const THEME_SET: &str = "Evo";

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

/// Install the design's two palettes on this app's theme, once.
///
/// Registering them and pointing [`ComponentTheme`]'s light and dark at them is
/// what makes every later `Theme::change` put *these* colours on the window: the
/// kit loads the mode's registered theme, and the registered one is the design's.
pub fn install(cx: &mut App) {
    if cx
        .try_global::<ComponentTheme>()
        .is_some_and(|theme| theme.light_theme.name.as_ref() == LIGHT_THEME)
    {
        return;
    }
    let body = theme_set();
    let registry = ThemeRegistry::global_mut(cx);
    if let Err(error) = registry.load_themes_from_str(&body) {
        // The kit's own defaults are a usable theme; a malformed file of ours is
        // a bug to fix, not a reason to refuse to draw.
        let log = cx.global::<Shell>().log.clone();
        log.error(format!(
            "theme: the design's palettes did not load: {error}"
        ));
        return;
    }
    let themes = registry.themes();
    let light = themes.get(LIGHT_THEME).cloned();
    let dark = themes.get(DARK_THEME).cloned();
    ComponentTheme::update(cx, |theme| {
        if let Some(light) = light {
            theme.light_theme = light;
        }
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
    });
}

/// Put the current answer on the window, and say in the log which one it is —
/// this line is how a launch's light/dark is checked from outside the UI.
fn apply(cx: &mut App, window: &mut Window) {
    install(cx);
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

/// The design's palette as the theme file gpui-component reads.
///
/// The numbers the kit keeps on the theme (the base and monospace sizes, the two
/// radii, the monospace family) come from `store::design` too, so there is one
/// place a size is written down.
fn theme_set() -> String {
    let themes = [
        theme(LIGHT_THEME, "light", &design::LIGHT),
        theme(DARK_THEME, "dark", &design::DARK),
    ];
    serde_json::json!({ "name": THEME_SET, "themes": themes }).to_string()
}

fn theme(name: &str, mode: &str, palette: &Palette) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "is_default": false,
        "mode": mode,
        "font.size": design::FONT_BASE,
        "mono_font.size": design::FONT_MONO,
        "mono_font.family": design::MONO_FONT,
        "radius": design::RADIUS as u32,
        "radius.lg": design::RADIUS_LG as u32,
        "shadow": true,
        "colors": colors(palette),
    })
}

/// The tokens the kit has, filled from the design's palette.
///
/// A field left out keeps the kit's own default for that mode
/// (`ThemeColor::apply_config` falls back per field), so this names what the
/// design fixes and nothing else.
///
/// Built as a list rather than one `json!`: the macro's nesting is the compiler's
/// recursion limit, and the design has more tokens than the limit has levels.
fn colors(p: &Palette) -> serde_json::Value {
    // A row's hover and selection, as the design's lane rows do it: the foreground
    // a few percent into the surface the row sits on.
    let row_hover = hex(p.fg.mix(p.sidebar, 0.05));
    let row_active = hex(p.fg.mix(p.sidebar, 0.09));
    let fg = hex(p.fg);
    let border = hex(p.border);
    let muted = hex(p.muted);
    let primary = hex(p.primary);
    let primary_fg = hex(p.primary_fg);
    let sidebar = hex(p.sidebar);
    let items: Vec<(&str, String)> = vec![
        ("background", hex(p.bg)),
        ("foreground", fg.clone()),
        // A panel that sits on the page, and the page itself: the design has one
        // deepest surface (`--bg`), and cards take `muted`/`secondary`.
        ("surface", hex(p.bg)),
        ("surface_foreground", fg.clone()),
        ("muted.background", muted.clone()),
        ("muted.foreground", hex(p.muted_fg)),
        ("border", border.clone()),
        ("primary.background", primary.clone()),
        ("primary.foreground", primary_fg.clone()),
        ("secondary.background", hex(p.secondary)),
        ("secondary.foreground", fg.clone()),
        ("accent.background", muted.clone()),
        ("accent.foreground", fg.clone()),
        ("info.background", hex(p.info)),
        ("info.foreground", primary_fg.clone()),
        ("warning.background", hex(p.warning)),
        ("warning.foreground", primary_fg.clone()),
        ("danger.background", hex(p.destructive)),
        ("danger.foreground", primary_fg.clone()),
        ("success.background", hex(p.success)),
        ("success.foreground", primary_fg.clone()),
        ("input.border", border.clone()),
        ("ring", primary.clone()),
        ("caret", fg.clone()),
        ("selection.background", muted.clone()),
        ("title_bar.background", hex(p.title_bar)),
        ("title_bar.border", border.clone()),
        ("tab.background", hex(p.tab_strip)),
        ("tab.active.background", hex(p.tab_active)),
        ("tab.active.foreground", fg.clone()),
        ("tab.foreground", hex(p.tab_ink)),
        ("tab_bar.background", hex(p.tab_strip)),
        ("popover.background", hex(p.input)),
        ("popover.foreground", fg.clone()),
        ("sidebar.background", sidebar.clone()),
        ("sidebar.foreground", fg.clone()),
        ("sidebar.border", border.clone()),
        ("sidebar.primary.background", primary.clone()),
        ("list.hover.background", row_hover),
        ("list.active.background", row_active),
        ("list.even.background", sidebar.clone()),
        ("list.head.background", sidebar.clone()),
        ("scrollbar.background", muted.clone()),
        ("scrollbar.thumb.background", border.clone()),
        (
            "scrollbar.thumb.hover.background",
            hex(p.fg.mix(p.border, 0.25)),
        ),
        ("switch.background", muted.clone()),
        ("switch.thumb.background", hex(p.input)),
        ("slider.background", muted.clone()),
        ("slider.thumb.background", primary.clone()),
        ("button.background", hex(p.secondary)),
        ("button.foreground", fg.clone()),
        ("group_box.background", hex(p.bg)),
        ("group_box.foreground", fg.clone()),
        ("status_bar.background", sidebar.clone()),
        ("progress.bar.background", primary.clone()),
        ("skeleton.background", muted.clone()),
        ("link", primary.clone()),
        ("link.hover", hex(p.fg.mix(p.primary, 0.25))),
        ("window.border", border.clone()),
        ("drag.border", primary.clone()),
    ];
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::String(value)))
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into()
}

/// `#RRGGBB`, the form the kit's colour parser takes.
fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::try_parse_color;

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

    /// The palette file is the kit's own schema, and every colour in it parses:
    /// a typo in a hex is a theme that silently keeps the kit's default.
    #[test]
    fn the_palettes_are_a_theme_the_kit_can_read() {
        let body = theme_set();
        let set: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        let themes = set["themes"].as_array().expect("themes");
        assert_eq!(themes.len(), 2);
        for theme in themes {
            assert_eq!(theme["radius"].as_u64(), Some(design::RADIUS as u64));
            assert_eq!(theme["radius.lg"].as_u64(), Some(design::RADIUS_LG as u64));
            assert_eq!(
                theme["mono_font.size"].as_f64(),
                Some(f64::from(design::FONT_MONO))
            );
            let colors = theme["colors"].as_object().expect("colors");
            assert!(colors.len() > 40, "the design's tokens, not three of them");
            for (key, value) in colors {
                let value = value.as_str().expect("a colour string");
                assert!(
                    try_parse_color(value).is_ok(),
                    "{key} = {value} is not a colour the kit reads"
                );
            }
        }
    }

    /// The two palettes are genuinely two: the design's light ramp and the dark
    /// tokens, and a mode switch is a different window.
    #[test]
    fn the_light_and_dark_palettes_differ_where_the_design_says_they_do() {
        assert_ne!(design::LIGHT.bg, design::DARK.bg);
        assert_ne!(design::LIGHT.tab_strip, design::DARK.tab_strip);
        assert_ne!(design::LIGHT.fg, design::DARK.fg);
        // The light ramp's order, which is the design's whole point: the editor's
        // surface is the deepest, the chrome sits above it, inputs are lightest.
        let scale = |c: Rgb| u32::from(c.r) + u32::from(c.g) + u32::from(c.b);
        assert!(scale(design::LIGHT.bg) < scale(design::LIGHT.sidebar));
        assert!(scale(design::LIGHT.sidebar) < scale(design::LIGHT.input));
        assert!(scale(design::LIGHT.tab_strip) < scale(design::LIGHT.tab_active));
    }
}
