//! The split between the tab page's two columns (§7.3): the divider's band and the
//! pill it grows, and the kit's state machine behind it.
//!
//! The tab page draws the divider with the kit's own handle (`h_resizable` with
//! `resize_handle_appearance`), inside a wrapper that carries the one gesture the kit
//! has no room for — a double-click that puts the column back where it starts. What
//! these tests hold is that the wrapper does not get in the band's way: the appearance
//! the page supplies is handed the state the design draws the pill from
//! (`idle` → `hovered` → `pressed` → `dragging`), and a pointer on the band is what
//! moves the column.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::{InteractiveElementExt as _, ResizeHandleContext, ResizeHandleState};
use gpui_kit::component::{h_resizable, resizable_panel, resize_handle_appearance};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, point, px, size, App, AppContext as _, Bounds, Context, InputEvent as _,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Render, Styled as _, TestAppContext, Window, WindowBounds,
    WindowOptions,
};

/// The column the design opens at, and the band's own width: nine pixels, four either
/// side of the hairline (`.divider{width:9px;margin-left:-5px}`).
const LEFT: f32 = 260.;
const WINDOW: (f32, f32) = (1280., 800.);

/// A two-panel group with the tab page's own split, whose divider is wrapped and
/// painted the way the page wraps and paints it, recording what it was told.
struct Split {
    state: Rc<Cell<ResizeHandleState>>,
}

impl Render for Split {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        let kit = resize_handle_appearance();
        h_resizable("split")
            .with_handle_appearance(Rc::new(
                move |handle: &ResizeHandleContext, window: &mut Window, cx: &mut App| {
                    state.set(handle.state());
                    // The wrapper the tab page puts around the kit's divider: the band's
                    // own painted element, with the double-click's element over it.
                    let painted = kit(handle, window, cx);
                    Some(
                        div()
                            .id("pane-handle")
                            .h_full()
                            .w(px(1.))
                            .cursor_col_resize()
                            .on_double_click(|_, _, _| {})
                            .child(painted.unwrap_or_else(|| div().into_any_element()))
                            .into_any_element(),
                    )
                },
            ))
            .child(
                resizable_panel()
                    .size(px(LEFT))
                    .flex_none()
                    .child(div().size_full()),
            )
            .child(resizable_panel().child(div().size_full()))
    }
}

/// The window, and the state its divider was last handed.
fn open(
    cx: &mut TestAppContext,
) -> (
    gpui_kit::WindowHandle<gpui_kit::base::Root>,
    Rc<Cell<ResizeHandleState>>,
) {
    cx.update(gpui_kit::init);
    let state = Rc::new(Cell::new(ResizeHandleState::Idle));
    let recorded = state.clone();
    let window = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(WINDOW.0), px(WINDOW.1)),
                })),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|_| Split { state: recorded }),
        )
        .expect("the split window");
        window
            .downcast::<gpui_kit::base::Root>()
            .expect("base Root")
    });
    (window, state)
}

/// One frame with the pointer at `at`, dispatched twice: the state machine reads the
/// pixel the pointer is on from the frame the move was routed through.
fn move_to(window: &mut Window, at: gpui_kit::Point<gpui_kit::Pixels>, cx: &mut App) {
    for _ in 0..2 {
        window.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    }
}

fn press(
    window: &mut Window,
    at: gpui_kit::Point<gpui_kit::Pixels>,
    button: MouseButton,
    cx: &mut App,
) {
    window.dispatch_event(
        MouseDownEvent {
            button,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

fn release(window: &mut Window, at: gpui_kit::Point<gpui_kit::Pixels>, cx: &mut App) {
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
    window.render_frame(cx);
}

/// The middle of the band's own height, and four pixels inside it on the left.
fn on_the_band() -> gpui_kit::Point<gpui_kit::Pixels> {
    point(px(LEFT - 2.), px(WINDOW.1 / 2.))
}

/// The pointer coming to the band is a hover, and holding it is a press; moving with it
/// held is the drag. Those three are the three sizes the design's pill takes
/// (20 / 28 / 44px at 35 / 60 / 90%).
#[gpui_kit::test]
fn the_divider_answers_the_pointer_with_the_kits_three_states(cx: &mut TestAppContext) {
    let (handle, state) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Idle,
            "the divider rests as a hairline with no pointer on it"
        );

        move_to(window, on_the_band(), cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Hovered,
            "the pointer in the band is a hover"
        );

        press(window, on_the_band(), MouseButton::Left, cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Pressed,
            "the button down on the band is a press"
        );

        move_to(window, point(px(LEFT + 40.), px(WINDOW.1 / 2.)), cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Dragging,
            "a press that moves is the drag"
        );

        // Letting go leaves it hovered: the band followed the pointer, so the pointer
        // is on it — and the kit keeps a released handle hovered rather than dropping
        // the pill for the frame before the next move says otherwise.
        release(window, point(px(LEFT + 40.), px(WINDOW.1 / 2.)), cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Hovered,
            "letting go on the band the drag moved leaves it hovered"
        );

        // And the pointer leaving the band is what leaves it at rest.
        move_to(window, point(px(LEFT + 200.), px(WINDOW.1 / 2.)), cx);
        assert_eq!(
            state.get(),
            ResizeHandleState::Idle,
            "the pointer away from the band is no hover"
        );
    })
    .unwrap();
}

/// §7.3: the band straddles the seam. The pointer four pixels to either side of the
/// hairline is on it; five pixels away is the column's own content.
#[gpui_kit::test]
fn the_band_is_the_nine_pixels_the_design_gives_it(cx: &mut TestAppContext) {
    let (handle, state) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        for (dx, expected) in [
            (-4., ResizeHandleState::Hovered),
            (4., ResizeHandleState::Hovered),
            (-5., ResizeHandleState::Idle),
            (5., ResizeHandleState::Idle),
        ] {
            move_to(window, point(px(LEFT + dx), px(WINDOW.1 / 2.)), cx);
            assert_eq!(
                state.get(),
                expected,
                "{dx}px from the seam: the band is the design's nine"
            );
        }
    })
    .unwrap();
}
