//! The effort slider: one rail for the whole app, ported from
//! `design/doc28/EffortSlider.tsx` + `EffortSlider.css`.
//!
//! A thin rail filled up to the choice, a notch at each inner level (the two ends
//! are the rail's own rounded caps), and a round thumb that snaps to the levels.
//! Click, drag, or use the arrows.
//!
//! Every number here is the design's: 22px tall, the rail inset 8px, a 4px track
//! at 11% ink and a 4px fill at 70%, 2px notches at 38% (75% of the input surface
//! once passed), a 16px thumb with two warm shadows, a 4px hover ring at 7%, a 3px
//! focus ring at 32% of the accent, 140ms of `cubic-bezier(.3,.7,.3,1)` on the
//! fill's width and the thumb's place, and 120ms of `ease` on the thumb's shadow
//! and on its scale.
//!
//! The motion is state, and the state is the caller's. This widget is built fresh
//! every render — a handful of styles and a callback — while a transition needs to
//! outlive one: where the thumb is drawn, where it is going, when it set off,
//! whether a button is down. A caller keeps one [`Motion`] per slider and hands it
//! in, and every change starts from whatever that motion is drawing at the moment
//! it changes — so a move interrupted half-way carries on from where it is, the
//! way a CSS transition does and the way a fresh element's animation cannot.
//!
//! gpui's own [`with_animation`](gpui_kit::AnimationExt::with_animation) is not
//! that: it keys an animation's start instant by the element id and plays it once,
//! so a constant id never restarts, and the id would have to change for every
//! level — with the "from" known at build time, which is the one thing a
//! transition is for.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{
    div, px, relative, AnyElement, App, Bounds, BoxShadow, ElementId, FocusHandle,
    InteractiveElement as _, IntoElement as _, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, SharedString,
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

/// The press, which the design draws as `transform: scale(1.08)` — and gpui has no
/// transform, so the thumb grows by the same eighth about its centre.
pub const PRESS_SCALE: f32 = 1.08;

/// The glove around the thumb: 4px at 7% ink under the pointer, and 3px at 32% of
/// the accent while the rail has focus.
pub const HOVER_RING: f32 = 4.;
pub const HOVER_RING_MIX: f32 = 7.;
pub const FOCUS_RING: f32 = 3.;
pub const FOCUS_RING_MIX: f32 = 32.;

/// The thumb's two warm shadows, `rgba(40,28,10,.18)` and `.10`, and the pair a
/// pointer over the slider deepens them to — the design's `.es:hover .es-thumb`.
pub const SHADOW_NEAR: f32 = 0.18;
pub const SHADOW_FAR: f32 = 0.10;
pub const SHADOW_NEAR_HOVER: f32 = 0.20;
pub const SHADOW_FAR_HOVER: f32 = 0.12;

/// The ink the thumb's shadows are cast in — the design's `rgba(40,28,10,…)`, a
/// warm shadow rather than black, so a thumb on the warm surface does not read as
/// a grey smudge.
pub const SHADOW_INK: Rgb = Rgb::new(0x28, 0x1c, 0x0a);

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

/// How long the thumb's shadow and its scale take: the design's `.12s ease`.
pub const FADE: Duration = Duration::from_millis(120);

/// `cubic-bezier(x1, y1, x2, y2)` at `t`, as CSS defines it: the easing a
/// transition names.
///
/// A Bézier whose control points' x both lie in `0..=1` is monotone in time, so
/// its value at `t` is the `y` at the parameter `u` where `x(u) = t`. Bisection
/// finds that `u`: stable at the curve's flat ends, and no derivative needed —
/// which is what CSS's own solvers do.
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    // The curve passes through (0,0) and (1,1), and bisection only reaches them
    // to within its own epsilon: a transition that has not set off, or has
    // arrived, is exactly where it was told to be.
    if t == 0. || t == 1. {
        return t;
    }
    let ease = |a: f32, b: f32, u: f32| {
        3. * a * u * (1. - u) * (1. - u) + 3. * b * u * u * (1. - u) + u * u * u
    };
    let (mut lo, mut hi) = (0., 1.);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.;
        if ease(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    ease(y1, y2, (lo + hi) / 2.)
}

/// The same curve as a closure, for the callers that hand one to
/// [`Animation::with_easing`](gpui_kit::Animation::with_easing).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 + 'static {
    move |t| bezier(x1, y1, x2, y2, t)
}

/// A named timing function. The two the slider moves on, as values a transition
/// can hold.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Curve {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Curve {
    /// The design's move: `cubic-bezier(.3,.7,.3,1)` — front-loaded, so a change
    /// is most of the way there in the first third of its time.
    const MOVE: Curve = Curve {
        x1: MOVE_X1,
        y1: MOVE_Y1,
        x2: MOVE_X2,
        y2: MOVE_Y2,
    };

    /// CSS's `ease`, `cubic-bezier(.25,.1,.25,1)`: what the thumb's shadow and its
    /// scale move on, and the curve `transition: … ease` names.
    const EASE: Curve = Curve {
        x1: 0.25,
        y1: 0.1,
        x2: 0.25,
        y2: 1.,
    };

    fn at(self, t: f32) -> f32 {
        bezier(self.x1, self.y1, self.x2, self.y2, t)
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

/// One of the design's transitions: a value on its way from where it was to where
/// it is going, over a fixed time and curve.
#[derive(Clone, Copy, Debug)]
struct Transition {
    from: f32,
    to: f32,
    /// When it set off. Only read while `moving`.
    started: Instant,
    duration: Duration,
    curve: Curve,
    /// False when there is nothing to animate: the value has nowhere to go.
    moving: bool,
}

impl Transition {
    /// Settled at `at`: a value with nothing in flight.
    fn settled(at: f32, duration: Duration, curve: Curve) -> Self {
        Self {
            from: at,
            to: at,
            started: Instant::now(),
            duration,
            curve,
            moving: false,
        }
    }

    /// Set off for `target`, from wherever this is now.
    ///
    /// A target it is already heading for changes nothing, so a re-render in the
    /// middle of a move is not a new move — which is what lets a caller draw the
    /// slider every frame without resetting the transition each time.
    fn go(&mut self, target: f32, now: Instant, reduce_motion: bool) {
        if target == self.to {
            return;
        }
        self.from = if reduce_motion {
            target
        } else {
            self.value(now)
        };
        self.to = target;
        self.started = now;
        self.moving = self.from != target;
    }

    fn value(&self, now: Instant) -> f32 {
        if !self.moving {
            return self.to;
        }
        self.from + (self.to - self.from) * self.curve.at(self.progress(now))
    }

    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        (elapsed / self.duration.as_secs_f32()).clamp(0., 1.)
    }

    fn running(&self, now: Instant) -> bool {
        self.moving && self.progress(now) < 1.
    }
}

/// What a frame of a slider is drawn for: where it should be, and the two things
/// the window knows rather than the widget.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Inputs {
    /// Where the thumb belongs, in `0..=1` — the level's [`fraction`].
    pub target: f32,
    /// Whether the rail has focus. The design's `:focus-visible`; gpui has no
    /// focus-visible, so focus is what shows the ring.
    pub focus: bool,
    /// gpui's own switch ([`App::reduce_motion`]): on, a change lands where it is
    /// going instead of travelling there.
    pub reduce_motion: bool,
}

/// One frame of a slider: the numbers the widget is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The thumb's place along the rail, `0..=1` — the design's `--p`, and the
    /// fill's width with it.
    pub position: f32,
    /// How pressed the thumb is, `0..=1` of [`PRESS_SCALE`]'s eighth.
    pub press: f32,
    /// How much of the hover ring is lit, `0..=1` of its 4px and 7%.
    pub hover: f32,
    /// How much of the focus ring is lit, `0..=1` of its 3px and 32%.
    pub focus: f32,
    /// Whether any of them is still on its way, so the widget knows to ask for
    /// the frame that draws it further along.
    pub moving: bool,
}

/// A slider's motion: what a caller keeps so that a change has somewhere to move
/// from.
///
/// One per slider, handed to [`EffortSlider::new`] (or `with_levels`) and shared
/// with the caller's own state. It carries the transitions, and the two flags the
/// widget's handlers write — a button down and the pointer on it — because the
/// widget itself is rebuilt every render and none of that outlives one.
#[derive(Debug)]
pub struct Motion {
    /// The thumb's place along the rail, and the fill's width with it.
    position: Cell<Transition>,
    /// The press, in `0..=1` of the design's eighth.
    press: Cell<Transition>,
    /// The hover ring, in `0..=1` of its 4px and 7%.
    hover: Cell<Transition>,
    /// The focus ring, in `0..=1` of its 3px and 32%.
    focus: Cell<Transition>,
    /// Whether a button is down on the rail — the design's `:active`.
    down: Cell<bool>,
    /// Whether the pointer is on the slider — the design's `:hover`.
    hovered: Cell<bool>,
    /// Whether a frame has been drawn yet. The first one takes its place rather
    /// than moving to it, because the design has no transition on mount.
    drawn: Cell<bool>,
}

impl Motion {
    /// A motion with nothing in flight; the first frame settles it where the
    /// slider already is.
    pub fn new() -> Self {
        Self {
            position: Cell::new(Transition::settled(0., MOVE, Curve::MOVE)),
            press: Cell::new(Transition::settled(0., FADE, Curve::EASE)),
            hover: Cell::new(Transition::settled(0., FADE, Curve::EASE)),
            focus: Cell::new(Transition::settled(0., FADE, Curve::EASE)),
            down: Cell::new(false),
            hovered: Cell::new(false),
            drawn: Cell::new(false),
        }
    }

    /// Whether a button is down on the rail, and whether the pointer is on it.
    pub fn down(&self) -> bool {
        self.down.get()
    }

    pub fn hovered(&self) -> bool {
        self.hovered.get()
    }

    /// The widget's own handlers write these.
    pub(crate) fn set_down(&self, down: bool) {
        self.down.set(down);
    }

    pub(crate) fn set_hovered(&self, hovered: bool) {
        self.hovered.set(hovered);
    }

    /// The numbers for the frame at `now`, with the transitions moved on.
    ///
    /// `now` is passed in rather than read from the clock here so a test can walk
    /// a 140ms move frame by frame without sleeping through it.
    pub fn frame(&self, now: Instant, inputs: Inputs) -> Frame {
        if !self.drawn.replace(true) {
            // The design transitions a change, not a mount: the first frame is
            // where the slider already is, whatever came before.
            self.position
                .set(Transition::settled(inputs.target, MOVE, Curve::MOVE));
            self.press
                .set(Transition::settled(lit(self.down.get()), FADE, Curve::EASE));
            self.hover.set(Transition::settled(
                lit(self.hovered.get()),
                FADE,
                Curve::EASE,
            ));
            self.focus
                .set(Transition::settled(lit(inputs.focus), FADE, Curve::EASE));
        }
        let position = self.step(&self.position, inputs.target, now, inputs.reduce_motion);
        let press = self.step(&self.press, lit(self.down.get()), now, inputs.reduce_motion);
        let hover = self.step(
            &self.hover,
            lit(self.hovered.get()),
            now,
            inputs.reduce_motion,
        );
        let focus = self.step(&self.focus, lit(inputs.focus), now, inputs.reduce_motion);
        Frame {
            position: position.value(now),
            press: press.value(now),
            hover: hover.value(now),
            focus: focus.value(now),
            moving: position.running(now)
                || press.running(now)
                || hover.running(now)
                || focus.running(now),
        }
    }

    /// Move one transition toward its target, and hand it back for this frame.
    fn step(
        &self,
        which: &Cell<Transition>,
        target: f32,
        now: Instant,
        reduce_motion: bool,
    ) -> Transition {
        let mut transition = which.get();
        transition.go(target, now, reduce_motion);
        which.set(transition);
        transition
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

/// A flag as the `0..=1` a transition moves on.
fn lit(on: bool) -> f32 {
    if on {
        1.
    } else {
        0.
    }
}

/// The slider, as the composer's foot and the new-swarm page draw it.
///
/// Built per render — it is a handful of styles and a callback — so everything
/// that has to outlive one lives in the caller's [`Motion`]: where the thumb is
/// drawn, what is in flight, and whether a button is down. The rail's own bounds,
/// which a click is measured against, live in the element's prepaint, where they
/// are written as the frame is painted.
pub struct EffortSlider {
    /// The caller's name for this slider, which its element ids are built from:
    /// the root is `"<id>"`, the rail `"<id>-rail"`, the thumb `"<id>-thumb"` —
    /// stable names a caller's test can look for without a shim around the slider.
    id: SharedString,
    levels: Vec<SharedString>,
    level: usize,
    palette: &'static Palette,
    focus: Option<FocusHandle>,
    /// The caller's motion: the transitions across renders.
    motion: Rc<Motion>,
    /// The rail's bounds, as painted: what a click's `clientX` is measured from.
    rail: Rc<Cell<Bounds<Pixels>>>,
    /// Whether the app is asking for less movement, so a change lands at once.
    reduce_motion: bool,
    /// A re-render the caller lends the slider, so a press draws its thumb in the
    /// same frame rather than on the next one the caller happens to do.
    notify: Option<Notify>,
    on_change: Option<OnChange>,
}

impl EffortSlider {
    /// A slider for `level` over the design's five levels, named by the caller:
    /// every element id it registers is built from this name, so a test can find
    /// the slider, its rail and its thumb.
    ///
    /// `motion` is the caller's own, and is what makes a change a *move*: keep one
    /// of these beside the level and hand the same one back on every render.
    pub fn new(id: impl Into<SharedString>, level: usize, motion: Rc<Motion>) -> Self {
        Self::with_levels(
            id,
            LEVELS.iter().map(|l| SharedString::from(*l)),
            level,
            motion,
        )
    }

    /// The same, with the levels the server's catalog actually lists. Whatever
    /// list is given is what is drawn: this widget has no opinion about which
    /// levels exist.
    pub fn with_levels(
        id: impl Into<SharedString>,
        levels: impl IntoIterator<Item = impl Into<SharedString>>,
        level: usize,
        motion: Rc<Motion>,
    ) -> Self {
        let levels: Vec<SharedString> = levels.into_iter().map(Into::into).collect();
        Self {
            id: id.into(),
            level: level.min(levels.len().saturating_sub(1)),
            levels,
            palette: &store::design::LIGHT,
            focus: None,
            motion,
            rail: Rc::new(Cell::new(Bounds::default())),
            reduce_motion: false,
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

    /// Whether the app is asking for less movement. A caller passes
    /// `cx.reduce_motion()`; on, every change lands where it is going.
    pub fn reduce_motion(mut self, reduce_motion: bool) -> Self {
        self.reduce_motion = reduce_motion;
        self
    }

    /// Called with the level a click, a drag or an arrow picked.
    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    /// A re-render the slider may ask for when the pointer goes down, up or over.
    /// A caller that passes one gets the pressed or ringed thumb in the same
    /// frame; one that does not gets it on its next render.
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
        self.render_at(window, Instant::now())
    }

    /// The same, drawn as it would be at `now`.
    ///
    /// A caller never needs this — the clock is the clock — but a test does: it is
    /// what lets a 140ms move be walked frame by frame instead of slept through.
    pub fn render_at(self, window: &Window, now: Instant) -> AnyElement {
        let last = self.levels.len().saturating_sub(1);
        let ink = self.palette;
        let focused = self.focus.as_ref().is_some_and(|f| f.is_focused(window));
        let frame = self.motion.frame(
            now,
            Inputs {
                target: fraction(self.level, last),
                focus: focused,
                reduce_motion: self.reduce_motion,
            },
        );

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
                    .id(part_id(&self.id, "fill"))
                    .test_support()
                    .absolute()
                    .top(px(1. - TRACK_OVERHANG))
                    .left(px(-TRACK_OVERHANG))
                    .h(px(TRACK_HEIGHT))
                    .rounded(px(TRACK_RADIUS))
                    .bg(fill_ink)
                    .w(relative(frame.position.max(0.))),
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

        // The thumb: 16px, two warm shadows, and 1.08 while pressed — grown about
        // its centre, since gpui has no transform, and eased over the design's
        // 120ms rather than snapped.
        let thumb_size = THUMB_SIZE * (1. + (PRESS_SCALE - 1.) * frame.press);
        let thumb = div()
            .id(part_id(&self.id, "thumb"))
            .test_support()
            .absolute()
            .top(px(0.))
            .left(relative(frame.position))
            .ml(px(-thumb_size / 2.))
            .mt(px(-thumb_size / 2.))
            .size(px(thumb_size))
            .rounded_full()
            .bg(thumb_fill)
            .border(px(1.))
            .border_color(thumb_border)
            .shadow(thumb_shadows(frame, ink));
        rail = rail.child(thumb);

        // The rail's own bounds, as painted: what a click's window x is measured
        // from — the design's `rail.getBoundingClientRect()`. And, while something
        // is in flight, the next frame: the same ask gpui's own animations make,
        // which is what keeps a move being drawn until it arrives.
        let measure = {
            let rail = self.rail.clone();
            let moving = frame.moving;
            move |bounds: Vec<Bounds<Pixels>>, window: &mut Window, _cx: &mut App| {
                if let Some(bounds) = bounds.first().copied() {
                    rail.set(bounds);
                }
                if moving {
                    window.request_animation_frame();
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
            .relative()
            .h(px(HEIGHT))
            .w_full()
            .cursor_pointer()
            .child(rail)
            .on_hover({
                let (motion, notify) = (self.motion.clone(), self.notify.clone());
                move |hovered: &bool, _window: &mut Window, cx: &mut App| {
                    motion.set_hovered(*hovered);
                    if let Some(notify) = &notify {
                        notify(cx);
                    }
                }
            })
            .on_mouse_down(MouseButton::Left, {
                let (motion, notify, pick) =
                    (self.motion.clone(), self.notify.clone(), pick.clone());
                move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    motion.set_down(true);
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
                let (motion, notify) = (self.motion.clone(), self.notify.clone());
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    motion.set_down(false);
                    if let Some(notify) = &notify {
                        notify(cx);
                    }
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let (motion, notify) = (self.motion.clone(), self.notify.clone());
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    motion.set_down(false);
                    if let Some(notify) = &notify {
                        notify(cx);
                    }
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

/// The thumb's shadows for one frame: the design's two warm ones, deepened as a
/// pointer arrives (`:hover`) and bled away as the rail takes focus
/// (`:focus-visible`), each over its own 120ms.
///
/// CSS interpolates the whole list, and a state that names fewer shadows pads it
/// with `0 0 0 0 transparent` — which is why the far shadow loses its offset and
/// its blur to the focus ring rather than dropping out of the list.
///
/// The two rings do not stack: `:focus-visible` is the later rule in the design,
/// so a hovered *and* focused thumb wears the accent ring only. Cross-fading the
/// one out as the other comes in is how a box-shadow list would have interpolated
/// between those two states anyway.
fn thumb_shadows(frame: Frame, palette: &Palette) -> Vec<BoxShadow> {
    let hover = frame.hover * (1. - frame.focus);
    let focus = frame.focus;
    let ink = color(SHADOW_INK);
    let near = (SHADOW_NEAR + (SHADOW_NEAR_HOVER - SHADOW_NEAR) * hover)
        + (SHADOW_NEAR_HOVER - (SHADOW_NEAR + (SHADOW_NEAR_HOVER - SHADOW_NEAR) * hover)) * focus;
    let far = (SHADOW_FAR + (SHADOW_FAR_HOVER - SHADOW_FAR) * hover) * (1. - focus);
    let mut shadows = vec![
        BoxShadow::new(px(0.), px(1.), ink.alpha(near)).blur_radius(px(1.)),
        BoxShadow::new(px(0.), px(2. * (1. - focus)), ink.alpha(far))
            .blur_radius(px(3. * (1. - focus))),
    ];
    if hover > 0. {
        shadows.push(
            BoxShadow::new(px(0.), px(0.), wash(palette.fg, HOVER_RING_MIX * hover))
                .spread_radius(px(HOVER_RING * hover)),
        );
    }
    if focus > 0. {
        shadows.push(
            BoxShadow::new(
                px(0.),
                px(0.),
                wash(palette.primary, FOCUS_RING_MIX * focus),
            )
            .spread_radius(px(FOCUS_RING * focus)),
        );
    }
    shadows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moment, `ms` after a test's own zero: the transitions here are all
    /// measured from an instant a test picks, so nothing sleeps.
    fn moment(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

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
        // The same curve, against the same Bézier solved a different way — the
        // parameter found by Newton on `x(u) = t` rather than by bisection, so
        // these are not this function's own answer read back to it.
        for (t, want) in [
            (0.1, 0.24297),
            (0.2, 0.48829),
            (0.25, 0.59710),
            (0.5, 0.89855),
            (0.75, 0.98299),
            (0.9, 0.99773),
        ] {
            let got = ease(t);
            assert!((got - want).abs() < 2e-4, "at {t}: {got} vs {want}");
        }
        // And CSS's `ease`, the curve the thumb's shadow and scale move on.
        for (t, want) in [(0.2, 0.29524), (0.5, 0.80240), (0.8, 0.97563)] {
            let got = Curve::EASE.at(t);
            assert!((got - want).abs() < 2e-4, "at {t}: {got} vs {want}");
        }
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

    /// A change moves over the design's 140ms on its curve: a frame part of the
    /// way through is between the two places, and the far one is reached on time.
    #[test]
    fn a_change_moves_over_the_designs_140ms() {
        let motion = Motion::new();
        let t0 = Instant::now();
        // The first frame settles: the design transitions a change, not a mount.
        let first = motion.frame(t0, Inputs::default());
        assert_eq!(first.position, 0.);
        assert!(!first.moving);

        let start = moment(t0, 10);
        let set_off = motion.frame(
            start,
            Inputs {
                target: 1.,
                ..Default::default()
            },
        );
        assert_eq!(set_off.position, 0., "a move starts where the thumb is");
        assert!(set_off.moving);

        let quarter = motion.frame(
            start + MOVE / 4,
            Inputs {
                target: 1.,
                ..Default::default()
            },
        );
        assert!(
            quarter.position > 0.3 && quarter.position < 1.,
            "a quarter of the time, most of the way, not all of it: {}",
            quarter.position
        );
        let half = motion.frame(
            start + MOVE / 2,
            Inputs {
                target: 1.,
                ..Default::default()
            },
        );
        assert!((half.position - Curve::MOVE.at(0.5)).abs() < 1e-4);

        let arrived = motion.frame(
            start + MOVE,
            Inputs {
                target: 1.,
                ..Default::default()
            },
        );
        assert_eq!(arrived.position, 1.);
        assert!(!arrived.moving, "and then nothing more to draw");
    }

    /// A change made while a move is in flight starts from what is being drawn,
    /// not from where the last move began — the thing a CSS transition does and a
    /// keyed animation cannot.
    #[test]
    fn a_change_in_flight_starts_from_where_the_thumb_is() {
        let motion = Motion::new();
        let t0 = Instant::now();
        motion.frame(t0, Inputs::default());
        let start = moment(t0, 10);
        let out = Inputs {
            target: 1.,
            ..Default::default()
        };
        motion.frame(start, out);
        let half = motion.frame(start + MOVE / 2, out);
        let drawn = half.position;
        assert!(drawn > 0. && drawn < 1., "{drawn}");

        // Turn back, half-way: the way back starts from the drawn thumb.
        let back = Inputs::default();
        let turn = motion.frame(start + MOVE / 2, back);
        assert_eq!(turn.position, drawn, "a turn starts where the thumb is");
        let quarter = motion.frame(start + MOVE / 2 + MOVE / 4, back);
        let want = drawn + (0. - drawn) * Curve::MOVE.at(0.25);
        assert!((quarter.position - want).abs() < 1e-4, "{quarter:?}");
        let home = motion.frame(start + MOVE / 2 + MOVE, back);
        assert_eq!(home.position, 0.);
        assert!(!home.moving);
    }

    /// A re-render in the middle of a move is not a new move: the same target
    /// leaves the transition where it is, so a caller drawing every frame does not
    /// restart the animation each time.
    #[test]
    fn a_rerender_mid_move_leaves_the_move_alone() {
        let motion = Motion::new();
        let t0 = Instant::now();
        motion.frame(t0, Inputs::default());
        let start = moment(t0, 10);
        let out = Inputs {
            target: 1.,
            ..Default::default()
        };
        motion.frame(start, out);
        let quarter = motion.frame(start + MOVE / 4, out);
        let again = motion.frame(start + MOVE / 4, out);
        assert_eq!(again.position, quarter.position);
        assert_eq!(again.moving, quarter.moving);
        let arrived = motion.frame(start + MOVE, out);
        assert_eq!(arrived.position, 1., "and it still arrives when it should");
    }

    /// The press is the design's 120ms, and the flag behind it lives in the motion
    /// — so it survives every render the button is held through.
    #[test]
    fn a_press_lives_in_the_motion_and_comes_back() {
        let motion = Motion::new();
        let t0 = Instant::now();
        motion.frame(t0, Inputs::default());
        assert!(!motion.down());

        motion.set_down(true);
        let down = motion.frame(moment(t0, 4), Inputs::default());
        assert_eq!(down.press, 0., "the press starts where the thumb is");
        assert!(down.moving);
        let held = motion.frame(moment(t0, 4) + FADE, Inputs::default());
        assert_eq!(held.press, 1.);
        assert!(!held.moving);

        // Two more renders while it is held — a new widget each time — still draw
        // it pressed, because the flag is not the widget's.
        motion.frame(moment(t0, 4) + FADE, Inputs::default());
        motion.frame(moment(t0, 4) + FADE, Inputs::default());
        assert!(motion.down());
        let still = motion.frame(
            moment(t0, 4) + FADE + Duration::from_millis(8),
            Inputs::default(),
        );
        assert_eq!(still.press, 1.);

        motion.set_down(false);
        let released = motion.frame(moment(t0, 4) + FADE, Inputs::default());
        assert_eq!(released.press, 1., "the release starts where it was");
        let settled = motion.frame(moment(t0, 4) + FADE + FADE, Inputs::default());
        assert_eq!(settled.press, 0.);
        assert!(!settled.moving);
    }

    /// The rings fade over the design's 120ms rather than appearing, and each is
    /// driven by its own flag: the pointer is the widget's, the focus is the
    /// window's.
    #[test]
    fn the_rings_fade_in_and_out() {
        let motion = Motion::new();
        let t0 = Instant::now();
        motion.frame(t0, Inputs::default());

        motion.set_hovered(true);
        let arriving = moment(t0, 20);
        assert_eq!(motion.frame(arriving, Inputs::default()).hover, 0.);
        let half = motion.frame(arriving + FADE / 2, Inputs::default());
        assert!((half.hover - Curve::EASE.at(0.5)).abs() < 1e-4);
        let lit = motion.frame(arriving + FADE, Inputs::default());
        assert_eq!(lit.hover, 1.);
        assert!(!lit.moving);

        // Focus comes in on the frame that draws it, and the pointer leaving
        // takes the hover ring back out.
        motion.set_hovered(false);
        let focused = Inputs {
            focus: true,
            ..Default::default()
        };
        let start = moment(t0, 20) + FADE;
        assert_eq!(motion.frame(start, focused).focus, 0.);
        let both = motion.frame(start + FADE, focused);
        assert_eq!(both.focus, 1.);
        assert_eq!(both.hover, 0., "the pointer left as the focus arrived");
        assert!(!both.moving);
    }

    /// gpui's reduce-motion switch: a change lands where it is going instead of
    /// travelling there.
    #[test]
    fn reduce_motion_lands_at_once() {
        let motion = Motion::new();
        let t0 = Instant::now();
        motion.frame(t0, Inputs::default());
        motion.set_down(true);
        let jumped = motion.frame(
            moment(t0, 4),
            Inputs {
                target: 1.,
                reduce_motion: true,
                ..Default::default()
            },
        );
        assert_eq!(jumped.position, 1.);
        assert_eq!(jumped.press, 1.);
        assert!(!jumped.moving);
    }

    /// The widget builds with the design's levels and with a caller's, and a
    /// level past the end is the last one rather than a panic.
    #[test]
    fn a_slider_builds_at_any_level() {
        let motion = Rc::new(Motion::new());
        let slider = EffortSlider::new("effort", 3, motion.clone()).palette(&store::design::LIGHT);
        assert_eq!(slider.label(), "xhigh");
        assert_eq!(slider.levels().len(), 5);
        let dark = EffortSlider::new("effort", 0, motion.clone()).palette(&store::design::DARK);
        assert_eq!(dark.label(), "low");
        let custom = EffortSlider::with_levels("effort", ["a", "b"], 9, motion);
        assert_eq!(custom.label(), "b", "a level past the end is the last one");
    }

    /// The design's shadows: two warm ones at rest, deepened under a pointer, with
    /// the hover ring at its 4px of 7%; focus bleeds the far one away and rings
    /// the thumb in the accent's 32% at 3px instead, as the design's own
    /// `:focus-visible` rule does.
    #[test]
    fn the_thumb_carries_the_designs_shadows() {
        let rest = Frame {
            position: 0.5,
            press: 0.,
            hover: 0.,
            focus: 0.,
            moving: false,
        };
        let dark = &store::design::DARK;
        let plain = thumb_shadows(rest, dark);
        assert_eq!(plain.len(), 2, "the two warm shadows, and nothing else");
        assert!(plain[0].offset.y > px(0.), "the near shadow sits under it");
        assert_eq!(plain[1].blur_radius, px(3.), "the far one is the wider");
        assert_eq!(SHADOW_INK, Rgb::new(0x28, 0x1c, 0x0a));

        let hovered = thumb_shadows(Frame { hover: 1., ..rest }, dark);
        assert_eq!(hovered.len(), 3, "a ring joins the two");
        assert_eq!(hovered[2].spread_radius, px(HOVER_RING));
        assert!(hovered[0].color.a > plain[0].color.a, "and they deepen");

        let focused = thumb_shadows(Frame { focus: 1., ..rest }, dark);
        assert_eq!(focused.len(), 3);
        assert_eq!(focused[2].spread_radius, px(FOCUS_RING));
        assert_eq!(
            focused[1].color.a, 0.,
            "CSS pads a shorter list with transparent: the far shadow goes"
        );
        assert_eq!(focused[1].blur_radius, px(0.));

        // Focused *and* hovered is the focus rule alone, as in the design.
        let both = thumb_shadows(
            Frame {
                hover: 1.,
                focus: 1.,
                ..rest
            },
            dark,
        );
        assert_eq!(both.len(), 3, "the accent ring, not both rings");
        assert_eq!(both[2].spread_radius, px(FOCUS_RING));
    }
}

/// The ids a caller's test looks for, and the levels it is handed.
#[cfg(test)]
mod naming {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        div, Context, InputEvent as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Point, Render, TestAppContext, VisualTestContext,
    };

    /// Every element a test might look for is named from the name the slider was
    /// built with — no debug formatting, no wrapper needed to give it an id.
    #[test]
    fn the_element_ids_are_the_callers_name() {
        let slider = EffortSlider::new("effort", 1, Rc::new(Motion::new()));
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
        let motion = Rc::new(Motion::new());
        let given =
            EffortSlider::with_levels("effort", ["low", "medium", "high"], 2, motion.clone());
        assert_eq!(given.levels(), ["low", "medium", "high"]);
        assert_eq!(given.label(), "high");
        assert!(!given.levels().iter().any(|level| level == "off"));
        // The design's own ladder is the same list, and has no `off` either.
        assert_eq!(LEVELS, ["low", "medium", "high", "xhigh", "max"]);
        // The design's five, handed in as a caller's list, are drawn as they are.
        let design = EffortSlider::with_levels("effort", LEVELS, 4, motion);
        assert_eq!(design.levels(), LEVELS);
        assert_eq!(design.label(), "max");
    }

    /// A host: the slider on its own, as a caller mounts it — rebuilt every
    /// render, over the level, the instant and the motion that caller keeps.
    struct Host {
        /// The level a caller holds, which the slider is built for.
        level: Rc<Cell<usize>>,
        /// The instant each frame is drawn at. A real window reads the clock; a
        /// test steps this, so it can walk a move frame by frame instead of
        /// sleeping through it.
        now: Rc<Cell<Instant>>,
        /// The motion, kept across renders — the thing this widget cannot hold.
        motion: Rc<Motion>,
        /// What the slider last asked for, which is what a caller's own state is
        /// fed from.
        seen: Rc<Cell<usize>>,
    }

    /// What a test drives a host by, once the host itself is in a window.
    struct Handles {
        level: Rc<Cell<usize>>,
        now: Rc<Cell<Instant>>,
        seen: Rc<Cell<usize>>,
    }

    impl Host {
        fn new(level: usize) -> (Self, Handles) {
            let level = Rc::new(Cell::new(level));
            let now = Rc::new(Cell::new(Instant::now()));
            let seen = Rc::new(Cell::new(usize::MAX));
            let host = Host {
                level: level.clone(),
                now: now.clone(),
                motion: Rc::new(Motion::new()),
                seen: seen.clone(),
            };
            let handles = Handles { level, now, seen };
            (host, handles)
        }
    }

    impl Handles {
        /// Draw one frame, `ms` after the frame before it, for `level`: the clock
        /// a real window reads, stepped by hand.
        fn draw(&self, level: usize, ms: u64, cx: &mut VisualTestContext) {
            self.level.set(level);
            self.now.set(self.now.get() + Duration::from_millis(ms));
            cx.update(|window, cx| window.render_frame(cx));
        }

        /// A part of the slider, as the last frame drew it.
        fn part(&self, id: impl Into<ElementId>, cx: &mut VisualTestContext) -> Bounds<Pixels> {
            let id = id.into();
            cx.update(|window, _cx| window.find(id).bounds())
        }

        /// Where the thumb's centre sits along the window — the one number a
        /// level's place is, once it is drawn.
        fn thumb_at(&self, cx: &mut VisualTestContext) -> f32 {
            self.part("effort-thumb", cx).center().x.into()
        }
    }

    /// Press the left button at a point, the way the platform does.
    fn press(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.update(|window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    /// Let it go.
    fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.update(|window, cx| {
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Default::default(),
                    click_count: 1,
                }
                .to_platform_input(),
                cx,
            );
        });
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let seen = self.seen.clone();
            let slider = EffortSlider::new("effort", self.level.get(), self.motion.clone())
                .palette(&store::design::LIGHT)
                .on_change(move |next, _window, _cx| seen.set(next));
            div()
                .id("slider-host")
                .test_support()
                .w(px(200.))
                .child(slider.render_at(window, self.now.get()))
        }
    }

    /// In a window: the slider's root, rail and thumb are all findable by name.
    #[gpui_kit::test]
    fn a_rendered_slider_is_findable_by_name(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (host, handles) = Host::new(2);
        let (_host, cx) = cx.add_window_view(|_window, _cx| host);
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
            handles.seen.get(),
            2,
            "a click in the middle of a five-level slider picks the middle level"
        );
    }

    /// In a window: a change of level is drawn moving over the design's 140ms
    /// rather than snapped, and a frame half-way through sits where the curve says
    /// — between the level the thumb left and the one it is taking.
    #[gpui_kit::test]
    fn a_change_is_drawn_on_its_way_between_two_levels(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (host, handles) = Host::new(0);
        let (_host, cx) = cx.add_window_view(|_window, _cx| host);
        // The first frame settles rather than moving: the design has no transition
        // on mount.
        handles.draw(0, 0, cx);
        let low = handles.thumb_at(cx);

        // The far end. The frame the move sets off on is still at the low end.
        handles.draw(4, 0, cx);
        let set_off = handles.thumb_at(cx);
        assert!(
            (set_off - low).abs() < 0.5,
            "a move starts where the thumb is: {set_off} vs {low}"
        );

        // Half of the design's 140ms is 0.89855 of the way on its curve.
        handles.draw(4, 70, cx);
        let midway = handles.thumb_at(cx);
        handles.draw(4, 70, cx);
        let high = handles.thumb_at(cx);
        let want = low + (high - low) * 0.89855;
        assert!(
            midway > low && midway < high,
            "mid-move the thumb is between the levels: {midway} in {low}..{high}"
        );
        assert!(
            (midway - want).abs() < 1.,
            "70ms into a 140ms move the curve says {want} (from {low} to {high}), drawn {midway}"
        );
    }

    /// In a window: a change made while a move is in flight starts from the thumb
    /// that is being drawn — the thing a CSS transition does, and the thing this
    /// widget could not do while its "from" was re-made on every render.
    #[gpui_kit::test]
    fn a_second_change_starts_from_the_thumb_it_is_drawing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (host, handles) = Host::new(0);
        let (_host, cx) = cx.add_window_view(|_window, _cx| host);
        handles.draw(0, 0, cx);
        let low = handles.thumb_at(cx);
        handles.draw(4, 0, cx);
        handles.draw(4, 70, cx);
        let midway = handles.thumb_at(cx);

        // Turn back half-way through the first move: the frame it turns on is the
        // one just drawn, not the low end the move started from.
        handles.draw(0, 0, cx);
        let turned = handles.thumb_at(cx);
        assert!(
            (turned - midway).abs() < 0.5,
            "a turn starts where the thumb is drawn: {turned} vs {midway}"
        );

        // A quarter of the way back is 0.59710 along the same curve.
        handles.draw(0, 35, cx);
        let quarter = handles.thumb_at(cx);
        let want = midway + (low - midway) * 0.59710;
        assert!(
            quarter < midway && quarter > low,
            "and it walks back rather than jumping: {quarter} in {low}..{midway}"
        );
        assert!(
            (quarter - want).abs() < 1.,
            "35ms back the curve says {want}, drawn {quarter}"
        );

        // And it arrives at the low end, at the end of its own 140ms.
        handles.draw(0, 105, cx);
        let home = handles.thumb_at(cx);
        assert!(
            (home - low).abs() < 0.5,
            "the way back ends where it started: {home} vs {low}"
        );
    }

    /// In a window: the press is the widget's own handler writing into the
    /// caller's motion, and it lasts the design's 120ms — through every render the
    /// button is held across, though each one builds a fresh widget.
    #[gpui_kit::test]
    fn a_press_survives_the_renders_it_lasts_through(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (host, handles) = Host::new(2);
        let (_host, cx) = cx.add_window_view(|_window, _cx| host);
        handles.draw(2, 0, cx);
        let rest: f32 = handles.part("effort-thumb", cx).size.width.into();

        // A real press on the rail, at the middle level's own place.
        let at = handles.part("effort", cx).center();
        press(cx, at);
        handles.draw(2, 60, cx);
        let half: f32 = handles.part("effort-thumb", cx).size.width.into();
        handles.draw(2, 60, cx);
        let held: f32 = handles.part("effort-thumb", cx).size.width.into();
        assert!(
            half > rest,
            "the thumb grows under the button: {half} vs {rest}"
        );
        assert!(
            half < held,
            "and it is still growing half-way through its 120ms: {half} vs {held}"
        );
        // The design's `scale(1.08)`, as the layout snaps it to a device pixel.
        assert!(
            (held - THUMB_SIZE * PRESS_SCALE).abs() <= 0.5,
            "and reaches the design's 8%: {held} vs {}",
            THUMB_SIZE * PRESS_SCALE
        );

        // Renders while it is still held — a new widget every one of them — keep
        // drawing it pressed, because the flag is the motion's, not the widget's.
        handles.draw(2, 8, cx);
        handles.draw(2, 8, cx);
        let still: f32 = handles.part("effort-thumb", cx).size.width.into();
        assert!(
            (still - held).abs() < 0.01,
            "a held button keeps its pressed thumb: {still} vs {held}"
        );

        // Let it go, and it comes back over the same 120ms.
        release(cx, at);
        handles.draw(2, 120, cx);
        let back: f32 = handles.part("effort-thumb", cx).size.width.into();
        assert!(
            (back - rest).abs() < 0.01,
            "and a released one goes back: {back} vs {rest}"
        );
    }
}
