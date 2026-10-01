//! A scripted look at the real window: open it the way the app does, then click,
//! hover, press, drag and type, taking a picture after any step asked to.
//!
//! ```sh
//! cargo run -p evo-desktop --example probe -- --world real --out /tmp/probe script.txt
//! ```
//!
//! `--world real` runs against the real `HOME` (the real `init.lisp`, extensions
//! and `swarm.lisp`, the real session index) with a throwaway app root seeded
//! from `~/.evo/desktop/app.json`, so the user's own `app.json` is never written.
//! `--world stub` runs in `crates/proofs`' fixture: the scripted model, so a tab can
//! be launched and driven without spending anything.
//!
//! The script is one step per line (`#` starts a comment):
//!
//! ```text
//! shot NAME [light|dark|both]    picture of the window (default both)
//! crop NAME X Y W H [scale]      picture of a region, scaled up (default 2);
//!                                 after a `tap` it says how long the thing
//!                                 being watched had had when it started to
//!                                 draw (its own two frames land a render later)
//! theme light|dark               switch the theme
//! click ID | dclick ID | hover ID   (never `click select-folder`: a real dialog)
//! at X Y                          move the pointer to window coordinates
//! scroll ID [DX] DY               a wheel over an element, at its centre
//! scroll X Y DX DY                a wheel at a point — both are dispatched as
//!                                 real wheel events, so they land in the box's
//!                                 own handler the way a reader's does
//! down X Y | up X Y               press / release the left button there
//! press KEY | input TEXT          keyboard
//! keydown KEY | keyup KEY         one half of a keystroke: a *held* key is
//!                                 keydown, a pump past the hold, keyup (§9.8)
//! focus                           the shown tab's primary control (its composer)
//! pump MS                         let the app run
//! find ID                         print the element's bounds
//! tap X Y                         press and release there and come back at once,
//!                                 with no pump — a pump is longer than any of
//!                                 the design's transitions, so a script that
//!                                 wants to watch one cannot use `down`+`up`
//! frames ID MS1 MS2 ...           a frame at each offset from here, printing
//!                                 where ID lands in each, and how long it has
//!                                 really been since the `tap` before it: a
//!                                 transition, frame by frame
//! launch                          launch the shown tab in the world's folder
//! wait-running                    wait for the shown tab to be running
//! quit                            ask the app to quit, the menu item's way
//! quit-wait [SECS]                wait for every swarm the quit stopped to be gone
//!                                 (25s without a number), then check the screen
//! activate                        bring the window to the front (the kit paints a
//!                                 selection, and blinks the caret, only into an
//!                                 active window, which a headless one is not)
//! ```
//!
//! An ID is an element name, or `name#N` for a `NamedInteger`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{AppLog, Shell};
use gpui_kit::component::theme::ThemeMode;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, ElementId,
    Entity, HeadlessAppContext, InputEvent as _, KeyUpEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, ScrollDelta, ScrollWheelEvent, TouchPhase,
    WindowBounds, WindowOptions,
};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{TabContentEvent, TabState, WorkspaceView};

const WINDOW_SIZE: (f32, f32) = (1440., 900.);

type Error = Box<dyn std::error::Error>;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut world = "real".to_owned();
    let mut out = PathBuf::from("/tmp/probe");
    let mut script: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--world" => world = args.next().expect("--world real|stub"),
            "--out" => out = PathBuf::from(args.next().expect("--out DIR")),
            other => script = Some(PathBuf::from(other)),
        }
    }
    let script = script.expect("usage: probe [--world real|stub] [--out DIR] SCRIPT");
    let text = std::fs::read_to_string(&script).expect("the script");
    if let Err(error) = run(&world, &out, &text) {
        eprintln!("probe failed: {error}");
        std::process::exit(1);
    }
}

fn run(world: &str, out: &Path, script: &str) -> Result<(), Error> {
    std::fs::create_dir_all(out)?;
    let mut fixture = None;
    let (root, bins, folder) = if world == "stub" {
        let f = proofs::fixture::Fixture::new("probe");
        // A second registration, so a chooser has something to choose between.
        let init = f.home.join(".evo").join("init.lisp");
        let mut lisp = std::fs::read_to_string(&init)?;
        lisp.push_str(
            "(evo:register-model \"stub-b\" :provider :stub :context-window 100000 :max-output 4000 :effort t)\n",
        );
        std::fs::write(&init, lisp)?;
        f.enter();
        let bins = Binaries {
            evo_swarm: f.bins.swarm.clone(),
            evo_agent: f.bins.agent.clone(),
        };
        let r = (f.root.clone(), bins, f.folder.clone());
        fixture = Some(f);
        r
    } else {
        let dir = std::env::temp_dir().join(format!("evo-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let real = store::paths::default_root_dir();
        for name in ["app.json", "model-cache.json"] {
            let _ = std::fs::copy(real.join(name), dir.join(name));
        }
        let root = AppRoot::at(&dir);
        let state = AppState::load(&root);
        let folder = dir.join("project");
        std::fs::create_dir_all(&folder)?;
        let mut bins = state.binaries;
        if let Some(bin) = std::env::var_os("PROBE_SWARM_BIN") {
            bins.evo_swarm = PathBuf::from(bin);
        }
        (root, bins, folder)
    };

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx.allow_parking();
    let (window, view, _held) = open(&mut cx, &root, bins)?;
    cx.update(evo_desktop::start_background_loads);
    pump(&mut cx, Duration::from_secs(4));

    let mut mode = ThemeMode::Light;
    set_theme(&mut cx, window, mode)?;
    let mut pointer = Point::default();
    // When the step being watched last happened: `tap` sets it, `frames` and
    // `crop` measure against it. A step's own render takes time and that time is
    // part of the move, so the nominal offsets a script writes are not the ages a
    // transition actually has.
    let mut watching: Option<Instant> = None;
    // Read before a frame is drawn, never after: a frame takes time to draw, and
    // the numbers in it are the ones at its start.
    let age = move |watching: Option<Instant>| match watching {
        Some(since) => format!(" ({:.0}ms into it)", since.elapsed().as_secs_f32() * 1000.),
        None => String::new(),
    };
    for (n, line) in script.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let nums = || -> Vec<f32> {
            rest.split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect()
        };
        println!("[probe] {}: {line}", n + 1);
        match cmd {
            "shot" => {
                let mut parts = rest.split_whitespace();
                let name = parts.next().unwrap_or("shot");
                let modes: Vec<ThemeMode> = match parts.next().unwrap_or("both") {
                    "light" => vec![ThemeMode::Light],
                    "dark" => vec![ThemeMode::Dark],
                    _ => vec![ThemeMode::Light, ThemeMode::Dark],
                };
                for m in modes {
                    set_theme(&mut cx, window, m)?;
                    // Hover state lives on the pointer: put it back after the repaint.
                    move_to(&mut cx, window, pointer)?;
                    let suffix = if matches!(m, ThemeMode::Dark) {
                        "dark"
                    } else {
                        "light"
                    };
                    save(
                        &mut cx,
                        window,
                        &out.join(format!("{name}-{suffix}.png")),
                        None,
                    )?;
                }
                set_theme(&mut cx, window, mode)?;
                move_to(&mut cx, window, pointer)?;
            }
            "crop" => {
                let mut parts = rest.split_whitespace();
                let name = parts.next().unwrap_or("crop").to_owned();
                let v: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
                let scale = v.get(4).copied().unwrap_or(2.);
                let suffix = if matches!(mode, ThemeMode::Dark) {
                    "dark"
                } else {
                    "light"
                };
                let into_it = age(watching);
                if !into_it.is_empty() {
                    println!("[probe] {into_it}");
                }
                save(
                    &mut cx,
                    window,
                    &out.join(format!("{name}-{suffix}.png")),
                    Some((v[0], v[1], v[2], v[3], scale)),
                )?;
            }
            "theme" => {
                mode = if rest == "dark" {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                };
                set_theme(&mut cx, window, mode)?;
            }
            "click" | "dclick" | "hover" => {
                // The folder card opens the platform's own Open panel, which nothing
                // in a headless run can answer: the run would stall until it is
                // killed. `hover` is fine; a launch is the `launch` step.
                if cmd != "hover" && rest == "select-folder" {
                    return Err(format!(
                        "line {}: clicking select-folder opens the real Open panel; \
                         use `hover select-folder`, or `launch` to start a tab",
                        n + 1
                    )
                    .into());
                }
                let id = element_id(rest);
                cx.update_window(window, |_, window, cx| {
                    let found = window.find(id.clone());
                    pointer = found.bounds().center();
                    match cmd {
                        "click" => window.click(id, cx),
                        "dclick" => window.double_click(id, cx),
                        _ => window.hover(id, cx),
                    }
                })?;
                pump(&mut cx, Duration::from_millis(300));
            }
            // `scroll ID [DX] DY` (a wheel over an element, at its centre) or
            // `scroll X Y DX DY` (a wheel at a point). Both are dispatched as real
            // wheel events, so they land in the box's own handler the way a
            // reader's does — a scroll of a named element with no wheel would move
            // the box without telling it anything, and the transcript unpins only
            // for a reader's own scroll.
            "scroll" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                let v: Vec<f32> = words.iter().filter_map(|word| word.parse().ok()).collect();
                if words.len() == 4 && v.len() == 4 {
                    let position = point(px(v[0]), px(v[1]));
                    let delta = point(px(v[2]), px(v[3]));
                    cx.update_window(window, |_, window, cx| {
                        window.dispatch_event(
                            ScrollWheelEvent {
                                position,
                                delta: ScrollDelta::Pixels(delta),
                                modifiers: Default::default(),
                                touch_phase: TouchPhase::Moved,
                            }
                            .to_platform_input(),
                            cx,
                        );
                    })?;
                } else {
                    let id = element_id(words.first().copied().unwrap_or_default());
                    let delta = match v.as_slice() {
                        [dy] => point(px(0.), px(*dy)),
                        [dx, dy] => point(px(*dx), px(*dy)),
                        _ => {
                            return Err(format!(
                                "line {}: scroll wants ID DY, ID DX DY, or X Y DX DY",
                                n + 1
                            )
                            .into());
                        }
                    };
                    cx.update_window(window, |_, window, cx| {
                        window.scroll(id, ScrollDelta::Pixels(delta), cx);
                    })?;
                }
                pump(&mut cx, Duration::from_millis(200));
            }
            "at" => {
                let v = nums();
                pointer = point(px(v[0]), px(v[1]));
                move_to(&mut cx, window, pointer)?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "down" | "up" => {
                let v = nums();
                pointer = point(px(v[0]), px(v[1]));
                let position = pointer;
                cx.update_window(window, |_, window, cx| {
                    let event = if cmd == "down" {
                        MouseDownEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                            first_mouse: false,
                        }
                        .to_platform_input()
                    } else {
                        MouseUpEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                        }
                        .to_platform_input()
                    };
                    window.dispatch_event(event, cx);
                    window.render_frame(cx);
                })?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "drag-to" => {
                // Move with the button held.
                let v = nums();
                pointer = point(px(v[0]), px(v[1]));
                let position = pointer;
                cx.update_window(window, |_, window, cx| {
                    window.dispatch_event(
                        MouseMoveEvent {
                            position,
                            pressed_button: Some(MouseButton::Left),
                            modifiers: Default::default(),
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                })?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "press" => {
                cx.update_window(window, |_, window, cx| window.press(rest, cx))?;
                pump(&mut cx, Duration::from_millis(200));
            }
            // A key held down and let go later: what a *hold* is, and what
            // `press` (down and up in one breath) is not (§9.8).
            "keydown" => {
                let key = Keystroke::parse(rest)?;
                cx.update_window(window, |_, window, cx| {
                    window.dispatch_keystroke(key, cx);
                })?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "keyup" => {
                let key = Keystroke::parse(rest)?;
                cx.update_window(window, |_, window, cx| {
                    window.dispatch_event(KeyUpEvent { keystroke: key }.to_platform_input(), cx);
                })?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "input" => {
                cx.update_window(window, |_, window, cx| window.input(rest, cx))?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "pump" => pump(&mut cx, Duration::from_millis(nums()[0] as u64)),
            "activate" => {
                cx.update_window(window, |_, window, _cx| window.activate_window())?;
                // The platform's own report of it arrives on the foreground executor,
                // so it takes a pump to land: printing it is how a script sees that.
                pump(&mut cx, Duration::from_millis(200));
                cx.update_window(window, |_, window, _cx| {
                    println!("[probe]   active={}", window.is_window_active());
                })?;
            }
            "find" => {
                let id = element_id(rest);
                cx.update_window(window, |_, window, _| match window.try_find(id) {
                    Some(found) => println!(
                        "[probe]   {rest}: {:?} value={:?} focused={:?} visible={}",
                        found.bounds(),
                        found.value(),
                        found.focused(),
                        found.visible()
                    ),
                    None => println!("[probe]   {rest}: not found"),
                })?;
            }
            "focus" => {
                // The shown tab's primary control: its composer, or the New Swarm
                // page's first field — what switching to the tab does.
                let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
                cx.update_window(window, |_, window, cx| {
                    tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
                })?;
                pump(&mut cx, Duration::from_millis(100));
            }
            // `tap X Y`: a press and a release at a point, with no pump after
            // them. Every other clicking step waits 200ms, which is longer than
            // any of the design's transitions — so this is the one a script uses
            // when the frames just after the click are the point.
            "tap" => {
                let v = rest
                    .split_whitespace()
                    .filter_map(|word| word.parse().ok())
                    .collect::<Vec<f32>>();
                let position = point(px(v[0]), px(v[1]));
                pointer = position;
                watching = Some(Instant::now());
                cx.update_window(window, |_, window, cx| {
                    window.dispatch_event(
                        MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers: Default::default(),
                        }
                        .to_platform_input(),
                        cx,
                    );
                    for event in [
                        MouseDownEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                            first_mouse: false,
                        }
                        .to_platform_input(),
                        MouseUpEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                        }
                        .to_platform_input(),
                    ] {
                        window.dispatch_event(event, cx);
                    }
                    window.render_frame(cx);
                })?;
            }
            // `frames ID MS1 MS2 ...`: a frame at each offset from the step before
            // it, printing where `ID` is drawn in each, and how long it has really
            // been since the `tap` — what a transition in flight looks like, frame
            // by frame, at the ages it really had.
            "frames" => {
                let mut words = rest.split_whitespace();
                let id = element_id(words.next().unwrap_or_default());
                let times = words
                    .filter_map(|word| word.parse::<u64>().ok())
                    .collect::<Vec<_>>();
                let start = Instant::now();
                for ms in times {
                    let at = start + Duration::from_millis(ms);
                    while Instant::now() < at {
                        cx.run_until_parked();
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    let into_it = age(watching);
                    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
                    cx.update_window(window, |_, window, _| match window.try_find(id.clone()) {
                        Some(found) => {
                            let bounds = found.bounds();
                            let centre: f32 = bounds.center().x.into();
                            let width: f32 = bounds.size.width.into();
                            println!(
                                "[probe]   {id:?} at {ms}ms{into_it}: centre x {centre}, width {width}"
                            );
                        }
                        None => println!("[probe]   {id:?} at {ms}ms{into_it}: not found"),
                    })?;
                }
            }
            "loads" => {
                // The app's own background loads again — what startup runs, and
                // what `screens.rs` runs between states: the session index a tab
                // has just written is in the history rows only after this read.
                cx.update(evo_desktop::start_background_loads);
                pump(&mut cx, Duration::from_secs(2));
            }
            "launch" => {
                let tab = cx.update(|cx| view.read(cx).selected_tab().clone());
                let folder = folder.clone();
                cx.update(move |cx| {
                    tab.update(cx, |_tab, cx| {
                        cx.emit(TabContentEvent::Launch {
                            folder,
                            plan: session::LaunchPlan::default(),
                        })
                    })
                });
            }
            "wait-running" => {
                let deadline = Instant::now() + Duration::from_secs(90);
                loop {
                    let state =
                        cx.update(|cx| view.read(cx).selected_tab().read(cx).state().clone());
                    if !matches!(state, TabState::Empty | TabState::Booting { .. }) {
                        println!("[probe]   {state:?}");
                        break;
                    }
                    if Instant::now() > deadline {
                        return Err("the tab did not come up".into());
                    }
                    pump(&mut cx, Duration::from_millis(100));
                }
            }
            "quit" => {
                // What the menu item and a held ⌘Q both do: the app's own quit
                // sequence, which covers the window while the swarms stop (§9.8).
                cx.update(evo_desktop::begin_quit);
                pump(&mut cx, Duration::from_millis(200));
                let stopping = cx.update(|cx| view.read(cx).quitting_swarms());
                println!("[probe]   the quit is up for {stopping:?}");
            }
            "quit-wait" => {
                // Wait for every swarm the quit stopped to be gone — the engine's
                // thread over, which is what the process ends on (§9.8).
                let how_long = nums()
                    .first()
                    .map(|secs| Duration::from_secs_f32(*secs))
                    .unwrap_or(Duration::from_secs(25));
                let started = Instant::now();
                let mut left = 0;
                while started.elapsed() < how_long {
                    left = cx.update(|cx| {
                        view.read(cx)
                            .swarms(cx)
                            .iter()
                            .filter(|swarm| swarm.is_running())
                            .count()
                    });
                    if left == 0 {
                        break;
                    }
                    pump(&mut cx, Duration::from_millis(50));
                }
                println!(
                    "[probe]   quit: {left} swarm(s) still running after {:?}",
                    started.elapsed()
                );
                if left > 0 {
                    return Err("a swarm outlived the quit".into());
                }
            }
            other => return Err(format!("line {}: unknown step {other:?}", n + 1).into()),
        }
    }

    // Every swarm the window still has running is stopped the app's way: the pipe
    // closes, and the engine thread runs the rest of the ladder in its own time.
    let swarms = cx.update(|cx| view.read(cx).swarms(cx));
    for swarm in swarms {
        swarm.shutdown();
    }
    pump(&mut cx, Duration::from_secs(1));
    drop(fixture);
    Ok(())
}

fn element_id(text: &str) -> ElementId {
    match text.rsplit_once('#') {
        Some((name, n)) if n.parse::<u64>().is_ok() => {
            ElementId::NamedInteger(name.to_owned().into(), n.parse().unwrap())
        }
        _ => ElementId::Name(text.to_owned().into()),
    }
}

fn open(
    cx: &mut HeadlessAppContext,
    root: &AppRoot,
    bins: Binaries,
) -> Result<
    (
        AnyWindowHandle,
        Entity<WorkspaceView>,
        gpui_kit::Subscription,
    ),
    Error,
> {
    let root = root.clone();
    let (window, view, held) = cx.update(move |cx| {
        let log = AppLog::open(&root);
        let loaded = AppState::load(&root);
        let state = AppState {
            binaries: bins,
            theme: Theme::System,
            ..loaded
        };
        let cache = ModelCache::load(&root);
        Shell::new(root.clone(), log, state, cache).install(cx);
        evo_desktop::install_menus(cx);
        let config = Arc::new(evo_desktop::launch_env(cx));
        let (window, view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
                })),
                focus: true,
                show: false,
                ..workspace::window_options(cx)
            },
            cx,
            |window, cx| {
                let _appearance = evo_desktop::follow_appearance(cx, window);
                cx.new(|cx| WorkspaceView::with_config(config, window, cx))
            },
        )?;
        cx.update_global::<Shell, _>(|shell, _| shell.view = Some(view.downgrade()));
        // The app's own wiring for a held ⌘Q, so a script's key reaches the same
        // quit the real app runs (§9.8).
        let held = cx.subscribe(&view, |_, _: &workspace::QuitHeld, cx| {
            evo_desktop::begin_quit(cx)
        });
        Ok::<_, Error>((window, view, held))
    })?;
    Ok((window, view, held))
}

fn move_to(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    at: Point<gpui_kit::Pixels>,
) -> Result<(), Error> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })?;
    Ok(())
}

fn set_theme(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    mode: ThemeMode,
) -> Result<(), Error> {
    cx.update_window(window, |_, window, cx| {
        gpui_kit::component::Theme::change(mode, Some(window), cx);
    })?;
    pump(cx, Duration::from_millis(300));
    Ok(())
}

fn pump(cx: &mut HeadlessAppContext, how_long: Duration) {
    let deadline = Instant::now() + how_long;
    // The headless dispatcher keeps a virtual clock that only moves when told to,
    // so without this every animation (a slider's move, the split's pill, a
    // breathing dot) would be drawn at its first frame forever. Each tick moves
    // it by the same 16ms the thread sleeps, so it tracks wall time.
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(16));
        cx.background_executor
            .advance_clock(Duration::from_millis(16));
    }
}

fn save(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    path: &Path,
    crop: Option<(f32, f32, f32, f32, f32)>,
) -> Result<(), Error> {
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let image = cx.capture_screenshot(window)?;
    match crop {
        None => image.save(path)?,
        Some((x, y, w, h, scale)) => {
            let factor = image.width() as f32 / WINDOW_SIZE.0;
            let sub = image::imageops::crop_imm(
                &image,
                (x * factor) as u32,
                (y * factor) as u32,
                (w * factor) as u32,
                (h * factor) as u32,
            )
            .to_image();
            image::imageops::resize(
                &sub,
                (w * scale) as u32,
                (h * scale) as u32,
                image::imageops::FilterType::Nearest,
            )
            .save(path)?;
        }
    }
    println!("[probe]   -> {}", path.display());
    Ok(())
}
