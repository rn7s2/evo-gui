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

use std::io::Write as _;
use std::path::{Path, PathBuf};
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

/// Show the tab at INDEX, the way a click or a shortcut does (§7.1).
fn show_tab(cx: &mut TestAppContext, bench: &Bench, index: usize) {
    cx.update_window(bench.window.into(), |_, window, cx| {
        bench
            .view
            .update(cx, |view, cx| view.select_tab(index, window, cx))
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
    // §9.5: `/state` names the session before the swarm has said anything, and
    // evo writes the journal at the *first assistant message* — so a Recent made
    // from the name alone would be a row `--resume` cannot open. The tab must
    // wait for the file, which this run has not produced yet.
    let root = store::paths::Root::at(b.fixture.dir.join("app"));
    let session = cx
        .update(|cx| tab.read(cx).session_path().map(Path::to_path_buf))
        .expect("/state named the session");
    assert!(session.is_absolute(), "and named it in full: {session:?}");
    assert!(!session.exists(), "nothing has been journalled yet");
    // Give the write its chance before believing it did not happen: the tab has
    // had the name since its first /state.
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !store::app_state::AppState::load(&root)
            .recents
            .iter()
            .any(|recent| recent.session == session),
        "a session with no journal yet is not a recent"
    );

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

    // §9.5: now the swarm has answered, its journal is on disk — and the same
    // resync that rebuilt the rows records the session, so the row the history
    // shows is one a resume can open.
    assert!(
        session.is_file(),
        "the journal is on disk: {}",
        session.display()
    );
    wait_for(cx, "the session to be recorded as a recent", |_cx| {
        store::app_state::AppState::load(&root)
            .recents
            .iter()
            .any(|recent| recent.folder == b.fixture.project && recent.session == session)
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

/// §7.3: a lane's step clock is an *age* — what `/lanes` and a `lane-state` event
/// report — so the row counts on from the moment it was seen. A clock that only
/// moved when `/lanes` was read again would freeze for exactly as long as nobody
/// asked, which is the whole time it is worth reading.
///
/// Nothing is asked of the swarm between the two readings: no prompt, no read, no
/// event driven by the test — the seconds pass on their own and the same row reads
/// a later number. That those frames are the tab's own ticker is the condition
/// `tab::tests::a_busy_lane_keeps_the_seconds_coming` pins down.
#[gpui_kit::test]
fn a_busy_lanes_clock_advances_with_nothing_asked_of_the_swarm(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    // A lane is `starting` until its baseline is evaluated, and the swarm's
    // delegate takes an idle lane, so wait for that first (§9.3).
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
    prompt(
        cx,
        &b,
        &tab,
        "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the test\"}",
    );
    // And the coordinator hands the work over and has nothing of its own in
    // flight: the only clock left on the page is the lane's, which is the clock in
    // question.
    wait_for(cx, "lane 1 to take the work", |cx| {
        lane_working(cx, &tab, 1)
    });
    wait_for(cx, "the coordinator to be done with its own turn", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.coordinator_step_started().is_none())
        }) && lane_working(cx, &tab, 1)
    });

    // The step began with the event that said the lane was working — no `/lanes`
    // read is needed for a clock to start.
    let started = lane_clock(cx, &b, 1).expect("a working lane shows its step clock");

    // A second of the frames the ticker draws (§7.3), with nothing in between but
    // the passing of time: the same row reads a later number. The tab's ticker is a
    // one-second timer, and in a test the clock it waits on is ours to move — the
    // step clock itself is the wall clock, which the sleep above is for.
    std::thread::sleep(Duration::from_millis(1_200));
    assert!(
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.coordinator_step_started().is_none())
        }),
        "still nothing but the lane: the clock that moved is the lane's own"
    );
    wait_within(
        cx,
        "the lane's clock to have moved",
        Duration::from_secs(5),
        &mut |cx| {
            cx.executor().advance_clock(Duration::from_secs(1));
            lane_clock(cx, &b, 1).is_some_and(|clock| clock != started)
        },
    );
    let moved = lane_clock(cx, &b, 1).expect("still working");
    assert_ne!(
        moved, started,
        "the clock counts on from the moment its age was read"
    );
    assert!(
        lane_working(cx, &tab, 1),
        "and the lane is still the reason it is moving"
    );
}

/// The step clock the left column draws on a lane's row (§7.3), read off the row's
/// own aria label — which is also what a reader who cannot see the cell is told.
fn lane_clock(cx: &mut TestAppContext, b: &Bench, n: u64) -> Option<String> {
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let row = window.find(agent_list::row_id(AgentKey::Lane(n as u32)));
        row.label().and_then(|label| {
            label
                .rsplit_once(", step ")
                .map(|(_, clock)| clock.to_owned())
        })
    })
    .ok()
    .flatten()
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
    show_tab(cx, &b, 0);
    prompt(cx, &b, &first, "SLOW first tab");
    show_tab(cx, &b, 1);
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

/// One stub reply's normalized usage, from `../evo-agent/tests/stub-messages.py`:
/// `message_start` reports `{"input_tokens": 10}`, `message_delta`
/// `{"output_tokens": 5}`, and the stub reports no cache fields at all. So every
/// `message-end` folds 15 tokens into the session's readout (§7.3).
const STUB_USAGE_TOKENS: u64 = 15;

/// §7.3: the readout's context figure is re-anchored from every `message-end`'s
/// usage, so the line moves **with the run** rather than jumping when the run
/// settles.
///
/// The coordinator's model here is a stub of its own with a 1000-token window,
/// which is what makes the move readable at all: with the stock stub's 200k
/// window both figures round to `0%`. The run is a delegation — the lane's work
/// is seconds long, so the first `message-end` (the coordinator's tool call) is
/// well inside the run, and the line has to have moved while the tab still shows
/// the coordinator working.
#[gpui_kit::test]
fn the_readout_moves_with_the_run_not_only_at_settled(cx: &mut TestAppContext) {
    let b = bench_with(
        cx,
        2,
        "(evo:register-model \"stub-tiny\" :provider :stub :context-window 1000 \
         :max-output 8000 :effort t)\n",
    );
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(
        cx,
        &b,
        &tab,
        b.fixture.project.clone(),
        LaunchPlan {
            model: Some(("stub-tiny".to_string(), "stub".to_string())),
            workers: Some(2),
            ..LaunchPlan::default()
        },
    );
    wait_for_running(cx, &tab);

    // The seed is `/state`: a session that has said nothing yet, and the window
    // its model registers.
    wait_for(cx, "the readout to be seeded from /state", |cx| {
        readout(cx, &tab)
            .is_some_and(|line| line.contains("stub-tiny") && line.contains("ctx 0k/1k"))
    });
    assert_eq!(
        context_tokens(cx, &tab),
        0,
        "nothing has been said, so the context is empty: {}",
        readout(cx, &tab).unwrap_or_default()
    );

    // A run that lasts: the coordinator delegates, and the lane takes seconds.
    prompt(
        cx,
        &b,
        &tab,
        "CALL delegate {\"lane\":1,\"task\":\"SLOW lane work for the test\"}",
    );

    // While the coordinator is still working, the first message's usage has
    // already re-anchored the line — 15 tokens of 1000 is 2%, where the seed
    // read 0%.
    wait_for(cx, "the line to move while the run is in flight", |cx| {
        cx.update(|cx| {
            let tab = tab.read(cx);
            let line = tab.model().map(|model| model.readout_text());
            tab.is_running() && line.is_some_and(|line| line.contains("ctx 0k/1k (2%)"))
        })
    });
    assert_eq!(
        context_tokens(cx, &tab),
        STUB_USAGE_TOKENS,
        "the figure is the folded usage, not the /state estimate it started from"
    );

    // And the settled resync says the same thing from `/state`, because the fold
    // that moved the line is the same arithmetic the server does (§9.1).
    wait_for(cx, "the run to settle", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.activity() == session::Activity::Idle)
        })
    });
    assert_eq!(context_tokens(cx, &tab), STUB_USAGE_TOKENS);
}

/// The line the composer's status row is handed, as the tab's model has it
/// (§7.3).
fn readout(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) -> Option<String> {
    cx.update(|cx| tab.read(cx).model().map(|model| model.readout_text()))
}

/// The context figure the same line is built from — the number §7.3 says every
/// `message-end` re-anchors.
fn context_tokens(cx: &mut TestAppContext, tab: &Entity<workspace::TabContent>) -> u64 {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| model.readout().context_tokens())
            .unwrap_or_default()
    })
}

/// §7.3: the two segments a coordinator's own state adds to the page — the goal
/// and the todo panel — come from the server and nowhere else: the goal from
/// `/state.goal` (the model's `create_goal` puts it there) and the todos from
/// `todo-changed` on the coordinator's stream, which `/state.todos` re-states on
/// every resync.
#[gpui_kit::test]
fn the_coordinators_goal_and_todos_reach_the_tab_page(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    // A session with no goal and no todos: the line carries no goal segment, and
    // the panel is not drawn at all — an empty panel is not a panel (§7.3).
    wait_for(cx, "the readout to name the model", |cx| {
        readout(cx, &tab).is_some_and(|line| line.contains(STUB_MODEL))
    });
    assert!(
        !readout(cx, &tab).unwrap_or_default().contains("goal "),
        "a session with no goal hides the segment"
    );
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("todo-panel").is_none());
    })
    .unwrap();

    // `create_goal`, through the tool the model is given. The objective ends in
    // the stub's `FINISH` marker on purpose: an active goal makes the coordinator
    // continue itself (its settled hook queues the next continuation), and the
    // marker is what makes the stub's continuation turn close the goal — so the
    // run ends, a `settled` arrives, and the segment comes with the resync that
    // follows it (§9.1). The goal is the server's either way; this only decides
    // when the tab gets to re-read it.
    prompt(
        cx,
        &b,
        &tab,
        "CALL create_goal {\"objective\":\"prove the goal segment FINISH\",\"token-budget\":50000}",
    );
    wait_for_tab(cx, &tab, "the goal segment to appear", BOOT, |cx| {
        readout(cx, &tab).is_some_and(|line| {
            line.contains(" · goal g-") && line.contains("(complete)") && line.ends_with("/50k")
        })
    });

    // And the checklist: `todo` replaces the whole list, and the panel that
    // appears is the *coordinator's* — the engine shows the selected agent's
    // todos, and `main` is what a tab page starts on.
    prompt(
        cx,
        &b,
        &tab,
        "CALL todo {\"items\":[\
         {\"text\":\"write the readout test\",\"status\":\"in-progress\"},\
         {\"text\":\"run it\",\"status\":\"pending\"}]}",
    );
    wait_for(cx, "the coordinator's todos to arrive", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.selected_todos().len() == 2)
        })
    });
    let model_line = cx.read(|cx| {
        tab.read(cx)
            .model()
            .and_then(|model| model.selected_readout_text())
            .expect("the coordinator's readout, from /state")
    });
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("todo-panel").visible(),
            "the panel is on the page"
        );
        assert!(window.find(("todo-item", 0usize)).visible());
        assert!(window.find(("todo-item", 1usize)).visible());
        assert!(
            window.try_find(("todo-item", 2usize)).is_none(),
            "two items, two rows"
        );
        assert!(
            window.find("todo-header").visible(),
            "and the header that counts them"
        );

        // §7.3: under the panel, the status line. It is the *page's* now — the
        // composer's row is the input and the action — and what it says is the
        // selected agent's readout, whole, as its accessible name.
        let line = window.find(workspace::READOUT_LINE_ID);
        let panel = window.find("todo-panel").bounds();
        let column = window.find("transcript-column").bounds();
        assert!(line.visible(), "the status line is on the page");
        assert!(
            line.bounds().top() >= panel.bottom(),
            "under the todo panel, at the foot of the column: {:?} against {:?}",
            line.bounds(),
            panel
        );
        assert!(
            (line.bounds().size.width - column.size.width).abs() <= px(1.),
            "and it spans the column: {:?} against {column:?}",
            line.bounds()
        );
        assert_eq!(
            line.label(),
            Some(model_line.as_str()),
            "the line the tab read off the selected agent, whole"
        );
        assert!(
            line.label().is_some_and(|line| line.contains("goal g-")),
            "which is the readout the swarm reported, goal segment and all: {:?}",
            line.label()
        );
    })
    .unwrap();

    // And it is not the composer's line any more: that row keeps the input and
    // the action (§7.3).
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(composer::READOUT_ID).is_none(),
            "the composer's row no longer carries the readout"
        );
        assert!(
            window.find(composer::BUTTON_ID).visible(),
            "the action is still there"
        );
    })
    .unwrap();
}

/// §7.3, §9.2: one button, whose face says what it does. While the coordinator
/// works it reads **Stop**, and clicking it interrupts — it never sends, and it
/// leaves the draft exactly where it was.
#[gpui_kit::test]
fn stop_interrupts_the_run_and_keeps_the_draft(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    // A reply that takes its time: 60 deltas, a tenth apart.
    prompt(cx, &b, &tab, "SLOW say something long");
    wait_for(cx, "the run to be in flight", |cx| {
        cx.update(|cx| tab.read(cx).is_running())
    });
    wait_for(cx, "the reply to start streaming", |cx| {
        assistant_chars(&tab_rows(cx, &tab)) > 0
    });

    // A draft typed while the coordinator works, and the button that will not
    // send it.
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(composer::BUTTON_ID).label(),
            Some("Stop"),
            "a working coordinator offers Stop"
        );
        window.input("half a next turn", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(composer::BUTTON_ID).label(),
            Some("Stop"),
            "a stop is a stop whatever the input holds"
        );
        window.click(composer::BUTTON_ID, cx);
    })
    .unwrap();

    // The interrupt landed: the run is over, and the stream stopped where it
    // was — a full reply is sixty deltas long.
    wait_for(cx, "the interrupt to end the run", |cx| {
        cx.update(|cx| !tab.read(cx).is_running())
    });
    let streamed = assistant_chars(&tab_rows(cx, &tab));
    assert!(
        streamed < 400,
        "the reply was interrupted, not finished: {streamed} chars"
    );

    // And the draft is still there: the button is a Send button again, and it
    // has something to send (§7.3 — Stop leaves the draft untouched).
    cx.update(|cx| {
        let composer = tab.read(cx).composer().read(cx);
        assert_eq!(composer.face(), composer::ActionFace::Send);
        assert!(
            composer.is_action_enabled(cx),
            "an interrupt's reply never clears the draft"
        );
    });
}

/// §7.3: `Esc` is the second route to that same interrupt — with the caret in
/// the composer, where a person's hands are while they read a reply.
#[gpui_kit::test]
fn escape_interrupts_the_run_and_keeps_the_draft(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    prompt(cx, &b, &tab, "SLOW say something long");
    wait_for(cx, "the run to be in flight", |cx| {
        cx.update(|cx| tab.read(cx).is_running())
    });
    wait_for(cx, "the reply to start streaming", |cx| {
        assistant_chars(&tab_rows(cx, &tab)) > 0
    });

    // The keyboard is in the composer — the tab puts it there (§7.1) — and Esc
    // is what it does there, not the input's own clear.
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("half a next turn", cx);
        window.press("escape", cx);
    })
    .unwrap();

    wait_for(cx, "escape to interrupt the run", |cx| {
        cx.update(|cx| !tab.read(cx).is_running())
    });
    cx.update(|cx| {
        let composer = tab.read(cx).composer().read(cx);
        assert_eq!(composer.face(), composer::ActionFace::Send, "idle again");
        assert!(
            composer.is_action_enabled(cx),
            "the draft Esc interrupted over is still in the input"
        );
    });
}

/// §9.7, §3: a swarm that is **gone** — its whole process group killed, the
/// supervisor with it — is a failure the tab shows, with the log the server was
/// writing and a Retry. Retrying a swarm that had been up resumes the session it
/// was writing (§3's `--resume`): the point of the retry is the conversation,
/// not a new one.
#[gpui_kit::test]
fn a_swarm_that_is_gone_shows_its_log_and_retry_resumes_the_session(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    wait_for(cx, "/state to name the session", |cx| {
        cx.update(|cx| tab.read(cx).session_path().is_some())
    });
    let session = cx
        .update(|cx| tab.read(cx).session_path().map(Path::to_path_buf))
        .expect("/state named the session");

    // One turn first: a conversation to keep, and the journal on disk that
    // `--resume` opens. The file is the answer to both — evo writes it at the
    // first assistant message, so waiting for it is waiting for the turn.
    prompt(cx, &b, &tab, "remember this turn");
    wait_for(cx, "the journal to be on disk", |_cx| session.is_file());
    wait_for(cx, "the turn to settle", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.activity() == session::Activity::Idle)
        })
    });

    // §3: what the tab shows of a log is its **last** ~40 lines, and §9.7: the
    // screen reads the paths in them back as places. Both need a log longer than
    // the tail it shows, so the tab's own directory gets one: 200 lines, and the
    // last of them naming a file in that directory, which is the shape of the
    // paths a server writes.
    let root = store::paths::Root::at(b.fixture.dir.join("app"));
    let store_id = cx
        .update(|cx| tab.read(cx).store_id().cloned())
        .expect("the tab's own directory, while it still has a swarm");
    let tab_dir = root.tab_dir(&store_id);
    {
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(root.tab_log(&store_id))
            .expect("the swarm's log, open for appending");
        for line in 1..=200 {
            writeln!(log, "line {line}").expect("a line of the log");
        }
        // The server's own wording for the token's place, which is what a real
        // log carries (and what the redactor leaves alone: it is the *value* of
        // a named key that gets masked, not a path named in a sentence).
        writeln!(
            log,
            "listening on http://127.0.0.1:1/ (token in {})",
            tab_dir.join("token").display()
        )
        .expect("a line of the log");
    }

    // The whole swarm goes at once. `evo-swarm serve` is spawned into a process
    // group of its own (§3), so one signal takes the supervisor, the server it
    // supervises and every lane — nothing is left to bring the server back, and
    // the reconnect the tab does for a *restarting* swarm never applies.
    let pid = cx
        .update(|cx| tab.read(cx).swarm_pid())
        .expect("the swarm's own pid, from /health");
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .status()
        .expect("kill");
    assert!(killed.success(), "kill -KILL -{pid}");

    // The tab says so, with the evidence: no one-line reason (nothing failed to
    // start), the log's own tail, and Retry (§9.7).
    wait_for_tab(cx, &tab, "the tab to fail", RECONNECT, |cx| {
        matches!(state(cx, &tab), TabState::Failed { .. })
    });
    let TabState::Failed {
        message,
        log_tail,
        was_up,
        ..
    } = state(cx, &tab)
    else {
        unreachable!("just matched")
    };
    assert!(was_up, "this swarm had answered /health");
    assert!(
        message.is_none(),
        "a swarm that went away has no boot reason"
    );
    assert!(
        !log_tail.trim().is_empty(),
        "the log tail is the evidence, and it is what the screen shows"
    );
    // §3's "last ~40 lines" is the last 40 of *this* log, not all of it: the log
    // is over 200 lines by now, and what came back is the newest of them.
    let lines = log_tail.lines().count();
    assert!(lines <= 40, "the tail is a tail: {lines} lines");
    assert!(
        log_tail.contains("line 200"),
        "the newest lines are the ones shown: {log_tail:?}"
    );
    assert!(
        !log_tail.contains("line 100"),
        "and the oldest are the ones dropped: {log_tail:?}"
    );
    // The state keeps the log's own lines, paths and all: what is shortened is
    // what the screen shows of them (§9.7).
    assert!(
        log_tail.contains(&tab_dir.display().to_string()),
        "the raw tail still names the tab's directory: {log_tail:?}"
    );
    let shown = cx
        .update_window(b.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .find("boot-log-tail")
                .label()
                .map(str::to_owned)
                .unwrap_or_default()
        })
        .unwrap();
    assert!(
        !shown.contains(&tab_dir.display().to_string()),
        "and the screen reads that directory as a place: {shown}"
    );
    assert!(
        shown.contains("<tab>/token"),
        "the paths into it read as <tab>/…: {shown}"
    );
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("boot-failure").visible());
        assert!(
            window.find("boot-log-tail").visible(),
            "the swarm's own log is on the screen"
        );
        assert!(
            window.find("boot-log-tail").bounds().size.height <= px(320.),
            "§9.7: a long tail scrolls inside a box of its own — got {:?}",
            window.find("boot-log-tail").bounds().size.height
        );
        assert!(
            window.find("failure-gone-note").visible(),
            "a swarm that was up has no boot reason, so the screen says what happened and what Retry does"
        );
        assert!(
            window.try_find("boot-failure-reason").is_none(),
            "and it is not dressed as a boot that could not start"
        );
        assert_eq!(window.find("tab-retry").label(), Some("Retry"));
    })
    .unwrap();

    // Retry: the same folder, the same session.
    cx.update_window(b.window.into(), |_, window, cx| {
        window.click("tab-retry", cx);
    })
    .unwrap();
    wait_for_running(cx, &tab);
    wait_for(cx, "the resumed swarm to be the same session", |cx| {
        cx.update(|cx| tab.read(cx).session_path() == Some(session.as_path()))
    });
    // And the conversation is back: the row the user typed before the crash is
    // in the resumed transcript, which is what resuming is for.
    wait_for(cx, "the earlier turn to come back", |cx| {
        tab_rows(cx, &tab).iter().any(
            |row| matches!(&row.kind, session::RowKind::User { text } if text.contains("remember this turn")),
        )
    });
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

/// The dot the strip draws on a tab, if it is drawing one (§7.1). `name` is which
/// of the two dots: a run in flight, or a run that finished out of sight.
fn tab_dot(cx: &mut TestAppContext, b: &Bench, name: &str, id: u64) -> bool {
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find(ElementId::NamedInteger(name.into(), id))
            .is_some()
    })
    .unwrap()
}

/// §7.1: the strip says what a tab is doing. A run in flight gets a dot; a run that
/// finished while the user was looking at another tab leaves a quieter one, which
/// stays until that tab is the one being shown.
///
/// The whole point is that the user did not watch it happen, so this is about a tab
/// in the background: the run is started while its tab is shown, the user moves to
/// another tab, and the finish lands out of sight.
#[gpui_kit::test]
fn the_strip_dots_a_run_and_the_finish_a_background_tab_kept(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    let id = cx.update(|cx| tab.read(cx).id().get());
    let label = ElementId::NamedInteger("tab-label".into(), id);
    launch(cx, &b, &tab, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &tab);

    // A tab that is doing nothing has no dot — the strip is quiet by default — and
    // the label has the room the tab gives it.
    assert!(!tab_dot(cx, &b, "tab-running", id), "no run, no dot");
    assert!(!tab_dot(cx, &b, "tab-finished", id));
    let quiet_label = cx
        .update_window(b.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.find(label.clone()).bounds().size.width
        })
        .unwrap();

    prompt(cx, &b, &tab, "SLOW say something long");
    wait_for(cx, "the run to be in flight", |cx| {
        cx.update(|cx| tab.read(cx).is_running())
    });
    assert!(
        tab_dot(cx, &b, "tab-running", id),
        "the coordinator working is dotted"
    );
    assert!(
        !tab_dot(cx, &b, "tab-finished", id),
        "and it is the running dot, not the other one"
    );

    // It is a dot, not a layout: a fixed few pixels beside the label, which keeps
    // every pixel it had. (`size_1_5` is 20% of the parent, which once made the dot
    // a fifth of the tab wide and squeezed the label into an ellipsis.)
    let (dot, label_width) = cx
        .update_window(b.window.into(), |_, window, cx| {
            window.render_frame(cx);
            (
                window
                    .find(ElementId::NamedInteger("tab-running".into(), id))
                    .bounds()
                    .size,
                window.find(label.clone()).bounds().size.width,
            )
        })
        .unwrap();
    assert!(
        dot.width <= px(8.) && dot.height <= px(8.),
        "the dot is tiny: {dot:?}"
    );
    assert_eq!(
        label_width, quiet_label,
        "and the label keeps its room: no ellipsis from a dot"
    );

    // Another tab is opened and shown — while the run is still going, so what
    // happens next happens where nobody is looking.
    let other = open_tab(cx, &b);
    let other_id = cx.update(|cx| other.read(cx).id().get());
    assert!(
        !tab_dot(cx, &b, "tab-running", other_id),
        "the new tab has no swarm to be running"
    );

    wait_for(cx, "the run to finish out of sight", |cx| {
        cx.update(|cx| !tab.read(cx).is_running())
    });
    assert!(
        !tab_dot(cx, &b, "tab-running", id),
        "the run is over, so the running dot is gone"
    );
    assert!(
        tab_dot(cx, &b, "tab-finished", id),
        "and the strip keeps the finish for the tab nobody was watching"
    );

    // Looking at the tab is what clears it: the dot has done its job.
    show_tab(cx, &b, 0);
    assert!(
        !tab_dot(cx, &b, "tab-finished", id),
        "showing the tab clears its dot"
    );
    show_tab(cx, &b, 1);
    assert!(
        !tab_dot(cx, &b, "tab-finished", id),
        "and it does not come back when the tab is left again"
    );
}

/// §7.1: the window's own shortcuts reach the strip with the caret in a composer,
/// which is where selecting a running tab puts it. The composer's key context sits
/// *inside* the workspace's context rather than instead of it, and the composer
/// intercepts only the keys it has a use for — so ⌘1…⌘9 and ⌃⇥ still mean the tab
/// strip.
#[gpui_kit::test]
fn the_tab_keys_reach_the_strip_with_the_caret_in_the_composer(cx: &mut TestAppContext) {
    let b = bench(cx, 2);
    let first = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(cx, &b, &first, b.fixture.project.clone(), two_workers());
    wait_for_running(cx, &first);

    // A second tab, then back to the running one: selecting a running tab is what
    // hands the caret to its composer (§7.1).
    open_tab(cx, &b);
    show_tab(cx, &b, 0);
    cx.update_window(b.window.into(), |_, window, cx| {
        window.render_frame(cx);
        // The keyboard really is in the composer: GPUI resolves a keystroke against
        // the focused element's context, and `Input` is what an editable field
        // carries.
        let contexts: Vec<String> = window
            .context_stack()
            .iter()
            .filter_map(|context| context.primary().map(|entry| entry.key.to_string()))
            .collect();
        assert!(
            contexts.iter().any(|context| context == "Input"),
            "the caret is in the composer: {contexts:?}"
        );

        window.press("cmd-2", cx);
        assert_eq!(
            b.view.read(cx).selected_index(),
            1,
            "⌘2 reached the strip with the caret in the composer"
        );
        window.press("cmd-1", cx);
        assert_eq!(b.view.read(cx).selected_index(), 0);
        window.press("ctrl-tab", cx);
        assert_eq!(b.view.read(cx).selected_index(), 1, "and so did ⌃⇥");
    })
    .unwrap();
}

/// The row ids the coordinator's **view** shows, in order.
fn view_row_ids(
    cx: &mut TestAppContext,
    tab: &Entity<workspace::TabContent>,
) -> Vec<session::RowId> {
    tab_rows(cx, tab).iter().map(|row| row.id).collect()
}

/// The row ids the coordinator's **model** holds, in order: what its view has to be
/// showing. A row the model dropped and the view kept is a row the model's ids do not
/// have — which is the whole shape of the stale-dots bug.
fn model_row_ids(
    cx: &mut TestAppContext,
    tab: &Entity<workspace::TabContent>,
) -> Vec<session::RowId> {
    cx.update(|cx| {
        tab.read(cx)
            .model()
            .map(|model| {
                model
                    .coordinator()
                    .rows()
                    .iter()
                    .map(|row| row.id)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// §9.1, and the stale waiting dots: a step that only calls a tool opens a row on
/// `message-start` and `message-end` drops it again — no text, no thinking, no error.
/// The model names the dropped id in `RowChanges::Changed` with no row behind it, and
/// the tab has to tell the view to **remove** that row. A sync that only pushes the rows
/// the model still has leaves the empty streaming row on screen, and it keeps drawing
/// its waiting dots over the tool row for as long as the tool runs — until the run's
/// `settled` resync finally rebuilds the transcript.
///
/// The window those dots lived in is the tool call itself: the stub's `CALL <tool> {…}`
/// gives the step its call, and `sleep 3` keeps it running long enough to read the
/// transcript while it is. What the view shows in that window is the assertion — the
/// tool row, no assistant row at all (the step carried nothing), and exactly the rows
/// the model holds.
#[gpui_kit::test]
fn a_tool_only_step_leaves_no_waiting_dots_over_the_tool_row(cx: &mut TestAppContext) {
    let b = bench(cx, 1);
    let tab = cx.update(|cx| b.view.read(cx).selected_tab().clone());
    launch(
        cx,
        &b,
        &tab,
        b.fixture.project.clone(),
        LaunchPlan {
            workers: Some(1),
            ..LaunchPlan::default()
        },
    );
    wait_for_running(cx, &tab);

    prompt(cx, &b, &tab, r#"CALL bash {"command":"sleep 3"}"#);

    // The call is out and its result has not come back: the row the step opened has
    // been dropped by now, in the same stream, in order.
    wait_for(cx, "the tool call to be running", |cx| {
        tab_rows(cx, &tab).iter().any(|row| {
            matches!(&row.kind, session::RowKind::Tool { name, result: None, .. } if name == "bash")
        })
    });

    let rows = tab_rows(cx, &tab);
    let dots: Vec<session::RowId> = rows
        .iter()
        .filter(|row| {
            matches!(
                &row.kind,
                session::RowKind::Assistant { markdown, thinking, streaming: true, .. }
                    if markdown.is_empty() && thinking.is_empty()
            )
        })
        .map(|row| row.id)
        .collect();
    assert!(
        dots.is_empty(),
        "the dropped row left its waiting dots over the tool row: {rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| matches!(&row.kind, session::RowKind::Assistant { .. })),
        "a tool-only step carries nothing, so it leaves no assistant row: {rows:#?}"
    );
    assert_eq!(
        view_row_ids(cx, &tab),
        model_row_ids(cx, &tab),
        "the transcript shows what the model has, and nothing else"
    );

    // The turn ends and the answer lands: it is the one assistant row, the tool row
    // carries its result, and the view is still exactly the model.
    wait_for(cx, "the run to settle", |cx| {
        cx.update(|cx| {
            tab.read(cx)
                .model()
                .is_some_and(|model| model.activity() == session::Activity::Idle)
        })
    });
    wait_for(cx, "the answer the tool call led to", |cx| {
        let rows = tab_rows(cx, &tab);
        rows.iter().any(|row| {
            matches!(
                &row.kind,
                session::RowKind::Tool {
                    result: Some(_),
                    ..
                }
            )
        }) && rows.iter().any(|row| {
            matches!(
                &row.kind,
                session::RowKind::Assistant { markdown, streaming, .. }
                    if !markdown.is_empty() && !streaming
            )
        })
    });

    let rows = tab_rows(cx, &tab);
    let assistants: Vec<&session::Row> = rows
        .iter()
        .filter(|row| matches!(&row.kind, session::RowKind::Assistant { .. }))
        .collect();
    assert_eq!(
        assistants.len(),
        1,
        "the run left one assistant row — the dropped one is not there to be counted: {rows:#?}"
    );
    assert!(
        matches!(
            &assistants[0].kind,
            session::RowKind::Assistant { markdown, streaming, .. }
                if !markdown.is_empty() && !*streaming
        ),
        "and it is the answer, not a row still waiting: {:#?}",
        assistants[0]
    );
    assert_eq!(
        view_row_ids(cx, &tab),
        model_row_ids(cx, &tab),
        "the settled rebuild left the view and the model agreeing"
    );
}
