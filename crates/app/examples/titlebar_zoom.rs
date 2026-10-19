//! What a double click on the title bar does to the app's real window.
//!
//! The title bar is the tab strip, so this is the gesture a reader makes to make
//! the window fill the desktop. It must zoom **once**: a second zoom straight
//! after the first would put the window back where it started — a flicker, and
//! exactly the bug this check exists for. Every platform's zoom is a *toggle*
//! (macOS `zoom:`, `_NET_WM_STATE` on X11, Wayland's maximize), so "once" is not
//! a detail of the implementation but the whole of the behaviour.
//!
//! It opens the window the app opens (`workspace::window_options` plus the real
//! `WorkspaceView`), well short of the work area, and drives the platform's own
//! mouse events — a move, then a down/up pair, then a second pair carrying
//! `click_count: 2`, which is how a double click arrives. Then it reads the
//! `NSWindow` frame:
//!
//! * a **single** click must leave the frame alone;
//! * a **double** click must fill the work area, and stay filled;
//! * a second double click must put it back — the toggle still toggles.
//!
//! Exits non-zero unless all three hold.
//!
//! ```sh
//! cargo run -p evo-desktop --example titlebar_zoom
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Duration;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, px, AppContext as _, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Window,
    };
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    /// A frame as AppKit holds it, rounded to whole points: a zoom lands on the
    /// work area exactly, and the rounding keeps the comparison off float dust.
    #[derive(Clone, Copy, PartialEq)]
    struct Frame(f64, f64, f64, f64);

    impl std::fmt::Debug for Frame {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{:.0}x{:.0} at {:.0},{:.0}",
                self.2, self.3, self.0, self.1
            )
        }
    }

    fn frame(window: &mut Window) -> Frame {
        let RawWindowHandle::AppKit(raw) = window.window_handle().expect("a handle").as_raw()
        else {
            panic!("not an AppKit window");
        };
        let view: &NSView = unsafe { raw.ns_view.cast().as_ref() };
        let frame = view.window().expect("the view's window").frame();
        Frame(
            frame.origin.x.round(),
            frame.origin.y.round(),
            frame.size.width.round(),
            frame.size.height.round(),
        )
    }

    /// Just below the strip's top edge: the sliver above the tabs, which is the
    /// strip's own background rather than any tab.
    const CLICK_Y: f32 = 2.0;

    /// How far along the strip a click lands. The middle is the tabs' own row;
    /// [`TRAFFIC_X`] is the head of the strip, left of the first tab, which is
    /// the `TitleBar`'s own padding and *not* inside the row the tabs live in —
    /// the two regions the gesture has to work in.
    fn click_x(size: f32, at: ClickAt) -> f32 {
        match at {
            ClickAt::Tabs => size / 2.0,
            ClickAt::TrafficHead => TRAFFIC_X,
        }
    }

    /// A point in the traffic lights' own room at the strip's head, which is the
    /// `TitleBar`'s padding rather than the row's.
    const TRAFFIC_X: f32 = 40.0;

    #[derive(Clone, Copy, PartialEq)]
    enum ClickAt {
        Tabs,
        TrafficHead,
    }

    /// How long the window is given to settle after a gesture. A zoom is
    /// animated, so the frame is read once it has stopped moving.
    const SETTLE: Duration = Duration::from_millis(900);

    /// One click, or `count` of them: the platform's own sequence, so the second
    /// pair carries the `click_count` the handlers read.
    fn click(window: &mut Window, cx: &mut gpui_kit::App, count: usize, at: ClickAt) {
        window.render_frame(cx);
        let size = window.bounds().size;
        let position = point(px(click_x(f32::from(size.width), at)), px(CLICK_Y));
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        for click_count in 1..=count {
            window.dispatch_event(
                MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            window.dispatch_event(
                MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Default::default(),
                    click_count,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        }
    }

    let failures = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let check = {
        let failures = failures.clone();
        move |what: &str, ok: bool, detail: String| {
            println!("{} {what}: {detail}", if ok { "ok  " } else { "FAIL" });
            if !ok {
                failures.set(failures.get() + 1);
            }
        }
    };

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let mut options = workspace::window_options(cx);
        // Above the app's minimum, and well short of the work area: a window that
        // already filled the screen would show nothing either way.
        options.window_bounds = Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
            origin: gpui_kit::point(px(120.), px(160.)),
            size: gpui_kit::size(px(1100.), px(750.)),
        }));
        let (handle, _view) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| workspace::WorkspaceView::new(window, cx))
        })
        .expect("the window opens");

        cx.spawn(async move |cx| {
            let settle = |cx: &mut gpui_kit::AsyncApp| cx.background_executor().timer(SETTLE);

            settle(cx).await;
            let started = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            println!("window opens at {started:?}");
            check(
                "the window has room to grow",
                started.2 < 1600.0,
                format!("{started:?}, so a zoom is visible"),
            );

            // A single click is not a zoom: the strip is the window's drag handle,
            // and nothing else.
            cx.update_window(handle, |_, window, cx| click(window, cx, 1, ClickAt::Tabs))
                .expect("the single click");
            settle(cx).await;
            let after_one = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check(
                "a single click does not zoom",
                after_one == started,
                format!("{after_one:?}"),
            );

            // The gesture in question: one double click, one zoom.
            cx.update_window(handle, |_, window, cx| click(window, cx, 2, ClickAt::Tabs))
                .expect("the double click");
            settle(cx).await;
            let zoomed = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check(
                "a double click fills the desktop",
                zoomed != started && zoomed.2 > started.2 && zoomed.3 > started.3,
                format!("{started:?} then {zoomed:?}"),
            );

            // And it stays there: a second zoom would have put it straight back,
            // which is the flicker.
            settle(cx).await;
            let still = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check("and stays there", still == zoomed, format!("{still:?}"));

            // The strip's head, left of the first tab: the `TitleBar`'s own
            // padding, outside the row the tabs are drawn in. The gesture has to
            // work there too — it is the same title bar — and still be *one*
            // zoom. From the zoomed window one toggle puts it back, so a head
            // click that lands on the started frame is exactly one zoom.
            cx.update_window(handle, |_, window, cx| {
                click(window, cx, 2, ClickAt::TrafficHead)
            })
            .expect("the double click on the head");
            settle(cx).await;
            let from_head = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check(
                "a double click at the strip's head zooms once too",
                from_head == started,
                format!("{zoomed:?} then {from_head:?} — one toggle, back to the start"),
            );
            // And once more, so the window is zoomed again for the check below.
            cx.update_window(handle, |_, window, cx| {
                click(window, cx, 2, ClickAt::TrafficHead)
            })
            .expect("the double click on the head again");
            settle(cx).await;
            let head_zoomed = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check(
                "and back",
                head_zoomed == zoomed,
                format!("{head_zoomed:?}"),
            );

            // The toggle still toggles: the zoom is one call, not a one-way trip.
            cx.update_window(handle, |_, window, cx| click(window, cx, 2, ClickAt::Tabs))
                .expect("the second double click");
            settle(cx).await;
            let restored = cx
                .update_window(handle, |_, window, _| frame(window))
                .expect("the window");
            check(
                "a second double click restores",
                restored == started,
                format!("{restored:?}"),
            );

            let failed = failures.get();
            println!(
                "{}",
                if failed == 0 {
                    "the title bar zooms once".to_owned()
                } else {
                    format!("{failed} check(s) failed")
                }
            );
            std::process::exit(if failed == 0 { 0 } else { 1 });
        })
        .detach();
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {}
