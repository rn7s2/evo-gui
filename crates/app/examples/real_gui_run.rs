//! real_gui_run — **the M0/M1 proof through the GUI**: the assembled app window
//! driving one real swarm, on the real `HOME`, with the installed binaries.
//!
//! ```sh
//! cargo run -p evo-desktop --example real_gui_run -- --capture docs/screens/real --scale 1
//! ```
//!
//! Where `app_snapshot` scripts its model and its `HOME`, this one scripts
//! nothing: the `Shell` global, the `WorkspaceView` window and the real
//! `tab_engine` are the production ones, the binaries are the installed
//! `/usr/local/bin/evo-swarm` and `/usr/local/bin/evo-agent` (whatever
//! `app.json` holds — the defaults here), the environment is this process's,
//! and the swarm therefore reads the user's own `~/.evo/init.lisp`,
//! `~/.evo/swarm.lisp` and credentials. One tab is opened the way the empty tab
//! opens one — its `TabContentEvent::Launch` — in a fresh temp folder, with
//! `--workers 1` and **no** model override, so the swarm and its lane run on the
//! models the user's own configuration names.
//!
//! The only thing not real is the app's own root: `~/.evo/desktop` is a temp
//! directory, because the quit sequence writes `app.json` and this proof must
//! not rewrite the tab set of a desktop app the user may have open. `HOME`, the
//! evo home, the binaries, the models and the credentials are the real ones.
//!
//! It sends one prompt through the composer, and captures four frames at 1x
//! (§12 M0, M1):
//!
//! * (a) mid-stream of the coordinator's first reply;
//! * (b) after the delegation, with the lane working (the lane list's dot);
//! * (c) lane 1 selected, showing its own transcript;
//! * (d) the final state, after the lane's report has been fed back and the
//!   coordinator's follow-up turn is over. Not the first `settled`: that is a
//!   turn boundary, and the coordinator's closing line comes after it
//!   (docs/proofs-real.md, finding R4).
//!
//! (a) and (b) are waited for in one loop, not one after the other: a lane on a
//! fast model can take the work, do it and report back before the coordinator's
//! thinking lets its first word out, so a proof that goes looking for the lane's
//! dot only after the reply has started finds the lane idle again.
//!
//! Then it quits through the app's own quit path (`evo_desktop::begin_quit`) and
//! checks that no process it started is still alive, and that the lane really
//! wrote `hello.txt` in the folder.
//!
//! Cost: a real coordinator and a real lane, a few short turns. Run it when the
//! point is to prove the GUI against the installed evo, not for a picture.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, Entity,
    HeadlessAppContext, WindowBounds, WindowOptions,
};
use session::{AgentKey, LaunchPlan, RowKind};
use store::app_state::AppState;
use store::model_cache::ModelCache;
use store::paths::Root;
use store::time;
use workspace::{TabContentEvent, TabState, WorkspaceView};

use evo_desktop::{AppLog, Shell};

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// The scale the headless renderer draws at (a retina platform window).
const RENDER_SCALE: f32 = 2.;

/// A transition samples the app clock; advance it before a capture.
const SETTLE: Duration = Duration::from_millis(400);

/// The prompt §12's M0/M1 proof sends: the coordinator is asked to delegate, and
/// every agent in the swarm is asked to stay short — a real run costs money.
const PROMPT: &str =
    "Delegate to a lane: create hello.txt containing 'hi' in this folder, then tell \
                      me when it is done. Keep every reply to one short sentence.";

fn main() {
    let mut dir: Option<PathBuf> = None;
    let mut scale = 1.0f32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => dir = args.next().map(PathBuf::from),
            "--scale" => {
                scale = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1.)
            }
            other => {
                eprintln!("usage: real_gui_run --capture <dir> [--scale <n>] (got {other})");
                std::process::exit(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: real_gui_run --capture <dir> [--scale <n>]");
        std::process::exit(2);
    };
    if let Err(error) = run(&dir, scale) {
        eprintln!("real_gui_run failed: {error}");
        std::process::exit(1);
    }
}

/// A window's logical size, and the scale its pictures are saved at: `1` means
/// 1600x1000 pixels, `2` the renderer's own 3200x2000.
#[derive(Clone, Copy)]
struct Screens {
    width: f32,
    height: f32,
    scale: f32,
}

impl Screens {
    const fn app(scale: f32) -> Screens {
        Screens {
            width: WINDOW_SIZE.0,
            height: WINDOW_SIZE.1,
            scale,
        }
    }

    /// The picture a file should hold, as the renderer's own pixels.
    fn device(&self) -> (f32, f32) {
        (self.width * RENDER_SCALE, self.height * RENDER_SCALE)
    }

    /// The pixel size a `--scale` run resamples that to.
    fn saved(&self) -> Option<(u32, u32)> {
        (self.scale != RENDER_SCALE).then_some((
            (self.width * self.scale) as u32,
            (self.height * self.scale) as u32,
        ))
    }
}

fn run(dir: &Path, scale: f32) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let screens = Screens::app(scale);
    let work = temp_dir()?;
    // The folder the swarm runs in: fresh, empty, and inside the temp tree, so
    // the lane's `hello.txt` is nobody's file but this run's.
    let project = work.join("project");
    std::fs::create_dir_all(&project)?;
    let session_started = time::now_rfc3339();
    println!("[utc] {session_started} start");
    println!(
        "[env] HOME {} project {}",
        std::env::var("HOME").unwrap_or_default(),
        project.display()
    );

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The engines boot on their own threads and their updates arrive on their own
    // channel: this test scheduler has to be told that real I/O is expected.
    cx.allow_parking();

    // The app's own assembly: the production `Shell`, with the binaries
    // `app.json` names (the installed ones) and an app root of this run's own.
    let root = Root::at(work.join("app"));
    cx.update(|cx| {
        let log = AppLog::open(&root);
        log.info("real_gui_run: building the app's shell");
        Shell::new(
            root.clone(),
            log,
            AppState::default(),
            ModelCache::default(),
        )
        .install(cx);
    });
    let config = Arc::new(cx.update(|cx| evo_desktop::swarm_config(cx)));
    println!(
        "[env] app root {} | evo-swarm {} | evo-agent {}",
        root.path().display(),
        config.swarm_bin.display(),
        config.agent_bin.display()
    );

    let (window, view) = open_workspace(&mut cx, config, screens)?;
    let appearance = cx.update_window(window, |_, window, cx| {
        let following = evo_desktop::follow_appearance(cx, window);
        let choice = cx.global::<Shell>().theme;
        let mode = evo_desktop::mode_for(choice, window.appearance());
        let label = if mode.is_dark() { "dark" } else { "light" };
        (following, label)
    })?;
    println!("[theme] the window is {}", appearance.1);

    cx.update(|cx| {
        cx.update_global::<Shell, _>(|shell, _| {
            shell.view = Some(view.downgrade());
            // No session scan and no catalog probe ran: this proof is about the
            // swarm, and on this machine the probe is finding R1's `/registry`
            // 500 anyway. The empty tab's choosers stand on `Default`, which is
            // what the launch below uses.
            shell.launcher.scanning = false;
        });
        evo_desktop::push_launcher_data(cx);
    });
    cx.run_until_parked();

    // The tab the window opened with: an empty one, the screen §7.2 gives a
    // folder nobody has chosen yet. Its own `Launch` event is what starts the
    // swarm — the path the folder card and the choosers take.
    let tab = view.read_with(&cx, |view, _| view.tabs()[0].clone());
    println!(
        "[utc] {} empty tab open; launching in {}",
        time::now_rfc3339(),
        project.display()
    );

    // The plan the empty tab's choosers would produce: one worker, no model (so
    // the swarm runs on the user's own `~/.evo/swarm.lisp`), no lanes model (so
    // nothing is written into the folder's `.evo/swarm.lisp`) (§7.2, §9.6).
    let plan = LaunchPlan {
        workers: Some(1),
        ..LaunchPlan::default()
    };
    launch_via_event(&mut cx, window, &tab, &project, plan)?;
    wait_running(&mut cx, &tab, "the real swarm")?;
    println!(
        "[utc] {} swarm running (pid {:?}, session {:?})",
        time::now_rfc3339(),
        tab.read_with(&cx, |tab, _| tab.swarm_pid()),
        tab.read_with(&cx, |tab, _| tab
            .session_path()
            .map(|path| path.display().to_string()))
    );
    print_readout(&cx, &tab, "at boot");

    let outcome = proof(&mut cx, window, &tab, &project, dir, screens);

    // The app's own quit: every tab's ladder, in that order, then `app.json`.
    println!(
        "[utc] {} quit: evo_desktop::begin_quit",
        time::now_rfc3339()
    );
    cx.update(evo_desktop::begin_quit);
    let quiet = wait_for_quiet(&mut cx, &root, &project, Duration::from_secs(45));
    report_processes(&root, &project, quiet);
    print_app_log(&root);

    // The work itself, once the transcript can no longer change.
    let hello = project.join("hello.txt");
    println!(
        "[check] hello.txt exists: {} | content: {:?}",
        hello.is_file(),
        std::fs::read_to_string(&hello).unwrap_or_default()
    );

    drop(appearance.0);
    if std::env::var_os("EVO_DESKTOP_KEEP_REAL_TMP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    } else {
        println!("[note] kept {}", work.display());
    }
    println!("[done] picture(s) in {}", dir.display());
    outcome
}

/// The four captures, in the order §12 describes them. Everything the proof
/// prints that a human has to read (the readout, the transcript excerpts) is
/// printed here, while the tab still holds its model.
fn proof(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    project: &Path,
    dir: &Path,
    screens: Screens,
) -> Result<(), Box<dyn std::error::Error>> {
    // The prompt goes through the composer, which is the app's own send path
    // (§9.2) — typed and Entered, not posted to a port.
    prompt(cx, window, tab, PROMPT)?;
    println!("[utc] {} prompt posted", time::now_rfc3339());
    println!("[prompt] {PROMPT}");

    // (a) and (b) are two moments of the same run and they do not wait for each
    //     other: the coordinator's first reply begins to stream, and the delegation
    //     lands and puts a lane to work. Whichever happens first is caught first —
    //     which is why they share one loop rather than running in the order their
    //     numbers suggest (a reply that opens with a tool call puts nothing on
    //     screen until the lane is already at work).
    let a_taken = Cell::new(false);
    let b_taken = Cell::new(false);
    let lane_states = RefCell::new(Vec::<String>::new());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(300);
    let mut told = started;
    loop {
        cx.run_until_parked();
        note_lane_states(cx, tab, &lane_states);
        if !a_taken.get() && markdown_chars(cx, tab) > 0 {
            // (a) the coordinator's first reply, mid-delta: no settle before the
            //     frame, so it is the row as it streamed.
            a_taken.set(true);
            println!(
                "[utc] {} (a) the first reply is streaming: {}",
                time::now_rfc3339(),
                first_reply_shape(cx, tab)
            );
            shot_now(cx, window, dir, "real-01-mid-stream.png", screens)?;
        }
        if !b_taken.get() && lane_busy(cx, tab, 1) && delegate_row(cx, tab, false).is_some() {
            // (b) the delegation is out and lane 1 is working: the coordinator's
            //     `delegate` row, and the lane list's dot — which is the same
            //     source the left column draws (a lane nobody has selected is not
            //     watched, so its own stream holds nothing yet).
            b_taken.set(true);
            println!(
                "[utc] {} (b) the delegation is out and a lane is working: {}",
                time::now_rfc3339(),
                delegation_state(cx, tab)
            );
            shot(cx, window, dir, "real-02-lane-working.png", screens)?;
        }
        if a_taken.get() && b_taken.get() {
            break;
        }
        let now = Instant::now();
        if now >= told + Duration::from_secs(3) {
            told = now;
            println!(
                "[ab-wait] t+{}s {}",
                started.elapsed().as_secs(),
                first_reply_shape(cx, tab)
            );
        }
        if now >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "[utc] {} (a) taken: {} | (b) taken: {} | {}",
        time::now_rfc3339(),
        a_taken.get(),
        b_taken.get(),
        delegation_state(cx, tab)
    );
    println!(
        "[ab-wait] lane 1 as the left column had it: {:?}",
        lane_states.borrow()
    );
    if !a_taken.get() {
        shot_now(cx, window, dir, "real-01-mid-stream.png", screens)?;
    }
    if !b_taken.get() {
        shot(cx, window, dir, "real-02-lane-working.png", screens)?;
    }

    // (c) lane 1 selected, showing its own transcript (§7.3 — one transcript
    //     per agent, fed by that agent's own stream).
    select_lane(cx, window, tab, 1)?;
    let written = wait_within(cx, 240, |cx| text_chars(cx, tab) > 0);
    println!(
        "[utc] {} (c) lane 1 selected; its transcript has {} char(s) (writing: {written})",
        time::now_rfc3339(),
        text_chars(cx, tab)
    );
    shot(cx, window, dir, "real-03-lane-transcript.png", screens)?;

    // (d) the report has been fed back to the coordinator and its follow-up turn
    //     is over. R4: the first `settled` is the coordinator handing the work
    //     over, not the run finishing — so wait for the lane's report and for the
    //     turn it starts, and let the transcript stand still before the frame.
    select_main(cx, window, tab)?;
    let reported = wait_watching(
        cx,
        420,
        "d-wait",
        |cx| delegation_state(cx, tab),
        |cx| report_seen(cx, tab),
    );
    let still = RefCell::new((String::new(), Instant::now()));
    let followed_up = reported
        && wait_watching(
            cx,
            420,
            "d-wait",
            |cx| delegation_state(cx, tab),
            |cx| {
                coordinator_activity(cx, tab) == session::Activity::Idle
                    && standing_still(cx, tab, &still, Duration::from_secs(6))
            },
        );
    println!(
        "[utc] {} (d) report seen: {reported} | the follow-up turn is over: {followed_up} | {}",
        time::now_rfc3339(),
        delegation_state(cx, tab)
    );
    shot(cx, window, dir, "real-04-follow-up.png", screens)?;

    println!(
        "[utc] {} run over; reading the transcript",
        time::now_rfc3339()
    );
    print_readout(cx, tab, "at the end");
    select_main(cx, window, tab)?;
    println!("--- the coordinator's transcript (final state) ---");
    for row in row_lines(cx, tab) {
        println!("{row}");
    }
    select_lane(cx, window, tab, 1)?;
    println!("--- lane 1's transcript (final state) ---");
    for row in row_lines(cx, tab) {
        println!("{row}");
    }
    println!(
        "[check] hello.txt in {}: {}",
        project.display(),
        project.join("hello.txt").is_file()
    );
    Ok(())
}

// --- the app's pieces ---------------------------------------------------------

fn open_workspace(
    cx: &mut HeadlessAppContext,
    config: Arc<workspace::SwarmConfig>,
    screens: Screens,
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Box<dyn std::error::Error>> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(screens.width), px(screens.height)),
        })),
        focus: false,
        show: false,
        ..gpui_kit::component::TitleBar::window_options()
    };
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| WorkspaceView::with_config(config, window, cx))
        })
    })?;
    Ok((window, view))
}

/// Launch through the **empty tab's own event**: the tab emits
/// [`TabContentEvent::Launch`] and the window starts the swarm, which is the
/// path the folder card and the choosers take (§7.2).
fn launch_via_event(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    folder: &Path,
    plan: LaunchPlan,
) -> Result<(), Box<dyn std::error::Error>> {
    let folder = folder.to_path_buf();
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch { folder, plan });
        });
    })?;
    Ok(())
}

/// Type a turn into the composer and press Enter — the app's own send path (§9.2).
fn prompt(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.activate_window();
        tab.update(cx, |tab, cx| {
            let composer = tab.composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        });
        window.input(text, cx);
        window.press("enter", cx);
        // The composer hands the draft on from its own key handler, which runs
        // when the window draws — a wait that only pumps tasks would sit on a
        // prompt that has not been posted yet.
        window.render_frame(cx);
    })?;
    Ok(())
}

fn select_lane(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    n: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| tab.select_agent(AgentKey::Lane(n), cx));
    })?;
    Ok(())
}

fn select_main(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| tab.select_agent(AgentKey::Coordinator, cx));
    })?;
    Ok(())
}

// --- what the tab is showing --------------------------------------------------

/// Whether the coordinator is mid-run.
fn coordinator_activity(
    cx: &HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
) -> session::Activity {
    tab.read_with(cx, |tab, _| {
        tab.model()
            .map(|model| model.activity())
            .unwrap_or(session::Activity::Idle)
    })
}

/// Whether lane `n` is doing something, as the **left column** sees it: the lane
/// list (§7.3) follows `/lanes` and the coordinator stream's `lane-state` events,
/// so it knows a lane is working without that lane's own stream being open.
fn lane_busy(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>, n: u32) -> bool {
    lane_row(cx, tab, n).is_some_and(|row| row.is_busy())
}

/// Lane `n`'s row as the left column draws it — its status glyph, its task and
/// its step clock.
fn lane_row(
    cx: &HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    n: u32,
) -> Option<session::LaneRow> {
    tab.read_with(cx, |tab, _| {
        tab.model()
            .and_then(|model| model.lanes().lane(u64::from(n)))
            .cloned()
    })
}

/// What the coordinator's transcript holds, row by row: what the (a) frame is a
/// frame of, and what its wait is waiting for.
fn first_reply_shape(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> String {
    let activity = coordinator_activity(cx, tab);
    let (users, parts) = tab.read_with(cx, |tab, cx| {
        let Some(view) = tab.transcript() else {
            return (0, Vec::new());
        };
        let rows = view.read(cx).rows(cx);
        let mut parts: Vec<String> = Vec::new();
        let mut users = 0;
        for row in rows {
            match &row.kind {
                RowKind::User { .. } => users += 1,
                RowKind::Context { key, .. } => parts.push(format!("context {key}")),
                RowKind::LaneNotice { lane, .. } => parts.push(format!("lane {lane} notice")),
                RowKind::GoalNudge { kind, .. } => parts.push(format!("goal {kind:?}")),
                RowKind::CommandNote { command, .. } => parts.push(format!("command {command}")),
                RowKind::Assistant {
                    markdown,
                    streaming,
                    ..
                } => parts.push(format!(
                    "assistant {}{}",
                    markdown.chars().count(),
                    if *streaming { " streaming" } else { "" }
                )),
                RowKind::Tool { name, result, .. } => parts.push(format!(
                    "tool {name}{}",
                    if result.is_some() { " (result)" } else { "" }
                )),
                RowKind::Dim { text, .. } => parts.push(format!(
                    "dim {:?}",
                    text.chars().take(30).collect::<String>()
                )),
                RowKind::Report { .. } => parts.push("report".to_string()),
                RowKind::RunOutcome { .. } => parts.push("outcome".to_string()),
            }
        }
        (users, parts)
    });
    format!(
        "coordinator {activity:?} | {users} user row(s) | {}",
        if parts.is_empty() {
            "nothing from the assistant yet".to_string()
        } else {
            parts.join(", ")
        }
    )
}

/// Record the left column's view of lane 1 whenever it changes: the sequence of
/// states the lane row went through, which is what a failing (b) wait is about.
fn note_lane_states(
    cx: &HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    seen: &RefCell<Vec<String>>,
) {
    let row = lane_row(cx, tab, 1)
        .map(|row| format!("{} {}", row.glyph(), row.state))
        .unwrap_or_else(|| "not in the list".to_string());
    let mut seen = seen.borrow_mut();
    if seen.last().map(String::as_str) != Some(row.as_str()) {
        seen.push(row);
    }
}

/// The transcript excerpt the (b) wait prints every few seconds: a conjunction of
/// two conditions that a timeout says nothing about, one of which at a time.
fn delegation_state(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> String {
    let lane = lane_row(cx, tab, 1)
        .map(|row| format!("{} {}", row.glyph(), row.state))
        .unwrap_or_else(|| "not in the list".to_string());
    let busy = tab.read_with(cx, |tab, _| {
        tab.model().map(|model| model.lanes().busy()).unwrap_or(0)
    });
    format!(
        "coordinator {:?} | lane list: {busy} busy, lane 1 {lane} | delegate row {}/{} | {} row(s)",
        coordinator_activity(cx, tab),
        delegate_row(cx, tab, false).is_some(),
        delegate_row(cx, tab, true).is_some(),
        tab.read_with(cx, |tab, cx| tab
            .transcript()
            .map(|view| view.read(cx).rows(cx).len())
            .unwrap_or(0)),
    )
}

/// [`wait_within`] that says what it is waiting on every few seconds, for a
/// condition that is a conjunction: a timeout then names the half that never held.
fn wait_watching(
    cx: &mut HeadlessAppContext,
    secs: u64,
    label: &str,
    watch: impl Fn(&HeadlessAppContext) -> String,
    done: impl Fn(&HeadlessAppContext) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let started = Instant::now();
    let mut told = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return true;
        }
        let now = Instant::now();
        if now >= told + Duration::from_secs(3) {
            told = now;
            println!("[{label}] t+{}s {}", started.elapsed().as_secs(), watch(cx));
        }
        if now >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The row of the coordinator's `delegate` call: the delegation as the transcript
/// shows it, with a result once the tool has answered.
fn delegate_row(
    cx: &HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    with_result: bool,
) -> Option<u64> {
    tab.read_with(cx, |tab, cx| {
        tab.transcript().and_then(|view| {
            view.read(cx)
                .rows(cx)
                .iter()
                .find(|row| match &row.kind {
                    RowKind::Tool { name, result, .. } => {
                        name == "delegate" && (!with_result || result.is_some())
                    }
                    _ => false,
                })
                .map(|row| row.id)
        })
    })
}

/// How much markdown the shown transcript's assistant rows carry: the first
/// character of it is the coordinator's first reply arriving.
fn markdown_chars(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> usize {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| {
                view.read(cx)
                    .rows(cx)
                    .iter()
                    .filter_map(|row| match &row.kind {
                        RowKind::Assistant { markdown, .. } => Some(markdown.chars().count()),
                        _ => None,
                    })
                    .sum()
            })
            .unwrap_or_default()
    })
}

/// How much text the shown agent's transcript carries, in chars.
fn text_chars(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> usize {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| view.read(cx).rows(cx).iter().map(row_chars).sum::<usize>())
            .unwrap_or_default()
    })
}

fn row_chars(row: &session::Row) -> usize {
    match &row.kind {
        RowKind::User { text } => text.chars().count(),
        RowKind::Context { text, .. } => text.chars().count(),
        RowKind::LaneNotice { text, .. } => text.chars().count(),
        RowKind::GoalNudge { text, .. } => text.chars().count(),
        RowKind::CommandNote { text, .. } => text.chars().count(),
        RowKind::Assistant { markdown, .. } => markdown.chars().count(),
        RowKind::Tool { name, .. } => name.chars().count(),
        RowKind::Report { done, .. } => done.chars().count(),
        RowKind::Dim { text, .. } => text.chars().count(),
        RowKind::RunOutcome { text, .. } => text.chars().count(),
    }
}

/// Whether the coordinator's transcript holds the lane's report. It arrives as a
/// `report` row while the event is the newest thing on the stream, and `/transcript`
/// rebuilds it as the user message the swarm fed back when a resync replaces the
/// rows — so both shapes are the report (§5).
fn report_seen(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> bool {
    tab.read_with(cx, |tab, cx| {
        tab.transcript().is_some_and(|view| {
            view.read(cx).rows(cx).iter().any(|row| match &row.kind {
                RowKind::Report { .. } => true,
                RowKind::User { text } => text.contains("[lane 1 report]"),
                _ => false,
            })
        })
    })
}

/// Whether the shown transcript has stopped changing, and has been still for
/// `hold`: the coordinator answers the report and then the lane's run-end arrives
/// as another turn, so "idle right now" is not "nothing more is coming".
fn standing_still(
    cx: &HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    state: &RefCell<(String, Instant)>,
    hold: Duration,
) -> bool {
    let now = tab.read_with(cx, |tab, cx| {
        let Some(view) = tab.transcript() else {
            return String::new();
        };
        let rows = view.read(cx).rows(cx);
        let chars: usize = rows.iter().map(row_chars).sum();
        let tail: String = rows
            .last()
            .map(|row| format!("{:?}", row.kind).chars().take(60).collect())
            .unwrap_or_default();
        format!("{}|{chars}|{tail}", rows.len())
    });
    let mut state = state.borrow_mut();
    if state.0 != now {
        *state = (now, Instant::now());
    }
    Instant::now().duration_since(state.1) >= hold
}

/// The shown transcript's rows, in full: what the pictures show, for the proof's
/// transcript excerpt.
fn row_lines(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> Vec<String> {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| {
                view.read(cx)
                    .rows(cx)
                    .iter()
                    .map(|row| {
                        let (kind, text): (&str, String) = match &row.kind {
                            RowKind::User { text } => ("user", text.to_string()),
                            RowKind::Context { key, text } => ("context", format!("{key} {text}")),
                            RowKind::LaneNotice { lane, text, .. } => {
                                ("lane notice", format!("{lane} {text}"))
                            }
                            RowKind::GoalNudge { kind, text, .. } => {
                                ("goal", format!("{kind:?} {text}"))
                            }
                            RowKind::CommandNote { command, text } => {
                                ("command", format!("{command} {text}"))
                            }
                            RowKind::Assistant {
                                markdown,
                                streaming,
                                ..
                            } => (
                                if *streaming {
                                    "assistant (streaming)"
                                } else {
                                    "assistant"
                                },
                                markdown.to_string(),
                            ),
                            RowKind::Tool {
                                name,
                                arguments,
                                result,
                                ..
                            } => (
                                "tool",
                                format!(
                                    "{name} {arguments} -> {}",
                                    match result {
                                        Some(result) => result.content.as_str(),
                                        None => "(no result yet)",
                                    }
                                ),
                            ),
                            RowKind::Report { done, .. } => ("report", done.to_string()),
                            RowKind::Dim { text, .. } => ("dim", text.to_string()),
                            RowKind::RunOutcome { text, .. } => ("outcome", text.to_string()),
                        };
                        let text: String = text.trim().chars().take(4000).collect();
                        format!("[{kind}] {text}")
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The §7.3 status line the composer shows, printed as the readout's own
/// segments — this is where the model and the provider the run used are visible.
fn print_readout(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>, when: &str) {
    let line = tab.read_with(cx, |tab, _| {
        tab.model().map(|model| {
            let readout = model.readout();
            (
                readout.text(),
                readout.segments().join(" | "),
                format!("{:?}", readout.model_id()),
                format!("{:?}", readout.provider()),
            )
        })
    });
    match line {
        Some((text, segments, model, provider)) => {
            println!("[readout {when}] {text}");
            println!("[readout {when}] segments: {segments}");
            println!("[readout {when}] model: {model} provider: {provider}");
        }
        None => println!("[readout {when}] the tab has no model"),
    }
}

// --- the frames ---------------------------------------------------------------

fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
    screens: Screens,
) -> Result<(), Box<dyn std::error::Error>> {
    // Something that animates (a spinner, a stream fade) settles on the app
    // clock, which the headless context only advances when asked.
    cx.advance_clock(SETTLE);
    std::thread::sleep(SETTLE);
    shot_now(cx, window, dir, name, screens)
}

/// Draw a frame and save its pixels at once — no settle, for a state that is
/// passing (a row mid-stream).
fn shot_now(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
    screens: Screens,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let image = cx.capture_screenshot(window)?;
    let path = dir.join(name);
    image.save(&path)?;
    let (px, py) = screens.device();
    println!(
        "[capture] {:.0}x{:.0} -> {}{}",
        px,
        py,
        path.display(),
        if resample(&path, screens)? {
            " (resampled)"
        } else {
            ""
        }
    );
    Ok(())
}

/// `--scale 1`: the renderer draws at the platform's 2x, and the picture is
/// scaled down with `sips` (macOS). No `sips` leaves the 2x file in place.
fn resample(path: &Path, screens: Screens) -> Result<bool, Box<dyn std::error::Error>> {
    let Some((w, h)) = screens.saved() else {
        return Ok(false);
    };
    let widest = w.max(h);
    let status = Command::new("sips")
        .args(["-Z", &widest.to_string(), &path.to_string_lossy()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(true),
        _ => {
            println!("[note] no sips: {} stays {}x{}", path.display(), w, h);
            Ok(false)
        }
    }
}

// --- waits --------------------------------------------------------------------

/// Spin the UI thread for at most `secs`, saying whether `done` came true.
fn wait_within(
    cx: &mut HeadlessAppContext,
    secs: u64,
    done: impl Fn(&HeadlessAppContext) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        // A real provider streams as it goes: poll quickly, so a picture of a
        // row mid-stream is of that row and not of its finished self.
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_running(
    cx: &mut HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let running = wait_within(cx, 120, |cx| {
        matches!(
            tab.read_with(cx, |tab, _| tab.state().clone()),
            TabState::Running { .. }
        )
    });
    if running {
        return Ok(());
    }
    let state = tab.read_with(cx, |tab, _| format!("{:?}", tab.state()));
    Err(format!("{what} never answered /health: {state}").into())
}

// --- the swarm processes ------------------------------------------------------

/// The pids whose command line names one of `needles`, with the line.
fn processes_naming(needles: &[String]) -> Vec<(u32, String)> {
    let output = match Command::new("ps").args(["-axo", "pid=,command="]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            if !needles
                .iter()
                .any(|needle| command.contains(needle.as_str()))
            {
                return None;
            }
            Some((pid.parse().ok()?, command.trim().to_string()))
        })
        .collect()
}

/// What this run started, by the two things that name it: the tab directory
/// `--token-file` points at and the folder the swarm runs in.
fn our_processes(root: &Root, project: &Path) -> Vec<(u32, String)> {
    processes_naming(&[
        format!("--token-file {}", root.tabs_dir().display()),
        project.display().to_string(),
    ])
}

/// Wait until nothing this run started is alive any more.
fn wait_for_quiet(
    cx: &mut HeadlessAppContext,
    root: &Root,
    project: &Path,
    within: Duration,
) -> bool {
    let deadline = Instant::now() + within;
    loop {
        // The quit sequence's own task lives on this context (it is what calls
        // `App::quit` once the ladder is done), so pump it while waiting.
        cx.run_until_parked();
        if our_processes(root, project).is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn report_processes(root: &Root, project: &Path, quiet: bool) {
    let alive = our_processes(root, project);
    println!("[utc] {} process check", time::now_rfc3339());
    println!(
        "[cleanup] swarm process(es) still alive after the quit: {} (quiet: {quiet})",
        alive.len()
    );
    for (pid, command) in &alive {
        // The command line can hold a path but no credential: the token is a
        // file, never an argument (§3).
        println!("[cleanup]   {pid} {command}");
    }
    if !alive.is_empty() {
        // Nothing of ours may outlive the proof, so a survivor is killed and
        // reported as a finding rather than left running.
        for (pid, _) in &alive {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        std::thread::sleep(Duration::from_millis(500));
        println!(
            "[cleanup] killed the survivors; still alive now: {}",
            our_processes(root, project).len()
        );
    }
}

/// The app's own log, at the end: the quit sequence writes there, and its lines
/// are the evidence that the ladder ran rather than that the processes happened
/// to be gone.
fn print_app_log(root: &Root) {
    let path = root.path().join(evo_desktop::LOG_NAME);
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("[log] {} is unreadable", path.display());
        return;
    };
    let interesting = |line: &str| {
        ["quit", "shutdown", "stopped", "tab(s)"]
            .iter()
            .any(|word| line.contains(word))
    };
    println!("--- {} (quit lines) ---", path.display());
    for line in text.lines().filter(|line| interesting(line)) {
        println!("{line}");
    }
}

/// A directory under the system temp dir for this run's project folder and app
/// root. `run` removes it when it finishes.
fn temp_dir() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "evo-desktop-real-gui-{}-{nanos:x}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
