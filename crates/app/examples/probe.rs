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
//! crop NAME X Y W H [scale]      picture of a region, scaled up (default 2)
//! theme light|dark               switch the theme
//! click ID | dclick ID | hover ID
//! at X Y                          move the pointer to window coordinates
//! down X Y | up X Y               press / release the left button there
//! press KEY | input TEXT          keyboard
//! pump MS                         let the app run
//! find ID                         print the element's bounds
//! launch                          launch the shown tab in the world's folder
//! wait-running                    wait for the shown tab to be running
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
    Entity, HeadlessAppContext, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Point, WindowBounds, WindowOptions,
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
    let (window, view) = open(&mut cx, &root, bins)?;
    cx.update(evo_desktop::start_background_loads);
    pump(&mut cx, Duration::from_secs(4));

    let mut mode = ThemeMode::Light;
    set_theme(&mut cx, window, mode)?;
    let mut pointer = Point::default();
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
            "input" => {
                cx.update_window(window, |_, window, cx| window.input(rest, cx))?;
                pump(&mut cx, Duration::from_millis(200));
            }
            "pump" => pump(&mut cx, Duration::from_millis(nums()[0] as u64)),
            "find" => {
                let id = element_id(rest);
                cx.update_window(window, |_, window, _| match window.try_find(id) {
                    Some(found) => println!(
                        "[probe]   {rest}: {:?} value={:?} focused={:?}",
                        found.bounds(),
                        found.value(),
                        found.focused()
                    ),
                    None => println!("[probe]   {rest}: not found"),
                })?;
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
            other => return Err(format!("line {}: unknown step {other:?}", n + 1).into()),
        }
    }

    let engines = cx.update(|cx| view.update(cx, |view, cx| view.take_engines(cx)));
    for mut engine in engines {
        engine.shutdown();
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
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Error> {
    let root = root.clone();
    let (window, view) = cx.update(move |cx| {
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
        Ok::<_, Error>((window, view))
    })?;
    Ok((window, view))
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
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(16));
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
    image.save(path)?;
    if let Some((x, y, w, h, scale)) = crop {
        let factor = image.width() as f32 / WINDOW_SIZE.0;
        let p = path.display().to_string();
        let ok = std::process::Command::new("sips")
            .args([
                "-c",
                &((h * factor) as u32).to_string(),
                &((w * factor) as u32).to_string(),
            ])
            .args([
                "--cropOffset",
                &((y * factor) as u32).to_string(),
                &((x * factor) as u32).to_string(),
            ])
            .arg(&p)
            .output()?;
        if !ok.status.success() {
            return Err("sips crop failed".into());
        }
        std::process::Command::new("sips")
            .args(["--resampleWidth", &((w * scale) as u32).to_string(), &p])
            .output()?;
    }
    println!("[probe]   -> {}", path.display());
    Ok(())
}
