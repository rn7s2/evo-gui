//! The quit sequence, in a headless app (§9.8, §3).
//!
//! The full path — a real window closing, every tab's swarm walking the ladder —
//! needs a display and real swarms: `scripts/single_instance_check.sh` drives
//! the real binary for the launch side, and `tab_engine`'s end-to-end tests
//! drive the ladder itself. What is left to prove here is this crate's half:
//! that the sequence starts exactly once, runs to its end, and remembers the
//! window on the way out.

use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell, begin_quit, is_quitting, take_engines};
use gpui_kit::{App, TestAppContext};
use store::app_state::{AppState, Binaries, SCHEMA_VERSION};
use store::model_cache::ModelCache;
use store::paths::Root;

fn temp_root(name: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Root::at(dir)
}

fn install(cx: &mut App, root: &Root) -> AppLog {
    let log = AppLog::open(root);
    let state = AppState {
        binaries: Binaries::default(),
        ..AppState::default()
    };
    Shell::new(root.clone(), log.clone(), state, ModelCache::default()).install(cx);
    log
}

fn log_text(log: &AppLog) -> String {
    std::fs::read_to_string(log.path()).unwrap_or_default()
}

#[gpui_kit::test]
fn quitting_starts_once_and_runs_to_the_end(cx: &mut TestAppContext) {
    let root = temp_root("quit");
    let log = {
        let root = root.clone();
        cx.update(move |cx| install(cx, &root))
    };

    cx.update(|cx| {
        assert!(!is_quitting(cx), "nothing has asked to quit yet");
        begin_quit(cx);
        assert!(is_quitting(cx), "the quit sequence has begun");
        assert!(take_engines(cx).is_empty(), "no tab has a swarm in this test");
        // Idempotent: a window close followed by Cmd-Q must not start a second.
        begin_quit(cx);
    });

    // The sequence finishes on its own: the ladder is empty here, so the
    // shutdown thread returns, app.json is written, and the task that follows
    // ends the app.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !log_text(&log).contains("stopped; exiting") {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }

    let text = log_text(&log);
    assert_eq!(
        text.matches("quitting: stopping every tab").count(),
        1,
        "the sequence ran exactly once:\n{text}"
    );
    assert!(text.contains("stopped; exiting"), "it ran to its end:\n{text}");
    assert!(root.app_json().exists(), "app.json was written on the way out");
    let state = AppState::load(&root);
    assert_eq!(state.version, SCHEMA_VERSION);
    let _ = std::fs::remove_dir_all(root.path());
}

#[gpui_kit::test]
fn the_sequence_survives_being_started_without_a_window(cx: &mut TestAppContext) {
    let root = temp_root("quit-nowindow");
    let log = {
        let root = root.clone();
        cx.update(move |cx| install(cx, &root))
    };
    cx.update(begin_quit);

    // `save_state` asks the window for its tabs, and there is none here: the
    // sequence still has to run to its end. Waiting for it also keeps the test
    // deterministic — the shutdown thread must be finished before the app the
    // test owns goes away.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !log_text(&log).contains("stopped; exiting") {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        log_text(&log).contains("stopped; exiting"),
        "the sequence ran to its end without a window:\n{}",
        log_text(&log)
    );
    cx.update(|cx| assert!(is_quitting(cx)));
    let _ = std::fs::remove_dir_all(root.path());
}
