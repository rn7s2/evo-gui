//! evo-desktop: the app shell (§7.1).
//!
//! Everything the window shows lives in the `workspace` crate; this crate is the
//! process around it — one instance, the window's remembered bounds, the
//! launch-time loading, and the way the app ends.
//!
//! The window opens before anything slow happens: `app.json` and the model cache
//! are two small files read on the way in, and everything else — the session
//! scan, a catalog read, the tabs stopping on the way out — runs on other
//! threads and arrives through a channel (§2 rule 6).
//!
//! ```text
//!   main ──▶ run()
//!             ├── single instance: lock, or knock and exit 0
//!             ├── window (bounds from app.json, clamped to the display)
//!             ├── activation watcher: a second launch raises this window
//!             ├── startup: cache → sessions → catalog, pushed to the empty tabs
//!             └── quit: app.json, the tabs' pipes closed, then exit
//! ```

mod about;
mod bounds;
mod housekeeping;
mod launcher;
mod logging;
mod menus;
mod quit;
mod settings;
mod startup;
mod theme;

pub use about::{start as start_version_probe, Versions};
pub use bounds::{window_bounds, window_options, Tracker};
pub use housekeeping::{prune_tab_dirs, Pruned, TAB_DIR_TTL};
pub use launcher::{history_entries, push_launcher_data, tab_count, Launcher};
pub use logging::{AppLog, Level, LOG_NAME};
pub use menus::open_about;
pub use menus::{install as install_menus, open_settings, CloseTab, NewTab, QuitApp};
pub use quit::{
    begin as begin_quit, is_quitting, open_tabs, remember_tab_set, take_engines, TabRecord,
};
pub use settings::{apply as apply_settings, open as open_settings_panel, DIALOG_CONTENT_ID};
pub use startup::{refresh_catalog, start as start_background_loads};
pub use theme::{follow_appearance, mode_for};

use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Global, QuitMode, Subscription, WeakEntity};

use store::app_state::{AppState, Binaries, Recent, Theme};
use store::model_cache::ModelCache;
use store::paths::Root;
use store::single::{Activation, SingleInstance};
use workspace::WorkspaceView;

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
    /// What every empty tab is shown (§9.4, §9.5): the catalog, the history,
    /// and the errors when either could not be learned.
    pub launcher: Launcher,
    /// What `evo-swarm --version` and `evo-agent --version` said, read once at
    /// startup (the About dialog's second line).
    pub versions: Versions,
    /// Light, dark, or whatever the system says (§7.1). `app.json`'s setting,
    /// kept here so the appearance observer and the quit path can both read it.
    pub theme: Theme,
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
    pub fn new(root: Root, log: AppLog, state: AppState, cache: ModelCache) -> Shell {
        Shell {
            root,
            log,
            versions: Versions::default(),
            theme: state.theme,
            binaries: state.binaries,
            recents: state.recents,
            launcher: Launcher::new(cache),
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

/// What a tab's launch needs from the app (§3): the binaries `app.json` names,
/// this app's own root, and the environment the child runs in.
///
/// The binary paths are in `app.json` so they can be pointed elsewhere (§9.1);
/// without passing them on, a window would run whatever the defaults happen to
/// be and the file would be a lie. The window builds a
/// [`LaunchSpec`](store::launch::LaunchSpec) out of this — the choosers are the
/// window's own state — and the client spawns that.
pub fn launch_env(cx: &App) -> workspace::LaunchEnv {
    let shell = cx.global::<Shell>();
    workspace::LaunchEnv {
        swarm_bin: shell.binaries.evo_swarm.clone(),
        agent_bin: shell.binaries.evo_agent.clone(),
        root: shell.root.clone(),
        env: Vec::new(),
        env_remove: Vec::new(),
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
            Shell::new(root.clone(), log.clone(), state, cache).install(cx);

            // The menu bar, its shortcuts, and the app's action handlers.
            menus::install(cx);

            let options = bounds::window_options(cx, stored_bounds);
            let opened = gpui_kit::open_window(options, cx, |window, cx: &mut App| {
                let tracker = cx.new(|cx| Tracker::new(window, cx));
                // The window's tabs start servers with the binaries `app.json`
                // names, out of this app's own root.
                let env = std::sync::Arc::new(launch_env(cx));
                let view = cx.new(|cx| WorkspaceView::with_config(env, window, cx));
                cx.update_global::<Shell, _>(|shell, _| shell.tracker = Some(tracker));
                view
            });
            let (window, view) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    log.error(format!("could not open the window: {error}"));
                    cx.quit();
                    return;
                }
            };
            log.info("window open");
            // Where the window actually is, after the clamp — `app.json`'s
            // numbers are a request (§2 rule 1), and this is the answer.
            if let Ok(bounds) =
                window.update(cx, |_root, window, _cx| bounds::stored_from_window(window))
            {
                log.info(format!(
                    "window bounds {}x{} at {:.0},{:.0}",
                    bounds.width,
                    bounds.height,
                    bounds.x.unwrap_or(0.0),
                    bounds.y.unwrap_or(0.0)
                ));
            }
            // Light or dark, and following the system from here on (§7.1).
            let appearance = window
                .update(cx, |_root, window, cx| theme::follow_appearance(cx, window))
                .ok();
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(appearance) = appearance {
                    shell.subscriptions.push(appearance);
                }
            });
            // Closing the window: the workspace hands the tabs' engines over, and
            // the app owns stopping them.
            view.update(cx, |view, _cx| {
                view.set_quit_hook(Box::new(|request, _window, cx| {
                    quit::begin_with(cx, request.engines);
                    // Let the close go ahead: the quit sequence ends the process
                    // itself (the quit mode is explicit).
                    false
                }));
            });
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

            // The binaries' own versions, for the About dialog (§7.1).
            about::start(cx);
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
        activation
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "?".to_owned())
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
                match primary
                    .activation_rx()
                    .recv_timeout(std::time::Duration::from_millis(250))
                {
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
