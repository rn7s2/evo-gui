//! The empty tab's data, read from the server that served this page.
//!
//! The desktop app fills its choosers and its history by running two commands
//! (`evo-swarm catalog --json`, `evo-agent sessions --json`) and handing the two
//! documents to the window. A browser runs nothing, so it asks the `evo-web`
//! server for the same two documents — `GET /api/catalog` and `GET /api/sessions`,
//! which *are* those commands run on the serving machine.
//!
//! ```text
//!   page ──▶ GET /api/catalog   ─┐
//!          ▶ GET /api/sessions  ─┴─▶ workspace::launcher_data ─▶ every empty tab
//! ```
//!
//! The conversion from a body to the window's [`LauncherData`] is
//! `workspace::launcher_data`'s, so a row in a browser is the row a desktop shows
//! for the same journal. What is here is only the *fetch* — through
//! `swarm_client`'s browser client, which is the same client a tab's session uses,
//! so there is no second HTTP client in the frontend.
//!
//! Without this a browser's history list and model choosers are permanently
//! empty: the window has nothing to push into them, because nothing ever read
//! them.

use gpui_kit::App;

use swarm_client::Client;
use workspace::launcher_data;
use workspace::WorkspaceView;

/// Read both documents and give them to the window.
///
/// Called once, after the window exists — the same place the desktop calls its own
/// startup loads (`crates/app/src/startup.rs`), and for the same reason: the
/// window has to exist before there is anything to hand the data to. `view` is
/// the workspace view that window opened, which is what the data is pushed into.
///
/// Both reads run on the page's own event loop and the window is updated once,
/// when both are in: a launcher that arrived half-filled would rebuild the
/// choosers twice, and the second rebuild is what a person would see.
pub fn start(cx: &mut App, view: gpui_kit::Entity<WorkspaceView>) {
    // The window the tabs live in. This shell opens exactly one, as the app does.
    let Some(window) = cx.windows().into_iter().next() else {
        log::error!("evo-webapp: launcher data has no window to reach");
        return;
    };

    // A same-origin prefix with no token: `evo-web` holds the credential and adds
    // it to every request it forwards, which is why nothing here names one.
    let client = match Client::at_base(crate::API_ROOT) {
        Ok(client) => client,
        Err(error) => {
            log::error!("evo-webapp: no client for {}: {error}", crate::API_ROOT);
            return;
        }
    };

    // The page's event loop drives both futures; `cx.spawn` is what puts the
    // result back on the UI thread, where the window can be borrowed.
    cx.spawn(async move |cx| {
        // Both are awaited, one after the other. They are two requests to the same
        // origin and each is answered by a process this server runs; the second
        // waits for the first rather than racing it, which costs one round trip and
        // buys not needing a runtime to join two futures on a page that has none.
        let catalog = client.get_json("/catalog").await;
        let sessions = client.get_json("/sessions").await;

        // A read that failed is a reason the tab can show (§9.5), not a silent
        // empty list: the first failure's own words, since both are the same
        // server and a person needs one thing to fix.
        let error = catalog
            .as_ref()
            .err()
            .or(sessions.as_ref().err())
            .map(|error| error.to_string());

        let (catalog, sessions) = (catalog.as_ref().ok(), sessions.as_ref().ok());
        // The rows' clock is the page's own (`Date.now`), not `std::time`: a clock is
        // the one thing the two targets do not share, and on `wasm32-unknown-unknown`
        // `SystemTime::now()` panics rather than returning an error. Seconds, because
        // that is what the rows' relative times are counted in.
        let now = (js_sys::Date::now() / 1000.0) as i64;
        let data = launcher_data::from_server(sessions, catalog, error, now);

        // One update, not two nested ones: `view.update` inside `window.update`
        // borrows the same app state twice and panics with "RefCell already
        // borrowed" — measured, before this was a single call. The view is reached
        // through the window's update, which is where a window's own state is
        // legal to touch.
        let shown = window.update(cx, |_root, window, cx| {
            view.update(cx, |view, cx| view.set_launcher_data(data, window, cx));
        });
        if let Err(error) = shown {
            log::error!("evo-webapp: the window could not be given its data: {error}");
        }
    })
    .detach();
}
