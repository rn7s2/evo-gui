//! First run (§9.4): an empty `~/.evo/desktop` and no `model-cache.json`.
//!
//! The window opens on the loading hint, and the catalog arrives behind it: the
//! probe really runs the installed `evo-agent` against the stub `EVO_HOME`
//! (`swarm_client::harness::Fixture`), so the registry that fills the choosers is
//! the one a user's own `init.lisp` would have produced. When that probe cannot
//! run at all — `app.json` pointing `evo_agent` at nothing — the tab says so
//! instead of waiting forever.
//!
//! Both tests point this process's `EVO_HOME` at their own temp home, so they are
//! run one at a time.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use common::{open, set_evo_home, wait_for, NOTE};
use evo_desktop::Shell;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use store::app_state::{AppState, Binaries};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use swarm_client::harness::{Fixture, HarnessConfig};

/// The empty tab's caption, under the lanes chooser (§9.4): the loading hint, or
/// the catalog's error.
const CAPTION_ID: &str = "lanes-caption";

/// The model the fixture's `init.lisp` registers.
const MODEL: &str = "stub-a";

/// The lock the two tests hold, because each points this process's `EVO_HOME` at
/// its own temp home. A poisoned lock still runs the next test: whatever it left
/// behind died with the process that panicked.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A fixture whose evo home registers the stub provider, and the desktop directory
/// of a first run under it: `<EVO_HOME>/desktop`, which is `~/.evo/desktop` with
/// `EVO_HOME` at the fixture's temp home, and does not exist yet.
fn first_run(name: &str) -> (Fixture, AppRoot) {
    let fixture =
        Fixture::new(HarnessConfig::default()).expect("the fixture: the installed binaries");
    set_evo_home(&fixture);
    let root = AppRoot::at(fixture.home.join("desktop"));
    assert!(
        !root.path().exists(),
        "a first run has no state directory ({}): {}",
        name,
        root.path().display()
    );
    (fixture, root)
}

/// The model ids of a registry, for an assertion that reads like the choosers do.
fn registry_ids(registry: &serde_json::Value) -> Vec<String> {
    registry
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("id").and_then(|id| id.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[gpui_kit::test]
fn a_first_run_shows_the_loading_hint_then_the_probed_catalog(cx: &mut TestAppContext) {
    let _serial = lock();
    // The probe's threads wake this test's tasks: that is the point.
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let (fixture, root) = first_run("first-run");
    let (window, _view) = open(cx, &fixture, &root, AppState::default());

    // The frame the window comes up on, before anything is loaded: the choosers
    // hold Default alone, and the tab says what it is waiting for.
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("a drawn frame");
    let launch = cx.update(|cx| cx.global::<Shell>().launcher.data());
    assert!(launch.registry.is_none(), "no catalog until one is read");
    assert!(
        launch.model_cache.is_none(),
        "and nothing for the lanes chooser"
    );
    assert!(launch.catalog_error.is_none(), "nothing has failed yet");
    let caption = cx
        .update_window(window, |_, window, _| window.find(CAPTION_ID).visible())
        .expect("a drawn frame");
    assert!(caption, "the tab says it is loading models");

    // What `run` does once the window exists: the cache (empty), the session scan,
    // and the catalog probe.
    let started = Instant::now();
    cx.update(evo_desktop::start_background_loads);
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .expect("a drawn frame");

    // The window did not wait for any of it: the catalog is still unknown when the
    // next frame is drawn, and the tab is still saying so.
    let launch = cx.update(|cx| cx.global::<Shell>().launcher.data());
    assert!(
        launch.registry.is_none(),
        "the probe runs behind the window, not in front of it"
    );
    println!(
        "{NOTE} the window is up and loading after {:?}",
        started.elapsed()
    );

    // Then the probe lands, and the choosers have a catalog.
    wait_for(cx, "the probed catalog", |cx| {
        cx.update(|cx| !cx.global::<Shell>().launcher.cache.is_empty())
    });
    let ids = cx.update(|cx| {
        cx.global::<Shell>()
            .launcher
            .cache
            .models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<String>>()
    });
    assert!(
        ids.iter().any(|id| id == MODEL),
        "the probe found the stub home's own registry: {ids:?}"
    );
    println!("{NOTE} the probe answered after {:?}", started.elapsed());

    // That registry is what the window is handed — the choosers' options, and the
    // kernel api set the lanes chooser checks a model against.
    let launch = cx.update(|cx| cx.global::<Shell>().launcher.data());
    let registry = launch.registry.expect("the catalog the window is given");
    assert!(
        registry_ids(&registry).iter().any(|id| id == MODEL),
        "the choosers are offered {MODEL}: {:?}",
        registry_ids(&registry)
    );
    let cache = launch.model_cache.expect("the cache the window is given");
    assert!(
        !cache.kernel_apis.is_empty(),
        "the lanes chooser knows the kernel's wire apis"
    );

    // And it is remembered, so the next launch does not have to probe again.
    let saved = ModelCache::load(&root);
    assert!(
        saved.models().iter().any(|model| model.id == MODEL),
        "{} holds the probed catalog",
        root.model_cache().display()
    );

    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn a_catalog_that_cannot_be_probed_says_so_on_the_tab(cx: &mut TestAppContext) {
    let _serial = lock();
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let (fixture, root) = first_run("broken-agent");
    // `app.json` naming an evo-agent that is not there: the probe cannot run.
    let broken = Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: PathBuf::from("/nonexistent/evo-agent"),
    };
    let (window, _view) = open(
        cx,
        &fixture,
        &root,
        AppState {
            binaries: broken,
            ..AppState::default()
        },
    );

    cx.update(evo_desktop::start_background_loads);
    wait_for(cx, "the catalog error", |cx| {
        cx.update(|cx| cx.global::<Shell>().launcher.catalog_error.is_some())
    });
    let error = cx.update(|cx| cx.global::<Shell>().launcher.catalog_error.clone());

    // The tab has the reason, in the app's own words, and keeps Default: a catalog
    // that could not be learned is not an empty registry (§9.4).
    let error = error.expect("the error the tab is shown");
    assert!(error.contains("evo-agent"), "{error}");
    let launch = cx.update(|cx| cx.global::<Shell>().launcher.data());
    assert!(launch.registry.is_none(), "the choosers keep Default");
    assert_eq!(launch.catalog_error.as_deref(), Some(error.as_str()));
    let caption = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find(CAPTION_ID).visible()
        })
        .expect("a drawn frame");
    assert!(caption, "the tab shows the error where the hint was");

    let _ = std::fs::remove_dir_all(root.path());
}
