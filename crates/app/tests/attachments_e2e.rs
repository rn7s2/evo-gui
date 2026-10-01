//! One turn with attachments, end to end, against a real `evo-agent`.
//!
//! ```sh
//! EVO_AGENT_BIN=/tmp/evo-main-bin/build/evo-agent \
//! EVO_SWARM_BIN=/tmp/evo-main-bin/build/evo-swarm \
//! EVO_AGENT_REPO=/tmp/evo-main-bin \
//! EVO_STUB_MESSAGES=/tmp/evo-main-bin/tests/stub-messages.py \
//!   cargo test -p evo-desktop --test attachments_e2e -- --nocapture
//! ```
//!
//! Everything below the composer is the real thing: the app's own tab (`TabContent`),
//! its engine, and a server that is `evo-agent serve` out of a real build, with the
//! scripted model behind it. What the test drives is what a person drives — the box's
//! `+` (answered by an injected picker, because no headless run can answer a dialog),
//! typed words, `Enter` — and what it reads is what the window drew: the strip's own
//! count, the box's own height, and the coordinator's transcript, whose image row is
//! only there once `GET /media` came back and the bytes decoded.
//!
//! Two turns: one the server takes, and one it refuses (a `.png` that is not there),
//! which is the half that says the reader does not lose what they wrote.
//!
//! It is a `#[gpui_kit::test]`, so the window is gpui's own headless one: the macOS
//! platform can only be created on the main thread, and a test body is not the main
//! thread — which is also why there is no picture here. The captures of this window
//! come from the app's own examples, which do run on it (`cargo run -p evo-desktop
//! --example probe`), and no headless run can answer a file dialog anyway: the
//! picker below is injected (`Composer::set_picker`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use composer::{PathPicker, ATTACH_ID};
use evo_desktop::{AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, ElementId, Entity,
    Point, Task, TestAppContext, WindowBounds, WindowOptions,
};
use proofs::fixture::Fixture;
use session::{AgentKey, Item, ItemKind, LaunchPlan};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContent, TabContentEvent, TabState, WorkspaceView};

/// The window these runs happen in: the app's own size, so the box is the width the
/// design gives it and a long draft wraps the way it really does.
const WINDOW_SIZE: (f32, f32) = (1440., 900.);

/// How long anything the server has to answer gets.
const WAIT: Duration = Duration::from_secs(60);

/// How long the app's own loads (the catalog, the session index) are given before
/// the tab is started: they are threads of their own.
const LOADS: Duration = Duration::from_secs(3);

/// A red 1×1 PNG: a real image, which is what evo's own reader sniffs and journals.
const RED: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// What the reader types: the same sentence over and over, so the box grows past its
/// two rows at rest — which is what makes its height say whether there is still a
/// draft in it — and with no `/` anywhere, which would raise the completion popup
/// and let `Enter` take a row instead of sending.
const SENTENCE: &str = "please look at the picture and the table I attached and tell me \
                        what the table's columns are and what the picture shows";

/// The message after it: short, and nothing like the draft, so the row it makes says
/// whether the box was still holding the draft when it was typed.
const SECOND: &str = "and now a second message, with nothing attached";

/// The draft itself, long enough to be several rows of the box.
fn draft() -> String {
    let mut text = SENTENCE.repeat(4);
    text.truncate(text.trim_end().len());
    text
}

#[gpui_kit::test]
fn attachments_end_to_end(cx: &mut TestAppContext) {
    let started = Instant::now();
    // A real server is a real process on a thread of its own, and its updates wake the
    // app's own tasks from that thread: the deterministic scheduler's own rule about
    // another thread touching it is exactly what this test is about, so parking is
    // allowed and the wake is not held against it.
    cx.executor().allow_parking();
    let fixture = Fixture::new("attach-e2e");
    fixture.enter();

    // The folder the swarm runs in, and the files the picker will answer with: a
    // picture that exists, a table that exists, and one `.png` that does not.
    let shot = fixture.folder.join("shot.png");
    let table = fixture.folder.join("notes.csv");
    let gone = fixture.folder.join("gone.png");
    std::fs::write(&shot, RED).expect("the picture");
    std::fs::write(&table, b"name,total\nada,1\n").expect("the table");

    let (window, view) = open(cx, &fixture.root, bins(&fixture));
    cx.update(evo_desktop::start_background_loads);
    pump(cx, LOADS);
    let tab = cx.update(|cx| view.read(cx).selected_tab().clone());

    // One `evo-agent`, not a swarm: this is about one turn and its attachments.
    launch(
        cx,
        &tab,
        &fixture.folder,
        LaunchPlan {
            swarm: false,
            ..LaunchPlan::default()
        },
    );
    wait_for(cx, "the tab to run", |cx| {
        matches!(tab_state(cx, &tab), TabState::Running { .. }).then_some(())
    });
    println!("[attach] the tab runs in {}", fixture.folder.display());

    let composer = cx.update(|cx| tab.read(cx).composer().clone());

    // --- one turn the server takes ------------------------------------------------
    pick(cx, window, &composer, vec![shot.clone(), table.clone()]);
    assert_eq!(
        strip(cx, window),
        Some("Attachments 2".to_string()),
        "the strip says what the box is carrying"
    );
    say(cx, window, &tab, &draft());

    // The user row, as the server published it: the words, the picture, no file
    // embedded anywhere — the table is named by its absolute path.
    let (id, text, images) = wait_for(cx, "the user row", |cx| {
        items(cx, &tab)
            .into_iter()
            .find_map(|item| match &item.kind {
                ItemKind::User(user) if user.text.contains("Attached files:") => {
                    Some((item.id.clone(), user.text.clone(), user.images.clone()))
                }
                _ => None,
            })
    });
    println!("[attach] the row {id}: {text:?} — {images:?}");
    assert_eq!(
        images.len(),
        1,
        "one picture attached, one image on the row"
    );
    assert_eq!(images[0].media_type, "image/png");
    assert_eq!(images[0].href, format!("/media/{id}/0"));
    assert!(
        text.ends_with(&format!(
            "\n\nAttached files:\n- {}",
            std::fs::canonicalize(&table)
                .expect("the table's path")
                .display()
        )),
        "the reader's words, then the table's absolute path: {text:?}"
    );

    // The transcript's own row: the thumbnail only exists once `GET /media` answered
    // and the bytes decoded — a row that is still loading draws a note instead.
    wait_for(cx, "the thumbnail to be drawn", |cx| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(row("transcript-image-0", &id)).is_some()
        })
        .expect("the window")
        .then_some(())
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(row("transcript-image-note-0", &id))
                .is_none(),
            "and not still loading"
        );
        assert!(
            window
                .try_find(row("transcript-user-images", &id))
                .is_some(),
            "the row holds its images"
        );
        // The picture the row drew, named: the thumbnail is the file the picker
        // answered with, fetched from `/media/<id>/0` and decoded.
        assert_eq!(
            window
                .find(row("transcript-image-0", &id))
                .label()
                .map(str::to_owned),
            Some("shot.png — click to open".to_string())
        );
    })
    .expect("the window");

    // --- what the accepted send left behind ---------------------------------------
    cx.update(|cx| {
        assert!(
            composer.read(cx).attachments().is_empty(),
            "the strip went with the message"
        );
    });
    assert_eq!(strip(cx, window), None, "and is not drawn at all");

    // And the words went with it: what is typed next is the *whole* next message, not
    // that draft with something after it — which is what a box that kept its text would
    // have sent.
    say(cx, window, &tab, SECOND);
    let second = wait_for(cx, "the second row", |cx| {
        items(cx, &tab)
            .into_iter()
            .rev()
            .find(|item| matches!(item.kind, ItemKind::User(_)))
            .and_then(|item| match &item.kind {
                ItemKind::User(user) if user.text == SECOND => Some(user.text.clone()),
                ItemKind::User(user) => {
                    panic!("the box was not empty: the next message is {:?}", user.text)
                }
                _ => None,
            })
    });
    println!("[attach] the next message is exactly {second:?}");
    assert_eq!(user_rows(cx, &tab), 2, "two turns have been sent");

    // --- one turn the server refuses ----------------------------------------------
    //
    // A `.png` the picker hands over that is not there: the composer calls it an image
    // because its name says so, and evo is the one that says it cannot be read.
    pick(cx, window, &composer, vec![gone.clone()]);
    assert_eq!(
        strip(cx, window),
        Some("Attachments 1".to_string()),
        "the tile is drawn before anything is sent"
    );
    let refused_draft = "this one names a picture that is not there";
    say(cx, window, &tab, refused_draft);

    let notice = wait_for(cx, "the refusal", |cx| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("composer-notice-error")
                .and_then(|line| line.label().map(str::to_owned))
        })
        .expect("the window")
    });
    println!("[attach] the refusal: {notice:?}");
    assert!(
        notice.starts_with("input.send: ") && notice.contains("could not be read"),
        "the server's own reason, on the composer's notice line: {notice:?}"
    );

    // The draft and its tile are still there: a refusal is not a send.
    cx.update(|cx| {
        let composer = composer.read(cx);
        assert_eq!(
            composer.attachments().len(),
            1,
            "the tile stayed where it was"
        );
        assert_eq!(
            composer.attachments()[0].name,
            "gone.png",
            "and it is the file that was refused"
        );
    });
    assert_eq!(
        strip(cx, window),
        Some("Attachments 1".to_string()),
        "the strip is still drawn"
    );
    assert_eq!(user_rows(cx, &tab), 2, "a refusal adds no row: {notice:?}");

    // The words are still in the box, and that is the half a reader cares about: take
    // the picture that was refused off the strip and press `Enter` again, and the same
    // message goes — with nothing attached this time.
    let before = user_rows(cx, &tab);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("attachment-remove-0", cx);
    })
    .expect("the window");
    pump(cx, Duration::from_millis(200));
    assert_eq!(strip(cx, window), None, "the tile is off the strip");
    enter(cx, window, &tab);
    let kept = wait_for(cx, "the draft the refusal kept", |cx| {
        items(cx, &tab)
            .into_iter()
            .rev()
            .find(|item| matches!(item.kind, ItemKind::User(_)))
            .and_then(|item| match &item.kind {
                ItemKind::User(user) => Some((user.text.clone(), user.images.len())),
                _ => None,
            })
    });
    println!(
        "[attach] the kept draft went: {} bytes, {} images",
        kept.0.len(),
        kept.1
    );
    assert_eq!(
        kept.0, refused_draft,
        "the very words the refusal left in the box — and not the first draft, which \
         had already gone"
    );
    assert_eq!(kept.1, 0, "and no picture, since the tile was taken off");
    assert_eq!(user_rows(cx, &tab), before + 1, "now it went");
    // And the refusal it answered is gone: a red line still standing over a message
    // that went would say the fix had failed.
    let refusal = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("composer-notice-error")
                .and_then(|line| line.label().map(str::to_owned))
        })
        .expect("the window");
    assert_eq!(
        refusal, None,
        "the accepted send took the refusal's line away"
    );

    // The tab's server is told to stop while the window is still here, so its engine
    // thread's last update lands on a context that is still running: a test scheduler
    // is explicit about activity on another thread, and an engine thread waking a
    // dead context is exactly that.
    let swarms = cx.update(|cx| view.read(cx).swarms(cx));
    println!("[attach] stopping {} tab(s)", swarms.len());
    for swarm in swarms {
        swarm.shutdown();
    }
    pump(cx, Duration::from_secs(2));

    println!("[attach] done in {:?}", started.elapsed());
}

// --- the window, as the app opens it ------------------------------------------------

/// The app's own wiring, minus the background loads: a `Shell` on the fixture's app
/// root, and the window the tab lives in.
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    bins: Binaries,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let root = root.clone();
    cx.update(gpui_kit::init);
    cx.update(move |cx| {
        let log = AppLog::open(&root);
        let state = AppState {
            binaries: bins,
            theme: Theme::System,
            ..AppState::default()
        };
        Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
        let config = Arc::new(evo_desktop::launch_env(cx));
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                })),
                focus: false,
                show: false,
                ..workspace::window_options(cx)
            },
            cx,
            |window, cx| {
                let _appearance = evo_desktop::follow_appearance(cx, window);
                cx.new(|cx| WorkspaceView::with_config(config, window, cx))
            },
        )
        .expect("the window");
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        (window, view)
    })
}

/// The binaries the app would run: the fixture's own (`EVO_SWARM_BIN` /
/// `EVO_AGENT_BIN`), and the stub home its children inherit.
fn bins(fixture: &Fixture) -> Binaries {
    Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    }
}

/// Start this tab in `folder`, the way the empty tab does.
fn launch(cx: &mut TestAppContext, tab: &Entity<TabContent>, folder: &Path, plan: LaunchPlan) {
    let folder = folder.to_path_buf();
    let tab = tab.clone();
    cx.update(move |cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch { folder, plan })
        })
    });
}

// --- what a person does -------------------------------------------------------------

/// Answer the box's `+` with these paths, as the platform's dialog would.
fn pick(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    composer: &Entity<composer::Composer>,
    paths: Vec<PathBuf>,
) {
    let picker: PathPicker = Arc::new(move |_, _| Task::ready(Some(paths.clone())));
    cx.update(|cx| composer.update(cx, |composer, cx| composer.set_picker(picker, cx)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(ATTACH_ID, cx);
    })
    .expect("the window");
    // The picker's answer is a task: it lands on the executor.
    pump(cx, Duration::from_millis(200));
}

/// Type the words and press `Enter`, as a reader does.
fn say(cx: &mut TestAppContext, window: AnyWindowHandle, tab: &Entity<TabContent>, text: &str) {
    let tab = tab.clone();
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        window.input(text, cx);
    })
    .expect("the window");
    pump(cx, Duration::from_millis(200));
    enter(cx, window, &tab);
}

/// `Enter` in the box: the same keystroke that sends, and the one that does nothing
/// when there is nothing to send.
fn enter(cx: &mut TestAppContext, window: AnyWindowHandle, tab: &Entity<TabContent>) {
    let tab = tab.clone();
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        window.press("enter", cx);
    })
    .expect("the window");
    pump(cx, Duration::from_millis(150));
}

// --- what the window drew -----------------------------------------------------------

/// The strip's own count, as the row says it — or `None` when no strip is drawn.
fn strip(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find("attachments-strip-row")
            .and_then(|row| row.label().map(str::to_owned))
    })
    .expect("the window")
}

// --- what the model holds -----------------------------------------------------------

fn tab_state(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
}

fn items(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> Vec<Item> {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.items(AgentKey::Coordinator).to_vec())
            .unwrap_or_default()
    })
}

fn user_rows(cx: &mut TestAppContext, tab: &Entity<TabContent>) -> usize {
    items(cx, tab)
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::User(_)))
        .count()
}

/// The element id a transcript row is drawn under.
fn row(name: &'static str, id: &str) -> ElementId {
    (ElementId::from(name), id.to_string()).into()
}

/// Wait for something the app's own threads have to deliver; `None` means not yet.
fn wait_for<T>(
    cx: &mut TestAppContext,
    what: &str,
    mut step: impl FnMut(&mut TestAppContext) -> Option<T>,
) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = step(cx) {
            return value;
        }
        assert!(Instant::now() < deadline, "no {what} before the deadline");
        pump(cx, Duration::from_millis(20));
    }
}

/// Let the frames, the tasks and the engine's own threads run for a while: the test's
/// clock is advanced so the app's own timers come due, and real time is waited out
/// because the server is a real process on the other end.
fn pump(cx: &mut TestAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    while Instant::now() < deadline {
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
}
