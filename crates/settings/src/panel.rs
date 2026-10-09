//! The Settings panel (§13): the two binaries this app spawns, and the theme. v1 has
//! nothing else to configure, so nothing else is here.
//!
//! ```text
//! new(values, window, cx)          the app's current state — `store::Binaries` + `Theme`
//! embedded(true)                   the host draws the frame: no border, fill or padding of
//!                                  our own, and its width rather than `PANEL_SIZE`
//! set_verdicts                     tests and captures: answer paths without a process
//! focus_into                       where the keyboard lands when the dialog opens
//! → SettingsEvent::Saved(values)   Save, followed by DismissEvent
//! → DismissEvent                   Cancel (and Escape): nothing was applied, so nothing
//!                                  is thrown away
//! ```
//!
//! A path is checked by running it (`probe`), never on the thread that draws: the row says
//! `checking…` at once, the app's executor runs `<path> --version`, and the answer comes back
//! to the panel through a weak entity. Answers are numbered per row, so the answer that lands
//! is the answer to the text as it is *then* — type fast and the stale probes are dropped
//! before they start a process.
//!
//! The theme is the one thing applied as it is chosen: picking Light or Dark repaints the
//! window behind the dialog, which is the only way to judge it. Cancel puts back the mode
//! the panel opened in; Save leaves it and hands the choice to the app, which persists it.
//! The paths are not applied anywhere — they are for the next tab to spawn with.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, StyledExt as _, Theme as ComponentTheme,
    ThemeMode,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable as _, IntoElement, KeyDownEvent, Render, Role, SharedString, Subscription,
    TestSupportExt as _, WeakEntity, Window,
};
use store::{Binaries, Theme as StoredTheme};

use crate::appearance::mode_for;
use crate::probe::{probe, Check};

/// The size the standalone panel draws itself at — the demo, a capture — and the height
/// floor of a hosted one, which takes its host's width instead
/// ([`SettingsPanel::embedded`]).
pub const PANEL_SIZE: (f32, f32) = (560., 420.);

/// The panel's own element id, and the ids of everything in it, so the app's tests (and
/// this crate's captures) can find a control rather than a position.
pub const PANEL_ID: &str = "settings";
pub const SWARM_PATH_ID: &str = "settings-evo-swarm-path";
pub const SWARM_CHOOSE_ID: &str = "settings-evo-swarm-choose";
pub const SWARM_STATUS_ID: &str = "settings-evo-swarm-status";
pub const AGENT_PATH_ID: &str = "settings-evo-agent-path";
pub const AGENT_CHOOSE_ID: &str = "settings-evo-agent-choose";
pub const AGENT_STATUS_ID: &str = "settings-evo-agent-status";
pub const THEME_ID: &str = "settings-theme";
pub const THEME_CHOICES_ID: &str = "settings-theme-choices";
pub const TERMINAL_FONT_ID: &str = "settings-terminal-font";
pub const RESET_ID: &str = "settings-reset";
pub const CANCEL_ID: &str = "settings-cancel";
pub const SAVE_ID: &str = "settings-save";

/// The row labels' column: wide enough for `evo-swarm`, which is the longest of them.
const LABEL_WIDTH: gpui_kit::Pixels = px(96.);

/// The gap between a label column and its control, and the height that centers a label
/// against the field beside it.
const LABEL_GAP: gpui_kit::Pixels = px(12.);
const FIELD_HEIGHT: gpui_kit::Pixels = px(38.);

/// The dialog's small type: the status lines and the note, one line each. The size is the
/// one the lane list uses for its own small type, and the leading is tighter than the default
/// — room *between* the dialog's rows is worth more than leading inside them.
const SMALL_TEXT: gpui_kit::Pixels = px(12.);
const LINE_HEIGHT: f32 = 1.2;

/// The path field's focus edge, the same one the composer's input wears (§7.3): one hairline
/// in the focus colour while the caret is in the field, and a soft wash around it. The input
/// itself draws neither, so the app's two text controls mark focus the same way.
const FIELD_RING: gpui_kit::Pixels = px(3.);
const FIELD_RING_INK: f32 = 0.12;

/// How long a path field waits before a process is started for it. Typing a path is a burst
/// of keystrokes; only the last of them is worth a `--version`, and this is the pause that
/// tells them apart.
const TYPING_PAUSE: Duration = Duration::from_millis(250);

const TITLE: &str = "Settings";
const SUBTITLE: &str = "The two binaries the app spawns, the app's theme, and the terminal's font.";
const THEME_LABEL: &str = "Theme";
const TERMINAL_FONT_LABEL: &str = "Terminal Font";
const CHOOSE_LABEL: &str = "Choose…";
const RESET_LABEL: &str = "Reset to defaults";
const CANCEL_LABEL: &str = "Cancel";
const SAVE_LABEL: &str = "Save";
/// The note that keeps a running tab from looking broken when the paths change (§13).
const NOTE: &str = "New tabs use these; a tab that is already running keeps what it started with.";

/// The three themes, in the order the segmented row shows them.
const THEMES: [StoredTheme; 3] = [StoredTheme::System, StoredTheme::Light, StoredTheme::Dark];

/// What the panel edits, and what `Save` hands over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsValues {
    pub evo_swarm: PathBuf,
    pub evo_agent: PathBuf,
    pub theme: StoredTheme,
    /// The terminal pane's font family.
    pub terminal_font: String,
}

impl SettingsValues {
    /// The app's current state, as the panel opens on it.
    pub fn from_state(
        binaries: &Binaries,
        theme: StoredTheme,
        terminal_font: String,
    ) -> SettingsValues {
        SettingsValues {
            evo_swarm: binaries.evo_swarm.clone(),
            evo_agent: binaries.evo_agent.clone(),
            theme,
            terminal_font,
        }
    }

    /// The two paths in the shape `store` persists them.
    pub fn binaries(&self) -> Binaries {
        Binaries {
            evo_swarm: self.evo_swarm.clone(),
            evo_agent: self.evo_agent.clone(),
        }
    }
}

impl Default for SettingsValues {
    fn default() -> SettingsValues {
        SettingsValues::from_state(
            &Binaries::default(),
            StoredTheme::System,
            store::design::MONO_FONT.to_owned(),
        )
    }
}

/// What the panel tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEvent {
    /// `Save`: persist these, and spawn the next tab with them. A [`DismissEvent`] follows,
    /// so the dialog closes on the same path Cancel takes.
    Saved(SettingsValues),
}

/// The two rows, in the order they are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binary {
    Swarm,
    Agent,
}

impl Binary {
    const ALL: [Binary; 2] = [Binary::Swarm, Binary::Agent];

    /// The name the binary introduces itself with: what the row is called, and what
    /// `--version` has to lead with.
    fn name(self) -> &'static str {
        match self {
            Binary::Swarm => "evo-swarm",
            Binary::Agent => "evo-agent",
        }
    }

    fn ix(self) -> usize {
        match self {
            Binary::Swarm => 0,
            Binary::Agent => 1,
        }
    }

    fn path_id(self) -> &'static str {
        match self {
            Binary::Swarm => SWARM_PATH_ID,
            Binary::Agent => AGENT_PATH_ID,
        }
    }

    fn choose_id(self) -> &'static str {
        match self {
            Binary::Swarm => SWARM_CHOOSE_ID,
            Binary::Agent => AGENT_CHOOSE_ID,
        }
    }

    fn status_id(self) -> &'static str {
        match self {
            Binary::Swarm => SWARM_STATUS_ID,
            Binary::Agent => AGENT_STATUS_ID,
        }
    }
}

/// Where a verdict comes from: the binary itself, or an answer already known.
#[derive(Clone, Default)]
enum Prober {
    /// Run it: `probe`, on the app's own executor.
    #[default]
    Command,
    /// Answer from this table, with nothing at a path being "not found". For tests and
    /// captures, which must not wait on a process.
    Fixed(Rc<Vec<(PathBuf, Check)>>),
}

/// Where a chosen path comes from: the system's file dialog, or a test's answer.
#[derive(Clone, Default)]
enum Picker {
    #[default]
    Dialog,
    /// What the dialog would have returned; `None` is a dialog that was cancelled.
    Fixed(Option<PathBuf>),
}

/// The two binary paths, the theme and the terminal's font, as a view (§13).
pub struct SettingsPanel {
    /// The two fields, in [`Binary`] order.
    fields: [Entity<InputState>; 2],
    /// What the row under each field says.
    checks: [Check; 2],
    /// One counter per row: an answer is only taken if it is the answer to the newest
    /// question that row has asked.
    revisions: [u64; 2],
    /// The terminal pane's font family. A draft like the paths: it takes effect when
    /// the app saves it, not as it is typed.
    terminal_font: Entity<InputState>,
    /// The choice being shown. Unlike the paths, it is applied as it is chosen.
    theme: StoredTheme,
    /// The mode in force when the panel opened — what Cancel puts back.
    opened_mode: ThemeMode,
    /// Whether the panel draws its own frame. See [`SettingsPanel::embedded`].
    embedded: bool,
    prober: Prober,
    picker: Picker,
    /// The theme row's focus, for the arrows and Escape.
    focus: FocusHandle,
    /// Kept for as long as the panel lives: dropping them unsubscribes.
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    /// A panel open on `values`, with both paths already being checked.
    pub fn new(
        values: SettingsValues,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SettingsPanel {
        let fields = [&values.evo_swarm, &values.evo_agent].map(|path| {
            let text = path.display().to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(text.clone())
                    .placeholder("path to the binary")
            })
        });

        let subscriptions = Binary::ALL
            .into_iter()
            .map(|binary| {
                cx.subscribe(
                    &fields[binary.ix()],
                    move |panel, _, event: &InputEvent, cx| {
                        // `set_value` does not emit this, so the panel's own edits (Choose…, a
                        // reset, a test setting the field) ask for their own check instead.
                        if matches!(event, InputEvent::Change) {
                            panel.check(binary, TYPING_PAUSE, cx);
                        }
                    },
                )
            })
            .collect();

        // The font is not a path: nothing is run for it and nothing is checked, so it
        // has no subscription to keep.
        let font = values.terminal_font.clone();
        let terminal_font = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(font)
                .placeholder("font family name")
        });

        let mut panel = SettingsPanel {
            fields,
            checks: [Check::Checking, Check::Checking],
            revisions: [0, 0],
            terminal_font,
            theme: values.theme,
            // Read now, before anything is previewed: this is what Cancel puts back.
            opened_mode: cx.theme().mode,
            embedded: false,
            prober: Prober::default(),
            picker: Picker::default(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        for binary in Binary::ALL {
            panel.check(binary, Duration::ZERO, cx);
        }
        panel
    }

    /// The panel for a host that draws the frame itself, which is how the app shows it: the
    /// panel goes inside a gpui-kit `Dialog` (`crates/app/src/settings.rs`), and the dialog
    /// already draws a frame, a fill and padding of its own. With `embedded(true)` the panel
    /// draws none of its own — no border, no background, no padding, no corner — and takes
    /// the width the host gives it instead of advertising [`PANEL_SIZE`]. Two frames read as
    /// a doubled border, and the inner one clips the rows the outer one has already made
    /// room for.
    ///
    /// Inside, nothing changes: the same rows, the same footer, the same keys. The default is
    /// the standalone panel, whose own frame is what the demo and a capture show.
    pub fn embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }

    /// What the fields say now.
    pub fn values(&self, cx: &App) -> SettingsValues {
        SettingsValues {
            evo_swarm: self.path(Binary::Swarm, cx),
            evo_agent: self.path(Binary::Agent, cx),
            theme: self.theme,
            terminal_font: self.terminal_font.read(cx).value().to_string(),
        }
    }

    /// Answer a path from this table instead of running it, and answer a path with nothing
    /// at it as [`Check::Missing`]. For tests and captures, which must not wait on a
    /// process: the answers can still be the real ones, read with [`probe`] up front.
    pub fn set_verdicts(
        &mut self,
        verdicts: impl IntoIterator<Item = (PathBuf, Check)>,
        cx: &mut Context<Self>,
    ) {
        self.prober = Prober::Fixed(Rc::new(verdicts.into_iter().collect()));
        for binary in Binary::ALL {
            self.check(binary, Duration::ZERO, cx);
        }
    }

    /// Where the keyboard starts: the swarm path, the first thing the panel is for.
    pub fn focus_into(&self, window: &mut Window, cx: &mut App) {
        let handle = self.fields[Binary::Swarm.ix()].read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// A file pick a test injects, in place of the platform dialog. `None` is a dialog that
    /// was cancelled, which leaves the field as it was. Test-only, hence the allow.
    #[allow(dead_code)]
    fn set_chooser(&mut self, answer: Option<PathBuf>, cx: &mut Context<Self>) {
        self.picker = Picker::Fixed(answer);
        cx.notify();
    }

    /// The field's text as a path. A text field cannot hold a path that is not UTF-8; one
    /// that was read from disk is shown, and saved back, as it was written.
    fn path(&self, binary: Binary, cx: &App) -> PathBuf {
        PathBuf::from(self.fields[binary.ix()].read(cx).value().to_string())
    }

    /// Put a path in the field, the way a chosen file or a reset does.
    fn set_path(
        &mut self,
        binary: Binary,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = path.display().to_string();
        self.fields[binary.ix()].update(cx, |field, cx| {
            field.set_value(text, window, cx);
        });
    }

    /// Ask what this row's path is, `pause` after the question is posed.
    ///
    /// The row says `checking…` at once and the process runs on the app's own executor, never
    /// on the thread that draws; `pause` is what keeps a typed path from being run once per
    /// keystroke.
    fn check(&mut self, binary: Binary, pause: Duration, cx: &mut Context<Self>) {
        let ix = binary.ix();
        let path = self.path(binary, cx);
        self.checks[ix] = if path.as_os_str().is_empty() {
            Check::Empty
        } else {
            Check::Checking
        };
        self.revisions[ix] += 1;
        let revision = self.revisions[ix];
        cx.notify();
        if matches!(self.checks[ix], Check::Empty) {
            // Nothing to run, and nothing to wait for.
            return;
        }

        // An answer already known: no process, no executor, no wait.
        if let Prober::Fixed(verdicts) = self.prober.clone() {
            let answer = verdicts
                .iter()
                .find(|(asked, _)| *asked == path)
                .map(|(_, answer)| answer.clone())
                .unwrap_or(Check::Missing);
            self.settle(binary, &path, answer, revision, cx);
            return;
        }

        let name = binary.name().to_owned();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            if !pause.is_zero() {
                cx.background_executor().timer(pause).await;
            }
            // A newer question has been asked in the meantime: this one's answer is not
            // wanted, and its process is not worth starting.
            if this.update(cx, |panel, _| panel.revisions[ix]).ok() != Some(revision) {
                return;
            }
            // A process, but not on the thread that draws: the app's own executor runs it,
            // and the row keeps saying `checking…` until this task comes back with it.
            let asked = path.clone();
            let answer = cx
                .background_executor()
                .spawn(async move { probe(&asked, &name) })
                .await;
            let _ = this.update(cx, |panel, cx| {
                panel.settle(binary, &path, answer, revision, cx)
            });
        })
        .detach();
    }

    /// Take an answer, if it is still the answer to the question the field is asking.
    fn settle(
        &mut self,
        binary: Binary,
        asked: &Path,
        answer: Check,
        revision: u64,
        cx: &mut Context<Self>,
    ) {
        if self.revisions[binary.ix()] != revision || self.path(binary, cx) != asked {
            // The field moved on while the binary was being asked.
            return;
        }
        self.checks[binary.ix()] = answer;
        cx.notify();
    }

    /// Ask for a file, then put it in the field and check it.
    ///
    /// The dialog is `rfd`'s async one: the UI thread neither blocks on it nor waits for
    /// it, and cancelling it leaves the path as it was.
    fn choose(&mut self, binary: Binary, window: &mut Window, cx: &mut Context<Self>) {
        let answer = match self.picker.clone() {
            Picker::Fixed(answer) => answer,
            Picker::Dialog => {
                let current = self.path(binary, cx);
                let handle = window.window_handle();
                let title = format!("Where is the {} binary?", binary.name());
                cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
                    // Open where the binary is, so a sibling is one click away.
                    if let Some(folder) = current.parent().filter(|folder| folder.is_dir()) {
                        dialog = dialog.set_directory(folder);
                    }
                    let Some(picked) = dialog.pick_file().await else {
                        return;
                    };
                    let path = picked.path().to_path_buf();
                    let _ = cx.update_window(handle, |_, window, cx| {
                        let _ = this
                            .update(cx, |panel, cx| panel.take_picked(binary, path, window, cx));
                    });
                })
                .detach();
                return;
            }
        };
        if let Some(path) = answer {
            self.take_picked(binary, path, window, cx);
        }
    }

    /// A path that was chosen or reset: show it, and check it now rather than after a pause.
    fn take_picked(
        &mut self,
        binary: Binary,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_path(binary, &path, window, cx);
        self.check(binary, Duration::ZERO, cx);
    }

    /// Save: hand the values over, then close. The app persists them and uses them for the
    /// next tab (§13).
    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let values = self.values(cx);
        cx.emit(SettingsEvent::Saved(values));
        cx.emit(DismissEvent);
    }

    /// Cancel: nothing was applied but the theme, so the theme is all that goes back.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        ComponentTheme::change(self.opened_mode, Some(window), cx);
        cx.emit(DismissEvent);
    }

    /// The defaults, as §13 names them: `/usr/local/bin/evo-{swarm,agent}`, System, and
    /// the design's own monospace.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let binaries = Binaries::default();
        self.take_picked(Binary::Swarm, binaries.evo_swarm, window, cx);
        self.take_picked(Binary::Agent, binaries.evo_agent, window, cx);
        self.choose_theme(StoredTheme::System, window, cx);
        self.terminal_font.update(cx, |field, cx| {
            field.set_value(store::design::MONO_FONT, window, cx);
        });
        cx.notify();
    }

    /// Choose a theme — the one thing that takes effect before Save, because a theme is
    /// judged by looking at it.
    fn choose_theme(&mut self, theme: StoredTheme, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = theme;
        ComponentTheme::change(self.applied_mode(theme, window), Some(window), cx);
        cx.notify();
    }

    /// The mode a choice means *here*, on this window.
    fn applied_mode(&self, theme: StoredTheme, window: &Window) -> ThemeMode {
        mode_for(theme, window.appearance())
    }

    /// The panel's own keys: Escape leaves without saving, as Cancel does.
    fn panel_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        // The panel is a dialog: Escape is answered here rather than by whatever the app
        // would do with it, so one press is one dismissal.
        cx.stop_propagation();
        self.cancel(window, cx);
    }

    /// The theme row's keys: the arrows walk the three choices, as a segmented control does.
    fn theme_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let current = THEMES
            .iter()
            .position(|theme| *theme == self.theme)
            .unwrap_or(0);
        let next = match event.keystroke.key.as_str() {
            "left" | "up" => current.checked_sub(1).unwrap_or(THEMES.len() - 1),
            "right" | "down" => (current + 1) % THEMES.len(),
            "home" => 0,
            "end" => THEMES.len() - 1,
            _ => return,
        };
        cx.stop_propagation();
        self.choose_theme(THEMES[next], window, cx);
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_1()
            .child(div().text_lg().font_semibold().child(TITLE))
            .child(
                div()
                    .text_sm()
                    .line_height(relative(LINE_HEIGHT))
                    .text_color(muted)
                    .child(SUBTITLE),
            )
    }

    /// One binary: what it is called, the path, the button that fills it, and what the path
    /// turns out to be — the label column and the field row the empty tab's choosers use
    /// (§7.2), so a second dialog in this app looks like the first.
    fn binary_row(
        &self,
        binary: Binary,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let field = self.fields[binary.ix()].clone();
        let check = self.checks[binary.ix()].clone();
        let name = binary.name();
        // The caret is what "focused" means for a field: the edge belongs to the card around
        // it, which the input does not own.
        let focused = field
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window);
        let (radius, ring) = (cx.theme().radius, cx.theme().ring);
        let edge = if focused { ring } else { cx.theme().border };
        let field_bg = cx.theme().input_background();
        // The row's verdict belongs in what the field says it is: the label is what a screen
        // reader reads when the field takes the keyboard, and the line below is only paint.
        let label = if check.is_problem() {
            SharedString::from(format!("{name} path, {}", check.text()))
        } else {
            SharedString::from(format!("{name} path"))
        };
        let status_color = if check.is_problem() {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        h_flex()
            .items_start()
            .gap(LABEL_GAP)
            .child(row_label(name, Some(FIELD_HEIGHT)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .rounded(radius + FIELD_RING)
                                    .p(FIELD_RING)
                                    .when(focused, |this| this.bg(ring.alpha(FIELD_RING_INK)))
                                    .child(
                                        div()
                                            .rounded(radius)
                                            .border_1()
                                            .border_color(edge)
                                            .bg(field_bg)
                                            .child(
                                                Input::new(&field)
                                                    .id(binary.path_id())
                                                    .aria_label(label)
                                                    .appearance(false)
                                                    .bordered(false)
                                                    .focus_bordered(false),
                                            ),
                                    ),
                            )
                            .child(
                                Button::new(binary.choose_id())
                                    .label(CHOOSE_LABEL)
                                    .on_click(cx.listener(move |panel, _, window, cx| {
                                        panel.choose(binary, window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id(binary.status_id())
                            .test_support()
                            .text_xs()
                            .text_color(status_color)
                            .child(check.text().to_owned()),
                    ),
            )
    }

    /// The theme, as three choices in a row: what the app draws with, from the next tab on.
    fn theme_row(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = THEMES
            .iter()
            .position(|theme| *theme == self.theme)
            .unwrap_or(0);
        // The row is a control of its own — the arrows walk the choices — so it shows the
        // keyboard's focus: which is the row's own handle when a click put it there, and a
        // choice inside the bar when Tab walked in.
        let ring = if self.focus.contains_focused(window, cx) {
            cx.theme().ring
        } else {
            cx.theme().transparent
        };
        h_flex()
            // The theme row's control is the bar itself, so its label centers against that
            // rather than against the height of a path field's card.
            .items_center()
            .gap(LABEL_GAP)
            .child(row_label(THEME_LABEL, None))
            .child(
                div()
                    .id(THEME_ID)
                    .test_support()
                    .track_focus(&self.focus)
                    .tab_stop(true)
                    .on_key_down(cx.listener(Self::theme_key))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(ring)
                    .role(Role::Group)
                    .aria_label(THEME_LABEL)
                    .child(
                        TabBar::new(THEME_CHOICES_ID)
                            .segmented()
                            .selected_index(selected)
                            .on_click(cx.listener(|panel, index: &usize, window, cx| {
                                // A click on a choice is also what hands the row the
                                // keyboard, so the arrows work from wherever the pointer
                                // landed.
                                panel.focus.focus(window, cx);
                                let theme = THEMES[(*index).min(THEMES.len() - 1)];
                                panel.choose_theme(theme, window, cx);
                            }))
                            .children(
                                THEMES
                                    .iter()
                                    .map(|theme| Tab::new().label(theme_label(*theme)))
                                    .collect::<Vec<_>>(),
                            ),
                    ),
            )
    }

    /// The terminal pane's font: a family name, typed. Nothing is run for it and
    /// nothing is checked — the pane draws with it from the next save on — so the row
    /// is the field and its label, where a path row is a field, a chooser and a
    /// verdict.
    fn terminal_font_row(&self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let radius = cx.theme().radius;
        let field_bg = cx.theme().input_background();
        h_flex()
            .items_center()
            .gap(LABEL_GAP)
            .child(row_label(TERMINAL_FONT_LABEL, Some(FIELD_HEIGHT)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(FIELD_HEIGHT)
                    .flex()
                    .items_center()
                    .rounded(radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(field_bg)
                    .child(
                        Input::new(&self.terminal_font)
                            .id(TERMINAL_FONT_ID)
                            .aria_label(TERMINAL_FONT_LABEL)
                            .w_full()
                            .cleanable(true)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
    }

    /// The note that says what a change does not do: a running tab keeps its own.
    fn note(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .text_size(SMALL_TEXT)
            .line_height(relative(LINE_HEIGHT))
            .text_color(cx.theme().muted_foreground)
            .child(NOTE)
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let save_disabled = self
            .fields
            .iter()
            .any(|field| field.read(cx).value().is_empty());
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                Button::new(RESET_ID)
                    .label(RESET_LABEL)
                    .on_click(cx.listener(|panel, _, window, cx| panel.reset(window, cx))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(CANCEL_ID)
                            .label(CANCEL_LABEL)
                            .on_click(cx.listener(|panel, _, window, cx| panel.cancel(window, cx))),
                    )
                    .child(
                        Button::new(SAVE_ID)
                            .label(SAVE_LABEL)
                            .primary()
                            // A path is what this panel is for: an empty field is not a
                            // value to save, and the row under it says so.
                            .disabled(save_disabled)
                            .on_click(cx.listener(|panel, _, window, cx| panel.save(window, cx))),
                    ),
            )
    }
}

/// A row's label column: the fixed width that lines the fields up, and the height that
/// centers a label against the field beside it.
fn row_label(text: &'static str, height: Option<gpui_kit::Pixels>) -> impl IntoElement {
    div()
        .w(LABEL_WIDTH)
        .when_some(height, |this, height| this.h(height))
        .flex()
        .items_center()
        .justify_end()
        .text_sm()
        .font_medium()
        .child(text)
}

/// The theme's own name, as the row shows it.
fn theme_label(theme: StoredTheme) -> &'static str {
    match theme {
        StoredTheme::System => "System",
        StoredTheme::Light => "Light",
        StoredTheme::Dark => "Dark",
    }
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}
impl EventEmitter<DismissEvent> for SettingsPanel {}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = v_flex()
            .id(PANEL_ID)
            .test_support()
            .gap_2()
            .text_color(cx.theme().foreground)
            // Escape, from anywhere inside: the panel is the whole dialog.
            .on_key_down(cx.listener(Self::panel_key));
        panel = if self.embedded {
            // The host's frame is the only one: it has already drawn the border, the fill
            // and the room around us, so ours would be a second frame inside it — and the
            // width is the host's to decide, not ours.
            panel.w_full().min_h(px(PANEL_SIZE.1))
        } else {
            panel
                .w(px(PANEL_SIZE.0))
                .min_h(px(PANEL_SIZE.1))
                .p_3p5()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
        };
        panel
            .child(self.header(cx))
            .child(self.binary_row(Binary::Swarm, window, cx))
            .child(self.binary_row(Binary::Agent, window, cx))
            .child(self.theme_row(window, cx))
            .child(self.terminal_font_row(window, cx))
            // The buttons sit on the dialog's floor, however much room is left.
            .child(div().flex_1())
            .child(self.note(cx))
            .child(self.footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, size, AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions,
    };
    use std::cell::{Cell, RefCell};

    /// A window with room for the dialog and nothing else: the panel is its own size.
    const WINDOW: (f32, f32) = (640., 420.);

    /// What the two installed binaries would say, read from the real ones where they are
    /// installed and made up where they are not: the rows are what is under test here, not
    /// the machine.
    fn installed() -> Vec<(PathBuf, Check)> {
        vec![
            (
                PathBuf::from("/usr/local/bin/evo-swarm"),
                Check::Ready("evo-swarm 0.1.0".to_owned()),
            ),
            (
                PathBuf::from("/usr/local/bin/evo-agent"),
                Check::Ready("evo-agent 0.1.0".to_owned()),
            ),
        ]
    }

    struct Fixture {
        window: AnyWindowHandle,
        panel: Entity<SettingsPanel>,
        events: Rc<RefCell<Vec<SettingsEvent>>>,
        dismissals: Rc<Cell<usize>>,
        _subscriptions: Vec<Subscription>,
    }

    impl Fixture {
        fn act<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("settings window")
        }

        /// Reach into the panel the way its own update does.
        fn panel<R>(
            &self,
            cx: &mut TestAppContext,
            f: impl FnOnce(&mut SettingsPanel, &mut Context<SettingsPanel>) -> R,
        ) -> R {
            cx.update(|cx| self.panel.update(cx, f))
        }

        fn values(&self, cx: &mut TestAppContext) -> SettingsValues {
            cx.update(|cx| self.panel.read(cx).values(cx))
        }

        fn checks(&self, cx: &mut TestAppContext) -> [Check; 2] {
            cx.update(|cx| self.panel.read(cx).checks.clone())
        }

        /// The accessible name of an element, which is what a screen reader reads out.
        fn label(&self, cx: &mut TestAppContext, id: &'static str) -> String {
            self.act(cx, |window, cx| {
                window.render_frame(cx);
                window.find(id).label().unwrap_or_default().to_string()
            })
        }

        fn click(&self, cx: &mut TestAppContext, id: &'static str) {
            self.act(cx, |window, cx| {
                window.click(id, cx);
                window.render_frame(cx);
            });
        }

        fn press(&self, cx: &mut TestAppContext, key: &str) {
            self.act(cx, |window, cx| {
                window.press(key, cx);
                window.render_frame(cx);
            });
        }

        /// Type at the end of a field, the way a person does: focus, then text. It is the
        /// field's own `Change` that asks for the path to be checked again, so a test that
        /// wrote the value in directly would not be testing the panel.
        fn type_into(&self, cx: &mut TestAppContext, id: &'static str, text: &str) {
            self.write_into(cx, id, text, false)
        }

        /// Replace the terminal font field's whole value: select all, then type over it, the
        /// way renaming a font does.
        fn replace_font(&self, cx: &mut TestAppContext, text: &str) {
            let field = cx.update(|cx| self.panel.read(cx).terminal_font.clone());
            self.act(cx, |window, cx| {
                let handle = field.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                window.press("cmd-a", cx);
                if text.is_empty() {
                    // Nothing to type: clearing the selection is what a person presses.
                    window.press("backspace", cx);
                } else {
                    window.input(text, cx);
                }
                window.render_frame(cx);
            });
        }

        /// Replace a field's whole value: select all, then type over it.
        fn replace_in(&self, cx: &mut TestAppContext, id: &'static str, text: &str) {
            self.write_into(cx, id, text, true)
        }

        fn write_into(&self, cx: &mut TestAppContext, id: &'static str, text: &str, replace: bool) {
            let field = cx.update(|cx| {
                let panel = self.panel.read(cx);
                let binary = if id == SWARM_PATH_ID {
                    Binary::Swarm
                } else {
                    Binary::Agent
                };
                panel.fields[binary.ix()].clone()
            });
            self.act(cx, |window, cx| {
                let handle = field.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                if replace {
                    window.press("cmd-a", cx);
                }
                if text.is_empty() {
                    // Nothing to type: clearing the selection is what a person presses.
                    window.press("backspace", cx);
                } else {
                    window.input(text, cx);
                }
                window.render_frame(cx);
            });
        }

        /// Put the keyboard on the theme row, as Tab does in the app.
        fn focus_theme_row(&self, cx: &mut TestAppContext) {
            self.act(cx, |window, cx| {
                let handle = self.panel.read(cx).focus.clone();
                window.focus(&handle, cx);
                window.render_frame(cx);
                assert_eq!(window.find(THEME_ID).focused(), Some(true));
            });
        }

        fn events(&self) -> Vec<SettingsEvent> {
            self.events.borrow().clone()
        }

        fn dismissals(&self) -> usize {
            self.dismissals.get()
        }
    }

    /// A panel open on `values`, in the Light theme. `verdicts` answers the paths without a
    /// process; `None` leaves the panel's own prober in place, which runs them.
    fn open_with(
        values: SettingsValues,
        verdicts: Option<Vec<(PathBuf, Check)>>,
        cx: &mut TestAppContext,
    ) -> Fixture {
        open_sized(WINDOW, values, verdicts, cx)
    }

    /// The same, in a window of a given size — which is how the panel's own advertised size
    /// is checked against what it actually needs.
    fn open_sized(
        window_size: (f32, f32),
        values: SettingsValues,
        verdicts: Option<Vec<(PathBuf, Check)>>,
        cx: &mut TestAppContext,
    ) -> Fixture {
        cx.update(gpui_kit::init);
        // A known starting mode, so a theme choice and a Cancel that puts it back are both
        // visible as a change.
        cx.update(|cx| ComponentTheme::change(ThemeMode::Light, None, cx));

        let (window, panel) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(window_size.0), px(window_size.1)),
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| SettingsPanel::new(values, window, cx))
            })
            .expect("settings window")
        });

        let events = Rc::new(RefCell::new(Vec::new()));
        let dismissals = Rc::new(Cell::new(0));
        let recorded = events.clone();
        let counted = dismissals.clone();
        let subscriptions = cx.update(|cx| {
            vec![
                cx.subscribe(&panel, move |_, event: &SettingsEvent, _| {
                    recorded.borrow_mut().push(event.clone())
                }),
                cx.subscribe(&panel, move |_, _: &DismissEvent, _| {
                    counted.set(counted.get() + 1)
                }),
            ]
        });

        let fixture = Fixture {
            window,
            panel,
            events,
            dismissals,
            _subscriptions: subscriptions,
        };
        if let Some(verdicts) = verdicts {
            fixture.panel(cx, |panel, cx| panel.set_verdicts(verdicts, cx));
        }
        fixture
    }

    /// The settings a fresh install would open on.
    fn fixture(cx: &mut TestAppContext) -> Fixture {
        open_with(SettingsValues::default(), Some(installed()), cx)
    }

    /// How much room the host draws around the panel — the dialog's own padding, which the
    /// embedded panel must not add to.
    const HOST_PADDING: f32 = 24.;

    /// The panel the way the app hosts it (§13): `embedded(true)`, inside a box that has
    /// already drawn the padding a `Dialog` draws, in a window exactly that much wider than
    /// the panel advertises. Answers the window and the panel's own handle.
    fn open_hosted(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<SettingsPanel>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ComponentTheme::change(ThemeMode::Light, None, cx));

        let slot: Rc<RefCell<Option<Entity<SettingsPanel>>>> = Rc::new(RefCell::new(None));
        let keep = slot.clone();
        let (window, _host) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(
                        px(PANEL_SIZE.0 + 2. * HOST_PADDING),
                        px(PANEL_SIZE.1 + 2. * HOST_PADDING),
                    ),
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                let panel = cx.new(|cx| {
                    SettingsPanel::new(SettingsValues::default(), window, cx).embedded(true)
                });
                *keep.borrow_mut() = Some(panel.clone());
                cx.new(|_cx| HostView { panel })
            })
            .expect("hosted settings window")
        });
        let panel = slot.borrow_mut().take().expect("the panel");
        (window, panel)
    }

    /// A view that draws the padding a `Dialog` draws and puts the panel inside it — the
    /// app's own host around the panel, in miniature.
    struct HostView {
        panel: Entity<SettingsPanel>,
    }

    impl Render for HostView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().p(px(HOST_PADDING)).child(self.panel.clone())
        }
    }

    /// The panel in a host that draws the frame: no second border, no fill, no padding, and
    /// the host's width rather than the panel's own advertised one.
    #[gpui_kit::test]
    fn an_embedded_panel_lets_the_host_draw_the_frame(cx: &mut TestAppContext) {
        let (window, _panel) = open_hosted(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let panel = window.find(PANEL_ID).bounds();
            // The panel starts where the host's padding ends, and is as wide as the host
            // made room for — not `PANEL_SIZE.0` again inside it.
            assert_eq!(
                panel.origin.x,
                px(HOST_PADDING),
                "the host's padding is the only one"
            );
            assert_eq!(
                panel.size.width,
                px(PANEL_SIZE.0),
                "the panel takes the width the host gives it"
            );
            // The footer's first control is the panel's own left edge: with no padding of
            // our own, the rows start there rather than an inset inside it.
            let reset = window.find(RESET_ID).bounds();
            assert_eq!(
                reset.origin.x, panel.origin.x,
                "the embedded panel pads nothing of its own"
            );
            for id in [
                PANEL_ID,
                SWARM_PATH_ID,
                THEME_ID,
                RESET_ID,
                CANCEL_ID,
                SAVE_ID,
            ] {
                assert!(window.find(id).visible(), "{id} is not on screen");
            }
        })
        .expect("hosted settings window");
    }

    /// The standalone panel keeps the frame it shows in the demo: its own padding is what
    /// sets its rows in from its edge.
    #[gpui_kit::test]
    fn a_standalone_panel_still_draws_its_own_frame(cx: &mut TestAppContext) {
        let f = fixture(cx);
        let (panel, reset) = f.act(cx, |window, cx| {
            window.render_frame(cx);
            (
                window.find(PANEL_ID).bounds(),
                window.find(RESET_ID).bounds().origin.x,
            )
        });
        assert!(
            reset > panel.origin.x,
            "the standalone panel pads its rows ({reset:?} vs {:?})",
            panel.origin.x
        );
        assert_eq!(panel.size.width, px(PANEL_SIZE.0));
    }

    #[gpui_kit::test]
    fn the_panel_opens_on_what_the_app_has(cx: &mut TestAppContext) {
        let f = fixture(cx);
        assert_eq!(f.values(cx), SettingsValues::default());
        // Both paths are answered by the binaries themselves, and the row says what they
        // said rather than only that they worked.
        assert_eq!(
            f.checks(cx),
            [
                Check::Ready("evo-swarm 0.1.0".to_owned()),
                Check::Ready("evo-agent 0.1.0".to_owned())
            ]
        );
        assert_eq!(f.label(cx, SWARM_PATH_ID), "evo-swarm path");
        assert_eq!(f.events(), vec![]);
        assert_eq!(f.dismissals(), 0);
    }

    #[gpui_kit::test]
    fn a_path_that_is_not_there_says_so_in_the_danger_tone(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.type_into(cx, AGENT_PATH_ID, "/usr/local/bin/evo-agent-old");
        assert_eq!(f.checks(cx)[1], Check::Missing);
        // The verdict is part of what the field says it is, which is where a screen reader
        // hears it — the line under the field is the same words, drawn.
        assert_eq!(f.label(cx, AGENT_PATH_ID), "evo-agent path, not found");
        assert!(f.checks(cx)[1].is_problem());
    }

    #[gpui_kit::test]
    fn the_other_two_reasons_are_told_apart(cx: &mut TestAppContext) {
        let f = open_with(
            SettingsValues::default(),
            Some(vec![
                (
                    PathBuf::from("/usr/local/bin/evo-swarm"),
                    Check::NotExecutable,
                ),
                (PathBuf::from("/usr/local/bin/evo-agent"), Check::NotEvo),
            ]),
            cx,
        );
        assert_eq!(f.label(cx, SWARM_PATH_ID), "evo-swarm path, not executable");
        assert_eq!(
            f.label(cx, AGENT_PATH_ID),
            "evo-agent path, not an evo binary"
        );
    }

    #[gpui_kit::test]
    fn save_hands_over_the_paths_and_the_theme(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.replace_in(cx, SWARM_PATH_ID, "/opt/evo/bin/evo-swarm");
        f.focus_theme_row(cx);
        f.press(cx, "right"); // System -> Light
        f.press(cx, "right"); // Light -> Dark
        f.click(cx, SAVE_ID);

        assert_eq!(
            f.events(),
            vec![SettingsEvent::Saved(SettingsValues {
                evo_swarm: PathBuf::from("/opt/evo/bin/evo-swarm"),
                evo_agent: PathBuf::from("/usr/local/bin/evo-agent"),
                theme: StoredTheme::Dark,
                terminal_font: store::design::MONO_FONT.to_owned(),
            })]
        );
        // Save closes the dialog the same way Cancel does, so the app has one path out.
        assert_eq!(f.dismissals(), 1);
    }

    #[gpui_kit::test]
    fn cancel_hands_over_nothing_and_puts_the_theme_back(cx: &mut TestAppContext) {
        let f = fixture(cx);
        // A font typed, like a path: it is a draft on the same panel, so Cancel is the
        // only way out that does not carry it.
        f.replace_font(cx, "Fira Code");
        f.focus_theme_row(cx);
        f.press(cx, "right"); // System -> Light
        f.press(cx, "right"); // Light -> Dark
                              // The choice is applied as it is made: that is the only way to judge a theme.
        assert_eq!(cx.update(|cx| cx.theme().mode), ThemeMode::Dark);

        f.click(cx, CANCEL_ID);
        assert_eq!(f.events(), vec![]);
        assert_eq!(f.dismissals(), 1);
        // And the window goes back to the mode the panel opened in.
        assert_eq!(cx.update(|cx| cx.theme().mode), ThemeMode::Light);
    }

    #[gpui_kit::test]
    fn escape_leaves_without_saving(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.type_into(cx, SWARM_PATH_ID, "/nowhere/evo-swarm");
        f.act(cx, |window, cx| {
            let handle = f.panel.read(cx).focus.clone();
            window.focus(&handle, cx);
            window.press("escape", cx);
        });
        assert_eq!(f.events(), vec![]);
        assert_eq!(f.dismissals(), 1);
    }

    #[gpui_kit::test]
    fn reset_puts_the_defaults_back(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.replace_in(cx, SWARM_PATH_ID, "/opt/evo/bin/evo-swarm");
        f.replace_in(cx, AGENT_PATH_ID, "/nowhere/evo-agent");
        f.press(cx, "right"); // System -> Light
        assert_ne!(f.values(cx), SettingsValues::default());

        f.click(cx, RESET_ID);
        assert_eq!(f.values(cx), SettingsValues::default());
        assert_eq!(cx.update(|cx| cx.theme().mode), ThemeMode::Light); // System, on a light window
        assert_eq!(
            f.checks(cx),
            [
                Check::Ready("evo-swarm 0.1.0".to_owned()),
                Check::Ready("evo-agent 0.1.0".to_owned())
            ]
        );
        // Nothing is saved by a reset: it is an edit like any other.
        assert_eq!(f.events(), vec![]);
    }

    #[gpui_kit::test]
    fn a_cleared_field_says_there_is_no_path_and_is_not_offered_to_save(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.replace_in(cx, SWARM_PATH_ID, "");
        assert_eq!(f.checks(cx)[0], Check::Empty);
        assert_eq!(f.label(cx, SWARM_PATH_ID), "evo-swarm path, no path");
        // Clicking Save does nothing at all: no event, and the dialog stays open.
        f.click(cx, SAVE_ID);
        assert_eq!(f.events(), vec![]);
        assert_eq!(f.dismissals(), 0);
    }

    #[gpui_kit::test]
    fn choosing_a_file_fills_the_field_and_checks_it(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.panel(cx, |panel, cx| {
            panel.set_chooser(Some(PathBuf::from("/opt/evo/bin/evo-agent")), cx)
        });
        f.click(cx, AGENT_CHOOSE_ID);
        assert_eq!(
            f.values(cx).evo_agent,
            PathBuf::from("/opt/evo/bin/evo-agent")
        );
        // A chosen path is checked at once, not after the typing pause, and "not found" is
        // what the table says about a path that is not in it.
        assert_eq!(f.checks(cx)[1], Check::Missing);
    }

    #[gpui_kit::test]
    fn a_cancelled_dialog_leaves_the_path_alone(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.panel(cx, |panel, cx| panel.set_chooser(None, cx));
        f.click(cx, SWARM_CHOOSE_ID);
        assert_eq!(f.values(cx).evo_swarm, Binaries::default().evo_swarm);
    }

    #[gpui_kit::test]
    fn an_answer_for_a_path_that_is_no_longer_there_is_dropped(cx: &mut TestAppContext) {
        let f = fixture(cx);
        // The field has been typed on since the probe started: the answer belongs to a path
        // nobody is looking at.
        f.panel(cx, |panel, cx| {
            panel.checks[1] = Check::Checking;
            panel.settle(
                Binary::Agent,
                Path::new("/usr/local/bin/evo-agent-old"),
                Check::Ready("evo-agent 0.1.0".to_owned()),
                9,
                cx,
            );
        });
        assert_eq!(f.checks(cx)[1], Check::Checking);
    }

    #[gpui_kit::test]
    fn the_terminal_font_opens_on_the_apps_own_and_is_saved_as_typed(cx: &mut TestAppContext) {
        let f = open_with(
            SettingsValues::from_state(
                &Binaries::default(),
                StoredTheme::Light,
                "JetBrains Mono".to_owned(),
            ),
            Some(installed()),
            cx,
        );
        // The field opens on what the app has, not on the default: the app hands the panel
        // `app.json`'s font.
        assert_eq!(f.values(cx).terminal_font, "JetBrains Mono");

        f.replace_font(cx, "Fira Code");
        assert_eq!(f.values(cx).terminal_font, "Fira Code");
        f.click(cx, SAVE_ID);
        let events = f.events();
        let SettingsEvent::Saved(saved) = &events[0];
        assert_eq!(saved.terminal_font, "Fira Code");
        // A font is not a path: nothing is run for it, so the paths' own verdicts are the
        // ones the panel opened with.
        assert_eq!(
            f.checks(cx),
            [
                Check::Ready("evo-swarm 0.1.0".to_owned()),
                Check::Ready("evo-agent 0.1.0".to_owned())
            ]
        );
    }

    #[gpui_kit::test]
    fn reset_puts_the_designs_monospace_back_in_the_font_row(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.replace_font(cx, "Fira Code");
        assert_ne!(f.values(cx), SettingsValues::default());

        f.click(cx, RESET_ID);
        assert_eq!(f.values(cx).terminal_font, store::design::MONO_FONT);
        assert_eq!(f.values(cx), SettingsValues::default());
        // An emptied field is still a value: nothing is checked and nothing is refused.
        f.replace_font(cx, "");
        assert_eq!(f.values(cx).terminal_font, "");
    }

    #[gpui_kit::test]
    fn every_control_says_what_it_is(cx: &mut TestAppContext) {
        let f = fixture(cx);
        assert_eq!(f.label(cx, SWARM_PATH_ID), "evo-swarm path");
        assert_eq!(f.label(cx, AGENT_PATH_ID), "evo-agent path");
        assert_eq!(f.label(cx, THEME_ID), THEME_LABEL);
        assert_eq!(f.label(cx, TERMINAL_FONT_ID), TERMINAL_FONT_LABEL);
        for (id, expected) in [
            (SWARM_CHOOSE_ID, CHOOSE_LABEL),
            (AGENT_CHOOSE_ID, CHOOSE_LABEL),
            (RESET_ID, RESET_LABEL),
            (CANCEL_ID, CANCEL_LABEL),
            (SAVE_ID, SAVE_LABEL),
        ] {
            assert_eq!(f.label(cx, id), expected, "{id}");
        }
    }

    #[gpui_kit::test]
    fn the_theme_row_walks_with_the_arrows_and_wraps(cx: &mut TestAppContext) {
        let f = fixture(cx);
        f.focus_theme_row(cx);
        f.press(cx, "left"); // System, the first choice: the arrows wrap
        assert_eq!(f.values(cx).theme, StoredTheme::Dark);
        f.press(cx, "home");
        assert_eq!(f.values(cx).theme, StoredTheme::System);
        f.press(cx, "end");
        assert_eq!(f.values(cx).theme, StoredTheme::Dark);
        // The kit's own rows are what a pointer uses, and each one is applied as it lands.
        assert_eq!(cx.update(|cx| cx.theme().mode), ThemeMode::Dark);
    }

    #[gpui_kit::test]
    fn a_click_on_a_choice_takes_the_keyboard_with_it(cx: &mut TestAppContext) {
        let f = fixture(cx);
        // The left third of the segmented row is its first choice — System, the theme the
        // panel opened on, so the only thing a click changes is where the keyboard is.
        f.act(cx, |window, cx| {
            window.click_at(THEME_ID, point(px(28.), px(14.)), cx);
            window.render_frame(cx);
        });
        assert_eq!(f.values(cx).theme, StoredTheme::System);
        assert_eq!(
            f.act(cx, |window, _| window.find(THEME_ID).focused()),
            Some(true)
        );
        // And the arrows work from there, which is the point of taking the keyboard.
        f.press(cx, "right");
        assert_eq!(f.values(cx).theme, StoredTheme::Light);
    }

    #[gpui_kit::test]
    fn a_real_probe_answers_the_row_without_holding_up_the_panel(cx: &mut TestAppContext) {
        // The panel's own prober: on this axis the answers come from the binaries.
        let f = open_with(SettingsValues::default(), None, cx);
        // The row asks at once. Nothing on this thread ran a `--version` to get here: the
        // process is the app executor's business, and the row says so until it is back.
        assert_eq!(f.checks(cx)[0], Check::Checking);

        cx.executor().advance_clock(TYPING_PAUSE);
        cx.run_until_parked();
        match f.checks(cx)[0].clone() {
            Check::Ready(line) => assert!(line.starts_with("evo-swarm"), "{line}"),
            // A machine without the binaries installed is still an answer, and says so.
            Check::Missing => {}
            other => panic!("the row settled on {other:?}"),
        }
    }

    #[gpui_kit::test]
    fn the_panel_fits_the_dialog_size_it_advertises(cx: &mut TestAppContext) {
        // The app places this view in a dialog of `PANEL_SIZE`; everything the panel offers
        // has to be inside it, and the note is the first thing to fall off the bottom when it
        // does not fit.
        let f = open_sized(PANEL_SIZE, SettingsValues::default(), Some(installed()), cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            for id in [
                PANEL_ID,
                SWARM_PATH_ID,
                SWARM_CHOOSE_ID,
                SWARM_STATUS_ID,
                AGENT_PATH_ID,
                AGENT_CHOOSE_ID,
                AGENT_STATUS_ID,
                THEME_ID,
                RESET_ID,
                CANCEL_ID,
                SAVE_ID,
            ] {
                assert!(window.find(id).visible(), "{id} is not on screen");
            }
            let bounds = window.find(PANEL_ID).bounds();
            assert!(
                bounds.size.width <= px(PANEL_SIZE.0) && bounds.size.height <= px(PANEL_SIZE.1),
                "the panel drew {:?}, and it advertises {PANEL_SIZE:?}",
                bounds.size
            );
        });
    }

    #[gpui_kit::test]
    fn the_swarm_path_is_where_the_keyboard_starts(cx: &mut TestAppContext) {
        let f = fixture(cx);
        cx.update_window(f.window, |_, window, cx| {
            f.panel.update(cx, |panel, cx| panel.focus_into(window, cx));
            window.render_frame(cx);
            assert_eq!(window.find(SWARM_PATH_ID).focused(), Some(true));
        })
        .expect("settings window");
    }
}
