//! The small pieces of the evo-gui design that more than one surface draws.
//!
//! Four of them, each one a thing the design repeats: the dot that says an agent
//! is working, the effort slider, the chip that states one fact about a setting,
//! and the two glyphs the chrome strokes itself. They are ports of
//! `design/doc28`'s `Workspace.tsx` + `TabStrip.tsx` (`Dot` / `BusyDot` and the
//! close and add glyphs), `EffortSlider.tsx` + `EffortSlider.css`, and
//! `Composer.css` (`.chip`).
//!
//! ## What a caller has to give them
//!
//! The palette: `store::design::LIGHT` or `DARK`, whichever mode the window is
//! in (`doc.tsx`'s `L` and `DARK`). Every colour and every number the design
//! names comes from there — [`store::design`] is the one place they are written
//! down, and nothing here retypes one. A widget also takes the *surface* it sits
//! on where the design mixes against it, because that is not the window's: a chip
//! inside a card is mixed against the card, a busy dot against the row it rests
//! on.
//!
//! ## What is faithful, and where gpui differs
//!
//! Sizes, radii, colours, mixes (every `color-mix(in srgb, …)` is
//! [`paint::mix`]), timings and easings are the design's, including the
//! `cubic-bezier(.3,.7,.3,1)` on the slider ([`effort::cubic_bezier`]) and the
//! `(1 − cos(2πt/1600ms))/2` breath ([`dot::breath_k`]). Two CSS mechanisms have
//! a different name in gpui, and are noted where they are used: `:active` is not
//! a style modifier but a flag the widget carries, and a percentage of a parent
//! is `relative(…)` rather than `calc(var(--p) + 3px)`.

pub mod chip;
pub mod dot;
pub mod effort;
pub mod glyph;
pub mod paint;

pub use chip::Chip;
pub use dot::BreathingDot;
pub use effort::EffortSlider;
pub use paint::{color, faded, mix, wash, Rgb};
pub use store::design::{self, Palette};
