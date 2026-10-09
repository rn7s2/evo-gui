//! §13, from the app's side: the Settings panel opens over the window, Save
//! persists what it hands over, and the *next* tab spawns with it.
//!
//! The panel's own rules — the probing, the reset, Escape, the live theme — are
//! `crates/settings`' tests. What this one is about is the app's half of the
//! contract: `SettingsEvent::Saved` reaching `app.json`, the [`Shell`], and the
//! window's next tab.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use evo_desktop::{open_settings_panel, AppLog, Shell};
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, Bounds, ElementId, Entity, Point,
    TestAppContext, WindowBounds, WindowOptions,
};
use session::LaunchPlan;
use settings::{
    Check, SettingsPanel, AGENT_PATH_ID, PANEL_ID, SAVE_ID, SWARM_CHOOSE_ID, SWARM_PATH_ID,
    TERMINAL_FONT_ID, TERMINAL_FONT_SIZE_ID, THEME_ID,
};
use store::app_state::{AppState, Binaries, Theme};
use store::model_cache::ModelCache;
use store::paths::Root as AppRoot;
use workspace::{LaunchEnv, TabContentEvent, TabState, WorkspaceView};

/// What the test saves: a swarm binary that is not there, so the next tab's launch
/// says which binary it could not run — and an agent path that is not there either.
const SAVED_SWARM: &str = "/nonexistent/evo-swarm-from-settings";
const SAVED_AGENT: &str = "/nonexistent/evo-agent-from-settings";
/// And the terminal's font, which is a value like the paths (§13): a family and the size it
/// draws at. The family is the design's own monospace — a family every machine either has or
/// is offered anyway — and the size is one the panel's own list of sizes carries.
const SAVED_FONT: &str = "Menlo";
const SAVED_FONT_SIZE: f32 = 16.;

/// How long a boot that cannot succeed is given to say so.
const WAIT: Duration = Duration::from_secs(60);

fn temp_root(name: &str) -> AppRoot {
    let dir = std::env::temp_dir().join(format!("evo-desktop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    AppRoot::at(dir)
}

/// A window on a temp app root and a `Shell` whose binaries and theme are the ones
/// the test starts from — `run`'s own wiring, minus the background loads.
fn open(
    cx: &mut TestAppContext,
    root: &AppRoot,
    binaries: Binaries,
    theme: Theme,
) -> (AnyWindowHandle, Entity<WorkspaceView>) {
    let log = AppLog::open(root);
    let root = root.clone();
    let (window, view) = cx
        .update(move |cx| {
            let state = AppState {
                binaries,
                theme,
                ..AppState::default()
            };
            Shell::new(root.clone(), log, state, ModelCache::default()).install(cx);
            // The same launch environment `run` builds, from the same binaries.
            let config = Arc::new(LaunchEnv {
                swarm_bin: cx.global::<Shell>().binaries.evo_swarm.clone(),
                agent_bin: cx.global::<Shell>().binaries.evo_agent.clone(),
                root,
                env: Vec::new(),
                env_remove: Vec::new(),
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
    cx.update(|cx| {
        cx.global_mut::<Shell>().view = Some(view.downgrade());
    });
    (window, view)
}

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
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How many frames a control is given to stop moving. Bounded, so a control that
/// never settles fails the test rather than hanging it.
const SETTLE_FRAMES: usize = 60;

/// How many frames in a row have to agree before the layout counts as settled.
const SETTLED: usize = 2;

/// Render frames until the element stops moving, and leave the last one rendered.
///
/// The harness looks a target's position up in one frame and lets the pointer land
/// in the next (`click_target`), so a layout that is still settling under the click
/// — a dialog's first frames, the panel's rows as their answers arrive, the theme
/// change re-resolving its fonts — can put the press on the dialog's backdrop
/// rather than on the control. The backdrop used to close the dialog without a
/// word, so the typed paths were gone and the assertions below saw the *defaults*:
/// one full-workspace run lost this test that way.
///
/// Frame-driven, not a sleep: the loop ends as soon as the frames agree, and the
/// gesture after it lands where the frame before it drew the control.
fn settle(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    let mut last = None;
    let mut agreed = 0;
    for _ in 0..SETTLE_FRAMES {
        let bounds = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.find(id.clone()).bounds()
            })
            .expect("the window");
        agreed = if last == Some(bounds) { agreed + 1 } else { 0 };
        if agreed >= SETTLED {
            return;
        }
        last = Some(bounds);
    }
    panic!("{id:?} is still moving after {SETTLE_FRAMES} frames");
}

/// Type a path into one of the panel's fields, the way a person does: click the
/// field, take everything, type over it.
fn type_into(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str, text: &str) {
    settle(cx, window, id);
    cx.update_window(window, |_, window, cx| {
        window.click(id, cx);
        window.press("cmd-a", cx);
        window.input(text, cx);
        window.render_frame(cx);
    })
    .expect("typing into the panel");
}

/// Choose the terminal's font the way the panel's own rows do: its family and its size,
/// through the panel's own API. The dropdown popups themselves are the kit's machinery and
/// are not drivable in this harness — a bare kit `Select` in a bare window does not open its
/// menu from a click either — so what this drives is the choice landing in the panel, which
/// is what the app's half takes from here (§13).
fn choose_font(cx: &mut TestAppContext, window: AnyWindowHandle, panel: &Entity<SettingsPanel>) {
    cx.update_window(window, |_, window, cx| {
        panel.update(cx, |panel, cx| {
            panel.choose_font(SAVED_FONT, SAVED_FONT_SIZE, window, cx)
        });
        window.render_frame(cx);
    })
    .expect("choosing a font");
}

/// Choose one of the three theme choices by clicking its third of the row — the
/// same gesture the settings crate's own capture uses.
fn choose_theme(cx: &mut TestAppContext, window: AnyWindowHandle, fraction: f32) {
    settle(cx, window, THEME_ID);
    cx.update_window(window, |_, window, cx| {
        let row = window.find(THEME_ID).bounds().size;
        window.click_at(THEME_ID, point(row.width * fraction, row.height / 2.), cx);
        window.render_frame(cx);
    })
    .expect("choosing a theme");
}

#[gpui_kit::test]
fn saving_settings_persists_them_and_the_next_tab_spawns_with_them(cx: &mut TestAppContext) {
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    let root = temp_root("settings");
    let (window, view) = open(
        cx,
        &root,
        Binaries {
            evo_swarm: PathBuf::from("/usr/local/bin/evo-swarm"),
            evo_agent: PathBuf::from("/usr/local/bin/evo-agent"),
        },
        Theme::System,
    );

    // What the Settings… menu item does with the window it is handed.
    // The entity itself is not needed below: everything under test shows up in
    // `app.json`, on the `Shell`, and in the next tab's failure.
    let panel: Entity<SettingsPanel> = cx
        .update_window(window, |_, window, cx| open_settings_panel(window, cx))
        .expect("the settings panel");
    assert!(
        cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "the panel is up, over the app's window"
    );

    // The panel's path checks answer from a fixed table rather than by running a
    // process: what this test is about is where Save puts the values, and a probe
    // would be a second, slower subject (`crates/settings`' own tests hold the
    // probing to account). It is also what keeps the panel's rows — and so the
    // footer the Save button sits in — from changing height under the clicks
    // below, as answers arrive.
    panel.update(cx, |panel, cx| {
        panel.set_verdicts(
            [
                (PathBuf::from(SAVED_SWARM), Check::Missing),
                (PathBuf::from(SAVED_AGENT), Check::Missing),
            ],
            cx,
        )
    });

    // The panel fits the dialog it is drawn in. The kit's dialog pads its content by
    // 16 pt on each side: left in place, the 560 pt panel was 32 pt wider than the box
    // it was drawn in, and the right-hand controls — `Choose…`, `Save` — were clipped
    // at the dialog's edge. The dialog's own bounds are its surface, which the kit
    // gives the layer's index as an element id.
    let (slot, save, choose, inner) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find(evo_desktop::DIALOG_CONTENT_ID).bounds(),
                window.find(SAVE_ID).bounds(),
                window.find(SWARM_CHOOSE_ID).bounds(),
                window.find(PANEL_ID).bounds(),
            )
        })
        .expect("the dialog is drawn");
    assert!(
        save.left() >= slot.left() && save.right() <= slot.right(),
        "Save is inside the dialog's content box, not clipped by it: {save:?} in {slot:?}"
    );
    assert!(
        choose.left() >= slot.left() && choose.right() <= slot.right(),
        "and so is a row's Choose… button: {choose:?} in {slot:?}"
    );
    assert!(
        inner.left() >= slot.left() && inner.right() <= slot.right(),
        "the panel is inside it too, rather than 32 pt wider: {inner:?} in {slot:?}"
    );
    assert_eq!(
        inner.size.width, slot.size.width,
        "the embedded panel takes the whole box the dialog gives it"
    );

    // A person's edits: two paths typed over, a font family and a size chosen from the
    // dropdowns, and Dark clicked.
    type_into(cx, window, SWARM_PATH_ID, SAVED_SWARM);
    type_into(cx, window, AGENT_PATH_ID, SAVED_AGENT);
    choose_font(cx, window, &panel);
    choose_theme(cx, window, 5. / 6.);

    // What the panel will hand over, checked before Save: a gesture that missed —
    // a lost keystroke, a click that landed beside the field — is named here, at
    // the step that lost it, rather than three assertions later as a value that
    // never changed.
    let typed = cx.update(|cx| panel.read(cx).values(cx));
    assert_eq!(
        typed.evo_swarm,
        PathBuf::from(SAVED_SWARM),
        "the typed swarm path is what the panel holds"
    );
    assert_eq!(typed.evo_agent, PathBuf::from(SAVED_AGENT));
    assert_eq!(typed.theme, Theme::Dark, "and the theme that was clicked");
    assert_eq!(
        typed.terminal_font, SAVED_FONT,
        "and the font family that was chosen"
    );
    assert_eq!(
        typed.terminal_font_size, SAVED_FONT_SIZE,
        "and the size that was chosen"
    );

    // The two choosers are drawn inside the dialog and are showing what was chosen: a row
    // clipped by the dialog's edge would be a value nobody could see or correct.
    let (family, size_field) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find(TERMINAL_FONT_ID),
                window.find(TERMINAL_FONT_SIZE_ID),
            )
        })
        .expect("the dialog is drawn");
    assert_eq!(
        family.value(),
        Some(SAVED_FONT),
        "the family chooser shows the chosen family"
    );
    assert_eq!(
        size_field.value(),
        Some("16"),
        "and the one beside it the chosen size"
    );
    for (what, bounds) in [("family", family.bounds()), ("size", size_field.bounds())] {
        assert!(
            bounds.left() >= slot.left() && bounds.right() <= slot.right(),
            "the {what} chooser is inside the dialog's box, not clipped by it: {bounds:?} in {slot:?}"
        );
    }

    settle(cx, window, SAVE_ID);
    cx.update_window(window, |_, window, cx| {
        window.click(SAVE_ID, cx);
        window.render_frame(cx);
    })
    .expect("Save");
    assert!(
        !cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
            .unwrap(),
        "Save closes the panel (the panel's own dismissal)"
    );

    // What was saved is in `app.json`...
    let state = AppState::load(&root);
    assert_eq!(
        state.binaries,
        Binaries {
            evo_swarm: PathBuf::from(SAVED_SWARM),
            evo_agent: PathBuf::from(SAVED_AGENT),
        },
        "both paths were persisted"
    );
    assert_eq!(state.theme, Theme::Dark, "and the theme that was chosen");
    assert_eq!(
        state.terminal_font, SAVED_FONT,
        "and the terminal's font: the panel's value is `app.json`'s"
    );
    assert_eq!(
        state.terminal_font_size, SAVED_FONT_SIZE,
        "and the size beside it"
    );
    // ...and on the Shell, which is what a launch and the About dialog read.
    let (binaries, theme, font) = cx.update(|cx| {
        let shell = cx.global::<Shell>();
        (
            shell.binaries.clone(),
            shell.theme,
            (shell.terminal_font.clone(), shell.terminal_font_size),
        )
    });
    assert_eq!(binaries.evo_swarm, PathBuf::from(SAVED_SWARM));
    assert_eq!(binaries.evo_agent, PathBuf::from(SAVED_AGENT));
    assert_eq!(theme, Theme::Dark);
    assert_eq!(
        font,
        (SAVED_FONT.to_owned(), SAVED_FONT_SIZE),
        "and the Settings panel reopens on the font"
    );
    // ...and in the global every open terminal draws with (§13): a pane that was already up
    // follows the choice without being reopened.
    let drawing = cx.update(|cx| cx.global::<terminal::TerminalFont>().clone());
    assert_eq!(drawing.family.as_ref(), SAVED_FONT);
    assert_eq!(drawing.size, SAVED_FONT_SIZE);

    // The window's *next* tab spawns with them: the swarm binary it was started
    // with is the one Settings saved, and that is what its failure names.
    let folder = root.path().join("project");
    std::fs::create_dir_all(&folder).expect("a folder to start in");
    let tab = cx
        .update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.open_empty_tab(window, cx))
        })
        .expect("a new tab");
    cx.update(|cx| {
        tab.update(cx, |_tab, cx| {
            cx.emit(TabContentEvent::Launch {
                folder,
                plan: LaunchPlan::default(),
            })
        });
    });
    wait_for(cx, "the new tab's failure", |cx| {
        cx.update(|cx| matches!(tab.read(cx).state(), TabState::Failed { .. }))
    });
    let TabState::Failed { message, .. } = cx.update(|cx| tab.read(cx).state().clone()) else {
        unreachable!("just matched")
    };
    assert!(
        message
            .as_deref()
            .is_some_and(|reason| reason.contains(SAVED_SWARM)),
        "the new tab ran the binary Settings saved: {message:?}"
    );

    let _ = std::fs::remove_dir_all(root.path());
}
