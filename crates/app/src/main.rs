//! evo-desktop: the app shell (§7.1).
//!
//! Everything the window shows lives in the `workspace` crate; this binary opens
//! the one window and decides when the app ends.

use gpui_kit::{AppContext as _, QuitMode};

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        // Closing the window quits, macOS included — where GPUI's default is to
        // keep running until asked (§7.1).
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(workspace::window_options(cx), cx, |window, cx| {
                cx.new(|cx| workspace::WorkspaceView::new(window, cx))
            })
            .expect("open the evo-desktop window");
        });
}
