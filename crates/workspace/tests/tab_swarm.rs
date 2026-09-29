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
    base::Root, px, size, AppContext as _, Bounds, ElementId, Entity, Point, TestAppContext,
    WindowBounds, WindowHandle, WindowOptions,
};
use session::{AgentKey, LaunchPlan};
use swarm_client::harness::{Fixture, HarnessConfig, STUB_MODEL};
use workspace::{Launch, SwarmConfig, TabState, WorkspaceView};

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

/// Send a signal to one process this test started (§9.7).
fn signal(pid: u32, which: &str) {
    let status = std::process::Command::new("kill")
        .arg(which)
        .arg(pid.to_string())
        .status()
        .expect("kill");
    assert!(status.success(), "kill {which} {pid}");
}

/// The pid the swarm's supervisor reports, and the pid that is actually serving:
/// `evo-swarm serve` runs the server as its own child, which is the one to kill to
/// leave a supervisor watching a closed port (§9.7).
fn served_by(supervisor: u32) -> Option<u32> {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &supervisor.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
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
    bench_with(cx, workers, "")
}

/// A bench whose temp `HOME` registers more than the stub's first model: `EXTRA`
/// is appended to the `init.lisp` the coordinator starts with, which is also what
/// its lanes learn their models from (§9.6).
fn bench_with(cx: &mut TestAppContext, workers: u16, extra: &str) -> Bench {
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
        init_extra: extra.to_owned(),
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

/// The plan the tests use unless they are about a chooser: two lanes, and evo's own
/// model for the coordinator.
fn two_workers() -> LaunchPlan {
    LaunchPlan {
        workers: Some(2),
        ..LaunchPlan::default()
    }
}

/// Start a tab's swarm in FOLDER, with PLAN's choosers (§3, §9.6).
fn launch(
    cx: &mut TestAppContext,
    bench: &Bench,
    tab: &Entity<workspace::TabContent>,
    folder: PathBuf,
    plan: LaunchPlan,
) {
    cx.update_window(bench.window.into(), |_, window, cx| {
        tab.update(cx, |tab, cx| {
            tab.launch(Launch::New { folder, plan }, window, cx)
        });
    })
    .unwrap();
}

/// Open a tab of its own, shown in the same window — what ⌘T does (§7.1).
fn open_tab(cx: &mut TestAppContext, bench: &Bench) -> Entity<workspace::TabContent> {
    cx.update_window(bench.window.into(), |_, window, cx| {
        bench
            .view
            .update(cx, |view, cx| view.open_empty_tab(window, cx))
    })
    .unwrap()
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

/// How long a wait that only depends on this machine gets: the UI folding an
/// answer that has already arrived, a POST coming back, a swarm answering /health
/// on a quiet box.
const WAIT: Duration = Duration::from_secs(120);

/// How long a wait around §3's boot gets. `/health` is polled for 90 s inside the
/// engine, and a *second* swarm starting on a loaded box is slower than a lonely
/// one — the deadline is not a performance test, so it stays well clear of boot.
const BOOT: Duration = Duration::from_secs(180);

/// How long §9.7's reconnect gets: a supervisor restarting its server (a boot,
/// again), then the client's exponential backoff (0.5 → 10 s) on top of it.
const RECONNECT: Duration = Duration::from_secs(180);

/// Pump the UI thread until `done` holds, or fail. The engine's work happens on
/// threads of its own and arrives through a channel, so waiting means keeping the
/// executor running, not sleeping on the state.
fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    wait_within(cx, what, WAIT, &mut done)
}

/// The same wait with the budget a slow phase really needs, and a failure that
/// says how long it waited. A wait that runs out must not be able to hide behind
/// a name: the panic carries the phase's own words.
fn wait_within(
    cx: &mut TestAppContext,
    what: &str,
    budget: Duration,
    done: &mut impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if started.elapsed() >= budget {
            panic!("timed out after {:?} waiting for {what}", started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the tab's swarm has answered `/health` (§3).
///
/// A boot that *failed* is the answer to "why is this not up" — the engine polls
/// for 90 s and then writes the tab off — so it fails here at once, with the
/// reason and the log tail, instead of running out a deadline for a boot that can
/// no longer arrive.
fn wait_for_running(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        match state(cx, tab) {
            TabState::Running { .. } => return,
            TabState::Failed {
                message, log_tail, ..
            } => panic!(
                "the swarm never came up: {}\n{log_tail}",
                message.unwrap_or_else(|| "no reason given".into())
            ),
            _ if started.elapsed() >= BOOT => {
                let seen = cx.update(|cx| format!("{:?}", tab.read(cx).state()));
                panic!(
                    "timed out after {:?} waiting for the swarm to answer /health — the tab: {seen}",
                    started.elapsed()
                );
            }
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Wait for something a tab does, where running out has to say what the tab was
/// doing: the state, the swarm's pid, and the tooltip a user would have hovered
/// (§7.1, §9.7). The swarm's own log is on disk, next to the failure.
fn wait_for_tab(
    cx: &mut TestAppContext,
    tab: &Entity<workspace::TabContent>,
    what: &str,
    budget: Duration,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if started.elapsed() >= budget {
            let seen = cx.update(|cx| {
                let tab = tab.read(cx);
                format!(
                    "state {:?}, pid {:?}, {}",
                    tab.state(),
                    tab.swarm_pid(),
                    tab.tooltip().replace('\n', " / ")
                )
            });
            panic!(
                "timed out after {:?} waiting for {what} — the tab: {seen}",
                started.elapsed()
            );
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
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());

    wait_for_running(cx, &tab);

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
        let record = &records[0];
        assert_eq!(record.folder.as_ref(), Some(&b.fixture.project));
        assert_eq!(
            record.session.as_deref(),
            tab.read(cx).session_path(),
            "the record carries the session the tab is writing to"
        );
        assert!(record.session.is_some(), "/state named the session");
        assert_eq!(
            record.store_id.as_ref(),
            tab.read(cx).store_id(),
            "and the id of the directory its swarm writes to (§6)"
        );
        assert!(
            record.store_id.as_ref().is_some_and(|id| {
                b.fixture
                    .dir
                    .join("app")
                    .join("tabs")
                    .join(id.as_str())
                    .is_dir()
            }),
            "the store id names a directory that is really there"
        );
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
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    // A lane comes up on demand and is `starting` until its baseline is evaluated;
    // the swarm's delegate takes an idle lane, so wait for that first (§9.3).
    wait_for(cx, "lane 1 to be idle and ready for work", |cx| {
        cx.update(|cx| {
            tab.read(cx).model().is_some_and(|model| {
                model
                    .lane_rows()
                    .iter()
                    .any(|lane| lane.n == 1 && lane.status == session::LaneStatus::Idle)
            })
        })
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
    launch(cx, &b, &first, b.fixture.project.clone(), two_workers());

    // A second swarm, in a project of its own. It shares the fixture's HOME —
    // one stub provider serves both, and each swarm keeps its own session and
    // lane directories inside it.
    let second_project = b.fixture.dir.join("proj2");
    std::fs::create_dir_all(&second_project).expect("a second project");
    let second = open_tab(cx, &b);
    launch(cx, &b, &second, second_project, two_workers());

    wait_for_running(cx, &first);
    wait_for_running(cx, &second);

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

/// §9.7: a swarm that stops answering. Its stream goes to reconnecting — the tab
/// says so, the agent list badges the coordinator's row — and a turn posted while
/// it is unreachable fails with an error notice above the composer, with the
/// draft still there. Never a modal, and the tab keeps its page.
///
/// What dies here is the process *serving*: `evo-swarm serve` is a supervisor
/// parent that re-spawns itself as the child that listens, so the tab's own
/// process is still running — which is why the engine reconnects instead of
/// declaring the swarm gone (the state the badge is for) — and why the supervisor
/// brings the server back, so the stream recovers on its own (§3).
///
/// The supervisor is *held* (`SIGSTOP`) for the dead window: with it running, a
/// POST that meets a restarted server is a healthy turn, and how long the box
/// takes to restart decides whether this test sees a refusal at all. Holding it
/// makes the unreachable swarm the test's own doing — and `Held` sends `SIGCONT`
/// on the way out, however the test leaves, so no stopped supervisor is left for
/// teardown to miss.
#[gpui_kit::test]
fn a_swarm_that_stops_answering_reconnects_and_refuses_quietly(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    let pid = cx
        .update(|cx| tab.read(cx).swarm_pid())
        .expect("the swarm's own pid, from /health");
    let server = served_by(pid).expect("the serving child of the supervisor");
    let held = Held::new(pid);
    signal(server, "-KILL");

    // A turn typed into a dead swarm: the POST goes out and nothing answers. The
    // composer keeps the draft, because the server never took it (§9.2).
    prompt(cx, &b, &tab, "SLOW frozen swarm");

    wait_for_tab(cx, &tab, "the unanswered POST to be shown", WAIT, |cx| {
        cx.update_window(b.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find("composer-notice-error").is_some()
        })
        .unwrap_or(false)
    });
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let notice = window.find("composer-notice-error");
        assert_eq!(
            notice.label(),
            Some("Can't reach the swarm — it may be restarting."),
            "a POST nothing answered is said in plain words (§4)"
        );
        assert!(
            window.find("tab-page").visible(),
            "a failure is a line, not a modal: the page is still there"
        );
        assert!(
            window.try_find("composer-notice-dim").is_none(),
            "a transport failure is not a 409 refusal"
        );
    })
    .unwrap();
    cx.update(|cx| {
        let tab = tab.read(cx);
        assert!(
            tab.notice_detail()
                .is_some_and(|detail| !detail.trim().is_empty()),
            "the socket's own words are kept for the hover, not dropped: {:?}",
            tab.notice_detail()
        );
        let composer = tab.composer().read(cx);
        assert_eq!(composer.face(), composer::ActionFace::Send);
        assert!(
            composer.is_action_enabled(cx),
            "the draft the server never took is still in the composer"
        );
    });

    // The connection the stream was reading is gone, so it retries rather than
    // sitting on a socket that will never speak again.
    wait_for_tab(
        cx,
        &tab,
        "the stream to go to reconnecting",
        RECONNECT,
        |cx| cx.update(|cx| tab.read(cx).is_reconnecting()),
    );
    cx.update(|cx| {
        let tooltip = tab.read(cx).tooltip();
        assert!(
            tooltip.contains("reconnecting"),
            "the tab says what the swarm is doing: {tooltip}"
        );
    });
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("reconnecting").visible(),
            "the page carries the reconnecting badge"
        );
        // The agent list badges the coordinator's own row (§9.7); `main` is row 0.
        assert!(
            window
                .find(ElementId::from(("agent-badge", 0u64)))
                .visible(),
            "and so does the list"
        );
    })
    .unwrap();

    // Release the supervisor: it sees the child it lost and brings the server
    // back, and the stream comes back with it (§3).
    drop(held);

    wait_for_tab(
        cx,
        &tab,
        "the stream to come back after the supervisor restarted the server",
        RECONNECT,
        |cx| cx.update(|cx| !tab.read(cx).is_reconnecting()),
    );
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("reconnecting").is_none(),
            "the badge left with the silence"
        );
    })
    .unwrap();
}

/// A supervisor held with `SIGSTOP`, so the server it would restart stays dead for
/// as long as the test needs. Dropping it — a panic in the middle included — sends
/// `SIGCONT`, so a stopped process never outlives the test that stopped it.
struct Held(u32);

impl Held {
    fn new(pid: u32) -> Held {
        signal(pid, "-STOP");
        Held(pid)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // Best effort: a test that troubled to hold a process releases it while
        // unwinding a panic too, and a `kill` that finds it already gone is not
        // the failure worth reporting there.
        let _ = std::process::Command::new("kill")
            .args(["-CONT", &self.0.to_string()])
            .status();
    }
}

/// §9.6: the lanes' model is the *folder's* project configuration, not a flag —
/// evo-swarm has no lanes-model option, and a launch writes the managed block into
/// `<folder>/.evo/swarm.lisp` before the swarm starts, so the lanes that come up
/// read it.
///
/// What this proves, in the order a user does it:
///
/// 1. a launch with a lanes model chosen leaves exactly one managed block, at the
///    top, naming the model **and** the provider (both matter: a model id alone
///    fails when the same id is registered under another provider), with the
///    user's own lines below it byte for byte;
/// 2. a lane really *runs* it — the stub's request log has the lane asking as
///    `stub-b` while the coordinator asks as `stub-a`, which is what the block is
///    for;
/// 3. launching again with **Default** takes the block away and leaves the user's
///    lines alone — and where the block was all the file held, the file goes too.
#[gpui_kit::test]
fn a_lanes_model_is_written_to_the_folders_swarm_lisp_and_default_takes_it_away(
    cx: &mut TestAppContext,
) {
    use store::swarm_config::{self, LanesModel};

    // The stub's second model, registered the way a real provider's models are:
    // the coordinator's `init.lisp` is where a lane's baseline learns them from.
    let b = bench_with(
        cx,
        2,
        "(evo:register-model \"stub-b\" :provider :stub :context-window 100000 \
         :max-output 8000 :effort t)\n",
    );
    let folder = b.fixture.project.clone();
    let file = swarm_config::swarm_lisp_path(&folder);

    // The folder is the user's: it already says something of its own.
    let user = ";; the project's own settings\n(evo:set-setting :thinking :high)\n";
    std::fs::create_dir_all(file.parent().expect(".evo")).expect("the folder's .evo");
    std::fs::write(&file, user).expect("the user's swarm.lisp");
    let lanes_model = ("stub-b".to_string(), "stub".to_string());

    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(
        cx,
        &b,
        &tab,
        folder.clone(),
        LaunchPlan {
            lanes_model: Some(lanes_model.clone()),
            ..two_workers()
        },
    );
    wait_for_running(cx, &tab);

    // 1. The block is at the top, exactly one of it, and the user's lines are
    //    where they were. The bytes are the spec's own block, so this also says
    //    the two settings are the pair a lane can actually register.
    let block =
        swarm_config::render_block(&LanesModel::new("stub-b", "stub")).expect("the spec's block");
    let written = std::fs::read_to_string(&file).expect("the file the launch wrote");
    assert_eq!(
        written,
        format!("{block}\n{user}"),
        "the managed block goes on top and nothing else moves"
    );

    // 2. A lane on the wire: the coordinator delegates, the lane comes up on
    //    demand, and the model it asks with is the one the block set — while the
    //    coordinator itself keeps evo's own model (§9.6).
    let delegate = "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the test\"}";
    wait_for(cx, "lane 1 to be idle and ready for work", |cx| {
        cx.update(|cx| {
            tab.read(cx).model().is_some_and(|model| {
                model
                    .lane_rows()
                    .iter()
                    .any(|lane| lane.n == 1 && lane.status == session::LaneStatus::Idle)
            })
        })
    });
    prompt(cx, &b, &tab, delegate);
    wait_for_tab(cx, &tab, "lane 1 to take the work", BOOT, |cx| {
        lane_working(cx, &tab, 1)
    });

    let role = |role: &str| {
        b.fixture
            .stub
            .requests()
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.get("role").and_then(serde_json::Value::as_str) == Some(role))
            .filter_map(|request| {
                request
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
    };
    wait_for_tab(cx, &tab, "lane 1 to ask the stub as stub-b", BOOT, |_cx| {
        role("lane 1").iter().any(|model| model == "stub-b")
    });
    let coordinator = role("coordinator");
    assert!(
        !coordinator.is_empty(),
        "the coordinator's own turns went through the same stub"
    );
    assert!(
        coordinator.iter().all(|model| model == STUB_MODEL),
        "the block is the lanes' model, not the coordinator's: {coordinator:?}"
    );

    // 3. Default is a launch of its own: it takes the block out and keeps the
    //    user's own lines (the file belongs to the folder, not the tab — §9.6).
    let relaunched = open_tab(cx, &b);
    launch(cx, &b, &relaunched, folder.clone(), two_workers());
    wait_for_running(cx, &relaunched);
    assert_eq!(
        std::fs::read_to_string(&file).expect("the file Default rewrote"),
        user,
        "Default takes the block away and leaves the rest"
    );
    assert!(
        !swarm_config::has_lanes_model_block(&folder),
        "no marker survives it either"
    );

    // And where our block was all there was, there is no file left to name.
    let bare = b.fixture.dir.join("bare");
    std::fs::create_dir_all(&bare).expect("a second folder");
    swarm_config::set_lanes_model(&bare, Some(&LanesModel::new("stub-b", "stub")))
        .expect("the block a previous launch would have left");
    assert!(swarm_config::swarm_lisp_path(&bare).is_file());
    let emptied = open_tab(cx, &b);
    launch(cx, &b, &emptied, bare.clone(), two_workers());
    wait_for_running(cx, &emptied);
    assert_eq!(
        swarm_config::set_lanes_model(&bare, None).expect("nothing to remove"),
        swarm_config::WriteOutcome::Absent,
        "the launch already deleted the file it emptied"
    );
    assert!(!swarm_config::swarm_lisp_path(&bare).exists());
}
