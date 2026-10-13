//! §13, from the app's side: a config file saved in the window takes effect without
//! restarting the app. The window announces it ([`workspace::ConfigChanged`]) and the app
//! reads evo's catalog again — with the `evo-swarm` it would spawn — and hands it to the
//! tabs, so the choosers and effort sliders follow the new file.
//!
//! The `evo-swarm` here is a script that prints a catalog document read from a file the
//! test rewrites: what a real one does after `init.lisp` registered another model.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{watch_config_saves, AppLog, Shell, LOG_NAME};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, Bounds, Entity, Point, TestAppContext,
    WindowBounds, WindowOptions,
};
use settings::{ConfigFile, ConfigSaved, ConfigScope};
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{ConfigChanged, LaunchEnv, WorkspaceView};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// A catalog naming one model, the way `evo-swarm catalog --json` prints it.
fn catalog(model: &str) -> String {
    serde_json::json!({
        "models": [{"id": model, "provider": "stub", "ready": true,
                    "effort_levels": ["low", "high"]}],
        "default_model": {"id": model, "provider": "stub"},
        "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
        "default_thinking": "high",
        "lanes": {"models": [{"id": model, "provider": "stub", "ok": true}]}
    })
    .to_string()
}

/// An `evo-swarm` whose `catalog --json` prints whatever `document` holds now.
fn fake_swarm(dir: &Path, document: &Path) -> PathBuf {
    let bin = dir.join("evo-swarm");
    std::fs::write(&bin, format!("#!/bin/sh\ncat '{}'\n", document.display()))
        .expect("a fake evo-swarm");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("executable");
    bin
}

fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    swarm: PathBuf,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            Shell::new(
                root.clone(),
                log,
                AppState {
                    binaries: Binaries {
                        evo_swarm: swarm,
                        ..Binaries::default()
                    },
                    ..AppState::default()
                },
                ModelCache::default(),
            )
            .install(cx);
            let config = Arc::new(LaunchEnv {
                swarm_bin: cx.global::<Shell>().binaries.evo_swarm.clone(),
                agent_bin: cx.global::<Shell>().binaries.evo_agent.clone(),
                root,
                ..LaunchEnv::default()
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
    cx.update(|cx| cx.global_mut::<Shell>().view = Some(view.downgrade()));
    (window, view)
}

fn cached_models(cx: &mut TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        cx.global::<Shell>()
            .launcher
            .cache
            .models()
            .into_iter()
            .map(|model| model.id)
            .collect()
    })
}

fn waited(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui_kit::test]
fn a_saved_init_file_reads_the_catalog_again(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);
    let dir = temp_dir("config-reload");
    let document = dir.join("catalog.json");
    std::fs::write(&document, catalog("before")).unwrap();
    let swarm = fake_swarm(&dir, &document);
    let root = AppRoot::at(dir.join("desktop"));
    let (_window, view) = open(cx, &root, swarm);
    let subscription = cx.update(|cx| watch_config_saves(cx, &view));
    let announce = |cx: &mut TestAppContext, scope: ConfigScope, file: ConfigFile| {
        cx.update(|cx| {
            view.update(cx, |_, cx| {
                cx.emit(ConfigChanged(ConfigSaved { scope, file }))
            })
        });
    };

    // The home's init.lisp now registers another model.
    std::fs::write(&document, catalog("after")).unwrap();

    // Lore and a project's file change nothing the app's catalog holds.
    announce(cx, ConfigScope::Global, ConfigFile::Lore);
    announce(cx, ConfigScope::Project(dir.clone()), ConfigFile::Init);
    cx.run_until_parked();
    assert!(
        cached_models(cx).is_empty(),
        "no catalog was read for those"
    );

    // The home's init.lisp is: the catalog is read again and handed to the tabs.
    announce(cx, ConfigScope::Global, ConfigFile::Init);
    waited(cx, "the catalog read again", |cx| {
        cached_models(cx) == ["after".to_string()]
    });
    let log = std::fs::read_to_string(root.path().join(LOG_NAME)).unwrap_or_default();
    assert!(
        log.contains("config: init.lisp saved, reading the catalog again"),
        "{log}"
    );
    drop(subscription);
}
