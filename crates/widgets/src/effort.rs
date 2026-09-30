//! The effort slider: one rail for the whole app, ported from
//! `design/doc28/EffortSlider.tsx` + `EffortSlider.css`.
//!
//! A thin rail filled up to the choice, a notch at each inner level (the two ends
//! are the rail's own rounded caps), and a round thumb that snaps to the levels.
//! Click, drag, or use the arrows.
//!
//! Every number here is the design's: 22px tall, the rail inset 8px, a 4px track
//! at 11% ink and a 4px fill at 70%, 2px notches at 38% (75% of the input surface
//! once passed), a 16px thumb with two shadows, a 4px hover ring at 7%, a 3px
//! focus ring at 32% of the accent, and 140ms of `cubic-bezier(.3,.7,.3,1)` on
//! the fill's width and the thumb's position.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    div, px, relative, Animation, AnimationExt as _, AnyElement, App, Bounds, BoxShadow, ElementId,
    FocusHandle, InteractiveElement as _, IntoElement as _, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
};
use store::design::Palette;

use crate::paint::{color, wash, Rgb};

/// A re-render a caller lends the slider.
pub type Notify = Rc<dyn Fn(&mut App)>;
/// The name a screen reader reads for the rail — `EffortSlider.tsx`'s
/// `aria-label="Effort"`. There is one slider in the app, and this is what it is
/// called.
pub const ARIA_LABEL: &str = "Effort";

/// What a slider does when a click, a drag or an arrow picks a level.
pub type OnChange = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The five levels the design's slider offers, in order.
pub const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The slider's own height, and the rail's inset from each end.
pub const HEIGHT: f32 = 22.;
pub const RAIL_INSET: f32 = 8.;

/// The track and the fill: 4px tall, overhanging the rail by 3px at the thumb's
/// end so a thumb at level 0 still sits on a rounded cap.
pub const TRACK_HEIGHT: f32 = 4.;
pub const TRACK_OVERHANG: f32 = 3.;
pub const TRACK_RADIUS: f32 = 2.;

/// The thumb, and the notch each inner level puts on the rail.
pub const THUMB_SIZE: f32 = 16.;
pub const TICK_SIZE: f32 = 2.;

/// The rail's hover group: what makes the thumb's ring appear when the pointer is
/// anywhere on the slider.
const SLIDER_GROUP: &str = "effort-slider";

/// An element id for an animation of one part of a named widget:
/// `"<name>-<part>-motion"`.
///
/// An animation's id is separate from the id of the element it animates: sharing
/// one would key the element and its motion by the same name.
pub fn motion_id(name: &SharedString, part: &str) -> ElementId {
    ElementId::Name(format!("{name}-{part}-motion").into())
}

/// An element id for one part of a named widget: `"<name>-<part>"`.
///
/// The name is the caller's and the parts are fixed, so a caller's test finds the
/// slider, its rail or its thumb by the name the slider was built with, rather
/// than by an id it has to wrap the widget to give it.
pub fn part_id(name: &SharedString, part: &str) -> ElementId {
    ElementId::Name(format!("{name}-{part}").into())
}

/// How long a move takes, and the curve it takes it on.
pub const MOVE: Duration = Duration::from_millis(140);
pub const MOVE_X1: f32 = 0.3;
pub const MOVE_Y1: f32 = 0.7;
pub const MOVE_X2: f32 = 0.3;
pub const MOVE_Y2: f32 = 1.0;

/// The ink the thumb's shadows are cast in — the design's `rgba(40,28,10,…)`, a
/// warm shadow rather than black, so a thumb on the warm surface does not read as
/// a grey smudge.
pub const SHADOW_INK: Rgb = Rgb::new(0x28, 0x1c, 0x0a);

/// `cubic-bezier(x1, y1, x2, y2)` as CSS defines it: the easing a transition
/// names.
///
/// A Bézier whose control points' x both lie in `0..=1` is monotone in time, so
/// its value at `t` is the `y` at the parameter `u` where `x(u) = t`. Bisection
/// finds that `u`: stable at the curve's flat ends, and no derivative needed —
/// which is what CSS's own solvers do.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 + 'static {
    move |t: f32| {
        let t = t.clamp(0., 1.);
        let bezier = |a: f32, b: f32, u: f32| {
            3. * a * u * (1. - u) * (1. - u) + 3. * b * u * u * (1. - u) + u * u * u
        };
        let (mut lo, mut hi) = (0., 1.);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.;
            if bezier(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bezier(y1, y2, (lo + hi) / 2.)
    }
}

/// The level a click at `x` picks: the design's `Math.round` of the pointer's
/// position along the rail, clamped to the ends.
///
/// `left` and `width` are the rail's own bounds — the `.es-rail` box, 8px inside
/// the slider — which is exactly what the design measures against.
pub fn snap_index(x: f32, left: f32, width: f32, last: usize) -> usize {
    if width <= 0. {
        return 0;
    }
    let along = ((x - left) / width) * last as f32;
    (along.round().max(0.) as usize).min(last)
}

/// Where a level sits along the rail, in `0..=1` — the design's `--p`.
pub fn fraction(level: usize, last: usize) -> f32 {
    if last == 0 {
        0.
    } else {
        level as f32 / last as f32
    }
}

/// The level an arrow key asks for, or `None` for a key that is not the slider's.
///
/// Left and Down step back, Right and Up step forward, and neither goes past an
/// end: the design clamps rather than wrapping, and only a key that moves
/// something is swallowed (`preventDefault`).
pub fn step(level: usize, key: &str, last: usize) -> Option<usize> {
    match key {
        "left" | "down" => Some(level.saturating_sub(1)).filter(|next| *next != level),
        "right" | "up" => Some((level + 1).min(last)).filter(|next| *next != level),
        _ => None,
    }
}

/// The inner levels: the ends are the rail's own caps, so only these get a notch.
pub fn inner(last: usize) -> impl Iterator<Item = usize> {
    (1..last).collect::<Vec<_>>().into_iter()
}

/// The slider, as the composer's foot and the new-swarm page draw it.
///
/// Built per render — it is a handful of styles and a callback — so a caller
/// keeps no state of its own beyond the level itself. The move between one level
/// and the next is remembered by the slider's element ids, and the rail's own
/// bounds (which a click is measured against) by the element's prepaint.
pub struct EffortSlider {
    /// The caller's name for this slider, which its element ids are built from:
    /// the root is `"<id>"`, the rail `"<id>-rail"`, the thumb `"<id>-thumb"` —
    /// stable names a caller's test can look for without a shim around the slider.
    id: SharedString,
    levels: Vec<SharedString>,
    level: usize,
    palette: &'static Palette,
    focus: Option<FocusHandle>,
    /// The fraction last drawn, so a move animates from where the thumb really
    /// was rather than from wherever a re-render catches it.
    drawn: Rc<Cell<f32>>,
    /// The rail's bounds, as painted: what a click's `clientX` is measured from.
    rail: Rc<Cell<Bounds<Pixels>>>,
    /// Whether a button is down on the rail — the design's `:active`.
    pressed: Rc<Cell<bool>>,
    /// A re-render the caller lends the slider, so a press draws its thumb in the
    /// same frame rather than on the next one the caller happens to do.
    notify: Option<Notify>,
    on_change: Option<OnChange>,
}

impl EffortSlider {
    /// A slider for `level`, named by the caller: every element id it registers is
    /// built from this name, so a test can find the slider, its rail and its thumb.
    ///
    /// The levels are the design's five ([`LEVELS`]); a caller with the server's
    /// own list passes it to [`EffortSlider::with_levels`]. Whatever list is given
    /// is what is drawn: this widget has no opinion about which levels exist.
    pub fn new(id: impl Into<SharedString>, level: usize) -> Self {
        Self::with_levels(id, LEVELS.iter().map(|l| SharedString::from(*l)), level)
    }

    /// The same, with the levels the server's catalog actually lists.
    pub fn with_levels(
        id: impl Into<SharedString>,
        levels: impl IntoIterator<Item = impl Into<SharedString>>,
        level: usize,
    ) -> Self {
        let levels: Vec<SharedString> = levels.into_iter().map(Into::into).collect();
        Self {
            id: id.into(),
            level: level.min(levels.len().saturating_sub(1)),
            levels,
            palette: &store::design::LIGHT,
            focus: None,
            drawn: Rc::new(Cell::new(-1.)),
            rail: Rc::new(Cell::new(Bounds::default())),
            pressed: Rc::new(Cell::new(false)),
            notify: None,
            on_change: None,
        }
    }

    /// The mode's palette.
    pub fn palette(mut self, palette: &'static Palette) -> Self {
        self.palette = palette;
        self
    }

    /// The focus handle the rail takes when it is clicked: the app's own, so the
    /// arrows work without a second tab stop.
    pub fn focus(mut self, focus: FocusHandle) -> Self {
        self.focus = Some(focus);
        self
    }

    /// Called with the level a click, a drag or an arrow picked.
    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// A re-render the slider may ask for when the pointer goes down or up. A
    /// caller that passes one gets the pressed thumb in the same frame; one that
    /// does not gets it on its next render.
    pub fn notify(mut self, notify: impl Fn(&mut App) + 'static) -> Self {
        self.notify = Some(Rc::new(notify));
        self
    }

    /// The level's name, for a readout and for a caller's own label.
    pub fn label(&self) -> SharedString {
        self.levels
            .get(self.level)
            .cloned()
            .unwrap_or_else(|| SharedString::from(LEVELS[0]))
    }

    /// How many levels this slider has, for a caller drawing its own readout.
    pub fn levels(&self) -> &[SharedString] {
        &self.levels
    }

    /// The element id a caller's test finds this slider's root by: the name it was
    /// built with.
    pub fn root_id(&self) -> ElementId {
        ElementId::Name(self.id.clone())
    }

    /// The rail's own id — the part a click is measured against, and the one a
    /// test reads to see where the slider's geometry is.
    pub fn rail_id(&self) -> ElementId {
        part_id(&self.id, "rail")
    }

    /// The thumb's: where the level shows.
    pub fn thumb_id(&self) -> ElementId {
        part_id(&self.id, "thumb")
    }

    /// The fill's: what is drawn between the rail's start and the thumb.
    pub fn fill_id(&self) -> ElementId {
        part_id(&self.id, "fill")
    }

    pub fn render(self, window: &Window) -> AnyElement {
        let last = self.levels.len().saturating_sub(1);
        let ink = self.palette;
        let frac = fraction(self.level, last);
        let from = {
            let last_drawn = self.drawn.replace(frac);
            if last_drawn < 0. {
                frac
            } else {
                last_drawn
            }
        };
        let pressed = self.pressed.get();
        let focused = self.focus.as_ref().is_some_and(|f| f.is_focused(window));
        let easing = Rc::new(cubic_bezier(MOVE_X1, MOVE_Y1, MOVE_X2, MOVE_Y2));

        // A pick: the pointer's window x, measured against the rail as painted.
        let pick = Rc::new({
            let (bounds, on_change) = (self.rail.clone(), self.on_change.clone());
            move |x: f32, window: &mut Window, cx: &mut App| {
                let bounds = bounds.get();
                let width: f32 = bounds.size.width.into();
                if width <= 0. {
                    return;
                }
                let next = snap_index(x, bounds.origin.x.into(), width, last);
                if let Some(on_change) = &on_change {
                    on_change(next, window, cx);
                }
            }
        });

        let track = wash(ink.fg, 11.);
        let fill_ink = wash(ink.fg, 70.);
        let tick_idle = wash(ink.fg, 38.);
        let tick_passed = wash(ink.input, 75.);
        let thumb_fill = color(ink.input);
        let thumb_border = wash(ink.fg, 16.);
        let hover_ring = ink.fg;

        let mut rail = div()
            .id(part_id(&self.id, "rail"))
            .test_support()
            .absolute()
            .left(px(RAIL_INSET))
            .right(px(RAIL_INSET))
            .top(px(HEIGHT / 2.))
            .h(px(0.))
            .child(
                div()
                    .absolute()
                    .top(px(1. - TRACK_OVERHANG))
                    .left(px(-TRACK_OVERHANG))
                    .right(px(-TRACK_OVERHANG))
                    .h(px(TRACK_HEIGHT))
                    .rounded(px(TRACK_RADIUS))
                    .bg(track),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1. - TRACK_OVERHANG))
                    .left(px(-TRACK_OVERHANG))
                    .h(px(TRACK_HEIGHT))
                    .rounded(px(TRACK_RADIUS))
                    .bg(fill_ink)
                    .with_animation(
                        motion_id(&self.id, "fill"),
                        Animation::new(MOVE).with_easing({
                            let easing = easing.clone();
                            move |t| easing(t)
                        }),
                        move |el, delta| el.w(relative((from + (frac - from) * delta).max(0.))),
                    ),
            );

        for i in inner(last) {
            rail = rail.child(
                div()
                    .absolute()
                    .top(px(-TICK_SIZE / 2.))
                    .left(relative(fraction(i, last)))
                    .ml(px(-TICK_SIZE / 2.))
                    .size(px(TICK_SIZE))
                    .rounded_full()
                    .bg(if i <= self.level {
                        tick_passed
                    } else {
                        tick_idle
                    }),
            );
        }

        // The thumb: 16px, two warm shadows, and 1.08 while pressed — which gpui
        // has no transform for, so it grows by the same eighth and the shadows
        // deepen, as the design's `transform: scale(1.08)` looks.
        let thumb_size = if pressed {
            THUMB_SIZE * 1.08
        } else {
            THUMB_SIZE
        };
        let thumb = div()
            .id(part_id(&self.id, "thumb"))
            .test_support()
            .absolute()
            .top(px(0.))
            .left(relative(frac))
            .ml(px(-THUMB_SIZE / 2.))
            .mt(px(-THUMB_SIZE / 2.))
            .size(px(thumb_size))
            .rounded_full()
            .bg(thumb_fill)
            .border(px(1.))
            .border_color(thumb_border)
            .shadow(thumb_shadows(pressed, focused, ink))
            // `.es:hover .es-thumb`: gpui has no descendant selector, so the rail
            // is a group and the thumb asks what state it is in.
            .group_hover(SLIDER_GROUP, move |style| {
                style.shadow(hover_shadows(hover_ring))
            })
            .with_animation(
                motion_id(&self.id, "thumb"),
                Animation::new(MOVE).with_easing({
                    let easing = easing.clone();
                    move |t| easing(t)
                }),
                move |el, delta| el.left(relative(from + (frac - from) * delta)),
            );
        rail = rail.child(thumb);

        // The rail's own bounds, as painted: what a click's window x is measured
        // from — the design's `rail.getBoundingClientRect()`.
        let measure = {
            let rail = self.rail.clone();
            move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, _cx: &mut App| {
                if let Some(bounds) = bounds.first().copied() {
                    rail.set(bounds);
                }
            }
        };
        // The design's own accessibility: `role="slider"`, a name, and the value
        // as both a number and the level's word.
        let value = self.label();
        let mut slider = div()
            .on_children_prepainted(measure)
            .id(ElementId::Name(self.id.clone()))
            .role(gpui_kit::Role::Slider)
            .aria_label(ARIA_LABEL)
            .aria_numeric_value(self.level as f64)
            .aria_numeric_value_step(1.)
            .aria_value(value)
            .test_support()
            .group(SLIDER_GROUP)
            .relative()
            .h(px(HEIGHT))
            .w_full()
            .cursor_pointer()
            .child(rail)
            .on_mouse_down(MouseButton::Left, {
                let (pressed, notify, pick) =
                    (self.pressed.clone(), self.notify.clone(), pick.clone());
                move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    pressed.set(true);
                    if let Some(notify) = &notify {
                        notify(cx);
                    }
                    pick(event.position.x.into(), window, cx);
                }
            })
            .on_mouse_move({
                let pick = pick.clone();
                move |event: &MouseMoveEvent, window: &mut Window, cx: &mut App| {
                    // The design's `if (e.buttons & 1)`: a move drags only while a
                    // button is down, never on a bare hover.
                    if event.pressed_button == Some(MouseButton::Left) {
                        pick(event.position.x.into(), window, cx);
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let (pressed, notify) = (self.pressed.clone(), self.notify.clone());
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    pressed.set(false);
                    if let Some(notify) = &notify {
                        notify(cx);
                    }
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let pressed = self.pressed.clone();
                move |_event: &MouseUpEvent, _window: &mut Window, _cx: &mut App| {
                    pressed.set(false);
                }
            })
            .on_key_down({
                let (levels, on_change) = (self.levels.len(), self.on_change.clone());
                let level = self.level;
                move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                    let last = levels.saturating_sub(1);
                    if let Some(next) = step(level, &event.keystroke.key, last) {
                        cx.stop_propagation();
                        if let Some(on_change) = &on_change {
                            on_change(next, window, cx);
                        }
                    }
                }
            });
        if let Some(focus) = self.focus.clone() {
            slider = slider.track_focus(&focus);
        }
        slider.into_any_element()
    }
}

/// The two shadows the design's thumb carries — `0 1px 2px rgba(40,28,10,.18)`
/// and `0 2px 6px rgba(40,28,10,.10)` — deepened a little while pressed.
///
/// A focused rail draws the accent ring instead, as `:focus-visible` does; gpui
/// has no focus-visible, so focus is what shows it.
fn thumb_shadows(pressed: bool, focused: bool, palette: &Palette) -> Vec<BoxShadow> {
    let (near, far) = if pressed { (0.2, 0.12) } else { (0.18, 0.10) };
    let shadow = color(SHADOW_INK);
    let mut shadows = vec![
        BoxShadow::new(px(0.), px(1.), shadow.alpha(near)).blur_radius(px(1.)),
        BoxShadow::new(px(0.), px(2.), shadow.alpha(far)).blur_radius(px(3.)),
    ];
    if focused {
        shadows
            .push(BoxShadow::new(px(0.), px(0.), wash(palette.primary, 32.)).blur_radius(px(1.5)));
    }
    shadows
}

/// The ring a hovered rail draws around its thumb: `0 0 0 4px` of 7% ink.
fn hover_shadows(fg: Rgb) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), wash(fg, 7.)).blur_radius(px(4.))]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The design's `pick`: round the pointer's position along the rail to a
    /// level, clamped at both ends.
    #[test]
    fn a_click_snaps_to_the_nearest_level() {
        assert_eq!(snap_index(0., 0., 200., 4), 0);
        assert_eq!(snap_index(25., 0., 200., 4), 1);
        assert_eq!(snap_index(24., 0., 200., 4), 0, "below halfway rounds down");
        assert_eq!(snap_index(100., 0., 200., 4), 2);
        assert_eq!(snap_index(200., 0., 200., 4), 4);
        // Outside the rail it clamps rather than running away.
        assert_eq!(snap_index(-40., 0., 200., 4), 0);
        assert_eq!(snap_index(340., 0., 200., 4), 4);
        // The caller's rail is the one that counts: 8px in from a 200px slider
        // is level 0, not level 0.16.
        assert_eq!(snap_index(8., 8., 184., 4), 0);
        assert_eq!(snap_index(100., 8., 184., 4), 2);
        assert_eq!(snap_index(192., 8., 184., 4), 4);
    }

    /// A rail with no width yet — before the first layout — picks the first level
    /// rather than dividing by zero.
    #[test]
    fn a_rail_with_no_width_does_not_divide_by_zero() {
        assert_eq!(snap_index(10., 0., 0., 4), 0);
    }

    /// Arrows move one level, stop at the ends, and are left alone otherwise.
    #[test]
    fn the_arrows_step_and_stop_at_the_ends() {
        assert_eq!(step(2, "left", 4), Some(1));
        assert_eq!(step(2, "down", 4), Some(1));
        assert_eq!(step(2, "right", 4), Some(3));
        assert_eq!(step(2, "up", 4), Some(3));
        // At an end the key does nothing, so the event stays the window's.
        assert_eq!(step(0, "left", 4), None);
        assert_eq!(step(0, "down", 4), None);
        assert_eq!(step(4, "right", 4), None);
        assert_eq!(step(4, "up", 4), None);
        // Anything else is not the slider's.
        assert_eq!(step(2, "a", 4), None);
        assert_eq!(step(2, "escape", 4), None);
    }

    /// The levels are the design's five, in its order, and the rail's ends are
    /// its caps — so only the inner levels get a notch.
    #[test]
    fn the_levels_are_the_designs_five() {
        assert_eq!(LEVELS, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(fraction(0, 4), 0.);
        assert_eq!(fraction(2, 4), 0.5);
        assert_eq!(fraction(4, 4), 1.);
        assert_eq!(fraction(3, 0), 0., "a one-level slider has one place");
        assert_eq!(inner(4).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(inner(1).count(), 0);
        assert_eq!(inner(0).count(), 0);
    }

    /// `cubic-bezier(.3,.7,.3,1)`: exact at both ends, monotone, and front-loaded
    /// — a third of the time is half the distance, which is the feel the design
    /// is after.
    #[test]
    fn the_move_eases_the_way_the_design_says() {
        let ease = cubic_bezier(MOVE_X1, MOVE_Y1, MOVE_X2, MOVE_Y2);
        assert!(ease(0.).abs() < 1e-3);
        assert!((ease(1.) - 1.).abs() < 1e-3);
        let mut last = -1.;
        for i in 0..=20 {
            let v = ease(i as f32 / 20.);
            assert!(v >= last - 1e-4, "at {i}: {v} < {last}");
            last = v;
        }
        assert!(
            ease(0.33) > 0.5,
            "a third of the time, half the way: {}",
            ease(0.33)
        );
        assert!(ease(0.5) > 0.5, "{}", ease(0.5));
    }

    /// A linear curve is the identity, which is what makes the easing testable.
    #[test]
    fn a_linear_curve_is_the_identity() {
        let linear = cubic_bezier(0., 0., 1., 1.);
        for i in 0..=10 {
            let t = i as f32 / 10.;
            assert!((linear(t) - t).abs() < 1e-3, "at {t}");
        }
    }

    /// The widget builds with the design's levels and with a caller's, and a
    /// level past the end is the last one rather than a panic.
    #[test]
    fn a_slider_builds_at_any_level() {
        let slider = EffortSlider::new("effort", 3).palette(&store::design::LIGHT);
        assert_eq!(slider.label(), "xhigh");
        assert_eq!(slider.levels().len(), 5);
        let dark = EffortSlider::new("effort", 0).palette(&store::design::DARK);
        assert_eq!(dark.label(), "low");
        let custom = EffortSlider::with_levels("effort", ["a", "b"], 9);
        assert_eq!(custom.label(), "b", "a level past the end is the last one");
    }

    /// The design's shadows are warm, and a focused rail adds the accent ring.
    #[test]
    fn the_thumb_carries_the_designs_shadows() {
        let plain = thumb_shadows(false, false, &store::design::LIGHT);
        assert_eq!(plain.len(), 2);
        assert!(plain[0].offset.y > px(0.), "the near shadow sits under it");
        let focused = thumb_shadows(false, true, &store::design::LIGHT);
        assert_eq!(focused.len(), 3, "a focused rail rings the thumb");
        assert_eq!(SHADOW_INK, Rgb::new(0x28, 0x1c, 0x0a));
    }
}

/// The ids a caller's test looks for, and the levels it is handed.
#[cfg(test)]
mod naming {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{div, Context, IntoElement, Render, TestAppContext};

    /// Every element a test might look for is named from the name the slider was
    /// built with — no debug formatting, no wrapper needed to give it an id.
    #[test]
    fn the_element_ids_are_the_callers_name() {
        let slider = EffortSlider::new("effort", 1);
        assert_eq!(slider.root_id(), ElementId::from("effort"));
        assert_eq!(slider.rail_id(), ElementId::from("effort-rail"));
        assert_eq!(slider.thumb_id(), ElementId::from("effort-thumb"));
        assert_eq!(slider.fill_id(), ElementId::from("effort-fill"));
        for id in [slider.rail_id(), slider.thumb_id(), slider.fill_id()] {
            let ElementId::Name(name) = id else {
                panic!("a named id")
            };
            assert!(!name.contains("Name("), "{name}");
            assert!(!name.contains('('), "{name}");
        }
    }

    /// `off` is not a level evo offers, and the slider does not add one: it draws
    /// the ladder it is given, which is the catalog's.
    #[test]
    fn the_slider_draws_the_ladder_it_is_given() {
        let given = EffortSlider::with_levels("effort", ["low", "medium", "high"], 2);
        assert_eq!(given.levels(), ["low", "medium", "high"]);
        assert_eq!(given.label(), "high");
        assert!(!given.levels().iter().any(|level| level == "off"));
        // The design's own ladder is the same list, and has no `off` either.
        assert_eq!(LEVELS, ["low", "medium", "high", "xhigh", "max"]);
        // The design's five, handed in as a caller's list, are drawn as they are.
        let design = EffortSlider::with_levels("effort", LEVELS, 4);
        assert_eq!(design.levels(), LEVELS);
        assert_eq!(design.label(), "max");
    }

    /// A host: the slider on its own, as a caller mounts it — built per render,
    /// which is how a caller draws it.
    struct Host {
        level: usize,
        /// What the slider last asked for, which is what a caller's own state is
        /// fed from.
        seen: Rc<Cell<usize>>,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let seen = self.seen.clone();
            let slider = EffortSlider::new("effort", self.level)
                .palette(&store::design::LIGHT)
                .on_change(move |next, _window, _cx| seen.set(next));
            div()
                .id("slider-host")
                .test_support()
                .w(px(200.))
                .child(slider.render(window))
        }
    }

    /// In a window: the slider's root, rail and thumb are all findable by name.
    #[gpui_kit::test]
    fn a_rendered_slider_is_findable_by_name(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let seen = Rc::new(Cell::new(usize::MAX));
        let (_host, cx) = cx.add_window_view(|_window, _cx| Host {
            level: 2,
            seen: seen.clone(),
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            for id in ["slider-host", "effort", "effort-rail", "effort-thumb"] {
                let element = window.find(id);
                assert!(
                    element.bounds().size.width > px(0.),
                    "{id}: {:?}",
                    element.bounds()
                );
            }
            let rail = window.find("effort-rail").bounds();
            let thumb = window.find("effort-thumb").bounds();
            assert!(
                thumb.origin.x > rail.origin.x,
                "the thumb sits on the rail, not at its start: {thumb:?} vs {rail:?}"
            );

            // And clicking the slider — no wrapper, no shim — picks the level under
            // the pointer: the middle of the rail is the middle level.
            window.click("effort", cx);
        });
        assert_eq!(
            seen.get(),
            2,
            "a click in the middle of a five-level slider picks the middle level"
        );
    }
}
