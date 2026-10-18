//! The browser shell: the process around the window, in a page.
//!
//! The desktop shell (`crates/app`) takes a single-instance lock, remembers the
//! window's bounds in `app.json`, adopts a login environment, prunes tab
//! directories and installs a menu bar. A page has none of that to do — the
//! browser owns the tab, the page owns the URL, and there is no child process to
//! inherit an environment — so this shell is the short version: initialize, open
//! one window on the shared [`WorkspaceView`], and let the platform drive.
//!
//! What it deliberately does **not** do is reimplement any of the app's
//! behaviour. The window is the same `workspace::WorkspaceView` the desktop app
//! opens, built with the same `LaunchEnv`; only the *transport* underneath a tab
//! differs, and [`crate::transport`] is where that is decided.

use gpui_kit::prelude::*;
use gpui_kit::{App, WindowOptions};

use store::app_state::Theme as StoredTheme;
use workspace::WorkspaceView;

/// This page's own origin — `scheme://host[:port]` — for building absolute URLs.
///
/// The kit's icon source needs an absolute origin (see the note at
/// `with_assets`), and the only one a page may fetch without CORS is its own.
/// `window.location.origin` is exactly that; the empty string is the honest
/// answer on a host with no `window` at all, which is a wasm runtime that is not
/// a browser and has no icons to fetch anyway.
#[cfg(target_family = "wasm")]
fn page_origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

#[cfg(not(target_family = "wasm"))]
fn page_origin() -> String {
    String::new()
}

/// The fonts the browser build lays text out with.
///
/// gpui's web platform builds its text system with **no system fonts** — a page
/// has no font database to ask — so `TextSystem::resolve_font` panics on the
/// first text layout unless a font is registered. The desktop app never meets
/// this: it reads the machine's own fonts, which is why a family name like
/// "Menlo" (`store::design::MONO_FONT`) is meaningful there and meaningless here.
///
/// IBM Plex Sans specifically, because that is the family gpui's web platform
/// names as its system UI font (`WebPlatform` constructs its text system with
/// `"IBM Plex Sans"`), and `.SystemUIFont` — what `Font::default()` asks for —
/// resolves to that. Shipping a different family would leave the default text
/// unresolved and panic exactly as no font at all does.
///
/// IBM Plex Mono is the monospace half. The kit's monospace default is the
/// platform's ("Menlo" on macOS, "DejaVu Sans Mono" elsewhere — which is what a
/// wasm build asks for, since it is neither), and the kit resolves it against the
/// fonts it can see; in a browser that is only what this list registered, so
/// without a mono face the code blocks, the symbol popup and the step clock would
/// all fall back to `.SystemUIFont` — proportional text where the design asks for
/// monospace. See [`MONO_FAMILY`].
///
/// Both families are SIL Open Font License (see `assets/licenses/IBMPlex*-OFL.txt`).
/// They are embedded with `include_bytes!`, so the browser has them the moment the
/// module loads rather than after a fetch the first frame would have to wait on.
#[cfg(target_family = "wasm")]
const UI_FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Bold.ttf"),
];

/// The monospace family the web build uses, named as the theme must spell it.
#[cfg(target_family = "wasm")]
const MONO_FAMILY: &str = "IBM Plex Mono";

/// Register the fonts this build lays text out with.
///
/// A no-op on a desktop, where the platform's own fonts are the answer — the same
/// window, the same family names, one shell's problem instead of both.
#[cfg(not(target_family = "wasm"))]
fn install_fonts(_cx: &mut App) {}

#[cfg(target_family = "wasm")]
fn install_fonts(cx: &mut App) {
    use std::borrow::Cow;
    let fonts: Vec<Cow<'static, [u8]>> = UI_FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        // Not fatal to the process, but the first text layout will panic, so it
        // must be loud rather than swallowed.
        log::error!("evo-webapp: could not register the UI fonts: {error}");
    }

    // Point the theme's monospace at the face just registered.
    //
    // The kit picks a monospace family by asking which of its candidates is
    // installed, and a browser's answer is this list — so "DejaVu Sans Mono"
    // (the kit's non-macOS, non-Windows default, which is what wasm gets) is not
    // there and it falls through to `.SystemUIFont`, a proportional face. Naming
    // the family outright is what makes code blocks and the symbol popup
    // monospace on the web.
    gpui_kit::component::Theme::update(cx, |theme| {
        theme.mono_font_family = MONO_FAMILY.into();
    });
}

/// Open the frontend in the page that loaded it. The `#[wasm_bindgen]` entry
/// point; the page calls it once the module is instantiated.
#[cfg_attr(target_family = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn start() {
    // The panic hook first: a panic after this point reaches the browser console
    // as a message rather than an opaque "unreachable executed".
    console_error_panic_hook::set_once();
    gpui_kit::web::init_logging();

    gpui_kit::application()
        // The kit's icon source fetches each icon over HTTP, one
        // `{endpoint}/assets/icons/<name>.svg` per icon, and its default endpoint
        // is the empty string — which makes that URL relative, and `reqwest`'s
        // wasm backend rejects a relative URL with "builder error" before any
        // request is sent. So every icon failed and the widget that wanted one
        // retried forever. The endpoint is therefore this page's own origin: the
        // bundle is served by the `evo-web` server that also serves
        // `/assets/icons/…`, so same-origin is both correct and the only thing a
        // page can reach without CORS.
        .with_assets(gpui_kit::assets::Assets::new(page_origin()))
        // No `QuitMode::Explicit`: a page is not a process, and closing it is the
        // browser's business. The desktop app owns its own exit because it must
        // stop the swarms it started; here the server owns those, and it outlives
        // any one page.
        .run(|cx| {
            gpui_kit::init(cx);
            // The design's palette, before any window exists: the same palettes
            // the desktop app installs, from the same crate. Without this the
            // browser drew gpui-kit's defaults — white where the design's paper is
            // `#EFE9DF` — which is the one thing that made the two shells look
            // like different applications.
            theme::install(cx, &theme::LogCrate);
            // Before any window exists: the first text layout needs a font, and
            // on the web there is no system one to fall back to.
            //
            // After the install, deliberately: `install` registers the palettes,
            // and the kit's own `mono_font.family` is applied when a mode is first
            // loaded — so setting the family first would be overwritten by it.
            install_fonts(cx);

            let options = WindowOptions {
                // The canvas is the page: the platform sizes the window to the
                // mount element, so there is no stored geometry to restore.
                window_bounds: None,
                ..Default::default()
            };

            // The view this shell opened, kept from `open_window`'s answer: the
            // desktop app holds the same handle on its `Shell`, and the launcher
            // data below has to reach it. `None` when the window did not open, in
            // which case there is nothing to fetch for either.
            let mut view: Option<gpui_kit::Entity<WorkspaceView>> = None;
            match gpui_kit::open_window(options, cx, |window, cx: &mut App| {
                // Light or dark, from the page's own `prefers-color-scheme`: a
                // browser has no `app.json`, so the system's answer *is* the
                // choice. `Window::appearance()` is the web platform's read of
                // `(prefers-color-scheme: dark)`, so `System` means exactly what
                // it means on the desktop.
                theme::apply(cx, window, StoredTheme::System, &theme::LogCrate);

                // The same view the desktop app opens. Its tabs reach their
                // sessions through `crate::transport`'s choice, not through a
                // spawn, so the launch environment names no binary: the server
                // that hosts this bundle is the one that runs them.
                cx.new(|cx| WorkspaceView::new(window, cx))
            }) {
                Ok((_window, opened)) => view = Some(opened),
                Err(error) => log::error!("evo-webapp: could not open the window: {error}"),
            }

            // The empty tab's catalog and history, from the server that served
            // this page (`GET /api/catalog`, `GET /api/sessions`). Without this the
            // choosers and the resume list are permanently empty: the desktop fills
            // them by running two commands, and a browser has none to run.
            //
            // After the window exists — there is nothing to hand the data to until
            // there is — and it lands whenever the server answers.
            if let Some(view) = view {
                crate::launcher::start(cx, view);
            }
        });
}
