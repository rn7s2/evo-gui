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
//!             ├── login env: a terminal's environment, for every child
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
mod login_env;
mod menus;
mod quit;
mod settings;
mod settings_guard;
mod startup;
mod theme;

pub use about::{start as start_version_probe, Version, Versions};
pub use bounds::{window_bounds, window_options, Tracker};
pub use housekeeping::{prune_tab_dirs, Pruned, TAB_DIR_TTL};
pub use launcher::{history_entries, push_launcher_data, tab_count, Launcher};
pub use logging::{AppLog, Level, LOG_NAME};
pub use menus::open_about;
pub use menus::{
    install as install_menus, open_settings, ActualSize, CloseTab, NewTab, QuitApp, ZoomIn, ZoomOut,
};
pub use quit::{
    begin as begin_quit, is_quitting, open_tabs, remember_tab_set, swarms, watch_held_quit,
    TabRecord,
};
pub use settings::{apply as apply_settings, open as open_settings_panel, DIALOG_CONTENT_ID};
pub use settings_guard::{
    asking as asking_about_settings, DISCARD_ID, KEEP_EDITING_ID, SAVING_HELD_LOG,
};
pub use startup::{refresh_catalog, start as start_background_loads};
pub use theme::{follow_appearance, mode_for};

use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Global, QuitMode, Subscription, WeakEntity};

use store::app_state::{AppState, Binaries, Recent, Theme};
use store::cli::{self, PROMPT_NOTE_FLAG};
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
    /// The transcript's font zoom `app.json` held at launch (§7.2). The live one is
    /// the [`transcript::TranscriptZoom`] global; this is what the menu reads when
    /// it steps from it, and what a test asserts against.
    pub zoom: f32,
    /// The terminal pane's font family `app.json` held at launch (§13). What the
    /// Settings panel opens on, and what Save puts back.
    pub terminal_font: String,
    /// The terminal pane's type size `app.json` held at launch (§13).
    pub terminal_font_size: f32,
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
            zoom: state.zoom,
            terminal_font: state.terminal_font,
            terminal_font_size: state.terminal_font_size,
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
        env: HOST_ENV
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        env_remove: Vec::new(),
        prompt_note: prompt_note(shell),
    }
}

/// What this app tells every session it starts about itself (§7.2): the note the
/// transcript ships, at the path under this app's own root, written here so that a
/// launch only has to name it.
///
/// Whether a session can be told at all is a question about the **binaries**, not
/// about this app: `app.json` can point at a build from before the flag existed,
/// and evo refuses to start over a flag it does not know. So each is asked what it
/// takes (`store::cli`, a cached read of its own usage text, warmed off the UI
/// thread at startup) — and the answer decides, not a version number. A swarm is a
/// pair: its lanes are the agent's binary, so both have to take the flag or a lane
/// dies on it. What was decided goes to the log, because the first question a
/// formula left as source asks is whether its session was ever told.
fn prompt_note(shell: &Shell) -> workspace::PromptNote {
    let agent = cli::supports_prompt_note(&shell.binaries.evo_agent);
    let swarm = cli::supports_prompt_note(&shell.binaries.evo_swarm);
    let note = workspace::PromptNote::for_binaries(&shell.root, agent, swarm);
    if note.is_empty() {
        // The agent's own binary is the one every session runs — a swarm's lanes
        // included — so this is every launch.
        shell.log.warn(format!(
            "prompt note: {} does not take {PROMPT_NOTE_FLAG}; sessions will not be told \
             how this client renders",
            shell.binaries.evo_agent.display()
        ));
        return note;
    }
    if note.swarm.is_none() {
        shell.log.warn(format!(
            "prompt note: {} does not take {PROMPT_NOTE_FLAG}; only single-agent sessions \
             will be told",
            shell.binaries.evo_swarm.display()
        ));
    }
    match workspace::prompt_note::write(&shell.root) {
        Ok(path) => {
            shell.log.info(format!("prompt note: {}", path.display()));
            note
        }
        // No file, no flag: evo reads the path at parse time, and a path it cannot
        // read is a session that refuses to start. A session without the note still
        // runs — a formula is then read as its source — which is what the log line
        // is for.
        Err(error) => {
            shell.log.warn(format!(
                "prompt note: could not write {}: {error}; sessions will not be told",
                shell.root.prompt_note().display()
            ));
            workspace::PromptNote::default()
        }
    }
}

/// What every swarm the app starts is told about its host.
///
/// `EVO_BABY_EVO=0`: the "Baby Evo: I'm done!" banner (evo's
/// `extensions/360-baby-evo.lisp`) is for a terminal left in the background; in
/// the desktop app the tab's own dot already says a swarm finished, so the
/// extension stays silent for the sessions this app runs — and only for those:
/// the user's `init.lisp` and a terminal `evo` keep it.
pub const HOST_ENV: &[(&str, &str)] = &[("EVO_BABY_EVO", "0")];

/// Open the app: one instance, one window, and the background loads. Returns
/// when the process is done.
pub fn run() {
    // First of all, while this is the only thread: an app the Dock started has
    // launchd's environment, not a terminal's, and every server a tab starts
    // inherits this process's. Make it the one a terminal would give.
    let login_env = login_env::adopt();
    let root = Root::default();
    let log = AppLog::open(&root);
    log.info(format!(
        "evo-desktop {} starting (pid {}, root {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        root.path().display()
    ));
    if login_env.failed() {
        log.warn(login_env.to_string());
    } else {
        log.info(login_env.to_string());
    }

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
            // Closing the window: the app owns the quit, so the close is vetoed
            // while it runs — the window is the screen the swarms' exit is shown
            // on, and the app ends the process itself (§9.8).
            view.update(cx, |view, _cx| {
                view.set_quit_hook(Box::new(|request, _window, cx| {
                    quit::begin_with(cx, request.swarms);
                    true
                }));
            });
            // A held ⌘Q: the window sees the key down and up — the menu item
            // cannot — and this is what the hold runs when it is complete (§9.8).
            let held = quit::watch_held_quit(cx, &view);
            // A tab that held a session was removed: the history is stale and
            // should be re-read so the freed session appears in the empty tabs.
            let history_refresh = {
                let bin = cx.global::<Shell>().binaries.evo_agent.clone();
                cx.subscribe(&view, move |_, _: &workspace::HistoryStale, cx| {
                    startup::load_history(cx, bin.clone());
                })
            };
            let closing = cx.on_window_closed(|cx: &mut App, _id| quit::begin(cx));
            cx.update_global::<Shell, _>(|shell, _| {
                shell.view = Some(view.downgrade());
                shell.subscriptions.push(closing);
                shell.subscriptions.push(held);
                shell.subscriptions.push(history_refresh);
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

#[cfg(test)]
mod host_env_tests {
    #[test]
    fn the_app_silences_baby_evo_in_the_swarms_it_starts() {
        assert!(super::HOST_ENV.contains(&("EVO_BABY_EVO", "0")));
    }
}

/// The chain the window runs before a launch: ask the binaries what they take,
/// write the note where the flag will name it, and hand the launch the path (§7.2).
#[cfg(test)]
mod prompt_note_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use store::launch::Program;

    /// A binary that answers `--help` the way the real ones do — with the flag in
    /// its usage text, or without it (a build from before the flag existed).
    fn stub(dir: &std::path::Path, name: &str, takes: bool) -> PathBuf {
        let usage = if takes {
            "usage: evo-agent serve [--ready-file PATH] [--prompt-note PATH]"
        } else {
            "usage: evo-agent serve [--ready-file PATH]"
        };
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{usage}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn home(name: &str) -> Root {
        let path =
            std::env::temp_dir().join(format!("evo-desktop-note-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Root::at(path)
    }

    fn shell(root: &Root, agent: PathBuf, swarm: PathBuf) -> Shell {
        let state = AppState {
            binaries: Binaries {
                evo_swarm: swarm,
                evo_agent: agent,
            },
            ..AppState::default()
        };
        Shell::new(
            root.clone(),
            AppLog::open(root),
            state,
            ModelCache::default(),
        )
    }

    fn with_binaries(name: &str, agent_takes: bool, swarm_takes: bool) -> (Root, Shell) {
        let root = home(name);
        let dir = root.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        let shell = shell(
            &root,
            stub(&dir, "evo-agent", agent_takes),
            stub(&dir, "evo-swarm", swarm_takes),
        );
        (root, shell)
    }

    /// Both binaries take the flag: every session this window starts is told, and
    /// the file the launch names holds the note the renderer ships.
    #[test]
    fn a_binary_that_takes_the_flag_is_told_and_the_note_is_written() {
        let (root, shell) = with_binaries("takes", true, true);
        let note = prompt_note(&shell);
        let path = root.prompt_note();
        assert_eq!(note.for_program(Program::Agent), Some(path.clone()));
        assert_eq!(note.for_program(Program::Swarm), Some(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), workspace::NOTE);
        let _ = std::fs::remove_dir_all(root.path());
    }

    /// A build from before the flag existed: nothing is written and no launch is
    /// told — the window opens and the tab starts, which is what matters here.
    #[test]
    fn an_old_binary_is_launched_without_the_note() {
        let (root, shell) = with_binaries("old", false, false);
        let note = prompt_note(&shell);
        assert!(note.is_empty());
        assert_eq!(note.for_program(Program::Agent), None);
        assert_eq!(note.for_program(Program::Swarm), None);
        assert!(
            !root.prompt_note().exists(),
            "a note nobody can be told is not written"
        );
        let _ = std::fs::remove_dir_all(root.path());
    }

    /// One new binary and one old one: a single agent can be told, a swarm cannot —
    /// its lanes are the very binary the coordinator would pass the flag to.
    #[test]
    fn a_swarm_learns_nothing_from_a_binary_its_lanes_would_refuse() {
        let (root, shell) = with_binaries("mixed", true, false);
        let note = prompt_note(&shell);
        assert_eq!(note.for_program(Program::Agent), Some(root.prompt_note()));
        assert_eq!(note.for_program(Program::Swarm), None);
        assert!(root.prompt_note().is_file());
        let _ = std::fs::remove_dir_all(root.path());
    }
}
