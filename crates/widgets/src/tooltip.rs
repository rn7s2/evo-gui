//! A tooltip whose text wraps inside a width cap.
//!
//! The kit's `Tooltip::new(text).max_w(…)` does not wrap: it lays its text in a flex
//! row, so a long path, task or one of evo's longer sentences ran past its own box
//! and off the window's edge. Here each line gets a box as wide as the widest line
//! (as the window's own text system shapes it) and never wider than the cap, which
//! is the definite width a line needs to wrap against.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, AnyView, App, ElementId, Pixels, SharedString, TestSupportExt as _, Window,
};

/// The text size tooltips are set at: the kit's `text_sm` is 14px; the design's
/// secondary text is 12px.
pub const TEXT: f32 = 12.;

/// A tooltip of `lines`, one per line, wrapping at `max_w`.
pub fn wrapped(
    id: impl Into<ElementId>,
    lines: Vec<SharedString>,
    max_w: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    let id = id.into();
    // A line with a `\n` in it is several lines (a tab's tooltip is the folder, then
    // what the tab is doing); the text system shapes one line at a time.
    let lines: Vec<SharedString> = lines
        .iter()
        .flat_map(|line| {
            line.split('\n')
                .map(|part| SharedString::from(part.to_string()))
        })
        .collect();
    Tooltip::element(move |window, _| {
        let style = window.text_style();
        let size = px(TEXT);
        let widest = lines
            .iter()
            .map(|line| {
                let run = style.to_run(line.len());
                window
                    .text_system()
                    .shape_line(line.clone(), size, &[run], None)
                    .width
            })
            .fold(px(0.), |a, b| a.max(b));
        gpui_kit::component::v_flex()
            .id(id.clone())
            .test_support()
            .w((widest + px(1.)).min(max_w))
            .text_size(size)
            .py_1()
            .gap_0p5()
            .children(
                lines
                    .iter()
                    .map(|line| div().w_full().min_w_0().child(line.clone())),
            )
    })
    .build(window, cx)
}

/// One run of text as a wrapping tooltip.
pub fn text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    max_w: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    wrapped(id, vec![text.into()], max_w, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{Render, TestAppContext};

    struct Host(&'static str);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            let text = self.0;
            div()
                .id("host")
                .test_support()
                .size(px(200.))
                .tooltip(move |window, cx| super::text("tip", text, px(300.), window, cx))
        }
    }

    /// Text with newlines is laid out as lines (the text system would panic on a
    /// newline in one line), and a long line wraps inside the cap.
    #[gpui_kit::test]
    fn a_multi_line_and_a_long_tooltip_stay_inside_their_cap(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let long: &'static str =
            Box::leak(format!("/a/path/with\n{}", "many-directories/".repeat(30)).into_boxed_str());
        let window = cx.add_window(|_, _| Host(long));
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.hover("host", cx);
        })
        .unwrap();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1500));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let tip = window.find("tip");
            assert!(tip.visible());
            assert!(tip.bounds().size.width <= px(300.), "{:?}", tip.bounds());
            assert!(
                tip.bounds().size.height > px(3. * 16.),
                "wrapped: {:?}",
                tip.bounds()
            );
        })
        .unwrap();
    }
}
