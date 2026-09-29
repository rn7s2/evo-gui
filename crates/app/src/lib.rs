//! evo-desktop: the app shell (§7.1).
//!
//! Everything the window shows lives in the `workspace` crate; this crate is the
//! process around it — one instance, the window's remembered bounds, the
//! launch-time loading, and the way the app ends.
//!
//! The window opens before anything slow happens: `app.json` and the model cache
//! are two small files read on the way in, and everything else — the session
//! scan, a catalog probe, the tabs shutting down on the way out — runs on other
//! threads and arrives through a channel (§2 rule 6).
//!
//! ```text
//!   main ──▶ run()
//!             ├── single instance: lock, or knock and exit 0
//!             ├── window (bounds from app.json, clamped to the display)
//!             ├── activation watcher: a second launch raises this window
//!             ├── startup: cache → scan → probe, pushed to the empty tabs
//!             └── quit: every tab's ladder, then app.json, then exit
//! ```

mod bounds;
mod logging;
mod quit;
mod startup;

pub use bounds::{Tracker, window_bounds, window_options};
pub use logging::{AppLog, Level, LOG_NAME};
pub use quit::{SHUTDOWN_DEADLINE, begin as begin_quit, is_quitting, take_engines};
pub use startup::{CACHE_MAX_AGE, TabData, cache_is_stale};

use gpui_kit::{App, Entity, Global, KeyBinding, QuitMode, Subscription, WeakEntity};
use gpui_kit::prelude::*;

use store::app_state::{AppState, Binaries, Recent};
use store::model_cache::ModelCache;
use store::paths::Root;
use store::single::{Activation, SingleInstance};
use workspace::WorkspaceView;


gpui_kit::actions!(evo_desktop, [QuitApp]);

/// Everything the app's threads share: where its files are, what the launch
/// loaded, and the window it is driving.
///
/// A gpui global rather than a field somewhere, because the pieces that need it
/// (the quit path, the activation watcher, the startup tasks) are registered as
/// callbacks with only an `App` to reach through.
pub struct Shell {
    pub root: Root,
    pub log: AppLog,
    /// The window's root view, once it exists. Weak: the window owns it.
    pub view: Option<WeakEntity<WorkspaceView>>,
    /// The bounds watcher, so the quit path can ask where the window was.
    pub tracker: Option<Entity<Tracker>>,
    /// The catalog the launch found on disk.
    pub cache: ModelCache,
    /// The binaries this launch spawns.
    pub binaries: Binaries,
    /// The recents `app.json` held at launch; the session scan merges with them
    /// (§9.5).
    pub recents: Vec<Recent>,
    /// Set once the quit sequence has begun.
    pub quitting: bool,
    /// Subscriptions that must outlive the closure that made them.
    subscriptions: Vec<Subscription>,
}

impl Global for Shell {}

impl Shell {
    /// The app's shared state. [`run`] builds one; a test can build one too,
    /// which is how the quit sequence is exercised without a real window.
    pub fn new(
        root: Root,
        log: AppLog,
        binaries: Binaries,
        recents: Vec<Recent>,
        cache: ModelCache,
    ) -> Shell {
        Shell {
            root,
            log,
            binaries,
            recents,
            cache,
            view: None,
            tracker: None,
            quitting: false,
            subscriptions: Vec::new(),
        }
    }

    /// Put it where the callbacks look for it.
    pub fn install(self, cx: &mut App) {
        cx.set_global(self);
    }
}

/// Open the app: one instance, one window, and the background loads. Returns
/// when the process is done.
pub fn run() {
    let root = Root::default();
    let log = AppLog::open(&root);
    log.info(format!(
        "evo-desktop {} starting (pid {}, root {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        root.path().display()
    ));

    // §2 rule 1: one instance. A second launch asks the first to come forward
    // and leaves with success — the user asked for the app, and it is up.
    let primary = match SingleInstance::acquire(&root) {
        Ok(SingleInstance::Primary(primary)) => Some(primary),
        Ok(SingleInstance::Secondary(secondary)) => {
            log.info(format!(
                "another instance is running (activation acknowledged: {}); exiting",
                secondary.activated
            ));
            return;
        }
        Err(error) => {
            // A lock we cannot take is not a reason to refuse to start.
            log.error(format!("single instance: {error} — starting anyway"));
            None
        }
    };

    let state = AppState::load(&root);
    let stored_bounds = state.window;
    let binaries = state.binaries.clone();
    let recents = state.recents.clone();
    let cache = ModelCache::load(&root);
    log.info(format!(
        "app.json: window {}x{} at {:?},{:?}, {} recent(s), binaries {} / {}",
        stored_bounds.width,
        stored_bounds.height,
        stored_bounds.x,
        stored_bounds.y,
        recents.len(),
        binaries.evo_swarm.display(),
        binaries.evo_agent.display()
    ));

    // The activation socket is listened to on a thread of its own; each knock
    // becomes a message the UI thread raises the window for.
    let (activation_tx, activation_rx) = async_channel::unbounded::<Activation>();
    if let Some(primary) = primary {
        spawn_activation_watcher(primary, activation_tx, log.clone());
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        // The quit sequence owns the exit: closing the window starts it, and the
        // process ends when every tab has stopped (or the deadline passed).
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx| {
            gpui_kit::init(cx);
            Shell::new(root.clone(), log.clone(), binaries, recents, cache).install(cx);

            // Cmd-Q goes through the same sequence as closing the window, rather
            // than ending the process on the spot and leaving swarms behind.
            cx.bind_keys([KeyBinding::new("cmd-q", QuitApp, None)]);
            cx.on_action(|_: &QuitApp, cx: &mut App| quit::begin(cx));

            let options = bounds::window_options(cx, stored_bounds);
            let opened = gpui_kit::open_window(options, cx, |window, cx: &mut App| {
                let tracker = cx.new(|cx| Tracker::new(window, cx));
                let view = cx.new(|cx| WorkspaceView::new(window, cx));
                cx.update_global::<Shell, _>(|shell, _| shell.tracker = Some(tracker));
                view
            });
            let (_window, view) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    log.error(format!("could not open the window: {error}"));
                    cx.quit();
                    return;
                }
            };
            log.info("window open");
            let closing = cx.on_window_closed(|cx: &mut App, _id| quit::begin(cx));
            cx.update_global::<Shell, _>(|shell, _| {
                shell.view = Some(view.downgrade());
                shell.subscriptions.push(closing);
            });

            // A launch while we are already running does nothing but raise this.
            cx.spawn(async move |cx| {
                while let Ok(activation) = activation_rx.recv().await {
                    cx.update(|cx| raise(cx, &log, &activation));
                }
            })
            .detach();

            startup::start(cx);
        });
}

/// Bring the app and its window to the front (§2 rule 1).
fn raise(cx: &mut App, log: &AppLog, activation: &Activation) {
    cx.activate(true);
    for handle in cx.windows() {
        let _ = handle.update(cx, |_view, window, _cx| window.activate_window());
    }
    log.info(format!(
        "activated by another launch (pid {})",
        activation.pid.map(|pid| pid.to_string()).unwrap_or_else(|| "?".to_owned())
    ));
}

/// Listen for knocks on the activation socket, on a thread of its own, and pass
/// them to the UI thread through a channel.
///
/// This thread owns the [`Primary`]: holding it is what keeps the lock — and
/// therefore what keeps this process *the* instance — for the life of the app.
fn spawn_activation_watcher(
    primary: store::single::Primary,
    tx: async_channel::Sender<Activation>,
    log: AppLog,
) {
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-activation".to_owned())
        .spawn(move || {
            let primary = primary;
            loop {
                match primary.activation_rx().recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(activation) => {
                        if tx.send_blocking(activation).is_err() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    if let Err(error) = spawned {
        log.error(format!("could not start the activation watcher: {error}"));
    }
}
