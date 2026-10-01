//! Where AppKit really put the traffic lights on the app's window: opens a real
//! (on-screen) window with `workspace::window_options`, reads the close / minimize
//! / zoom buttons' frames off the `NSWindow`, prints their distance from the top
//! and from the tab strip's bottom edge, and exits non-zero unless they are
//! vertically centred in the strip (±0.5pt).
//!
//! ```sh
//! cargo run -p evo-desktop --example traffic_lights
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Duration;

    use gpui_kit::{div, AppContext as _, IntoElement, Render, Styled as _, Window};
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    struct Blank;
    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let options = workspace::window_options(cx);
        let (handle, _) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Blank))
            .expect("the window opens");
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            let ok = cx
                .update_window(handle, |_, window, _| {
                    let RawWindowHandle::AppKit(raw) =
                        window.window_handle().expect("a handle").as_raw()
                    else {
                        panic!("not an AppKit window");
                    };
                    let view: &NSView = unsafe { raw.ns_view.cast().as_ref() };
                    let ns_window = view.window().expect("the view's window");
                    let content = ns_window.contentView().expect("a content view").frame();
                    let height = content.size.height;
                    let strip = store::design::STRIP_HEIGHT as f64;
                    let mut ok = true;
                    for (name, kind) in [
                        ("close", NSWindowButton::CloseButton),
                        ("minimize", NSWindowButton::MiniaturizeButton),
                        ("zoom", NSWindowButton::ZoomButton),
                    ] {
                        let button = ns_window.standardWindowButton(kind).expect("the button");
                        let frame = button.convertRect_toView(button.bounds(), None);
                        let top = height - (frame.origin.y + frame.size.height);
                        let bottom = strip - top - frame.size.height;
                        println!(
                            "{name}: x {:.1} size {:.1}x{:.1}, top gap {top:.1}, gap to strip bottom {bottom:.1}",
                            frame.origin.x, frame.size.width, frame.size.height
                        );
                        ok &= (top - bottom).abs() <= 0.5;
                    }
                    ok
                })
                .unwrap_or(false);
            println!("{}", if ok { "centred" } else { "NOT centred" });
            std::process::exit(if ok { 0 } else { 1 });
        })
        .detach();
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {}
