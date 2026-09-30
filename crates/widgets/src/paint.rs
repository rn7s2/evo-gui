//! The design's colours, ready to draw with.
//!
//! `store::design` owns the tokens — the palette and the numbers, `doc.tsx`'s `L`
//! and `DARK` — and has no renderer in it. These are the two lines that turn a
//! token into something gpui draws: a colour, and the design's
//! `color-mix(in srgb, A p%, B)`.

use gpui_kit::{Hsla, Rgba};
pub use store::design::Rgb;

/// A token, or a colour already, as a colour gpui draws with: what a widget takes
/// when a caller has either.
pub fn ink(c: impl Into<Hsla>) -> Hsla {
    c.into()
}

/// A token as a colour to draw with.
pub fn color(c: Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: 1.,
    }
    .into()
}

/// The same colour at `alpha`, for the design's `color-mix(…, transparent)`.
pub fn faded(c: Rgb, alpha: f32) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: alpha.clamp(0., 1.),
    }
    .into()
}

/// `color-mix(in srgb, a pct%, b)`, with the CSS's percentages.
pub fn mix(a: Rgb, pct: f32, b: Rgb) -> Rgb {
    a.mix(b, pct / 100.)
}

/// `color-mix(in srgb, c pct%, transparent)`: `c` at `pct`% alpha — the shape
/// every wash in the design's CSS takes.
pub fn wash(c: Rgb, pct: f32) -> Hsla {
    faded(c, pct / 100.)
}

/// [`mix`], on two colours that are already drawn with: what a widget mixes when
/// one of the two is an override rather than a token.
pub fn mix_ink(a: Hsla, pct: f32, b: Hsla) -> Hsla {
    let (a, b) = (gpui_kit::Rgba::from(a), gpui_kit::Rgba::from(b));
    let t = (pct / 100.).clamp(0., 1.);
    gpui_kit::Rgba {
        r: a.r * t + b.r * (1. - t),
        g: a.g * t + b.g * (1. - t),
        b: a.b * t + b.b * (1. - t),
        a: a.a * t + b.a * (1. - t),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_draws_at_its_own_colour() {
        let black = Rgba::from(color(Rgb::hex(0x000000)));
        assert_eq!((black.r, black.g, black.b), (0., 0., 0.));
        assert_eq!(black.a, 1.);
        let mid = Rgba::from(color(Rgb::hex(0x808080)));
        assert!((mid.r - 0.502).abs() < 0.002, "{mid:?}");
    }

    #[test]
    fn a_faded_token_keeps_its_colour_and_takes_the_alpha() {
        let ink = Rgba::from(faded(Rgb::hex(0x202020), 0.45));
        assert!((ink.a - 0.45).abs() < 1e-6);
        assert!((ink.r - 0.125).abs() < 0.002, "{ink:?}");
    }

    /// The design writes mixes as percentages; `store::design::Rgb::mix` takes a
    /// fraction, so this is the one place the two meet.
    #[test]
    fn a_percentage_mix_is_the_designs_mix() {
        let black = Rgb::hex(0x000000);
        let white = Rgb::hex(0xFFFFFF);
        assert_eq!(mix(black, 50., white), Rgb::new(128, 128, 128));
        assert_eq!(mix(black, 100., white), black);
        assert_eq!(mix(black, 0., white), white);
        // `.es-track` is 11% of the ink over nothing: the same colour, an alpha
        // the painter applies.
        assert_eq!(mix(black, 11., white), Rgb::new(227, 227, 227));
    }
}
