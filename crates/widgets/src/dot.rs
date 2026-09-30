//! The breathing dot: how a working agent looks, everywhere.
//!
//! `Workspace.tsx`'s `Dot` and `TabStrip.tsx`'s `BusyDot` are one thing: a working
//! agent fills the dot between the ink and the surface it sits on —
//! `k = (1 − cos(2πt/period))/2`, so both ends ease — ringed with `BUSY_RING` of
//! ink at 45%; an idle one is an `IDLE_RING` ring in the muted ink.
//!
//! The numbers are `store::design`'s (`DOT`, `IDLE_RING`, `BUSY_RING`,
//! `IDLE_OPACITY`, `BUSY_PERIOD_MS`). The two a caller chooses are the surface the
//! dot rests on and how long a breath takes.

use std::f32::consts::PI;
use std::time::Duration;

use gpui_kit::{
    div, px, Animation, AnimationExt as _, AnyElement, ElementId, Hsla, IntoElement as _,
    SharedString, Styled as _,
};
use store::design::{Palette, BUSY_PERIOD_MS, BUSY_RING, DOT, IDLE_OPACITY, IDLE_RING};

use crate::paint::{color, mix_ink, wash, Rgb};

/// How long one breath lasts: `标签栏.工作周期`, 1600ms.
pub const BREATH_PERIOD: Duration = Duration::from_millis(BUSY_PERIOD_MS as u64);

/// The ink a busy dot's ring is drawn at: `color-mix(in srgb, var(--fg) 45%,
/// transparent)`.
const RING_MIX: f32 = 45.;

/// How lit the dot is at `delta` through the breath, in `0..=1`.
///
/// The design's cosine, not a linear ramp: `(1 − cos(2πt/period))/2` is the pulse
/// CSS would not give it — an `opacity` animation runs in and out on a straight
/// line, which reads as a blink rather than a breath.
pub fn breath_k(delta: f32) -> f32 {
    (1. - (2. * PI * delta).cos()) / 2.
}

/// The busy dot's fill at `delta`: `k` of the ink over the surface it sits on —
/// black to white on an active tab in the light theme, black to grey on a
/// background one (one rule, both ends).
pub fn breath_fill(fg: Rgb, surface: Rgb, delta: f32) -> Rgb {
    crate::paint::mix(fg, breath_k(delta) * 100., surface)
}

/// The same rule on two colours a widget is already drawing with: what an
/// overridden ink mixes towards.
pub fn breath_between(ink: Hsla, surface: Hsla, delta: f32) -> Hsla {
    mix_ink(ink, breath_k(delta) * 100., surface)
}

/// `color-mix(in srgb, ink pct%, transparent)` on a colour already in hand.
fn wash_ink(ink: Hsla, pct: f32) -> Hsla {
    gpui_kit::Hsla {
        a: ink.a * (pct / 100.).clamp(0., 1.),
        ..ink
    }
}

/// A working agent's dot, or an idle agent's ring.
///
/// One widget for both states, because the design reserves one slot for them: the
/// tab strip, a lane row and an agent header all keep the same `DOT` and turn it
/// to one or the other.
pub struct BreathingDot {
    id: ElementId,
    busy: bool,
    /// The ink a busy dot fills towards, and the idle ring's colour unless
    /// [`BreathingDot::ring`] overrides it.
    ink: Hsla,
    /// The colour an idle dot's ring is drawn in. `currentColor` in the design:
    /// the foreground on a shown tab, the muted ink on a background one.
    ring: Hsla,
    /// The surface a busy dot mixes towards at the quiet end of its breath.
    surface: Hsla,
    size: f32,
    idle_opacity: f32,
    period: Duration,
}

impl BreathingDot {
    /// A dot for `busy`, naming the element it animates.
    ///
    /// The id is per slot (`"tab-busy-evo-1"`, `"lane-3-dot"`): the animation is
    /// keyed by it, so two dots sharing an id would share a phase.
    pub fn new(id: impl Into<ElementId>, busy: bool) -> Self {
        let light = &store::design::LIGHT;
        Self {
            id: id.into(),
            busy,
            ink: color(light.fg),
            ring: color(light.muted_fg),
            surface: color(light.muted),
            size: DOT,
            idle_opacity: IDLE_OPACITY,
            period: BREATH_PERIOD,
        }
    }

    /// The mode's palette: the ink, the idle ring (`muted_fg`) and the surface
    /// (`muted`, which is the strip) in one call.
    pub fn palette(mut self, palette: &'static Palette) -> Self {
        self.ink = color(palette.fg);
        self.ring = color(palette.muted_fg);
        self.surface = color(palette.muted);
        self
    }

    /// The ink a busy dot fills towards — the foreground unless a caller says
    /// otherwise.
    pub fn ink(mut self, ink: impl Into<Hsla>) -> Self {
        self.ink = ink.into();
        self
    }

    /// The colour an idle dot's ring is drawn in, on its own.
    ///
    /// The design writes the ring as `currentColor`: the foreground on the tab
    /// being shown and the muted ink on the others, so the two are set apart by
    /// weight as well as by fill.
    pub fn ring(mut self, ring: impl Into<Hsla>) -> Self {
        self.ring = ring.into();
        self
    }

    /// The surface the dot rests on: what a busy dot mixes towards at the quiet
    /// end of its breath. `--row-surface` in the design — a lane row mixes against
    /// the sidebar, a selected row against the row's own fill, a tab against the
    /// strip. Defaults to the palette's `muted`, which is the strip.
    pub fn surface(mut self, surface: Rgb) -> Self {
        self.surface = color(surface);
        self
    }

    /// The same, for a caller that already has a colour.
    pub fn on(mut self, surface: impl Into<Hsla>) -> Self {
        self.surface = surface.into();
        self
    }

    /// How long one breath takes, where a caller has a reason to choose.
    pub fn period(mut self, period: Duration) -> Self {
        self.period = period;
        self
    }

    /// An idle ring's opacity: `IDLE_OPACITY` (the tab strip's 0.55) by default,
    /// 0.8 in a workspace row.
    pub fn idle_opacity(mut self, opacity: f32) -> Self {
        self.idle_opacity = opacity;
        self
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn render(self) -> AnyElement {
        if !self.busy {
            // Idle: a 1.3px ring in the muted ink. `box-sizing: border-box`, so
            // the ring is inside the dot and idle rows do not breathe.
            return div()
                .size(px(self.size))
                .flex_none()
                .rounded_full()
                .border(px(IDLE_RING))
                .border_color(self.ring)
                .opacity(self.idle_opacity)
                .into_any_element();
        }

        let (ink, surface) = (self.ink, self.surface);
        div()
            .size(px(self.size))
            .flex_none()
            .rounded_full()
            .border(px(BUSY_RING))
            .border_color(wash_ink(ink, RING_MIX))
            .with_animation(
                self.id,
                Animation::new(self.period).repeat(),
                move |dot, delta| dot.bg(breath_between(ink, surface, delta)),
            )
            .into_any_element()
    }
}

/// An animation id for a dot, so callers name their slots the same way.
pub fn dot_id(slot: impl Into<SharedString>) -> ElementId {
    ElementId::from(format!("busy-dot-{}", slot.into()))
}

/// The colour a busy dot has mid-breath, and the ring it carries — the two values
/// a test can hold the design to without a window.
pub fn mid_breath(palette: &Palette, surface: Rgb) -> Hsla {
    color(breath_fill(palette.fg, surface, 0.5))
}

pub fn ring(palette: &Palette) -> Hsla {
    wash(palette.fg, RING_MIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Rgba;
    use store::design::{DARK, LIGHT};

    #[test]
    fn the_breath_is_the_designs_cosine() {
        assert_eq!(breath_k(0.), 0.);
        assert!((breath_k(1.) - 0.).abs() < 1e-6);
        assert!((breath_k(0.5) - 1.).abs() < 1e-6);
        assert!((breath_k(0.25) - 0.5).abs() < 1e-6);
        for t in [0.1, 0.2, 0.3, 0.4] {
            assert!((breath_k(t) - breath_k(1. - t)).abs() < 1e-6, "t={t}");
            assert!(breath_k(t) < breath_k(t + 0.1), "t={t}");
        }
    }

    /// At the quiet end of its breath the dot *is* the surface; at the loud end it
    /// is the ink. One rule, and both themes fall out of it.
    #[test]
    fn the_dot_breathes_between_the_surface_and_the_ink() {
        for palette in [&LIGHT, &DARK] {
            assert_eq!(
                breath_fill(palette.fg, palette.tab_strip, 0.),
                palette.tab_strip,
                "quiet end is the surface"
            );
            assert_eq!(
                breath_fill(palette.fg, palette.tab_strip, 0.5),
                palette.fg,
                "loud end is the ink"
            );
            let mid = breath_fill(palette.fg, palette.tab_strip, 0.25);
            assert_ne!(mid, palette.fg);
            assert_ne!(mid, palette.tab_strip);
        }
    }

    /// The dot's numbers are the design's, and the two states do not look alike.
    #[test]
    fn the_dots_numbers_are_the_designs() {
        assert_eq!(DOT, 9.);
        assert_eq!(IDLE_RING, 1.3);
        assert_eq!(BUSY_RING, 1.);
        assert_eq!(BREATH_PERIOD.as_millis(), 1600);
        assert_eq!(IDLE_OPACITY, 0.55);
        let ring = Rgba::from(ring(&LIGHT));
        assert!((ring.a - 0.45).abs() < 1e-6, "a 45% wash: {ring:?}");
    }

    /// The dot mixes against the surface it was given, not the window's: that is
    /// what lets one widget serve a lane row, a selected lane and a tab.
    #[test]
    fn the_surface_is_the_callers() {
        let row = breath_fill(LIGHT.fg, LIGHT.sidebar, 0.25);
        let selected = breath_fill(LIGHT.fg, LIGHT.muted, 0.25);
        assert_ne!(row, selected);
        assert!(selected.r < row.r, "darker over a darker surface");
    }

    /// A widget builds in both states, at a caller's size, surface and period.
    #[test]
    fn a_dot_renders_in_both_states() {
        let busy = BreathingDot::new(dot_id("lane-1"), true)
            .palette(&LIGHT)
            .surface(LIGHT.sidebar);
        let idle = BreathingDot::new(dot_id("lane-2"), false)
            .palette(&DARK)
            .idle_opacity(0.8);
        let _ = (busy.render(), idle.render());
        let small = BreathingDot::new("lane-3", true)
            .size(12.)
            .period(Duration::from_millis(800));
        assert_eq!(Rgba::from(mid_breath(&LIGHT, LIGHT.tab_strip)).a, 1.0);
        let _ = small;
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    use gpui_kit::Rgba;

    /// The ring is `currentColor` in the design: a caller sets it on its own, and
    /// it is what an idle dot is drawn in — the shown tab in the foreground, the
    /// others in the muted ink.
    #[test]
    fn a_ring_can_be_set_apart_from_the_ink() {
        let dot = BreathingDot::new("tab-1", false)
            .palette(&store::design::LIGHT)
            .ink(color(store::design::LIGHT.fg))
            .ring(color(store::design::LIGHT.fg));
        assert_eq!(dot.ring, color(store::design::LIGHT.fg));

        // A busy dot: its fill breathes between the ink and the surface, and its
        // ring is 45% of the ink whatever the ink is.
        let busy = BreathingDot::new("tab-2", true)
            .palette(&store::design::LIGHT)
            .ink(color(store::design::DARK.fg))
            .surface(store::design::LIGHT.tab_strip);
        assert_eq!(Rgba::from(wash_ink(busy.ink, 45.)).a, 0.45);
        let mid = breath_between(busy.ink, busy.surface, 0.5);
        assert_eq!(mid, busy.ink, "the loud end is the ink it was given");
        assert_eq!(
            breath_between(busy.ink, busy.surface, 0.),
            busy.surface,
            "the quiet end is the surface"
        );
    }
}
