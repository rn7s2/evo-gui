//! §13, from the app's side: the Settings panel opens over the window, Save
//! persists what it hands over, and the *next* tab spawns with it.
//!
//! The panel's own rules — the probing, the reset, Escape, the live theme — are
//! `crates/settings`' tests. What this one is about is the app's half of the
//! contract: `SettingsEvent::Saved` reaching `app.json`, the [`Shell`], and the
//! window's next tab.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{open_settings_panel, AppLog, Shell};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use settings::{SettingsPanel, AGENT_PATH_ID, SAVE_ID, SWARM_PATH_ID, THEME_ID};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{SwarmConfig, TabContentEvent, TabState, WorkspaceView};

/// What the test saves: a swarm binary that is not there, so the next tab's launch
/// says which binary it could not run — and an agent path that is not there either.
const SAVED_SWARM: &str = "/nonexistent/evo-swarm-from-settings";
const SAVED_AGENT: &str = "/nonexistent/evo-agent-from-settings";

/// How long a boot that cannot succeed is given to say so.
const WAIT: Duration = Duration::from_secs(60);

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

/// A window on a temp app root and a `Shell` whose binaries and theme are the ones
/// the test starts from — `run`'s own wiring, minus the background loads.
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    binaries: Binaries,
    theme: Theme,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            let state = AppState {
                binaries,
                theme,
                ..AppState::default()
            };
            Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
            // The same config `run` builds, from the same binaries.
            let config = Arc::new(SwarmConfig {
                swarm_bin: cx.global::<Shell>().binaries.evo_swarm.clone(),
                agent_bin: cx.global::<Shell>().binaries.evo_agent.clone(),
                root,
                env: Vec::new(),
                env_remove: Vec::new(),
            });
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1200.), px(800.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
            )
        })
        .expect("the window");
    cx.update(|cx| {
        cx.global_mut::<Shell>().view = Some(view.downgrade());
    });
    (window, view)
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Type a path into one of the panel's fields, the way a person does: click the
/// field, take everything, type over it.
fn type_into(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str, text: &str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
        window.press("cmd-a", cx);
        window.input(text, cx);
        window.render_frame(cx);
    })
    .expect("typing into the panel");
}

/// Choose one of the three theme choices by clicking its third of the row — the
/// same gesture the settings crate's own capture uses.
fn choose_theme(cx: &mut TestAppContext, window: AnyWindowHandle, fraction: f32) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let row = window.find(THEME_ID).bounds().size;
        window.click_at(THEME_ID, point(row.width * fraction, row.height / 2.), cx);
        window.render_frame(cx);
    })
    .expect("choosing a theme");
}

#[gpui_kit::test]
fn saving_settings_persists_them_and_the_next_tab_spawns_with_them(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let root = temp_root("settings");
    let (window, view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from("/usr/local/bin/evo-swarm"),
            evo_agent: PathBuf::from("/usr/local/bin/evo-agent"),
        },
        Theme::System,
    );

    // What the Settings… menu item does with the window it is handed.
    // The entity itself is not needed below: everything under test shows up in
    // `app.json`, on the `Shell`, and in the next tab's failure.
    let _panel: Entity<SettingsPanel> = cx
        .update_window(window, |_, window, cx| open_settings_panel(window, cx))
        .expect("the settings panel");
    assert!(
        cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "the panel is up, over the app's window"
    );

    // A person's edits: two paths typed over, and Dark clicked.
    type_into(cx, window, SWARM_PATH_ID, SAVED_SWARM);
    type_into(cx, window, AGENT_PATH_ID, SAVED_AGENT);
    choose_theme(cx, window, 5. / 6.);

    cx.update_window(window, |_, window, cx| {
        window.click(SAVE_ID, cx);
        window.render_frame(cx);
    })
    .expect("Save");
    assert!(
        !cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "Save closes the panel (the panel's own dismissal)"
    );

    // What was saved is in `app.json`...
    let state = AppState::load(&root);
    assert_eq!(
        state.binaries,
        Binaries {
            evo_swarm: PathBuf::from(SAVED_SWARM),
            evo_agent: PathBuf::from(SAVED_AGENT),
        },
        "both paths were persisted"
    );
    assert_eq!(state.theme, Theme::Dark, "and the theme that was chosen");
    // ...and on the Shell, which is what a launch and the About dialog read.
    let (binaries, theme) = cx.update(|cx| {
        let shell = cx.global::<Shell>();
        (shell.binaries.clone(), shell.theme)
    });
    assert_eq!(binaries.evo_swarm, PathBuf::from(SAVED_SWARM));
    assert_eq!(binaries.evo_agent, PathBuf::from(SAVED_AGENT));
    assert_eq!(theme, Theme::Dark);

    // The window's *next* tab spawns with them: the swarm binary it was started
    // with is the one Settings saved, and that is what its failure names.
    let folder = root.path().join("project");
    std::fs::create_dir_all(&folder).expect("a folder to start in");
    let tab = cx
        .update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.open_empty_tab(window, cx))
        })
        .expect("a new tab");
    cx.update(|cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: LaunchPlan::default(),
            })
        });
    });
    wait_for(cx, "the new tab's failure", |cx| {
        cx.update(|cx| matches!(tab.read(cx).state(), TabState::Failed { .. }))
    });
    let TabState::Failed { log_tail, .. } = cx.update(|cx| tab.read(cx).state().clone()) else {
        unreachable!("just matched")
    };
    assert!(
        log_tail.contains(SAVED_SWARM),
        "the new tab ran the binary Settings saved: {log_tail:?}"
    );

    let _ = std::fs::remove_dir_all(root.path());
}
