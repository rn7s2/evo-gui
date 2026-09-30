//! The tab strip — `design/doc28/TabStrip.tsx` and `TabStrip.css`, drawn in gpui.
//!
//! One band, [`STRIP_HEIGHT`](store::design::STRIP_HEIGHT) tall, whose background
//! is the design's `--tab-strip`, holding a tab per swarm and the `+`. It is the
//! window's title bar too: the kit's [`TitleBar`] is the host, so dragging moves
//! the window, a double click zooms it, and the traffic lights and the
//! non-macOS window controls stay the system's — the design's own three dots are
//! a stage prop (the README says so), and the 84px they sit in is
//! [`TRAFFIC_WIDTH`](store::design::TRAFFIC_WIDTH) of clearance.
//!
//! The parts of the design that are behaviour rather than paint, and where they
//! live here:
//!
//! * a tab's fill is the design's `--tab-surface`: the active tab takes the
//!   page's own top surface (`tab_active`, so it runs into the header row below),
//!   a hovered one takes `tab_hover`, and the rest are the strip's background;
//! * the outward curve at each bottom corner is drawn by *the neighbour that owns
//!   those pixels* — the design's `::before` pseudo-element and its `z-index: 1/2`
//!   are a way to paint outside a box, and here each tab paints the piece of its
//!   left or right neighbour's flare that falls inside its own box. The same
//!   pixels, and no stacking order to get wrong;
//! * the divider between two tabs is hidden on the active tab, the one before it,
//!   the hovered tab and the one before that (`:has(+ .tab:hover)` — a
//!   previous-sibling selector, which is why the hovered tab is *state* on the
//!   view and not a hover style);
//! * the slot holds a working tab's breathing dot or an idle tab's ring, and the
//!   dot mixes against the tab's own surface ([`BreathingDot`]);
//! * the close button appears on the active tab and on whatever the pointer is
//!   over, and clicking it does not select the tab it closes;
//! * the `+` is always last, and a hovered one takes `tab_hover`.

use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, ActiveTheme as _, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, point, px, AnyElement, Context, ElementId, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, SharedString, TestSupportExt as _,
    Window,
};
use store::design::{self, Palette, Rgb};
use widgets::dot::dot_id;
use widgets::glyph;
use widgets::paint::{color, mix};
use widgets::BreathingDot;

use crate::chrome::WorkspaceView;
use crate::tab::{TabContent, TabId};

/// The row the strip is drawn in: the tabs' box and the `+`'s box, side by side.
pub(crate) const STRIP_ID: &str = "tab-strip";

/// The box the tabs scroll inside — the edge they are clipped at, which is the
/// edge the `+` begins at. A tab is never drawn past it.
pub(crate) const SCROLL_ID: &str = "tab-strip-scroll";

/// The tabs' own scroller inside that box. A `ScrollHandle` indexes the children
/// of the element that tracks it, which is why the tabs are *its* children.
pub(crate) const BAR_ID: &str = "tab-bar";

/// The `+` at the end of the strip: always present, always the last thing.
pub(crate) const ADD_ID: &str = "tab-add";

/// The strip's bottom edge carries no rule in the design (`border-bottom: 0`),
/// because the active tab runs into the header row below it.
const NO_RULE: f32 = 0.;

/// The strip, with the kit's title bar around it so the window keeps its own
/// gestures (drag, double click, window controls).
pub(crate) fn strip(
    view: &WorkspaceView,
    window: &Window,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let palette = palette(cx);
    TitleBar::new()
        .h(px(design::STRIP_HEIGHT))
        .pl(px(leading()))
        .bg(color(palette.tab_strip))
        .border_b(px(NO_RULE))
        .child(row(view, window, cx))
}

/// What the strip keeps clear at its left edge: the traffic lights on macOS,
/// where the window's own chrome is; the row's own padding everywhere else, where
/// the strip starts at the window's edge.
fn leading() -> f32 {
    if cfg!(target_os = "macos") {
        design::TRAFFIC_WIDTH
    } else {
        design::TAB_ROW_PAD.0
    }
}

/// The tabs and the `+`, bottom-aligned in the strip's band.
fn row(view: &WorkspaceView, window: &Window, cx: &mut Context<WorkspaceView>) -> impl IntoElement {
    let room = window.bounds().size.width - px(leading());
    div()
        .id(STRIP_ID)
        .flex()
        .items_end()
        .h_full()
        .min_w_0()
        .flex_1()
        .max_w(room)
        // The strip is the window's drag handle: a press here, then a move, moves
        // the window — and a press that starts on a tab stops before it gets here.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _, _| this.set_strip_dragging(true)),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| this.set_strip_dragging(false)),
        )
        .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
            if this.strip_dragging() {
                this.set_strip_dragging(false);
                window.start_window_move();
            }
        }))
        .on_double_click(|_, window, _| {
            if cfg!(target_os = "macos") {
                window.titlebar_double_click();
            } else {
                window.zoom_window();
            }
        })
        .test_support()
        .child(scroller(view, cx))
        .child(add(cx))
}

/// The tabs, in the box that clips them and the bar that scrolls them.
///
/// The box ends where the `+` begins, so a tab is cut at that edge rather than
/// drawn under the button. Scrolling is the inner bar's own business — it is the
/// element the scroll handle indexes, so `scroll_to_item` finds the tab being
/// shown.
fn scroller(view: &WorkspaceView, cx: &mut Context<WorkspaceView>) -> impl IntoElement {
    div()
        .id(SCROLL_ID)
        .test_support()
        // `flex_1`, not merely a shrinker: the tabs' width comes from this box's
        // width, so it has to be a definite one — the row's free space, which is
        // what `flex_1` hands it.
        .flex_1()
        .min_w_0()
        .overflow_x_hidden()
        .child(
            h_flex()
                .id(BAR_ID)
                .relative()
                .items_end()
                .flex_1()
                .min_w_0()
                // A corner's width of padding on each side: the first and last tabs'
                // outward corners are drawn outside their own boxes, and this is the
                // room that keeps them inside the box that clips them. The design's
                // own row padding (`TAB_ROW_PAD`) is not missed: the corner sits in
                // its place.
                .pl(px(design::TAB_RADIUS))
                .pr(px(design::TAB_RADIUS))
                .overflow_x_scroll()
                .lock_scroll_axis()
                .track_scroll(view.strip_scroll())
                .children(
                    view.tabs()
                        .iter()
                        .enumerate()
                        .map(|(index, content)| tab(view, index, content, cx)),
                ),
        )
}

/// One tab: its slot, its label and its close button.
fn tab(
    view: &WorkspaceView,
    index: usize,
    tab: &Entity<TabContent>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let palette = palette(cx);
    let hovered = view.hovered_tab();
    let (id, title, tooltip, busy, active) = {
        let content = tab.read(cx);
        (
            content.id(),
            content.title(),
            content.tooltip(),
            content.is_running(),
            index == view.selected_index(),
        )
    };
    let unseen = view.has_unseen_finish(id);
    let pointed = hovered == Some(index);
    let surface = surface_of(palette, active, pointed);
    let ink = if active { palette.fg } else { palette.tab_ink };
    let divider = wears_divider(index, view.selected_index(), hovered);
    let group = SharedString::from(format!("tab-content-{}", id.get()));
    let mut element = div()
        .id(ElementId::NamedInteger("tab".into(), id.get()))
        .test_support()
        .flex_grow(0.)
        .flex_shrink(1.)
        .flex_basis(px(design::TAB_BASIS))
        .min_w(px(design::TAB_MIN_WIDTH))
        .h(px(design::TAB_HEIGHT))
        .flex()
        .items_center()
        .gap(px(design::TAB_GAP))
        .pl(px(design::TAB_PAD.0))
        .pr(px(design::TAB_PAD.1))
        .rounded_t(px(design::TAB_RADIUS))
        .group(group)
        .text_size(px(design::TAB_FONT))
        .cursor_default()
        .when(active || pointed, |this| this.bg(color(surface)))
        .text_color(color(ink))
        .on_hover(cx.listener(move |this, over: &bool, _, cx| {
            this.set_hovered_tab(over.then_some(index), cx)
        }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, _: &MouseDownEvent, window, cx| this.close_tab(id, window, cx)),
        )
        .on_click(cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)));
    // The outward corners. The design draws one at each side of the tab being
    // shown or pointed at, and each piece is either inside this tab's box — the
    // neighbour's half of it, which is what a neighbour drawing it means — or, at
    // the end of the strip, outside this tab's own box, where nothing else is
    // drawn to be under.
    let own = (active || pointed).then_some(surface);
    match index.checked_sub(1) {
        Some(left) => {
            if let Some(neighbour) = flare_surface(view, Some(left), palette) {
                element = element.child(flare(neighbour, Place::LeftInside));
            }
        }
        None => {
            if let Some(own) = own {
                element = element.child(flare(own, Place::LeftOutside));
            }
        }
    }
    if index + 1 < view.tabs().len() {
        if let Some(neighbour) = flare_surface(view, Some(index + 1), palette) {
            element = element.child(flare(neighbour, Place::RightInside));
        }
    } else if let Some(own) = own {
        element = element.child(flare(own, Place::RightOutside));
    }
    element
        .when(divider, |this| this.child(rule(cx)))
        .child(slot(id, busy, unseen, surface, ink, cx))
        .child(
            div()
                .id(ElementId::NamedInteger("tab-label".into(), id.get()))
                .test_support()
                .flex_1()
                .min_w_0()
                .truncate()
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(title),
        )
        .child(close(id, active || pointed, surface, cx))
        .into_any_element()
}

/// Whether the tab at `index` wears the 1px rule at its right edge.
///
/// The design hides it on the tab being shown, on the tab *before* it, on the tab
/// the pointer is over and on the tab before that — the last two are
/// `:has(+ .tab:hover)`, a rule about the next tab along, which is why the hovered
/// tab is state on the view rather than a hover style.
fn wears_divider(index: usize, active: usize, hovered: Option<usize>) -> bool {
    index != active && index + 1 != active && Some(index) != hovered && hovered != Some(index + 1)
}

/// The surface a neighbour's flare takes: the colour the tab at `neighbour` is
/// filled with, or nothing when that tab is neither the one being shown nor the
/// one being pointed at (a plain tab's pixels are the strip's own).
fn flare_surface(
    view: &WorkspaceView,
    neighbour: Option<usize>,
    palette: &'static Palette,
) -> Option<Rgb> {
    let index = neighbour?;
    if index == view.selected_index() {
        Some(palette.tab_active)
    } else if view.hovered_tab() == Some(index) {
        Some(palette.tab_hover)
    } else {
        None
    }
}

/// The colour a tab's own box is filled with.
fn surface_of(palette: &'static Palette, active: bool, pointed: bool) -> Rgb {
    if active {
        palette.tab_active
    } else if pointed {
        palette.tab_hover
    } else {
        palette.tab_strip
    }
}

/// Where a tab's outward corner sits, and so which way its arc is drawn.
///
/// The design gives each of the two pieces a box one radius wide and one radius
/// high, at the tab's bottom corner, and fills the part of that box farthest from
/// the box's outer-top corner. Which corner that is depends only on where the box
/// is — the piece always hugs the corner nearest the tab it flares out of — so the
/// four places are four cases of one shape.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Inside the tab, against its left edge: the piece of the tab to its left.
    LeftInside,
    /// Outside the tab, to the left of it: the tab's own corner, at the strip's
    /// first tab.
    LeftOutside,
    /// Inside the tab, against its right edge: the piece of the tab to its right.
    RightInside,
    /// Outside the tab, to the right of it: the tab's own corner, at the strip's
    /// last tab.
    RightOutside,
}

impl Place {
    /// Whether the piece hugs the bottom-**right** of its box (and so the arc
    /// turns around the box's top-left corner).
    ///
    /// A piece against a tab's left edge hugs the *left* of its box when it is
    /// outside that tab (its own corner) and the *right* when it is inside (the
    /// neighbour's); the right-hand cases are the mirror.
    fn hugs_bottom_right(self) -> bool {
        matches!(self, Place::LeftOutside | Place::RightInside)
    }

    /// Which edge of the tab the box is pinned to, and by how much: inside the
    /// box at 0, or outside it by one radius.
    fn offset(self) -> (bool, f32) {
        match self {
            Place::LeftInside => (true, 0.),
            Place::LeftOutside => (true, -design::TAB_RADIUS),
            Place::RightInside => (false, 0.),
            Place::RightOutside => (false, -design::TAB_RADIUS),
        }
    }
}

/// One outward corner, `TAB_RADIUS` square, in the colour of the tab it flares
/// out of.
///
/// The design draws these with a radial gradient whose circle is centred on the
/// box's outer-top corner and whose transparent inside lets whatever is behind
/// show through, so what is painted is the piece between the arc and the corner
/// the piece hugs. A filled path is that shape without the cut — and, drawn by the
/// tab whose box these pixels are in, it is over that tab and under nothing.
fn flare(colour: Rgb, place: Place) -> AnyElement {
    let r = design::TAB_RADIUS;
    let (left, offset) = place.offset();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (origin, size) = (bounds.origin, px(r));
            let mut builder = PathBuilder::fill();
            if place.hugs_bottom_right() {
                // The box's top-left corner: the arc runs from its top-right to
                // its bottom-left, and the piece is the rest of the box.
                builder.move_to(point(origin.x + size, origin.y));
                builder.arc_to(
                    point(size, size),
                    px(0.),
                    false,
                    true,
                    point(origin.x, origin.y + size),
                );
                builder.line_to(point(origin.x + size, origin.y + size));
            } else {
                // The box's top-right corner: the arc runs from its top-left to
                // its bottom-right, and the piece hugs the bottom-left.
                builder.move_to(origin);
                builder.arc_to(
                    point(size, size),
                    px(0.),
                    false,
                    false,
                    point(origin.x + size, origin.y + size),
                );
                builder.line_to(point(origin.x, origin.y + size));
            }
            builder.close();
            if let Ok(path) = builder.build() {
                window.paint_path(path, color(colour));
            }
        },
    )
    .absolute()
    .bottom_0()
    .h(px(r))
    .w(px(r))
    .when(left, |this| this.left(px(offset)))
    .when(!left, |this| this.right(px(offset)))
    .into_any_element()
}

/// The rule between two tabs: `::after` — 1px wide, from a quarter of the way
/// down the tab to a quarter of the way up.
fn rule(cx: &Context<WorkspaceView>) -> AnyElement {
    let palette = palette(cx);
    div()
        .absolute()
        .right_0()
        .top(px(design::TAB_HEIGHT * design::TAB_DIVIDER_INSET))
        .bottom(px(design::TAB_HEIGHT * design::TAB_DIVIDER_INSET))
        .w(px(1.))
        .bg(color(palette.tab_divider))
        .into_any_element()
}

/// The slot before a tab's name: a working tab breathes, an idle one wears a
/// ring, and a tab whose run ended while it was not being looked at wears the ring
/// at full strength — the design's "something happened here", told in the design's
/// own vocabulary.
fn slot(
    id: TabId,
    busy: bool,
    unseen: bool,
    surface: Rgb,
    ink: Rgb,
    cx: &Context<WorkspaceView>,
) -> AnyElement {
    let palette = palette(cx);
    let state = if busy {
        "tab-running"
    } else if unseen {
        "tab-finished"
    } else {
        "tab-idle"
    };
    div()
        .id(ElementId::NamedInteger(state.into(), id.get()))
        .test_support()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(design::TAB_SLOT))
        .h(px(design::TAB_SLOT))
        .child(
            BreathingDot::new(dot_id(format!("tab-{}", id.get())), busy)
                .palette(palette)
                .surface(surface)
                // The idle ring is the tab's own ink (`currentColor` in the
                // design): the foreground on the tab being shown, the tab ink on
                // the others.
                .ring(color(ink))
                .idle_opacity(if unseen { 1. } else { design::IDLE_OPACITY })
                .render(),
        )
        .into_any_element()
}

/// A tab's close button: a 22px circle, out of sight until its tab is the one
/// being shown or pointed at, and never the thing that selects a tab.
fn close(id: TabId, shown: bool, surface: Rgb, cx: &Context<WorkspaceView>) -> AnyElement {
    let palette = palette(cx);
    div()
        .id(ElementId::NamedInteger("tab-close".into(), id.get()))
        .test_support()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(design::CLOSE))
        .rounded_full()
        .text_color(color(palette.tab_ink))
        .when(!shown, |this| this.opacity(0.))
        .hover(move |style| {
            style
                .bg(color(mix(
                    palette.fg,
                    design::CLOSE_HOVER_MIX * 100.,
                    surface,
                )))
                .text_color(color(palette.fg))
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            // The tab itself selects on click; closing must not.
            cx.stop_propagation();
            this.close_tab(id, window, cx);
        }))
        .child(glyph::cross_in(palette))
        .into_any_element()
}

/// The `+` at the end of the strip.
fn add(cx: &mut Context<WorkspaceView>) -> impl IntoElement {
    let palette = palette(cx);
    div()
        .id(ADD_ID)
        .test_support()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(design::ADD))
        .rounded(px(design::ADD_RADIUS))
        .ml(px(design::ADD_GAP))
        .mb(px((design::TAB_HEIGHT - design::ADD) / 2.))
        .text_color(color(palette.tab_ink))
        .hover(move |style| {
            style
                .bg(color(palette.tab_hover))
                .text_color(color(palette.fg))
        })
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_empty_tab(window, cx);
        }))
        .child(glyph::plus_in(palette))
}

/// The window's palette, as the strip's drawing reads it.
fn palette(cx: &Context<WorkspaceView>) -> &'static Palette {
    design::palette(cx.theme().is_dark())
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::design::{DARK, LIGHT};

    /// The divider rule, all four of the design's cases and a tab that keeps one.
    #[test]
    fn a_divider_is_hidden_around_the_shown_and_the_pointed_at_tab() {
        // Four tabs, the third being shown.
        let (active, hovered) = (2, None);
        assert!(!wears_divider(2, active, hovered), "the shown tab");
        assert!(!wears_divider(1, active, hovered), "the tab before it");
        assert!(wears_divider(0, active, hovered));
        assert!(wears_divider(3, active, hovered));

        // Pointing at the last tab hides its own divider and the one before it.
        let hovered = Some(3);
        assert!(!wears_divider(3, active, hovered), "the pointed-at tab");
        assert!(
            !wears_divider(2, active, hovered),
            "which is the shown one too"
        );
        assert!(wears_divider(0, active, hovered));

        // Pointing at the first tab: its own and its neighbour's, nothing else.
        let hovered = Some(0);
        assert!(!wears_divider(0, active, hovered));
        assert!(!wears_divider(1, active, hovered), ":has(+ .tab:hover)");
        assert!(wears_divider(3, active, hovered));
    }

    /// The fill: the shown tab's surface wins over the pointer's, and a tab that
    /// is neither is the strip's own colour.
    #[test]
    fn a_tab_takes_its_surface_from_what_it_is() {
        let palette = &LIGHT;
        assert_eq!(surface_of(palette, true, false), palette.tab_active);
        assert_eq!(surface_of(palette, true, true), palette.tab_active);
        assert_eq!(surface_of(palette, false, true), palette.tab_hover);
        assert_eq!(surface_of(palette, false, false), palette.tab_strip);
        // The dark theme's own three, which are not the light theme's.
        assert_eq!(surface_of(&DARK, false, false), DARK.tab_strip);
        assert_ne!(DARK.tab_active, DARK.tab_strip);
    }

    /// The outward corners: which way the arc turns is a function of where the
    /// piece is, and each place is one of the design's two radial gradients.
    #[test]
    fn an_outward_corner_hugs_the_corner_nearest_its_tab() {
        // Inside a tab, against its left edge: the tab on its left flares out of
        // *its* bottom-right, so the piece hugs this box's bottom-left.
        assert!(!Place::LeftInside.hugs_bottom_right());
        assert_eq!(Place::LeftInside.offset(), (true, 0.));
        // Outside it: the tab's own corner at the strip's first tab, which hugs
        // the bottom-right of the box it occupies.
        assert!(Place::LeftOutside.hugs_bottom_right());
        assert_eq!(Place::LeftOutside.offset(), (true, -design::TAB_RADIUS));
        // The mirror pair.
        assert!(Place::RightInside.hugs_bottom_right());
        assert_eq!(Place::RightInside.offset(), (false, 0.));
        assert!(!Place::RightOutside.hugs_bottom_right());
        assert_eq!(
            Place::RightOutside.offset(),
            (false, -design::TAB_RADIUS),
            "outside the last tab's right edge"
        );
    }

    /// The strip's own numbers, from the design: a corner is a corner, and the
    /// pieces are one radius square.
    #[test]
    fn the_strips_numbers_are_the_designs() {
        assert_eq!(design::STRIP_HEIGHT, 42.);
        assert_eq!(design::TAB_HEIGHT, 38.);
        assert_eq!(design::TAB_RADIUS, 8.);
        assert_eq!(design::TAB_MIN_WIDTH, 72.);
        assert_eq!(design::TAB_GAP, 9.);
        assert_eq!(design::CLOSE, 22.);
        assert_eq!(design::ADD, 30.);
        // What the strip keeps clear at its left edge is the traffic lights' room.
        assert_eq!(leading(), design::TRAFFIC_WIDTH);
    }
}
