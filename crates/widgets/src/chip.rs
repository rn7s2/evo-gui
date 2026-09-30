//! The chip: one fact about a setting, in a quiet rounded region of its own.
//!
//! `Composer.css`'s `.chip`: 26px tall, a 999px pill, one step of the ink into the
//! input surface, 12px text with tabular figures so a readout does not jitter as
//! it counts (`font-variant-numeric: tabular-nums` is `tnum` among gpui's font
//! features). A chip that opens something (`chip-button`) darkens on hover and
//! stays darker while its drawer is open; a chip that states something is not a
//! button at all.

use gpui_kit::{
    div, px, AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use store::design::Palette;

use crate::paint::{mix, Rgb};

/// What a chip-button does when it is clicked.
pub type OnClick = Box<dyn Fn(&mut Window, &mut App)>;

/// The pill's height, radius and text size, and its padding.
pub const HEIGHT: f32 = 26.;
pub const RADIUS: f32 = 999.;
pub const FONT: f32 = 12.;
pub const PAD: f32 = 10.;
pub const GAP: f32 = 5.;

/// How far the ink steps into the surface: at rest, hovered, and open.
pub const REST_MIX: f32 = 6.;
pub const HOVER_MIX: f32 = 11.;
pub const OPEN_MIX: f32 = 14.;

/// One fact, as a chip.
pub struct Chip {
    id: ElementId,
    label: SharedString,
    dim: Option<SharedString>,
    palette: &'static Palette,
    interactive: bool,
    open: bool,
    on_click: Option<OnClick>,
}

impl Chip {
    /// A chip that states something.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            dim: None,
            palette: &store::design::LIGHT,
            interactive: false,
            open: false,
            on_click: None,
        }
    }

    /// The second half of the fact, in the muted ink: `ctx 112k/200k (56%)` — the
    /// part the design sets in `chip-dim`.
    pub fn dim(mut self, dim: impl Into<SharedString>) -> Self {
        self.dim = Some(dim.into());
        self
    }

    pub fn palette(mut self, palette: &'static Palette) -> Self {
        self.palette = palette;
        self
    }

    /// Make it a `chip-button`: it darkens on hover and clicks.
    pub fn interactive(mut self, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.interactive = true;
        self.on_click = Some(Box::new(on_click));
        self
    }

    /// A `chip-button` whose drawer is open, which keeps the darker fill.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// The fill this chip draws with — the design's three states.
    pub fn fill(&self, hovered: bool) -> Rgb {
        let pct = if self.open {
            OPEN_MIX
        } else if hovered && self.interactive {
            HOVER_MIX
        } else {
            REST_MIX
        };
        mix(self.palette.fg, pct, self.palette.input)
    }

    pub fn render(self) -> AnyElement {
        let palette = self.palette;
        let interactive = self.interactive;
        // Both fills come from [`Chip::fill`], which is where the design's three
        // states and their precedence live: `.chip-button.open` is written after
        // `.chip-button:hover` in the sheet and has the same weight, so an open
        // chip keeps its deeper fill with the pointer on it.
        let base = self.fill(false);
        let hover = self.fill(true);

        let mut chip = div()
            .id(self.id)
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .gap(px(GAP))
            .px(px(PAD))
            .rounded(px(RADIUS))
            .bg(crate::paint::color(base))
            .text_size(px(FONT))
            // `font-variant-numeric: tabular-nums`: a count that ticks over does
            // not shift the chip around it.
            .font_features(tabular())
            .text_color(crate::paint::color(palette.fg))
            .child(self.label);
        if interactive {
            // The design keeps the arrow over its chrome (`.chip{cursor:default}`),
            // and a chip that opens a drawer is chrome like the rest.
            chip = chip.cursor_default();
        }
        if let Some(dim) = self.dim {
            chip = chip.child(
                div()
                    .text_color(crate::paint::color(palette.muted_fg))
                    .child(dim),
            );
        }
        if interactive {
            chip = chip.hover(move |style| style.bg(crate::paint::color(hover)));
        }
        if let Some(on_click) = self.on_click {
            chip = chip.on_click(move |_event, window, cx| on_click(window, cx));
        }
        chip.into_any_element()
    }
}

/// The design's tabular figures, as gpui's font features name them.
fn tabular() -> gpui_kit::FontFeatures {
    gpui_kit::FontFeatures(std::sync::Arc::new(vec![("tnum".to_string(), 1)]))
}

/// The chip's fill at each state, for a caller drawing its own.
pub fn fill(palette: &Palette, hovered: bool, open: bool) -> Hsla {
    let pct = if open {
        OPEN_MIX
    } else if hovered {
        HOVER_MIX
    } else {
        REST_MIX
    };
    crate::paint::color(mix(palette.fg, pct, palette.input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Rgba;
    use store::design::{DARK, LIGHT};

    /// `.chip{font-variant-numeric: tabular-nums}`: the figures a count ticks over
    /// in do not change width.
    #[test]
    fn a_chip_counts_in_tabular_figures() {
        assert_eq!(tabular().tag_value_list(), [("tnum".to_string(), 1)]);
    }

    /// The three states are three different fills, each a step further in — and
    /// the chips of both themes are one rule.
    ///
    /// `render` draws exactly these two ([`Chip::fill`] with and without the
    /// pointer), so what this pins is what the screen shows: an open chip is the
    /// open fill even with the pointer on it, the way `.chip-button.open` beats
    /// `.chip-button:hover` in the design's sheet.
    #[test]
    fn a_chip_has_three_fills_and_the_open_one_wins() {
        for palette in [&LIGHT, &DARK] {
            let rest = Rgba::from(fill(palette, false, false));
            let hovered = Rgba::from(fill(palette, true, false));
            let opened = Rgba::from(fill(palette, false, true));
            assert_ne!(rest.r, hovered.r);
            assert_ne!(hovered.r, opened.r);
            let opened_hovered = Rgba::from(fill(palette, true, true));
            assert_eq!(opened_hovered.r, opened.r, "open outranks hover");
        }
    }

    /// A chip is a pill at 26px with 12px text, as the design draws it.
    #[test]
    fn the_chips_numbers_are_the_designs() {
        assert_eq!(HEIGHT, 26.);
        assert_eq!(FONT, 12.);
        assert_eq!(REST_MIX, 6.);
        assert_eq!(HOVER_MIX, 11.);
        assert_eq!(OPEN_MIX, 14.);
        assert!(opened_fill_is_darker());
    }

    fn opened_fill_is_darker() -> bool {
        let rest = mix(LIGHT.fg, REST_MIX, LIGHT.input);
        let open = mix(LIGHT.fg, OPEN_MIX, LIGHT.input);
        open.r < rest.r && open.g < rest.g
    }

    /// A chip builds as a statement and as a button.
    #[test]
    fn a_chip_builds_in_both_kinds() {
        let stated = Chip::new("ctx-chip", "ctx 112k/200k").dim("(56%)");
        assert!(!stated.interactive);
        assert_eq!(stated.fill(false), mix(LIGHT.fg, REST_MIX, LIGHT.input));
        let button = Chip::new("model-chip", "stub-a")
            .palette(&DARK)
            .interactive(|_window, _cx| {})
            .open(true);
        assert_eq!(button.fill(false), mix(DARK.fg, OPEN_MIX, DARK.input));
        let _ = (stated.render(), button.render());
    }
}
