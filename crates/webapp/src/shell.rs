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

/// The CJK face the browser falls back to when a requested family has no glyph.
///
/// IBM Plex Sans has **no CJK coverage at all**, which is why CJK text was
/// missing from the web build: every Han, kana and hangul codepoint exhausted the
/// family and drew as nothing. Noto Sans SC is the Simplified-Chinese subset of
/// Google's Noto CJK — a sans-serif designed to sit beside a Latin UI face — and
/// it is the family this shell names in the fallback chain below.
///
/// Chosen for three reasons, in this order:
///
/// * **Licence.** SIL Open Font License 1.1, the same licence as the IBM Plex
///   faces already here, and its `OS/2.fsType` is `0` — installable embedding, so
///   there is nothing to negotiate about shipping it. The licence text is
///   `assets/licenses/NotoSansSC-OFL.txt`.
/// * **Coverage.** 30,890 codepoints: the 20,976 of the CJK Unified Ideographs
///   block, 6,582 of Extension A, kana, hangul, CJK punctuation and fullwidth
///   forms — the range a UI actually draws, rather than a print typesetter's.
/// * **Size.** See [`CJK_FONT_SIZE_NOTE`].
#[cfg(target_family = "wasm")]
const CJK_FAMILY: &str = "Noto Sans SC";

/// What shipping the CJK face costs, measured rather than guessed.
///
/// This is the one number a reviewer will ask about, so it is written down where
/// the decision was made:
///
/// * the `.otf` is **8,331,336 bytes** (8.0 MiB);
/// * the built `dist/web/evo_webapp_bg.wasm` goes from **32,174,915** to
///   **40,741,231 bytes** (+8,566,316, a 27% increase); the fonts are
///   `include_bytes!`d, so that is the whole cost;
/// * `evo-web` serves the module **uncompressed** (`application/wasm`, no
///   `Content-Encoding`), so that is also what a browser downloads. gzip would
///   take it to 7.2 MB and brotli to 6.0 MB, but the server is another lane's and
///   does neither.
///
/// A **subset** was measured and rejected, and the reason is worth recording
/// because it is counter-intuitive: the CJK Unified block *is* the font. Cutting
/// everything outside it — and everything outside the 6,763 GB2312 hanzi that
/// cover ~99.7% of modern Chinese text — saves only 8.3 MB → 5.5 MB, because the
/// CFF data for those 6,763 ideographs is nearly all of the file. The smaller win
/// is not worth a build-time font-subsetting pipeline, and that pipeline would
/// need `fontTools` (a Python dependency this repo's toolchain does not have)
/// plus a decision about *which* characters to keep — a product question, not a
/// build one. So the full OFL face ships, and the honest cost is stated here
/// instead of hidden behind a subset.

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
    // The CJK face. One weight, deliberately: a second would cost another 8 MB
    // for a bold the fallback chain would rarely pick, since a missing glyph is
    // chosen per *character* and CJK text is not usually bold. See
    // [`CJK_FAMILY`] for what this costs.
    include_bytes!("../../../assets/fonts/NotoSansSC-Regular.otf"),
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

    // And give the *proportional* family the CJK fallback. Registering the face
    // above is necessary but nowhere near sufficient — the chain has to be set on
    // an element, which `CjkFallbackRoot` does. See [`cjk_fallback_font`].
}

/// Make the proportional font family reach the CJK face.
///
/// Registering a font only puts it in the text system's database; nothing selects
/// it. What selects a font, read from the source rather than guessed:
///
/// * `TextSystem::resolve_font` (`gpui-pre`'s `text_system.rs`) tries the
///   requested family, then walks a **fixed** `fallback_font_stack` —
///   `.ZedMono`, `.ZedSans`, `Helvetica`, `Segoe UI`, `Ubuntu`, `Adwaita Sans`,
///   `Cantarell`, `Noto Sans`, `DejaVu Sans`, `Arial` — and then **panics**. That
///   list has no CJK family and a page cannot change it, so the panic is not what
///   happens here: `.SystemUIFont` maps to `"IBM Plex Sans"` (the web platform
///   constructs its text system with that name) and that *does* resolve.
/// * What happens instead is per-character, in
///   `gpui-pre-wgpu`'s `cosmic_text_system.rs`: `compute_run_spans` walks the
///   text grapheme by grapheme and, for each character the primary font does not
///   cover, picks a font from `Font.fallbacks` — the **`fallbacks` field on the
///   requested font**, built from `font_fallbacks` in the text style
///   (`gpui-pre`'s `style.rs`). `charmap_covers` is what decides, and a character
///   nothing covers gets glyph id 0.
/// * That is why the chain must be set explicitly: `Font.fallbacks` is `None` by
///   default, and with it `None` a missing character has nowhere to go.
/// * `cosmic-text`'s own script fallback cannot save this either, and it is worth
///   knowing why: it selects its platform table with
///   `#[cfg(not(any(all(unix, …), windows)))]` → `fallback/other.rs`, and
///   **`wasm32-unknown-unknown` is not unix**, so the web build gets `other.rs`,
///   whose `common_fallback()` and `script_fallback()` both return `&[]`. There is
///   no built-in CJK list on this target at all.
///
/// The chain, as the one place it is written down: Latin keeps the family the
/// theme names; anything that family cannot draw is looked up here, in order.
///
/// See [`cjk_fallback_font`] for why this is set on an element at all rather than
/// on the platform or the theme.
#[cfg(target_family = "wasm")]
fn cjk_fallback_font() -> gpui_kit::Font {
    gpui_kit::Font {
        // The virtual name, resolved by `font_name_with_fallbacks` to whatever the
        // platform calls its system UI font — `"IBM Plex Sans"` on the web.
        // Naming the concrete family here would pin the browser to it even if the
        // platform's choice changed.
        family: ".SystemUIFont".into(),
        fallbacks: Some(gpui_kit::FontFallbacks::from_fonts(vec![
            CJK_FAMILY.to_owned()
        ])),
        ..Default::default()
    }
}

/// The window's content, with the CJK fallback chain on it.
///
/// A wrapper rather than a change to `WorkspaceView` for two reasons: the chain is
/// a *web-only* fact (the desktop reads the machine's own fonts, which have CJK
/// coverage, and this crate is not compiled for it at all), and `workspace` is
/// another lane's crate. A text style cascades to every child, so one `.font(…)`
/// on a full-size div covers every element the window draws.
struct CjkFallbackRoot {
    view: gpui_kit::Entity<WorkspaceView>,
}

impl gpui_kit::Render for CjkFallbackRoot {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        _cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        gpui_kit::div()
            .size_full()
            .font(cjk_fallback_font())
            .child(self.view.clone())
    }
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
            //
            // It is the *workspace* view, not the wrapper: `open_window` returns
            // whatever its closure built, and the closure hands back the wrapper,
            // so the inner entity is taken from the wrapper's own field.
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
                let workspace = cx.new(|cx| WorkspaceView::new(window, cx));
                view = Some(workspace.clone());
                // Wrapped in [`CjkFallbackRoot`], which is the one thing this
                // shell adds to the view: the fallback chain that makes CJK draw.
                cx.new(|_cx| CjkFallbackRoot { view: workspace })
            }) {
                Ok(_) => {}
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
