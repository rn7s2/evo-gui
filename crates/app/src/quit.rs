//! Quitting the app (§9.8, §3).
//!
//! Closing the window (or Cmd-Q) does not end the process: it starts *this*.
//! Every tab's swarm is stopped the ladder's way — `POST /shutdown`, then
//! `SIGTERM`, then `SIGKILL` — on a thread of its own, so the UI never waits;
//! `app.json` is written while that runs; and the process exits when the last
//! tab is gone or the deadline passes.
//!
//! If the platform terminates us some other way (a logout, the dock's Quit
//! while our key binding never saw the keystroke), there is no time for any of
//! this: GPUI's own shutdown gives observers
//! [`SHUTDOWN_TIMEOUT`](gpui_kit::SHUTDOWN_TIMEOUT) — 200 ms — so the ladder is
//! started and left behind, and the swarms stop because this process
//! (`EVO_SERVE_WATCH_PID`) does.

use std::time::Duration;

use gpui_kit::{App, WeakEntity};

use store::app_state::AppState;
use tab_engine::{EngineHandle, shutdown_all};
use workspace::WorkspaceView;

use crate::Shell;

/// How long §3's ladder is given before a tab is left to finish stopping on its
/// own: `POST /shutdown` (≤10 s), then `SIGTERM` (5 s), then `SIGKILL`.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);

/// Whether the quit sequence has begun.
pub fn is_quitting(cx: &App) -> bool {
    cx.global::<Shell>().quitting
}

/// Start the quit sequence. Idempotent: the second caller (a close, then Cmd-Q)
/// returns at once.
pub fn begin(cx: &mut App) {
    if is_quitting(cx) {
        return;
    }
    cx.global_mut::<Shell>().quitting = true;

    let log = cx.global::<Shell>().log.clone();
    log.info("quitting: stopping every tab");

    // Take the engines first, then persist: the bounds and the recents are what
    // the next launch needs, and they do not depend on the swarms stopping.
    let engines = take_engines(cx);
    let tabs = engines.len();
    save_state(cx);

    let (done_tx, done_rx) = async_channel::bounded::<()>(1);
    let thread_log = log.clone();
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-shutdown".to_owned())
        .spawn(move || {
            let report = shutdown_all(engines, SHUTDOWN_DEADLINE);
            if report.all_exited() {
                thread_log.info(format!("shutdown: all {tabs} tab(s) exited"));
            } else {
                thread_log.warn(format!(
                    "shutdown: {} of {tabs} tab(s) exited; still stopping: {:?}",
                    report.exited.len(),
                    report.pending
                ));
            }
            let _ = done_tx.send_blocking(());
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the shutdown thread: {error}"));
    }

    // Quit once the ladder is done (or the deadline passed, which is when the
    // thread returns anyway).
    cx.spawn(async move |cx| {
        let _ = done_rx.recv().await;
        cx.update(|cx| {
            log.info("stopped; exiting");
            cx.quit();
        });
    })
    .detach();
}

/// The snapshot the quit sequence persists: the window's last bounds and the
/// recents `app.json` already knew.
fn save_state(cx: &mut App) {
    let (root, log, bounds) = {
        let shell = cx.global::<Shell>();
        let bounds = shell.tracker.as_ref().map(|tracker| tracker.read(cx).bounds());
        (shell.root.clone(), shell.log.clone(), bounds)
    };
    let mut state = AppState::load(&root);
    if let Some(bounds) = bounds {
        state.window = bounds;
    }
    match state.save(&root) {
        Ok(()) => log.info(format!("saved {}", root.app_json().display())),
        Err(error) => log.error(format!("could not save {}: {error}", root.app_json().display())),
    }
}

/// Every live tab's engine, taken out of the workspace.
///
/// **Seam.** The tab list owns its engines (lane 1's `launch::Started`), and the
/// call this needs — `WorkspaceView::take_engines` — is not there yet. Until it
/// is, a quit cannot reach them and the swarms are left to
/// `EVO_SERVE_WATCH_PID`: they stop when this process does. Wiring it is this
/// one expression.
pub fn take_engines(cx: &mut App) -> Vec<EngineHandle> {
    let Some(view) = view(cx) else {
        return Vec::new();
    };
    // TODO(lane 1): `view.update(cx, |view, cx| view.take_engines(cx))`. Until
    // it lands the tab list's engines are unreachable from the app, so a quit
    // leaves the swarms to `EVO_SERVE_WATCH_PID` (§3): they stop when this
    // process does.
    let _tabs = view.read(cx).tabs().len();
    Vec::new()
}

/// The window's root view, while it exists.
pub fn view(cx: &App) -> Option<gpui_kit::Entity<WorkspaceView>> {
    view_handle(cx)?.upgrade()
}

fn view_handle(cx: &App) -> Option<&WeakEntity<WorkspaceView>> {
    cx.global::<Shell>().view.as_ref()
}
