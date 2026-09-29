//! A tab driving a **real** swarm, in a test window — the M0/M1 proof.
//!
//! Every test here starts an actual `evo-swarm serve` through `swarm_client`'s
//! harness (a temp `HOME` whose `init.lisp` registers the scripted stub model),
//! launches a tab into it through the production path, and then drives the tab
//! the way a user does: type in the composer, press Enter, click a lane row.
//!
//! The engine runs on its own threads and hands its updates to a `cx.spawn` task,
//! so these tests pump the UI thread (`wait_for`) instead of blocking on it.
//! Against GPUI's deterministic scheduler a real thread waking a GPUI task is
//! rejected as non-determinism, so each test opts into parking — the supported
//! switch for a test that talks to real I/O.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    base::Root, px, size, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions,
};
use session::{AgentKey, LaunchPlan};
use swarm_client::harness::{Fixture, HarnessConfig, STUB_MODEL};
use workspace::{Launch, SwarmConfig, TabState, WorkspaceView};

/// How long a test waits for the swarm before it gives up (§3 gives boot 90 s).
const WAIT: Duration = Duration::from_secs(120);

/// A swarm, a window and the tab that drives it.
struct Bench {
    fixture: Fixture,
    window: WindowHandle<Root>,
    view: Entity<WorkspaceView>,
    /// These tests each boot real swarms (two of them, here); three at once on
    /// one machine starves the boot and turns a healthy test into a timeout, so
    /// they take turns. Held for the test's lifetime.
    _serial: std::sync::MutexGuard<'static, ()>,
}

/// The lock `Bench` holds, so the swarm tests run one at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The last `/registry` the window's hook was handed (§9.4).
static SEEN_REGISTRY: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);

/// Install the window's registry hook, which is how the app refreshes its model
/// cache from a live server (§9.4).
fn watch_registry(cx: &mut TestAppContext, bench: &Bench) {
    *SEEN_REGISTRY.lock().expect("the registry slot") = None;
    cx.update(|cx| {
        bench.view.update(cx, |view, cx| {
            view.on_registry(
                |raw, _cx| {
                    *SEEN_REGISTRY.lock().expect("the registry slot") = Some(raw.clone());
                },
                cx,
            );
        });
    });
}

fn bench(cx: &mut TestAppContext, workers: u16) -> Bench {
    // A poisoned lock means an earlier test panicked while holding it; the next
    // one still runs, because the swarms it started died with that process.
    let serial = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // A real swarm's threads wake this test's tasks: that is the point.
    cx.dispatcher.allow_parking();

    let fixture = Fixture::new(HarnessConfig {
        workers,
        model: STUB_MODEL.to_owned(),
        ..HarnessConfig::default()
    })
    .expect("a stub swarm to run against");
    let config = Arc::new(SwarmConfig {
        swarm_bin: fixture.bins.swarm.clone(),
        agent_bin: fixture.bins.agent.clone(),
        root: store::paths::Root::at(fixture.dir.join("app")),
        env: fixture.env(),
        env_remove: fixture.env_remove(),
    });

    cx.update(gpui_kit::init);
    let (window, view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1280.), px(800.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| WorkspaceView::with_config(config, window, cx)),
            )
        })
        .expect("open the workspace window");

    Bench {
        fixture,
        window: window.downcast::<Root>().expect("base Root"),
        view,
        _serial: serial,
    }
}

/// Start the first tab's swarm in FOLDER (§3).
fn launch(
    cx: &mut TestAppContext,
    bench: &Bench,
    tab: &Entity<workspace::TabContent>,
    folder: PathBuf,
    workers: Option<u16>,
) {
    cx.update_window(bench.window.into(), |_, window, cx| {
        tab.update(cx, |tab, cx| {
            tab.launch(
                Launch::New {
                    folder,
                    plan: LaunchPlan {
                        workers,
                        ..LaunchPlan::default()
                    },
                },
                window,
                cx,
            )
        });
    })
    .unwrap();
}

/// Type a turn into the composer and press Enter — the app's own send path (§9.2).
///
/// The tab has to be the one the window is showing: a composer that was never
/// laid out cannot take the focus the typing goes to.
fn prompt(cx: &mut TestAppContext, bench: &Bench, tab: &Entity<workspace::TabContent>, text: &str) {
    cx.update_window(bench.window.into(), |_, window, cx| {
        // The composer has to have been laid out before it can take focus, so
        // the frame the user is looking at is drawn first.
        window.render_frame(cx);
        tab.update(cx, |tab, cx| {
            let composer = tab.composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        });
        window.input(text, cx);
        window.press("enter", cx);
    })
    .unwrap();
}

/// Pump the UI thread until `done` holds, or fail. The engine's work happens on
/// threads of its own and arrives through a channel, so waiting means keeping the
/// executor running, not sleeping on the state.
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
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn state(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) -> TabState {
    cx.update(|cx| tab.read(cx).state().clone())
}

fn tab_rows(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) -> Vec<session::Row> {
    cx.update(|cx| {
        tab.read(cx)
            .transcript()
            .map(|view| view.read(cx).rows(cx).to_vec())
            .unwrap_or_default()
    })
}

/// How much assistant text the shown transcript carries: the number a streaming
/// message grows.
fn assistant_chars(rows: &[session::Row]) -> usize {
    rows.iter()
        .filter_map(|row| match &row.kind {
            session::RowKind::Assistant { markdown, .. } => Some(markdown.chars().count()),
            _ => None,
        })
        .sum()
}

/// Whether lane N is working, from the tab's own lane list.
fn lane_working(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>, n: u64) -> bool {
    cx.update(|cx| {
        tab.read(cx).model().is_some_and(|model| {
            model
                .lane_rows()
                .iter()
                .any(|lane| lane.n == n && lane.status == session::LaneStatus::Working)
        })
    })
}

fn selected_agent(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) -> AgentKey {
    cx.update(|cx| tab.read(cx).selected_agent())
}

fn select(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>, agent: AgentKey) {
    cx.update(|cx| tab.update(cx, |tab, cx| tab.select_agent(agent, cx)));
}

/// One tab boots a swarm of its own, streams a turn into its transcript, and the
/// composer's single button reports the outcome (§3, §9.1, §9.2).
#[gpui_kit::test]
fn a_tab_boots_a_real_swarm_and_streams_a_turn(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    watch_registry(cx, &b);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), Some(2));

    wait_for(cx, "the swarm to answer /health", |cx| {
        matches!(state(cx, &tab), TabState::Running { .. })
    });

    // §9.4: the window hands every live tab's /registry to the app, which is how
    // the model cache is refreshed from a server that really answered.
    wait_for(cx, "the registry to reach the app", |_cx| {
        SEEN_REGISTRY
            .lock()
            .expect("the registry slot")
            .as_ref()
            .is_some_and(|raw| raw.to_string().contains(STUB_MODEL))
    });

    // The page is the real one: the coordinator's readout comes from /state and
    // /registry, not from a placeholder (§7.3). It arrives right after Ready.
    wait_for(cx, "the readout to name the model", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.readout_text().contains(STUB_MODEL))
        })
    });

    // §9.5: the session this tab started is recorded, so a swarm the app brought
    // up is in the history next time. The write is the app's own file, off the UI
    // thread, so the test waits for the file rather than the frame.
    let root = store::paths::Root::at(b.fixture.dir.join("app"));
    wait_for(cx, "the session to be recorded as a recent", |_cx| {
        store::app_state::AppState::load(&root)
            .recents
            .iter()
            .any(|recent| {
                recent.folder == b.fixture.project && !recent.session.as_os_str().is_empty()
            })
    });

    // §7.1: the label is the folder's own name, and the tooltip carries the whole
    // path plus what the swarm is doing.
    cx.update(|cx| {
        let tab = tab.read(cx);
        assert_eq!(
            tab.title().as_ref(),
            "proj",
            "the tab is named after its folder"
        );
        let tooltip = tab.tooltip();
        assert!(
            tooltip.contains(&b.fixture.project.display().to_string()),
            "the tooltip names the whole path: {tooltip}"
        );
        assert!(
            tooltip.contains("running"),
            "and what the swarm is doing: {tooltip}"
        );
    });

    // §9.8: the window can hand the app its tabs to persist — the folder each one
    // runs in, and the session it writes to.
    cx.update(|cx| {
        let records = b.view.read(cx).tab_records(cx);
        assert_eq!(records.len(), 1, "one tab is open");
        let (_, folder, session) = &records[0];
        assert_eq!(folder.as_ref(), Some(&b.fixture.project));
        assert_eq!(
            session.as_deref(),
            tab.read(cx).session_path(),
            "the record carries the session the tab is writing to"
        );
        assert!(session.is_some(), "/state named the session");
    });

    prompt(cx, &b, &tab, "SLOW say something long");

    // The turn reached the swarm: the user's own row is in the transcript.
    wait_for(cx, "the user's row to appear", |cx| {
        tab_rows(cx, &tab).iter().any(
            |row| matches!(&row.kind, session::RowKind::User { text } if text.contains("SLOW")),
        )
    });

    // The assistant row is rendered markdown **while it grows**: two samples that
    // differ are the proof that deltas are landing in the view as they arrive.
    wait_for(cx, "the first delta", |cx| {
        assistant_chars(&tab_rows(cx, &tab)) > 0
    });
    let first = assistant_chars(&tab_rows(cx, &tab));
    wait_for(cx, "more of the message", |cx| {
        assistant_chars(&tab_rows(cx, &tab)) > first + 20
    });

    // The turn ends; the settled resync rebuilds the same rows from /transcript.
    wait_for(cx, "the run to settle", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.activity() == session::Activity::Idle)
        })
    });
    wait_for(cx, "the resync to rebuild the rows", |cx| {
        let rows = tab_rows(cx, &tab);
        rows.iter()
            .any(|row| matches!(row.kind, session::RowKind::User { .. }))
            && assistant_chars(&rows) > 100
    });

    // §9.2: the button is a Send button again, disabled because the draft was
    // cleared only after the server took the text.
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(composer::BUTTON_ID).label(),
            Some("Send"),
            "an idle coordinator offers Send again"
        );
        assert!(window.find("tab-page").visible());
        assert!(window.find("agent-column").visible());
    })
    .unwrap();
    // The draft was cleared only after the server took it, so the Send face has
    // nothing to do and clicking it does nothing (§9.2).
    cx.update(|cx| {
        let composer = tab.read(cx).composer().read(cx);
        assert!(
            !composer.is_action_enabled(cx),
            "an empty draft leaves the button inert"
        );
        assert_eq!(composer.face(), composer::ActionFace::Send);
    });
}

/// The lane's own prompt row: the task text and nothing else. The coordinator's
/// row for the same words is the `CALL delegate {...}` line the user typed, so
/// only the lane has a row that is exactly the task.
fn is_lane_task(text: &str) -> bool {
    text.trim() == "SLOW lane work for the test"
}

/// A delegated lane: the coordinator's transcript shows the tool call, and
/// selecting the lane shows **its own** transcript — its rows, its stream
/// (§7.3, §9.3, and docs/review-1.md F5).
#[gpui_kit::test]
fn a_delegated_lane_shows_its_own_transcript(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), Some(2));
    wait_for(cx, "the swarm to answer /health", |cx| {
        matches!(state(cx, &tab), TabState::Running { .. })
    });

    // Choose the lane first: its stream is opened while it is idle, so what it
    // streams when it starts working arrives live (§9.3 — one lane, the shown one).
    select(cx, &tab, AgentKey::Lane(1));
    assert_eq!(selected_agent(cx, &tab), AgentKey::Lane(1));

    let coordinator = session::AgentKey::Coordinator;
    prompt(
        cx,
        &b,
        &tab,
        "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the test\"}",
    );

    wait_for(cx, "lane 1 to take the work", |cx| {
        lane_working(cx, &tab, 1)
    });
    wait_for(cx, "the lane's own transcript to stream", |cx| {
        assistant_chars(&tab_rows(cx, &tab)) > 20
    });

    // The lane's rows are the lane's: its own task, its own answer.
    let lane_rows = tab_rows(cx, &tab);
    assert!(
        lane_rows
            .iter()
            .any(|row| matches!(&row.kind, session::RowKind::User { text } if is_lane_task(text))),
        "the lane's transcript carries the task it was given: {lane_rows:#?}"
    );

    // The coordinator's own transcript is untouched by the lane's rows: it has
    // the turn of the user, and the delegate tool call.
    select(cx, &tab, coordinator);
    wait_for(
        cx,
        "the coordinator's transcript to show the delegation",
        |cx| {
            tab_rows(cx, &tab).iter().any(|row| {
            matches!(&row.kind, session::RowKind::Tool { name, .. } if name == "delegate")
                || matches!(&row.kind, session::RowKind::User { text } if text.contains("delegate"))
        })
        },
    );

    let coordinator_rows = tab_rows(cx, &tab);
    assert!(
        !coordinator_rows
            .iter()
            .any(|row| matches!(&row.kind, session::RowKind::User { text } if is_lane_task(text))),
        "the lane's task is the lane's own row, not the coordinator's: {coordinator_rows:#?}"
    );
}

/// §11's M0 requirement: two tabs streaming at once, with the UI thread still
/// answering. The numbers are printed so the run reports them.
#[gpui_kit::test]
fn two_tabs_stream_at_once_and_the_ui_stays_responsive(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let first = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &first, b.fixture.project.clone(), Some(2));

    // A second swarm, in a project of its own. It shares the fixture's HOME —
    // one stub provider serves both, and each swarm keeps its own session and
    // lane directories inside it.
    let second_project = b.fixture.dir.join("proj2");
    std::fs::create_dir_all(&second_project).expect("a second project");
    cx.update_window(b.window.into(), |_, window, cx| {
        let second = b
            .view
            .update(cx, |view, cx| view.open_empty_tab(window, cx));
        second.update(cx, |tab, cx| {
            tab.launch(
                Launch::New {
                    folder: second_project.clone(),
                    plan: LaunchPlan {
                        workers: Some(2),
                        ..LaunchPlan::default()
                    },
                },
                window,
                cx,
            )
        });
    })
    .unwrap();
    let second = cx.update(|cx| b.view.read(cx).selected_tab().clone());

    wait_for(cx, "both swarms to answer /health", |cx| {
        matches!(state(cx, &first), TabState::Running { .. })
            && matches!(state(cx, &second), TabState::Running { .. })
    });

    // Both stream at once: 60 deltas a tenth apart, each. The user sends in the
    // first tab, switches to the second and sends there — from then on both
    // swarms are streaming while the window shows one of them, which is the
    // shape that has to stay responsive.
    cx.update(|cx| b.view.update(cx, |view, cx| view.select_tab(0, cx)));
    prompt(cx, &b, &first, "SLOW first tab");
    cx.update(|cx| b.view.update(cx, |view, cx| view.select_tab(1, cx)));
    prompt(cx, &b, &second, "SLOW second tab");

    wait_for(cx, "both tabs to be streaming", |cx| {
        assistant_chars(&tab_rows(cx, &first)) > 0 && assistant_chars(&tab_rows(cx, &second)) > 0
    });

    // While both stream, sample what the UI thread costs: draining the updates
    // that arrived, and drawing the frame the user is looking at.
    let first_before = assistant_chars(&tab_rows(cx, &first));
    let second_before = assistant_chars(&tab_rows(cx, &second));
    let mut drain = Vec::new();
    let mut frame = Vec::new();
    for _ in 0..40 {
        let started = Instant::now();
        cx.run_until_parked();
        drain.push(started.elapsed());

        let started = Instant::now();
        cx.update_window(b.window.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        frame.push(started.elapsed());
        // A tenth of the two streams' own pace: the sample covers several deltas
        // of each, so the numbers are not a single quiet instant.
        std::thread::sleep(Duration::from_millis(40));
    }

    let stat = |samples: &[Duration]| {
        let max = samples.iter().max().copied().unwrap_or_default();
        let mean = Duration::from_secs_f64(
            samples.iter().map(|d| d.as_secs_f64()).sum::<f64>() / samples.len() as f64,
        );
        (mean, max)
    };
    let (drain_mean, drain_max) = stat(&drain);
    let (frame_mean, frame_max) = stat(&frame);
    // Both transcripts kept growing while we measured: the streams really were
    // live for the whole sample, not just when it started.
    let first_chars = assistant_chars(&tab_rows(cx, &first));
    let second_chars = assistant_chars(&tab_rows(cx, &second));
    println!(
        "[two tabs streaming] both pumps, one shown page — update drain: \
         mean {drain_mean:?}, max {drain_max:?} | frame: mean {frame_mean:?}, \
         max {frame_max:?} | chars {first_before}->{first_chars}, \
         {second_before}->{second_chars}"
    );
    assert!(
        first_chars > first_before && second_chars > second_before,
        "both streams grew during the sample: {first_before}->{first_chars}, \
         {second_before}->{second_chars}"
    );

    // A frame is what a user feels: it has to stay in interactive territory even
    // with two swarms streaming through the same UI thread.
    assert!(
        frame_max < Duration::from_millis(250),
        "a frame took {frame_max:?} while two tabs streamed"
    );
    assert!(
        drain_max < Duration::from_millis(250),
        "draining the updates took {drain_max:?} while two tabs streamed"
    );
}
