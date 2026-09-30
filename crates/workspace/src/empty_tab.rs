//! The empty tab (§7.2): what a new tab shows before a folder is chosen.
//!
//! One centred column on the page's own surface: a header, the two role cards with the
//! folder card beside them, the check's own lines under the cards, and the resumable
//! swarms. The controls' values, the catalog behind them and the history rows all come
//! from [`session::Launcher`]; this module owns only what the session model cannot know:
//! the widgets, the folder dialog, whether the catalog has arrived, and whether the
//! session index is still being fetched.
//!
//! ```text
//! set_catalog                      the /catalog body (§5.6): the cache, or a server
//! set_history_entries              the session index's rows (§2)
//! set_history_loading / set_catalog_error  the two states with nothing to show yet
//! → TabContentEvent::Launch        the folder plus the controls' plan (§7.2, §1)
//! → TabContentEvent::Resume        a history row, by the row's own click
//! ```
//!
//! Every control opens on a **resolved** value (§7.2) and nothing is called "Default":
//! the coordinator card is the model and effort the launch passes as `--model` and
//! `--thinking`, the workers card the count, the lanes' model and the lanes' effort
//! (`--workers`, `--lane-model`, `--lane-thinking`). See [`session::Launcher`] for how
//! each is resolved, and for the two evo does not publish offline.
//!
//! Colours and numbers come from [`store::design`] — the design's own `L`/`DARK`
//! palettes and the radii — through `cx.theme()` where the kit has the token and
//! through [`store::design`] itself where the design names a number. The design's
//! `color-mix` is [`widgets::wash`].

use std::path::PathBuf;

use gpui_kit::base::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::{ListDelegate, ListItem, ListState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::FocusableExt as _;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    Theme,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, AnyElement, App, BoxShadow, Context, ElementId, Entity, FocusHandle, Focusable as _,
    Hsla, IntoElement, Pixels, SharedString, Subscription, TestSupportExt as _, TextAlign,
    WeakEntity, Window,
};
use serde_json::Value;
use session::{HistoryEntry, LaunchPlan, Launcher, ModelOption, Role as Card};
use store::catalog::{CheckReport, Problem, ProblemTarget};
use store::cli::{self, CliError};
use store::design;

use crate::history::{rows_from_session, HistoryRow};
use crate::tab::{TabContent, TabContentEvent};

// --- the design's numbers -----------------------------------------------------------

/// `.empty{padding:58px 32px 44px}`, with the page's own bottom padding
/// (`.empty{padding-bottom:32px!important}`).
const PAGE_TOP: Pixels = px(58.);
const PAGE_X: Pixels = px(32.);
const PAGE_BOTTOM: Pixels = px(32.);
/// `.empty-head{margin-bottom:20px}`.
const HEAD_GAP: Pixels = px(20.);
/// `.launch-layout{grid-template-columns:minmax(0,1fr) 240px;gap:16px}`.
const LAYOUT_GAP: Pixels = px(16.);
const FOLDER_COLUMN: Pixels = px(240.);
/// `.role-stack{gap:10px}`, `.role-stack .config-group{padding:12px 14px}`.
const CARD_GAP: Pixels = px(10.);
const CARD_PAD_X: Pixels = px(14.);
const CARD_PAD_Y: Pixels = px(12.);
/// `.config-heading{height:28px;margin-bottom:8px;grid-template-columns:minmax(0,1fr) 180px;column-gap:14px}`.
const CARD_HEADING_H: Pixels = px(28.);
const CARD_HEADING_GAP: Pixels = px(8.);
/// `.role-fields{grid-template-columns:minmax(0,1fr) 180px;gap:14px}`.
const FIELD_COLUMN: Pixels = px(180.);
const FIELD_GAP: Pixels = px(14.);
/// `.field label{margin-bottom:4px}`.
const LABEL_GAP: Pixels = px(4.);
/// `.shad-select-wrap{height:34px}`, `.select-summary{padding:0 34px 0 10px}`.
const SELECT_H: Pixels = px(34.);
const SELECT_PAD: Pixels = px(10.);
/// `.number-input{height:28px}` and `.number-input button{width:25px}`.
const COUNT_H: Pixels = px(28.);
const COUNT_STEP: Pixels = px(25.);
/// `.history{margin-top:24px}`, `.history-head{gap:8px;margin-bottom:8px}`.
const HISTORY_GAP: Pixels = px(24.);
const HISTORY_HEAD_GAP: Pixels = px(8.);
/// `.history-row{min-height:58px;gap:12px;padding:8px 12px}`.
const ROW_MIN_H: Pixels = px(58.);
const ROW_GAP: Pixels = px(12.);
const ROW_PAD_X: Pixels = px(12.);
const ROW_PAD_Y: Pixels = px(8.);
/// `.badge{padding:0 7px}`, `.folder-card-large{padding:20px;gap:8px}` and
/// `.folder-card-large .folder-icon{margin-bottom:3px}`.
const BADGE_PAD_X: Pixels = px(7.);
const FOLDER_PAD: Pixels = px(20.);
const FOLDER_GAP: Pixels = px(8.);
const FOLDER_ICON_GAP: Pixels = px(3.);
const FOLDER_ICON: Pixels = px(36.);
/// `.folder-card-large .folder-hint{max-width:180px}`.
const FOLDER_HINT_W: Pixels = px(180.);

/// The type sizes: `.empty-title{font-size:20px;line-height:28px}`, `.field label{12px}`,
/// `.select-summary span{13px;line-height:18px}`, `.config-title{14px}`, `.badge{11px}`
/// and `.folder-card-large .folder-label{14px}`.
const TITLE_SIZE: Pixels = px(20.);
const TITLE_LINE: Pixels = px(28.);
const SMALL: Pixels = px(12.);
const TINY: Pixels = px(11.);
const FIELD_TEXT: Pixels = px(13.);
const FIELD_LINE: Pixels = px(18.);
const CARD_TITLE: Pixels = px(14.);

/// The check's problem lines under the cards (§9): one calm line each, and a click opens
/// the control the line is about. There are none when the launch is fine.
const PROBLEMS_ID: &str = "check-problems";
const PROBLEM_ID: &str = "check-problem";

/// The folder card, and the two role cards' own ids.
const FOLDER_ID: &str = "select-folder";
const COORDINATOR_CARD_ID: &str = "config-group-coordinator";
const WORKERS_CARD_ID: &str = "config-group-workers";

/// The two model fields, the two effort sliders and the count control.
const COORDINATOR_MODEL_ID: &str = "coordinator-model";
const WORKERS_MODEL_ID: &str = "workers-model";
const COORDINATOR_EFFORT_ID: &str = "coordinator-effort";
const WORKERS_EFFORT_ID: &str = "workers-effort";
const COUNT_ID: &str = "workers-count";
const COUNT_BOX_ID: &str = "workers-count-box";
const COUNT_FIELD_ID: &str = "workers-count-field";
const COUNT_MINUS_ID: &str = "workers-count-minus";
const COUNT_PLUS_ID: &str = "workers-count-plus";

/// The history region and its states.
const HISTORY_ID: &str = "history";
const HISTORY_LIST_ID: &str = "history-list";
const HISTORY_ROW_ID: &str = "history-row";
const HISTORY_HINT_ID: &str = "history-hint";
const HISTORY_COUNT_ID: &str = "history-count";
/// What the history section is called, for a screen reader: the rows name themselves, but
/// the list around them has no name of its own.
const HISTORY_LABEL: &str = "Resumable swarms";
const ROW_ICON: Pixels = px(16.);
const OPEN_AT_QUIT_ID: &str = "history-open-at-quit";
const OPEN_AT_QUIT_TEXT: &str = "open at last quit";
const RESUME_ARROW: &str = "›";

/// What the words in the page are: the design's own copy, and the two labels the fields
/// wear.
const TITLE: &str = "New Swarm";
const SUBTITLE: &str = "Choose how it runs, then select a project folder.";
const COORDINATOR_TITLE: &str = "Coordinator";
const WORKERS_TITLE: &str = "Workers";
const MODEL_LABEL: &str = "Model";
const EFFORT_LABEL: &str = "Effort";
const COUNT_LABEL: &str = "Count";
const FOLDER_LABEL: &str = "Select folder…";
const FOLDER_HINT: &str = "The swarm starts in the folder you pick";
const HISTORY_TITLE: &str = "History";

gpui_kit::actions!(
    workspace,
    [
        /// Open the app's Settings panel — what a problem line about the machine rather
        /// than the launch opens (§9.7).
        ///
        /// The panel and the paths in it are the *app's*; the workspace only knows that
        /// the person asked for it. So the empty tab, which is what draws the line,
        /// declares the action, and the app answers it (`evo_desktop`'s Settings…).
        OpenSettings,
    ]
);

/// The colour a line that is not the page's quiet grey wears: the design's `warning`, an
/// olive on the light page and an amber on the dark one — readable either way.
fn warning_ink(theme: &Theme) -> Hsla {
    theme.warning
}

/// `box-shadow: 0 0 0 <spread>px <colour>` — the ring the design draws on a focused field.
fn ring(spread: f32, color: Hsla) -> BoxShadow {
    BoxShadow::new(px(0.), px(0.), color)
        .blur_radius(px(0.))
        .spread_radius(px(spread))
}

/// One registration, as a model field offers it: what the design's `<option>` carries,
/// plus evo's own reason when this card cannot run it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelItem {
    /// `ID@PROVIDER`: what choosing this option selects.
    key: SharedString,
    /// The registration's id, and the provider that makes it one registration.
    id: SharedString,
    provider: SharedString,
    /// The menu's second line: the context window, or why this card cannot run it.
    detail: SharedString,
    available: bool,
}

impl From<(&ModelOption, Card)> for ModelItem {
    fn from((model, role): (&ModelOption, Card)) -> Self {
        let reason = model
            .reason(role)
            .unwrap_or("not usable in this session")
            .to_string();
        ModelItem {
            key: SharedString::from(model.key.clone()),
            id: SharedString::from(model.id.clone()),
            provider: SharedString::from(model.provider.clone()),
            detail: SharedString::from(if model.usable(role) {
                model.detail.clone()
            } else {
                reason
            }),
            available: model.usable(role),
        }
    }
}

impl SelectItem for ModelItem {
    type Value = SharedString;

    /// What a screen reader hears, and what the menu's own first line is.
    fn title(&self) -> SharedString {
        self.id.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.key
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.id.to_lowercase().contains(&query)
            || self.provider.to_lowercase().contains(&query)
            || self.detail.to_lowercase().contains(&query)
    }

    /// A model this card cannot run is shown, not hidden: seeing why is the point (§5.6).
    fn disabled(&self) -> bool {
        !self.available
    }

    /// The trigger's own line, which is the design's markup: `<b>provider</b> · id`.
    fn display_title(&self) -> Option<AnyElement> {
        Some(
            h_flex()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(div().flex_none().font_medium().child(self.provider.clone()))
                .child(div().flex_none().child(" · "))
                .child(div().min_w_0().child(self.id.clone()))
                .into_any_element(),
        )
    }

    fn render(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let detail_color = if self.available {
            cx.theme().muted_foreground
        } else {
            warning_ink(cx.theme())
        };
        let detail = self.detail.clone();
        v_flex()
            .gap_0p5()
            .py_0p5()
            .child(div().child(self.id.clone()))
            .when(!detail.is_empty(), |row| {
                row.child(div().text_xs().text_color(detail_color).child(detail))
            })
    }
}

/// Where a launch's folder comes from.
///
/// [`FolderPicker::Fixed`] is this module's one test seam: it answers what the dialog would
/// have answered, so a test never opens one. Only tests construct it, hence the allow.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum FolderPicker {
    /// The platform dialog: what the product does (§7.2).
    Dialog,
    /// A stand-in answer, so a test needs no dialog.
    Fixed(Option<PathBuf>),
}

/// Where a check's answer comes from: the swarm binary, or an answer already known.
#[derive(Clone, Default)]
enum CheckProbe {
    /// Run it: `evo-swarm check --json`, on the app's own executor.
    #[default]
    Command,
    /// Answer with this report, starting no process. For tests, which must not wait on a
    /// swarm binary.
    Fixed(Box<CheckReport>),
}

/// The empty tab's controls and the widgets over them.
///
/// It is an entity of its own so the widgets' own events can update the model: a click in
/// a model menu moves [`session::Launcher`]'s choice, which is what the check and the
/// launch plan read.
pub(crate) struct Choosers {
    state: Entity<EmptyTabState>,
}

impl Choosers {
    /// The two model fields need the window they will be rendered in, and the tab they
    /// will report a launch to.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<TabContent>) -> Self {
        let tab = cx.weak_entity();
        Choosers {
            state: cx.new(|cx| EmptyTabState::new(window, tab, cx)),
        }
    }

    /// The coordinator's model (`--model`) — what the tab strip shows, and what a launch
    /// passes. [`session::Launcher`] resolves it, so it is never "Default".
    pub(crate) fn coordinator_model(&self, cx: &App) -> SharedString {
        model_label(&self.state, Card::Coordinator, cx)
    }

    /// The lanes' model (`--lane-model`).
    pub(crate) fn lanes_model(&self, cx: &App) -> SharedString {
        model_label(&self.state, Card::Lanes, cx)
    }

    /// The worker count (`--workers`), as its own digits.
    pub(crate) fn workers(&self, cx: &App) -> SharedString {
        SharedString::from(self.state.read(cx).launcher.workers().to_string())
    }

    /// What the controls add up to (§7.2, §1).
    pub(crate) fn plan(&self, cx: &App) -> LaunchPlan {
        self.state.read(cx).launcher.plan()
    }

    /// Put the keyboard where the empty tab begins: the coordinator card's model field —
    /// the first control on the page — or, if that cannot take the focus, the folder card.
    ///
    /// The window is the owner's: selecting an empty tab lands the keyboard in it (§7.1's
    /// polish), which is the window's own `TabContent::focus_primary` to call. Answers
    /// whether anything took the focus; a window that takes no focus at all (a capture, a
    /// window on its way out) answers `false` and is left alone.
    pub fn focus_primary(&self, window: &mut Window, cx: &mut App) -> bool {
        // Read out of the state first: focusing borrows the window and the app, and the
        // state is borrowed through both.
        let (field, folder) = {
            let state = self.state.read(cx);
            (
                state.coordinator.read(cx).focus_handle(cx),
                state.folder_focus.clone(),
            )
        };
        window.focus(&field, cx);
        if window.focused(cx).as_ref() == Some(&field) {
            return true;
        }
        // The model field would not have the keyboard: the folder card is the next thing
        // on the page that can.
        window.focus(&folder, cx);
        window.focused(cx).as_ref() == Some(&folder)
    }
}

/// What a model field shows, for the tab strip and the tab's own title: the registration
/// the launch passes, or evo's own words when it resolved none.
fn model_label(state: &Entity<EmptyTabState>, role: Card, cx: &App) -> SharedString {
    let state = state.read(cx);
    match state.launcher.chosen(role) {
        Some(model) => SharedString::from(model.key.clone()),
        None => SharedString::from(
            state
                .launcher
                .unresolved_note(role)
                .unwrap_or_else(|| "no model configured".to_string()),
        ),
    }
}

struct EmptyTabState {
    /// The controls' model: what is resolved, the options behind it, the levels and the
    /// history rows (§7.2, §2).
    launcher: Launcher,
    coordinator: Entity<SelectState<Vec<ModelItem>>>,
    workers: Entity<SelectState<Vec<ModelItem>>>,
    /// The count field, and the two steppers beside it.
    count: Entity<InputState>,
    /// The home directory the `~` paths are shortened around.
    home: Option<String>,
    /// The catalog has arrived, from the cache or from a server: until it has, the model
    /// fields hold nothing to choose from.
    catalog: bool,
    /// The catalog could not be read: shown under the cards, where the check's lines are.
    catalog_error: Option<String>,
    /// The `evo-swarm` a check runs. The app's own path from Settings, so the check is
    /// about the swarm this app would really spawn.
    swarm_bin: PathBuf,
    /// What the last check found wrong with the launch the controls describe (§9). One
    /// line each, in evo's own words; empty when the launch is fine.
    problems: Vec<Problem>,
    /// Bumped on every check asked: an answer that is no longer the newest is dropped
    /// before it is applied.
    check_revision: u64,
    /// Where a check's answer comes from.
    check_probe: CheckProbe,
    /// Where a folder pick answers from.
    picker: FolderPicker,
    /// The history list's two states (§2): still being fetched, or it could not be read.
    history_loading: bool,
    history_error: Option<String>,
    /// The folder card's own focus handle, so keyboard traversal and its focus ring have
    /// somewhere to land.
    folder_focus: FocusHandle,
    /// The two sliders' focus handles: a click on a rail focuses it, which is what makes
    /// the arrows work.
    effort_focus: [FocusHandle; 2],
    /// The tab that owns this state: what a launch is emitted on.
    tab: WeakEntity<TabContent>,
    _subscriptions: Vec<Subscription>,
}

impl EmptyTabState {
    fn new(window: &mut Window, tab: WeakEntity<TabContent>, cx: &mut Context<Self>) -> Self {
        let launcher = Launcher::new();
        let coordinator = model_state(&launcher, Card::Coordinator, window, cx);
        let workers = model_state(&launcher, Card::Lanes, window, cx);
        let count = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .default_value(launcher.workers().to_string())
                .placeholder("1");
            // `.number-input input{text-align:center}`.
            state.set_text_align(TextAlign::Center, cx);
            state
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &coordinator,
                window,
                |this, _, event: &SelectEvent<Vec<ModelItem>>, window, cx| {
                    this.on_choose(Card::Coordinator, event, window, cx)
                },
            ),
            cx.subscribe_in(
                &workers,
                window,
                |this, _, event: &SelectEvent<Vec<ModelItem>>, window, cx| {
                    this.on_choose(Card::Lanes, event, window, cx)
                },
            ),
            cx.subscribe_in(&count, window, |this, _, event: &InputEvent, window, cx| {
                this.on_type(event, window, cx)
            }),
        ];

        EmptyTabState {
            launcher,
            coordinator,
            workers,
            count,
            home: std::env::var("HOME").ok(),
            catalog: false,
            catalog_error: None,
            // The app hands its own path in as soon as it can; until then this is where
            // the installed binary is (§1).
            swarm_bin: cli::swarm_bin(),
            problems: Vec::new(),
            check_revision: 0,
            check_probe: CheckProbe::default(),
            picker: FolderPicker::Dialog,
            history_loading: false,
            history_error: None,
            folder_focus: cx.focus_handle(),
            effort_focus: [cx.focus_handle(), cx.focus_handle()],
            tab,
            _subscriptions: subscriptions,
        }
    }

    /// A model menu committed an option: the launcher decides whether it means anything —
    /// a rebuilt field can drop a choice that no longer exists.
    fn on_choose(
        &mut self,
        role: Card,
        event: &SelectEvent<Vec<ModelItem>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(key)) = event else {
            return;
        };
        if self.launcher.choose(role, key) {
            // A different launch is a different answer: ask again (§9).
            self.run_check(cx);
            cx.notify();
        }
    }

    /// The count field was typed in. The design's own rule: a number that cannot be a
    /// count is the nearest count it can be, and the field is put back to what the model
    /// says — `Math.max(1, Math.min(64, n || 1))`, with an empty box reading 1.
    fn on_type(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let typed = self.count.read(cx).value();
        let count = typed
            .trim()
            .parse::<u16>()
            .unwrap_or(0)
            .clamp(session::WORKERS_MIN, session::WORKERS_MAX);
        if self.launcher.set_workers(count) {
            cx.notify();
        }
        if typed.as_ref() != count.to_string() {
            self.count.update(cx, |state, cx| {
                state.set_value(count.to_string(), window, cx)
            });
        }
    }

    /// One of the count box's steppers: `−` is one fewer, `+` one more, both clamped.
    fn step_count(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.launcher.workers();
        let next = if up {
            current.saturating_add(1)
        } else {
            current.saturating_sub(1)
        };
        if self.launcher.set_workers(next) {
            let count = self.launcher.workers().to_string();
            self.count
                .update(cx, |state, cx| state.set_value(count, window, cx));
            cx.notify();
        }
    }

    /// Rebuild the two model fields and the count from the launcher: the catalog arrived,
    /// or the resolved values changed with it.
    fn sync_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (role, field) in [
            (Card::Coordinator, self.coordinator.clone()),
            (Card::Lanes, self.workers.clone()),
        ] {
            let items = model_items(&self.launcher, role);
            let selected = selected_row(&self.launcher, role, &items);
            field.update(cx, |state, cx| {
                state.set_items(items, window, cx);
                state.set_selected_index(selected, window, cx);
            });
        }
        self.sync_count(window, cx);
    }

    /// The count box reads what the launcher holds — the only count there is, so the box
    /// cannot show one number while the launch passes another.
    fn sync_count(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workers = self.launcher.workers().to_string();
        if self.count.read(cx).value().as_ref() != workers {
            self.count
                .update(cx, |state, cx| state.set_value(workers, window, cx));
        }
    }

    /// The catalog: the `/catalog` body (§5.6) the disk cache holds, or the one the running
    /// server just answered with.
    ///
    /// The body carries everything the controls need — the models with their
    /// `ready`/`reason`, the `lanes.models` list with evo's own `ok`/`reason` for each
    /// registration a lane may run, the default registration and the levels a session takes
    /// — so nothing here has to compare API sets.
    fn set_catalog(&mut self, catalog: &Value, window: &mut Window, cx: &mut Context<Self>) {
        self.catalog = true;
        self.catalog_error = None;
        self.launcher.set_catalog(catalog);
        self.sync_fields(window, cx);
        self.run_check(cx);
        cx.notify();
    }

    /// The swarm binary a check runs: the app's own path, from Settings (§13).
    fn set_swarm_bin(&mut self, bin: PathBuf, cx: &mut Context<Self>) {
        if self.swarm_bin == bin {
            return;
        }
        self.swarm_bin = bin;
        self.run_check(cx);
    }

    /// What a check found, without running one: the tab's answer for a test. The whole
    /// report, because its models are what the fields resolve to — not only its lines.
    #[allow(dead_code)]
    fn set_check_report(&mut self, report: CheckReport, cx: &mut Context<Self>) {
        self.check_probe = CheckProbe::Fixed(Box::new(report));
        self.run_check(cx);
    }

    /// Ask `evo-swarm check --json` about the launch the controls describe (§9), off the
    /// thread that draws: are the models resolvable, can a lane reach its API, is the key
    /// there. The answer is both the lines under the cards and what the fields resolve to.
    ///
    /// A check that cannot run at all — the binary is not there, or is not one that runs —
    /// is one more line of the same kind: it wears evo's own shape, says which path it
    /// tried, and a click on it opens Settings, which is where a path is fixed (§13).
    fn run_check(&mut self, cx: &mut Context<Self>) {
        let plan = self.launcher.plan();
        let spec = crate::launch::check_spec(&plan);
        self.check_revision += 1;
        let revision = self.check_revision;

        if let CheckProbe::Fixed(report) = self.check_probe.clone() {
            self.launcher.set_check(&check_body(&report));
            self.settle(&report.problems, revision, cx);
            return;
        }

        let bin = self.swarm_bin.clone();
        let argv = spec.check_argv();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            // A process, but not on the thread that draws: the app's own executor runs it.
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    // `check` exits 1 when it found problems and prints them anyway (§2),
                    // so the document is read on that exit too: those problems are exactly
                    // what these lines are.
                    match cli::run_json_reporting(&bin, &argv) {
                        Ok(body) => CheckReport::from_json(&body),
                        Err(error) => CheckReport {
                            problems: vec![check_failed(&error)],
                            ..CheckReport::default()
                        },
                    }
                })
                .await;
            let report = check_body(&outcome);
            let problems = outcome.problems;
            let _ = this.update(cx, move |state, cx| {
                if state.check_revision != revision {
                    return;
                }
                state.launcher.set_check(&report);
                state.settle(&problems, revision, cx);
            });
        })
        .detach();
    }

    /// Take a check's answer, if it is still the answer to the newest question asked.
    fn settle(&mut self, problems: &[Problem], revision: u64, cx: &mut Context<Self>) {
        if self.check_revision != revision {
            return;
        }
        if self.problems != problems {
            self.problems = problems.to_vec();
        }
        cx.notify();
    }

    /// A click on a problem line: the field the line is about takes the keyboard — so the
    /// keyboard is already where the fix is — or, for anything about the machine rather
    /// than the launch, the app's Settings panel opens.
    fn open_target(&mut self, target: ProblemTarget, window: &mut Window, cx: &mut Context<Self>) {
        // A problem about the machine the swarm would run on — a binary that is not there
        // — is not about anything on this screen: the app's Settings panel is where it is
        // fixed (§13, §9.7).
        let handle = match target {
            ProblemTarget::Model => Some(self.coordinator.read(cx).focus_handle(cx)),
            ProblemTarget::LaneModel => Some(self.workers.read(cx).focus_handle(cx)),
            ProblemTarget::Thinking => Some(self.effort_focus[slot(Card::Coordinator)].clone()),
            ProblemTarget::Workers => Some(self.count.read(cx).focus_handle(cx)),
            ProblemTarget::Other => None,
        };
        let Some(handle) = handle else {
            window.dispatch_action(Box::new(OpenSettings), cx);
            return;
        };
        window.focus(&handle, cx);
    }

    fn set_catalog_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.catalog_error = error.filter(|error| !error.trim().is_empty());
        cx.notify();
    }

    /// The cfg the folder dialog starts in, and the `~` the history rows shorten around.
    fn set_home(&mut self, home: Option<String>, cx: &mut Context<Self>) {
        self.home = home;
        cx.notify();
    }

    /// A folder pick, synchronously: what a test injects in place of the dialog.
    #[allow(dead_code)]
    fn set_picker(&mut self, picker: FolderPicker, cx: &mut Context<Self>) {
        self.picker = picker;
        cx.notify();
    }

    /// Ask for a folder, then launch in it (§7.2).
    ///
    /// The dialog is `rfd`'s async one: the UI thread neither blocks on it nor waits for
    /// it, its answer arrives on the GPUI foreground executor, and cancelling it leaves the
    /// tab empty.
    fn pick_folder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let answer = match self.picker.clone() {
            FolderPicker::Fixed(answer) => answer,
            FolderPicker::Dialog => {
                let starting_folder = self.home.clone().map(PathBuf::from);
                let tab = self.tab.clone();
                cx.spawn(async move |_this: WeakEntity<Self>, cx| {
                    let mut dialog = rfd::AsyncFileDialog::new()
                        .set_title("Choose the folder this swarm runs in");
                    if let Some(folder) = starting_folder {
                        dialog = dialog.set_directory(folder);
                    }
                    let Some(picked) = dialog.pick_folder().await else {
                        return;
                    };
                    let folder = picked.path().to_path_buf();
                    let _ = tab.update(cx, |tab, cx| tab.request_launch(folder, cx));
                })
                .detach();
                return;
            }
        };
        if let Some(folder) = answer {
            // The plan is read here, with the state in hand: asking the tab for it would
            // read this entity again while it is being updated.
            let plan = self.launcher.plan();
            let _ = self
                .tab
                .update(cx, |tab, cx| tab.emit_launch(folder, plan, cx));
        }
    }

    // --- the pieces the page is made of ---------------------------------------------

    fn header(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .id("empty-head")
            .test_support()
            .flex_none()
            .mb(HEAD_GAP)
            .child(
                div()
                    .text_size(TITLE_SIZE)
                    .line_height(TITLE_LINE)
                    .font_semibold()
                    .child(TITLE),
            )
            .child(
                div()
                    .text_size(px(design::FONT_BASE - 2.))
                    .text_color(cx.theme().muted_foreground)
                    .child(SUBTITLE),
            )
    }

    /// One role card: its title, the count control when it has one, and the two fields.
    fn role_card(&self, role: Card, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (title, id) = match role {
            Card::Coordinator => (COORDINATOR_TITLE, COORDINATOR_CARD_ID),
            Card::Lanes => (WORKERS_TITLE, WORKERS_CARD_ID),
        };
        v_flex()
            .id(id)
            .test_support()
            .w_full()
            .min_w_0()
            .px(CARD_PAD_X)
            .py(CARD_PAD_Y)
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .id(ElementId::Name(format!("{id}-heading").into()))
                    .test_support()
                    .w_full()
                    .h(CARD_HEADING_H)
                    .mb(CARD_HEADING_GAP)
                    .gap(FIELD_GAP)
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(CARD_TITLE)
                            .font_semibold()
                            .child(title),
                    )
                    .when(role == Card::Lanes, |heading| {
                        heading.child(
                            div()
                                .w(FIELD_COLUMN)
                                .flex_none()
                                .child(self.count_box(window, cx)),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(FIELD_GAP)
                    .items_end()
                    .child(self.model_field(role, cx))
                    .child(
                        div()
                            .w(FIELD_COLUMN)
                            .flex_none()
                            .child(self.effort_field(role, window, cx)),
                    ),
            )
    }

    /// The model field: the design's `.field` — a label, then the select's own box.
    fn model_field(&self, role: Card, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (id, field, label) = match role {
            Card::Coordinator => (COORDINATOR_MODEL_ID, &self.coordinator, "Coordinator model"),
            Card::Lanes => (WORKERS_MODEL_ID, &self.workers, "Workers model"),
        };
        // The design's `:focus-within`: the box around the control wears the ring, and
        // the handle is the control's own, so there is no second tab stop to land on.
        let handle = match role {
            Card::Coordinator => self.coordinator.read(cx).focus_handle(cx),
            Card::Lanes => self.workers.read(cx).focus_handle(cx),
        };
        let border = theme.border;
        let primary = theme.primary;
        let muted = theme.muted;
        // A field that resolved no model says so in the box, in evo's own words: the click
        // still opens the menu, which is where another registration would come from.
        let note = self
            .launcher
            .unresolved_note(role)
            .unwrap_or_else(|| "no model configured".to_string());
        v_flex()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .mb(LABEL_GAP)
                    .text_size(SMALL)
                    .text_color(theme.muted_foreground)
                    .child(MODEL_LABEL),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("{id}-box").into()))
                    .test_support()
                    .relative()
                    .w_full()
                    .h(SELECT_H)
                    .track_focus(&handle)
                    // `:focus-within`: the ring is the design's `0 0 0 2px var(--muted)`
                    // over a primary border. Painted by the element that carries the
                    // handle, so it follows the keyboard without a re-render of the page.
                    .focus(move |style| style.border_color(primary).shadow(vec![ring(2., muted)]))
                    .child(
                        Select::new(field)
                            .id(id)
                            .appearance(false)
                            .focus_ring(false)
                            // A field that resolved no model says so in the box, in evo's
                            // own words: the click still opens the menu, which is where
                            // another registration would come from.
                            .placeholder(note)
                            .w_full()
                            .h(SELECT_H)
                            .px(SELECT_PAD)
                            .rounded(px(design::RADIUS))
                            .border_1()
                            .border_color(border)
                            .bg(theme.background)
                            .text_size(FIELD_TEXT)
                            .line_height(FIELD_LINE)
                            .text_color(theme.foreground)
                            .menu_width(px(340.))
                            .accessibility_label(label),
                    ),
            )
    }

    /// The effort field: the label row — the level's own name on the right, as the design
    /// puts it — and the slider under it.
    fn effort_field(&self, role: Card, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let level = self
            .launcher
            .level(role)
            .map(str::to_owned)
            .unwrap_or_else(|| "—".to_string());
        v_flex()
            .w_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .mb(LABEL_GAP)
                    .child(
                        div()
                            .text_size(SMALL)
                            .text_color(theme.muted_foreground)
                            .child(EFFORT_LABEL),
                    )
                    .child(
                        div()
                            .text_size(SMALL)
                            .font_medium()
                            .text_color(theme.foreground)
                            .child(level),
                    ),
            )
            .child(self.effort_slider(role, window, cx))
    }

    /// The slider itself: the shared widget, with this page's levels and palette.
    ///
    /// `--thinking` / `--lane-thinking` take the levels the catalog lists, so the slider
    /// offers exactly those — the caller owns the label, the widget the rail.
    fn effort_slider(&self, role: Card, window: &Window, cx: &Context<Self>) -> AnyElement {
        let id = match role {
            Card::Coordinator => COORDINATOR_EFFORT_ID,
            Card::Lanes => WORKERS_EFFORT_ID,
        };
        let levels: Vec<SharedString> = self
            .launcher
            .levels()
            .iter()
            .map(|level| SharedString::from(level.clone()))
            .collect();
        let weak = cx.entity().downgrade();
        // The slider is named with the page's own id: it registers `<id>`, `<id>-rail`,
        // `<id>-thumb` and `<id>-fill` itself, which is what a test finds them by.
        widgets::EffortSlider::with_levels(id, levels, self.launcher.effort(role))
            .palette(design::palette(cx.theme().is_dark()))
            .focus(self.effort_focus[slot(role)].clone())
            .notify({
                let weak = weak.clone();
                move |cx: &mut App| {
                    let _ = weak.update(cx, |_, cx| cx.notify());
                }
            })
            .on_change(move |level, _window, cx| {
                let _ = weak.update(cx, |state, cx| {
                    if state.launcher.set_effort(role, level) {
                        cx.notify();
                    }
                });
            })
            .render(window)
    }

    /// The count control: `.worker-count` — a label, then the box the `−`/`+` steppers and
    /// the field share.
    fn count_box(&self, _window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let handle = self.count.read(cx).focus_handle(cx);
        let border = theme.border;
        let primary = theme.primary;
        let muted = theme.muted;
        let ink = theme.muted_foreground;
        // The design's steppers are one tone, with no hover or pressed rule of their own:
        // `.number-input button{border:0;background:var(--muted);color:var(--muted-fg)}`.
        let step = |id: &'static str, label: &'static str, up: bool, cx: &Context<Self>| {
            Button::new(id)
                .rounded(px(0.))
                .bg(muted)
                .text_color(ink)
                .w(COUNT_STEP)
                .h_full()
                .flex_none()
                .text_size(SMALL)
                .child(label)
                // The design's own `onClick`: a click steps, and a focused button's
                // `Enter` or `Space` is a click too.
                .on_click(cx.listener(move |this, _, window, cx| this.step_count(up, window, cx)))
        };
        h_flex()
            .id(COUNT_ID)
            .test_support()
            .w_full()
            .h(COUNT_H)
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .flex_none()
                    .text_size(SMALL)
                    .text_color(theme.muted_foreground)
                    .child(COUNT_LABEL),
            )
            .child(
                h_flex()
                    .id(COUNT_BOX_ID)
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .h(COUNT_H)
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(design::RADIUS))
                    .border_1()
                    .border_color(border)
                    .track_focus(&handle)
                    // `.number-input:focus-within{border-color:var(--primary);box-shadow:0 0 0 2px var(--muted)}`
                    .focus(move |style| style.border_color(primary).shadow(vec![ring(2., muted)]))
                    .child(step(COUNT_MINUS_ID, "−", false, cx))
                    .child(
                        div()
                            .id(COUNT_FIELD_ID)
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                // `.number-input input{border:0;background:transparent}`:
                                // the box's own surface is the card's, and the field only
                                // holds the digits.
                                Input::new(&self.count)
                                    .appearance(false)
                                    .bg(theme.transparent)
                                    .h_full()
                                    .text_size(SMALL)
                                    .aria_label(COUNT_LABEL),
                            ),
                    )
                    .child(step(COUNT_PLUS_ID, "+", true, cx)),
            )
    }

    /// The folder card (§7.2): the one thing on the page that is not a control's value —
    /// pick the folder the swarm runs in.
    fn folder_card(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let face = theme.secondary;
        let hover_face = theme.muted;
        let border = theme.border;
        let accent = theme.primary;
        let ring = theme.ring;
        Button::new(FOLDER_ID)
            .track_focus(&self.folder_focus)
            .accessibility_label(FOLDER_LABEL)
            .bg(face)
            .text_color(theme.foreground)
            .w(FOLDER_COLUMN)
            .flex_none()
            .p(FOLDER_PAD)
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(border)
            // `.folder-card-large:hover{border-color:var(--primary);background:var(--muted)}`
            .hover(move |style| style.border_color(accent).bg(hover_face))
            .focus_visible(move |style| style.border_color(ring))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(FOLDER_GAP)
            .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx)))
            .child(
                div().mb(FOLDER_ICON_GAP).child(
                    Icon::new(IconName::Folder)
                        .with_size(FOLDER_ICON)
                        .text_color(theme.foreground),
                ),
            )
            .child(div().text_size(px(14.)).font_medium().child(FOLDER_LABEL))
            .child(
                div()
                    .max_w(FOLDER_HINT_W)
                    .text_size(SMALL)
                    .line_height(px(16.))
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(FOLDER_HINT),
            )
    }

    /// What `evo-swarm check --json` found wrong with this launch (§9), and what the
    /// catalog could not do: one calm line each, under the two cards, with a click on a
    /// check line opening the field it is about. Nothing at all when everything is fine.
    fn render_problems(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let mut lines: Vec<AnyElement> =
            self.problems
                .iter()
                .enumerate()
                .map(|(ix, problem)| {
                    let line = SharedString::from(problem.line());
                    let hovered = line.clone();
                    let target = problem.target();
                    div()
                        .id(ElementId::NamedInteger(PROBLEM_ID.into(), ix as u64))
                        .test_support()
                        .w_full()
                        .min_w_0()
                        .text_size(SMALL)
                        .text_color(warning_ink(cx.theme()))
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .aria_label(line.clone())
                        .tooltip(move |window, cx| {
                            Tooltip::new(hovered.clone())
                                .max_w(px(460.))
                                .build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_target(target, window, cx)
                        }))
                        .child(line)
                        .into_any_element()
                })
                .collect();
        // The catalog's own trouble is one more line of the same kind: what the page does
        // without it, with the server's own words in the hover.
        if let Some(error) = &self.catalog_error {
            let text = if self.catalog {
                CATALOG_STALE
            } else {
                CATALOG_FAILED
            };
            let detail = SharedString::from(error.clone());
            lines.push(
                div()
                    .id("catalog-problem")
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .text_size(SMALL)
                    .text_color(warning_ink(cx.theme()))
                    .tooltip(move |window, cx| {
                        Tooltip::new(detail.clone())
                            .max_w(px(460.))
                            .build(window, cx)
                    })
                    .child(text)
                    .into_any_element(),
            );
        }
        if lines.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .id(PROBLEMS_ID)
                .test_support()
                .w_full()
                .gap_1()
                .children(lines)
                .into_any_element(),
        )
    }

    /// The configuration and the folder beside it: `.launch-layout`.
    fn launch_layout(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .id("launch-layout")
            .test_support()
            .w_full()
            // `.empty-head,.launch-layout{flex:0 0 auto}`: the cards keep their height and
            // the history is what gives.
            .flex_none()
            .items_stretch()
            .gap(LAYOUT_GAP)
            .child(
                v_flex()
                    .id("role-stack")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .gap(CARD_GAP)
                    .child(self.role_card(Card::Coordinator, window, cx))
                    .child(self.role_card(Card::Lanes, window, cx))
                    // The check's own lines belong under the cards they are about.
                    .when_some(self.render_problems(cx), |stack, problems| {
                        stack.child(problems)
                    }),
            )
            .child(self.folder_card(cx))
    }

    /// The resumable swarms: a fixed head, then a list that scrolls in whatever height is
    /// left — the header and the cards never move for it.
    fn history_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let rows = self.launcher.history();
        let count = match rows.len() {
            0 => SharedString::default(),
            1 => SharedString::from("1 resumable"),
            n => SharedString::from(format!("{n} resumable")),
        };
        v_flex()
            .id(HISTORY_ID)
            .test_support()
            .w_full()
            .flex_1()
            .min_h_0()
            .mt(HISTORY_GAP)
            .child(
                h_flex()
                    .id("history-head")
                    .test_support()
                    .flex_none()
                    .w_full()
                    .items_center()
                    .gap(HISTORY_HEAD_GAP)
                    .mb(HISTORY_HEAD_GAP)
                    .child(
                        div()
                            .text_size(CARD_TITLE)
                            .font_semibold()
                            .child(HISTORY_TITLE),
                    )
                    .child(
                        div()
                            .id(HISTORY_COUNT_ID)
                            .test_support()
                            .text_size(SMALL)
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    ),
            )
            .child(self.history_body(cx))
    }

    /// The list itself, or the one line that stands in for it: it is still being fetched,
    /// it could not be fetched, there is nothing to resume, or the rows.
    fn history_body(&self, cx: &Context<Self>) -> AnyElement {
        let rows = self.launcher.history();
        if rows.is_empty() {
            let note = match (&self.history_error, self.history_loading) {
                (Some(error), _) => SharedString::from(error.clone()),
                (None, true) => SharedString::from("Looking for sessions…"),
                (None, false) => SharedString::from("No swarms to resume yet."),
            };
            return div()
                .id(HISTORY_HINT_ID)
                .test_support()
                .w_full()
                .pb(px(8.))
                .text_size(CARD_TITLE)
                .text_color(cx.theme().muted_foreground)
                .child(note)
                .into_any_element();
        }
        let theme = cx.theme();
        let rows: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(ix, row)| self.history_row(ix, row, cx).into_any_element())
            .collect();
        div()
            .id(HISTORY_LIST_ID)
            .test_support()
            .w_full()
            // `.history-list{flex:0 1 auto;min-height:0;overflow-y:auto}`: it takes its
            // content's height until the page runs out, then scrolls in place.
            .flex_grow_0()
            .flex_shrink_1()
            .min_h_0()
            .overflow_y_scroll()
            .role(gpui_kit::Role::Group)
            .aria_label(HISTORY_LABEL)
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .children(rows)
            .into_any_element()
    }

    /// One history row: the folder glyph, the title with the badge the app's own recents
    /// earn, the path and how long ago under it, and the arrow that says what a click does.
    fn history_row(
        &self,
        ix: usize,
        row: &session::HistoryRow,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let session = PathBuf::from(&row.session_path);
        let folder = PathBuf::from(&row.folder);
        let badge = row.open_at_quit.then(|| {
            div()
                .id(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), ix as u64))
                .test_support()
                .flex_none()
                .px(BADGE_PAD_X)
                .py(px(1.))
                .rounded(px(999.))
                .bg(theme.muted)
                .text_color(muted)
                .text_size(TINY)
                .child(OPEN_AT_QUIT_TEXT)
                .into_any_element()
        });
        Button::new(ElementId::NamedInteger(HISTORY_ROW_ID.into(), ix as u64))
            .bg(theme.transparent)
            .text_color(theme.foreground)
            .w_full()
            .min_h(ROW_MIN_H)
            .flex_none()
            .px(ROW_PAD_X)
            .py(ROW_PAD_Y)
            .rounded(px(0.))
            .gap(ROW_GAP)
            .justify_start()
            .when(ix > 0, |row| row.border_t_1().border_color(theme.border))
            .tooltip({
                let tooltip = SharedString::from(row.tooltip.clone());
                move |window, cx| {
                    Tooltip::new(tooltip.clone())
                        .max_w(px(460.))
                        .build(window, cx)
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.tab.update(cx, |tab, cx| {
                    tab.request_resume(session.clone(), folder.clone(), cx)
                });
            }))
            .child(
                div()
                    .flex_none()
                    .child(Icon::new(IconName::Folder).with_size(ROW_ICON)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .items_start()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(8.))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(14.))
                                    .font_medium()
                                    .child(row.title.clone()),
                            )
                            .children(badge),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(5.))
                            .text_size(SMALL)
                            .text_color(muted)
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(row.folder_short.clone()),
                            )
                            .child(div().flex_none().child("·"))
                            .child(div().flex_none().child(row.when.clone())),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(14.))
                    .text_color(muted)
                    .child(RESUME_ARROW),
            )
    }
}

impl Render for EmptyTabState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The count box shows what the launcher holds, whoever moved it: the catalog
        // arrived, a check resolved another count, or the person typed one.
        self.sync_count(window, cx);
        v_flex()
            .id("empty-tab")
            .test_support()
            .size_full()
            .overflow_hidden()
            .items_center()
            .pt(PAGE_TOP)
            .pb(PAGE_BOTTOM)
            .px(PAGE_X)
            .child(
                v_flex()
                    .id("empty-tab-block")
                    .test_support()
                    .w_full()
                    .max_w(px(design::MEASURE))
                    .flex_1()
                    .min_h_0()
                    .child(self.header(cx))
                    .child(self.launch_layout(window, cx))
                    .child(self.history_section(cx)),
            )
    }
}

/// Where one card's own slots are in the state: the two model fields (`0`, `1`), the two
/// sliders (`3`, `4`, after the count box's `2`).
fn slot(role: Card) -> usize {
    match role {
        Card::Coordinator => 0,
        Card::Lanes => 1,
    }
}

/// The registrations one card's menu offers.
fn model_items(launcher: &Launcher, role: Card) -> Vec<ModelItem> {
    launcher
        .models()
        .iter()
        .map(|model| ModelItem::from((model, role)))
        .collect()
}

/// Where a card's chosen registration sits in its menu, for a select that takes an index.
fn selected_row(launcher: &Launcher, role: Card, items: &[ModelItem]) -> Option<IndexPath> {
    let key = launcher.chosen_key(role)?;
    items
        .iter()
        .position(|item| item.key.as_ref() == key)
        .map(|row| IndexPath::default().row(row))
}

/// A card's model field, built from the launcher's options.
fn model_state(
    launcher: &Launcher,
    role: Card,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SelectState<Vec<ModelItem>>> {
    let items = model_items(launcher, role);
    let selected = selected_row(launcher, role, &items);
    cx.new(|cx| SelectState::new(items, selected, window, cx))
}

/// What the caption says when the catalog could not be fetched at all (§5.6): one sentence
/// about what the page still does, because the server's own words — `http 500: The value
/// "Bearer …"` — are evidence, not a message. They go in the line's tooltip and in
/// `app.log`; the fields keep whatever they had, so a swarm can still be started.
const CATALOG_FAILED: &str = "Couldn't load the model list — evo's own defaults will apply.";
/// … and when the catalog could not be fetched but the last one is still in the fields.
const CATALOG_STALE: &str = "Couldn't refresh the model list — using the last one it loaded.";

/// The one line a check that could not run becomes (§9): evo's own answer shape, naming the
/// binary it tried and where a path is fixed. A click on it opens Settings, which is where
/// the path in `app.json` lives (§13, §9.7).
///
/// The child's own stderr is not in the line: it is evidence a person can read in
/// `app.log`, and it may quote a value this app has no business putting on screen.
fn check_failed(error: &CliError) -> Problem {
    Problem {
        code: "check_failed".to_string(),
        message: format!("{} — fix it in Settings…", error.summary()),
    }
}

/// A check report as the JSON body `session::Launcher::set_check` reads: the same document
/// `evo-swarm check --json` prints, rebuilt from the report a run produced.
fn check_body(report: &CheckReport) -> Value {
    fn one(check: &Option<store::catalog::ModelCheck>) -> Value {
        serde_json::json!({
            "id": check.as_ref().and_then(|check| check.id.clone()),
            "provider": check.as_ref().and_then(|check| check.provider.clone()),
            "ok": check.as_ref().map(|check| check.ok),
            "reason": check.as_ref().and_then(|check| check.reason.clone()),
        })
    }
    serde_json::json!({
        "ok": report.ok,
        "model": one(&report.model),
        "lane_model": one(&report.lane_model),
        // What evo would resolve with no flags (§2): the two efforts and the count the
        // controls open on.
        "thinking": report.thinking,
        "lane_thinking": report.lane_thinking,
        "workers": report.workers,
        "problems": report
            .problems
            .iter()
            .map(|problem| serde_json::json!({"code": problem.code, "message": problem.message}))
            .collect::<Vec<_>>(),
    })
}

impl TabContent {
    /// The empty tab (§7.2): the configuration block, then the resumable swarms.
    pub(crate) fn render_empty(&self, _cx: &mut Context<Self>) -> AnyElement {
        self.choosers.state.clone().into_any_element()
    }

    /// The model catalog (§5.6): the body the disk cache holds, or the one the running
    /// server answered `GET /catalog` with.
    pub fn set_catalog(&mut self, catalog: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_catalog(catalog, window, cx));
        cx.notify();
    }

    /// The `evo-swarm` this app would spawn, which is the binary a check runs (§9, §13).
    pub fn set_swarm_bin(&mut self, bin: PathBuf, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_swarm_bin(bin, cx));
        cx.notify();
    }

    /// The catalog could not be learned: say so under the cards, where the check's own
    /// lines are.
    pub fn set_catalog_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_catalog_error(error, cx));
        cx.notify();
    }

    /// The resumable swarms the index lists (§2), merged with the app's own recents.
    /// `now` is the clock the relative times read against, `offset_seconds` the local UTC
    /// offset the rows are shown in, `home` the directory the paths are shortened around.
    pub fn set_history_entries(
        &mut self,
        entries: &[HistoryEntry],
        now: i64,
        offset_seconds: i32,
        home: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let state = self.choosers.state.clone();
        let home = home.map(str::to_string);
        let rows = state.update(cx, |state, cx| {
            state
                .launcher
                .set_history(entries, now, offset_seconds, home.as_deref());
            if home.is_some() {
                state.home = home;
            }
            cx.notify();
            rows_from_session(state.launcher.history())
        });
        self.history
            .update(cx, |list, cx| list.delegate_mut().set_rows(rows, cx));
        cx.notify();
    }

    /// The session index is still being fetched (§2): the list says so instead of claiming
    /// there is nothing.
    pub fn set_history_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| {
            state.history_loading = loading;
            cx.notify();
        });
        cx.notify();
    }

    /// The session index could not be read: the list says why.
    pub fn set_history_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let error = error.filter(|error| !error.trim().is_empty());
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| {
            state.history_error = error;
            cx.notify();
        });
        cx.notify();
    }

    /// The `~` the history paths are shortened around, and where the folder dialog starts.
    pub fn set_home(&mut self, home: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_home(home, cx));
        cx.notify();
    }

    /// What the controls add up to (§7.2, §1): the coordinator's `--model` and
    /// `--thinking`, `--workers`, and the lanes' `--lane-model` and `--lane-thinking`.
    pub fn launch_plan(&self, cx: &App) -> LaunchPlan {
        self.choosers.plan(cx)
    }

    /// A folder is chosen: hand the window the launch it asked for — the models, the
    /// efforts and the worker count are fixed when the swarm starts (§7.2). The window
    /// starts the swarm; this only reports the intent.
    pub(crate) fn emit_launch(
        &mut self,
        folder: PathBuf,
        plan: LaunchPlan,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TabContentEvent::Launch { folder, plan });
    }

    /// [`TabContent::emit_launch`] with the plan read from the controls, for a caller that
    /// is not inside this tab's own update.
    pub(crate) fn request_launch(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let plan = self.launch_plan(cx);
        self.emit_launch(folder, plan, cx);
    }

    /// A history row is clicked: resume that session in the folder it ran in (§2).
    pub(crate) fn request_resume(
        &mut self,
        session_path: PathBuf,
        folder: PathBuf,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TabContentEvent::Resume {
            session_path,
            folder,
        });
    }

    /// A folder pick a test injects, in place of the platform dialog. `None` is a
    /// cancelled dialog: the tab stays empty. Test-only, hence the allow.
    #[allow(dead_code)]
    pub(crate) fn set_folder_picker(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| {
            state.set_picker(FolderPicker::Fixed(folder), cx)
        });
    }
}

/// The empty tab's history rows (§2): what `TabContent::history_rows` reads, and what a
/// row's click resumes.
///
/// The rows are drawn by the empty tab itself, from the session model's own rows; this
/// holds the two paths a *resume* needs, and the states before there are any rows.
pub(crate) struct HistoryList {
    rows: Vec<HistoryRow>,
}

impl HistoryList {
    pub(crate) fn new(rows: Vec<HistoryRow>) -> Self {
        HistoryList { rows }
    }

    pub(crate) fn rows(&self) -> &[HistoryRow] {
        &self.rows
    }

    /// One row, by its place in the list: what a caller resumes.
    pub(crate) fn row(&self, row: usize) -> Option<&HistoryRow> {
        self.rows.get(row)
    }

    fn set_rows(&mut self, rows: Vec<HistoryRow>, cx: &mut Context<ListState<Self>>) {
        if self.rows == rows {
            return;
        }
        self.rows = rows;
        cx.notify();
    }
}

impl ListDelegate for HistoryList {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.rows.len()
    }

    /// The rows are the empty tab's own to draw (it wants the design's row, not the
    /// kit's), so this delegate only ever holds them: nothing asks it to render an item.
    fn render_item(
        &mut self,
        _ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        None
    }

    /// The same: there is no list selection here — a row is a button of its own.
    fn set_selected_index(
        &mut self,
        _ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, size, AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions,
    };
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use store::catalog::ModelCheck;

    /// A window wide enough for the 800 px block, and tall enough for the whole page.
    const WINDOW: (f32, f32) = (1200., 800.);

    /// A `/catalog` body as `evo-swarm catalog --json` prints it (§5.6): the default
    /// registration, one every lane may run, one no lane may run, and the ladder.
    fn catalog_body() -> Value {
        serde_json::json!({
            "models": [
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "name": "DeepSeek V4.1",
                 "api": "ark-chat", "context_window": 200000,
                 "reasoning": false, "images": false, "ready": true, "reason": null},
                {"id": "claude-opus-4.5", "provider": "anthropic", "name": "Claude Opus 4.5",
                 "api": "anthropic-messages", "context_window": 1000000,
                 "reasoning": true, "images": true, "ready": true, "reason": null},
                {"id": "claude-sonnet-5", "provider": "proxy", "name": "Claude Sonnet 5",
                 "api": "anthropic-oauth-messages", "context_window": 1000000,
                 "reasoning": true, "images": true, "ready": false, "reason": "no credential"}
            ],
            "default_model": {"id": "claude-opus-4.5", "provider": "anthropic"},
            "thinking_levels": ["low", "medium", "high", "xhigh", "max"],
            "lanes": {"models": [
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "ok": false,
                 "reason": "ark-chat is not an api a lane has"},
                {"id": "claude-opus-4.5", "provider": "anthropic", "ok": true, "reason": null},
                {"id": "claude-sonnet-5", "provider": "proxy", "ok": false,
                 "reason": "api anthropic-oauth-messages is not in a lane"}
            ]},
            "warnings": []
        })
    }

    fn check(model: (&str, &str), lane: (&str, &str), problems: Vec<Problem>) -> CheckReport {
        let one = |(id, provider): (&str, &str)| {
            Some(ModelCheck {
                id: Some(id.to_string()),
                provider: Some(provider.to_string()),
                ok: true,
                reason: None,
            })
        };
        CheckReport {
            ok: problems.is_empty(),
            model: one(model),
            lane_model: one(lane),
            // What a launch with no flags resolves to, which is what the controls open
            // on. A test that wants another answer says so: `CheckReport { thinking:
            // Some("high".into()), ..check(…) }`.
            thinking: Some("medium".to_string()),
            lane_thinking: Some("medium".to_string()),
            workers: Some(6),
            problems,
        }
    }

    struct Fixture {
        window: AnyWindowHandle,
        tab: Entity<TabContent>,
        events: Rc<RefCell<Vec<TabContentEvent>>>,
        _subscription: Subscription,
    }

    impl Fixture {
        fn events(&self) -> Vec<TabContentEvent> {
            self.events.borrow().clone()
        }

        fn act<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("tab window")
        }

        /// The empty tab's own state, for the assertions that are about the model rather
        /// than about what was drawn.
        fn state(&self, cx: &mut TestAppContext) -> Entity<EmptyTabState> {
            cx.update(|cx| self.tab.read(cx).choosers.state.clone())
        }

        fn set_catalog(&self, cx: &mut TestAppContext, catalog: &Value) {
            let state = self.state(cx);
            self.act(cx, |window, cx| {
                state.update(cx, |state, cx| state.set_catalog(catalog, window, cx))
            });
        }

        fn set_check(&self, cx: &mut TestAppContext, report: CheckReport) {
            let state = self.state(cx);
            self.act(cx, |_, cx| {
                state.update(cx, |state, cx| state.set_check_report(report, cx))
            });
        }

        fn history(&self, cx: &mut TestAppContext, entries: &[HistoryEntry], home: &str) {
            let tab = self.tab.clone();
            self.act(cx, |_, cx| {
                tab.update(cx, |tab, cx| {
                    tab.set_history_entries(entries, 1_700_000_000, 0, Some(home), cx)
                })
            });
        }

        fn render(&self, cx: &mut TestAppContext) {
            self.act(cx, |window, cx| window.render_frame(cx));
        }
    }

    fn open(cx: &mut TestAppContext) -> Fixture {
        cx.update(gpui_kit::init);
        let (window, tab) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(WINDOW.0), px(WINDOW.1)),
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    TabContent::new(
                        crate::tab::TabId::new(1),
                        // The check starts no process here: this tab's checks are answered
                        // by `set_check`, and the resolution never reaches a binary.
                        Arc::new(crate::LaunchEnv {
                            swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                            ..crate::LaunchEnv::default()
                        }),
                        window,
                        cx,
                    )
                })
            })
            .expect("tab window")
        });

        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&tab, move |_, event: &TabContentEvent, _| {
                recorded.borrow_mut().push(event.clone())
            })
        });

        Fixture {
            window,
            tab,
            events,
            _subscription: subscription,
        }
    }

    fn entry(session: &str, folder: &str, title: &str, open: bool) -> HistoryEntry {
        HistoryEntry {
            session_path: session.to_string(),
            folder: folder.to_string(),
            title: title.to_string(),
            when: Some(1_700_000_000),
            lanes: Some(4),
            coordinator_model: Some("claude-opus-4.5@anthropic".to_string()),
            lanes_model: None,
            source: session::HistorySource::Index,
            open_at_quit: open,
        }
    }

    /// The page is the design's own markup: the head, the two cards with the folder card
    /// beside them, and the history under them.
    #[gpui_kit::test]
    fn the_page_is_the_designs_own_layout(cx: &mut TestAppContext) {
        let f = open(cx);
        f.render(cx);
        f.act(cx, |window, _| {
            for id in [
                "empty-tab",
                "empty-tab-block",
                "empty-head",
                "launch-layout",
                "role-stack",
                COORDINATOR_CARD_ID,
                WORKERS_CARD_ID,
                COORDINATOR_MODEL_ID,
                WORKERS_MODEL_ID,
                COORDINATOR_EFFORT_ID,
                WORKERS_EFFORT_ID,
                COUNT_ID,
                COUNT_MINUS_ID,
                COUNT_PLUS_ID,
                FOLDER_ID,
                "history",
                "history-head",
            ] {
                assert!(window.find(id).visible(), "{id} is on the page");
            }
            // Nothing has been read yet, so the history says so rather than showing an
            // empty box.
            assert!(window.find(HISTORY_HINT_ID).visible());
            assert!(window.try_find(HISTORY_LIST_ID).is_none());
        });
    }

    /// §7.2: the controls open on what evo resolved, and nothing is labelled "Default".
    #[gpui_kit::test]
    fn the_controls_open_on_the_catalogs_own_resolution(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        f.render(cx);

        let tab = f.tab.clone();
        let state = f.state(cx);
        f.act(cx, |window, cx| {
            let state = state.read(cx);
            // The catalog's own default, for both cards — a lane can register it.
            assert_eq!(
                state.launcher.chosen_key(Card::Coordinator),
                Some("claude-opus-4.5@anthropic")
            );
            assert_eq!(
                state.launcher.chosen_key(Card::Lanes),
                Some("claude-opus-4.5@anthropic")
            );
            // A check has not answered yet, so the sliders sit on the ladder's middle
            // rung and the count on evo's own — the values `check` will replace.
            assert_eq!(state.launcher.level(Card::Coordinator), Some("medium"));
            assert_eq!(state.launcher.level(Card::Lanes), Some("medium"));
            assert_eq!(state.count.read(cx).value().as_ref(), "6");
            window.render_frame(cx);

            // What the tab strip and the tab's own title read from it.
            assert_eq!(
                tab.read(cx).coordinator_model(cx).as_ref(),
                "claude-opus-4.5@anthropic"
            );
            assert_eq!(tab.read(cx).workers(cx).as_ref(), "6");
        });
    }

    /// §2/§7.2: `check --json` resolves the two efforts and the count as a launch with no
    /// flags would, and the controls open on those rather than on a rung the page picked.
    #[gpui_kit::test]
    fn the_controls_open_on_what_check_resolved(cx: &mut TestAppContext) {
        let f = open(cx);
        let folder = PathBuf::from("/Users/you/coding/from-check");
        f.set_catalog(cx, &catalog_body());
        f.set_check(
            cx,
            CheckReport {
                thinking: Some("xhigh".to_string()),
                lane_thinking: Some("low".to_string()),
                workers: Some(12),
                ..check(
                    ("claude-opus-4.5", "anthropic"),
                    ("claude-opus-4.5", "anthropic"),
                    Vec::new(),
                )
            },
        );
        f.render(cx);

        let state = f.state(cx);
        f.act(cx, |window, cx| {
            {
                let state = state.read(cx);
                assert_eq!(state.launcher.level(Card::Coordinator), Some("xhigh"));
                assert_eq!(state.launcher.level(Card::Lanes), Some("low"));
            }
            // The box catches up on the frame the window paints.
            window.render_frame(cx);
            assert_eq!(state.read(cx).count.read(cx).value().as_ref(), "12");
            // The sliders' own thumbs are where those levels put them.
            let rail = window.find("coordinator-effort-rail").bounds();
            let thumb = window.find("coordinator-effort-thumb").bounds();
            let travelled: f32 = (thumb.origin.x - rail.origin.x).into();
            let span: f32 = rail.size.width.into();
            assert!(
                (travelled / span - 0.75).abs() < 0.05,
                "`xhigh` is the fourth of five rungs"
            );
        });

        // A launch from here passes exactly what the controls show.
        let tab = f.tab.clone();
        f.act(cx, |_, cx| {
            tab.update(cx, |tab, cx| {
                tab.set_folder_picker(Some(folder.clone()), cx)
            })
        });
        f.act(cx, |window, cx| window.click(FOLDER_ID, cx));
        assert_eq!(
            f.events(),
            vec![TabContentEvent::Launch {
                folder,
                plan: LaunchPlan {
                    model: Some(("claude-opus-4.5".to_string(), "anthropic".to_string())),
                    thinking: Some("xhigh".to_string()),
                    workers: Some(12),
                    lanes_model: Some(("claude-opus-4.5".to_string(), "anthropic".to_string())),
                    lane_thinking: Some("low".to_string()),
                },
            }]
        );
    }

    /// §7.2: a control the launcher opened moves when `check` answers; a control the person
    /// moved stays where they put it.
    #[gpui_kit::test]
    fn a_control_the_person_moved_is_left_alone(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        f.set_check(
            cx,
            check(
                ("claude-opus-4.5", "anthropic"),
                ("claude-opus-4.5", "anthropic"),
                Vec::new(),
            ),
        );
        f.render(cx);
        let state = f.state(cx);
        // A person drags the coordinator's slider to the end and types a count.
        f.act(cx, |window, cx| {
            state.update(cx, |state, cx| {
                state.launcher.set_effort(Card::Coordinator, 4);
                for _ in 0..3 {
                    state.step_count(true, window, cx);
                }
            });
        });

        // A newer check resolves other values.
        f.set_check(
            cx,
            CheckReport {
                thinking: Some("low".to_string()),
                lane_thinking: Some("low".to_string()),
                workers: Some(3),
                ..check(
                    ("claude-opus-4.5", "anthropic"),
                    ("claude-opus-4.5", "anthropic"),
                    Vec::new(),
                )
            },
        );
        f.act(cx, |window, cx| {
            {
                let state = state.read(cx);
                // The moved ones stand; the one nobody touched follows the check.
                assert_eq!(state.launcher.level(Card::Coordinator), Some("max"));
                assert_eq!(state.launcher.workers(), 9);
                assert_eq!(state.launcher.level(Card::Lanes), Some("low"));
            }
            window.render_frame(cx);
            assert_eq!(state.read(cx).count.read(cx).value().as_ref(), "9");
        });
    }

    /// A check from an evo that did not carry the resolved fields leaves the controls on
    /// evo's own last values for that frame.
    #[gpui_kit::test]
    fn a_check_without_resolved_values_falls_back(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        f.set_check(
            cx,
            CheckReport {
                thinking: None,
                lane_thinking: None,
                workers: None,
                ..check(
                    ("claude-opus-4.5", "anthropic"),
                    ("claude-opus-4.5", "anthropic"),
                    Vec::new(),
                )
            },
        );
        f.render(cx);
        let state = f.state(cx);
        f.act(cx, |_, cx| {
            assert_eq!(
                state.read(cx).launcher.level(Card::Coordinator),
                Some("medium")
            );
            let state = state.read(cx);
            assert_eq!(state.launcher.workers(), 6);
        });
    }

    /// A model a lane cannot register is offered, greyed out, with evo's own reason —
    /// which is also what the field would say if it were the chosen one.
    #[gpui_kit::test]
    fn a_lane_that_cannot_run_a_model_says_why(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        let state = f.state(cx);
        f.act(cx, |_, cx| {
            let state = state.read(cx);
            let lanes = model_items(&state.launcher, Card::Lanes);
            let blocked = lanes
                .iter()
                .find(|item| item.key.as_ref() == "claude-sonnet-5@proxy")
                .expect("the catalog lists it");
            assert!(!blocked.available);
            assert_eq!(
                blocked.detail.as_ref(),
                "api anthropic-oauth-messages is not in a lane"
            );
            // The coordinator card may run it as far as readiness goes… no: evo cannot
            // reach it either, and its own reason says that.
            let coordinators = model_items(&state.launcher, Card::Coordinator);
            let unready = coordinators
                .iter()
                .find(|item| item.key.as_ref() == "claude-sonnet-5@proxy")
                .expect("the catalog lists it");
            assert!(!unready.available);
            assert_eq!(unready.detail.as_ref(), "no credential");
        });
    }

    /// What `check --json` resolved is what the launch would run, so the fields show it —
    /// over the catalog's own default.
    #[gpui_kit::test]
    fn the_check_that_resolved_the_launch_is_what_the_fields_show(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        f.set_check(
            cx,
            check(
                ("ark-deepseek-v4.1-flash", "aiden"),
                ("claude-opus-4.5", "anthropic"),
                Vec::new(),
            ),
        );
        let tab = f.tab.clone();
        f.act(cx, |_, cx| {
            assert_eq!(
                tab.read(cx).coordinator_model(cx).as_ref(),
                "ark-deepseek-v4.1-flash@aiden"
            );
            assert_eq!(
                tab.read(cx).lanes_model(cx).as_ref(),
                "claude-opus-4.5@anthropic"
            );
        });
    }

    /// §14.2's own rule, as the design writes it: a number that cannot be a count is the
    /// nearest count it can be — `Math.max(1, Math.min(64, n || 1))`.
    #[gpui_kit::test]
    fn a_count_typed_out_of_range_is_put_back_in_range(cx: &mut TestAppContext) {
        let f = open(cx);
        let state = f.state(cx);
        let cases = [
            ("99", "64"),
            ("0", "1"),
            ("", "1"),
            ("four", "1"),
            ("7", "7"),
            ("1", "1"),
        ];
        for (typed, expected) in cases {
            f.act(cx, |window, cx| {
                state.update(cx, |state, cx| {
                    state.count.update(cx, |count, cx| {
                        count.set_value(typed.to_string(), window, cx)
                    });
                    state.on_type(&InputEvent::Change, window, cx);
                    assert_eq!(
                        state.launcher.workers().to_string(),
                        expected,
                        "typing {typed:?}"
                    );
                    // The field is put back to what the model took: a box showing `99`
                    // would be lying about the launch.
                    assert_eq!(
                        state.count.read(cx).value().as_ref(),
                        expected,
                        "typing {typed:?}"
                    );
                });
            });
        }
    }

    /// The two steppers walk one count at a time and stop at the ends.
    #[gpui_kit::test]
    fn the_count_steppers_walk_and_stop_at_the_ends(cx: &mut TestAppContext) {
        let f = open(cx);
        let state = f.state(cx);
        f.act(cx, |window, cx| {
            state.update(cx, |state, cx| {
                state.step_count(true, window, cx);
                assert_eq!(state.launcher.workers(), 7);
                assert_eq!(state.count.read(cx).value().as_ref(), "7");
                state.step_count(false, window, cx);
                assert_eq!(state.launcher.workers(), 6);
                // The ends hold.
                state.launcher.set_workers(session::WORKERS_MAX);
                state.step_count(true, window, cx);
                assert_eq!(state.launcher.workers(), session::WORKERS_MAX);
                state.launcher.set_workers(session::WORKERS_MIN);
                state.step_count(false, window, cx);
                assert_eq!(state.launcher.workers(), session::WORKERS_MIN);
            });
        });
    }

    /// §9: the check's own lines sit under the cards, and a click on one puts the keyboard
    /// where the fix is.
    #[gpui_kit::test]
    fn the_checks_lines_sit_under_the_cards_and_open_their_control(cx: &mut TestAppContext) {
        let f = open(cx);
        let state = f.state(cx);
        f.set_catalog(cx, &catalog_body());
        f.set_check(
            cx,
            check(
                ("claude-sonnet-5", "proxy"),
                ("claude-sonnet-5", "proxy"),
                vec![
                    Problem {
                        code: "model_not_ready".to_string(),
                        message: "claude-sonnet-5 has no credential\npick another model"
                            .to_string(),
                    },
                    Problem {
                        code: "lane_model_not_ready".to_string(),
                        message: "a lane cannot register claude-sonnet-5".to_string(),
                    },
                ],
            ),
        );
        f.render(cx);
        f.act(cx, |window, cx| {
            assert!(window.find(PROBLEMS_ID).visible());
            // One line per problem, folded onto one line each.
            assert!(window
                .find(ElementId::NamedInteger(PROBLEM_ID.into(), 0))
                .visible());
            assert!(window
                .find(ElementId::NamedInteger(PROBLEM_ID.into(), 1))
                .visible());
            let line = window.find(ElementId::NamedInteger(PROBLEM_ID.into(), 0));
            assert_eq!(
                line.label(),
                Some("claude-sonnet-5 has no credential pick another model")
            );

            // A click on the first line opens the coordinator's model field.
            window.click(ElementId::NamedInteger(PROBLEM_ID.into(), 0), cx);
            let expected = state.read(cx).coordinator.read(cx).focus_handle(cx);
            assert_eq!(window.focused(cx).as_ref(), Some(&expected));
        });
    }

    /// A catalog that could not be read is one more calm line under the cards — never a
    /// sentence hanging off a field.
    #[gpui_kit::test]
    fn a_catalog_that_could_not_be_read_says_so_under_the_cards(cx: &mut TestAppContext) {
        let f = open(cx);
        let state = f.state(cx);
        f.act(cx, |_, cx| {
            state.update(cx, |state, cx| {
                state.set_catalog_error(Some("http 500: no".to_string()), cx)
            })
        });
        f.render(cx);
        f.act(cx, |window, _| {
            assert!(window.find("catalog-problem").visible());
            assert!(window.find(PROBLEMS_ID).visible());
        });
    }

    /// §2: a row resumes its session in the folder it ran in — and only a row the app had
    /// open wears the badge.
    #[gpui_kit::test]
    fn a_history_row_resumes_its_session_in_its_folder(cx: &mut TestAppContext) {
        let f = open(cx);
        f.history(
            cx,
            &[
                entry("/j/1.sexp", "/Users/you/coding/foo", "make the tab", true),
                entry("/j/2.sexp", "/Users/you/coding/bar", "wire it up", false),
            ],
            "/Users/you",
        );
        f.render(cx);
        assert!(f.events().is_empty());

        f.act(cx, |window, cx| {
            assert!(window.find(HISTORY_LIST_ID).visible());
            assert!(window.find(HISTORY_COUNT_ID).visible());
            assert!(window
                .find(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), 0))
                .visible());
            assert!(window
                .find(ElementId::NamedInteger(HISTORY_ROW_ID.into(), 1))
                .visible());
            // The second row is not the app's own, so it wears no badge.
            assert!(window
                .try_find(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), 1))
                .is_none());
            window.click(ElementId::NamedInteger(HISTORY_ROW_ID.into(), 1), cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Resume {
                session_path: PathBuf::from("/j/2.sexp"),
                folder: PathBuf::from("/Users/you/coding/bar"),
            }]
        );
    }

    /// Before the index arrives the history says what it is doing, and a failure says why.
    #[gpui_kit::test]
    fn the_history_says_what_it_is_doing_before_it_has_rows(cx: &mut TestAppContext) {
        let f = open(cx);
        let tab = f.tab.clone();
        f.act(cx, |_, cx| {
            tab.update(cx, |tab, cx| tab.set_history_loading(true, cx))
        });
        f.render(cx);
        f.act(cx, |window, _| {
            assert!(window.find(HISTORY_HINT_ID).visible());
        });
        f.act(cx, |_, cx| {
            tab.update(cx, |tab, cx| {
                tab.set_history_error(Some("no index".to_string()), cx)
            })
        });
        f.render(cx);
        f.act(cx, |window, _| {
            assert!(window.find(HISTORY_HINT_ID).visible());
        });
    }

    /// §7.2: the folder card launches in the folder it picked, with the flags the controls
    /// show — the two models, the two rungs and the count.
    #[gpui_kit::test]
    fn a_chosen_folder_launches_with_the_resolved_plan(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        let folder = PathBuf::from("/Users/you/coding/foo");
        let tab = f.tab.clone();
        f.act(cx, |_, cx| {
            tab.update(cx, |tab, cx| {
                tab.set_folder_picker(Some(folder.clone()), cx)
            })
        });
        f.render(cx);
        f.act(cx, |window, cx| window.click(FOLDER_ID, cx));

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Launch {
                folder,
                plan: LaunchPlan {
                    model: Some(("claude-opus-4.5".to_string(), "anthropic".to_string())),
                    thinking: Some("medium".to_string()),
                    workers: Some(6),
                    lanes_model: Some(("claude-opus-4.5".to_string(), "anthropic".to_string())),
                    lane_thinking: Some("medium".to_string()),
                },
            }]
        );
    }

    /// The design's own geometry: the folder card is exactly as tall as the two cards
    /// beside it, and both cards put their fields in the same two columns.
    #[gpui_kit::test]
    fn the_folder_card_matches_the_cards_beside_it(cx: &mut TestAppContext) {
        let f = open(cx);
        f.set_catalog(cx, &catalog_body());
        f.render(cx);
        f.act(cx, |window, _| {
            let stack = window.find("role-stack").bounds();
            let folder = window.find(FOLDER_ID).bounds();
            assert_eq!(folder.size.height, stack.size.height);
            assert_eq!(
                folder.origin.x,
                stack.origin.x + stack.size.width + LAYOUT_GAP
            );
            assert_eq!(folder.size.width, FOLDER_COLUMN);

            // The model field takes what is left, the effort field its own 180 — the same
            // column in both cards.
            let coordinator = window
                .find(ElementId::Name(
                    format!("{COORDINATOR_MODEL_ID}-box").into(),
                ))
                .bounds();
            let workers = window
                .find(ElementId::Name(format!("{WORKERS_MODEL_ID}-box").into()))
                .bounds();
            let effort = window.find(COORDINATOR_EFFORT_ID).bounds();
            let effort_below = window.find(WORKERS_EFFORT_ID).bounds();
            assert_eq!(coordinator.origin.x, workers.origin.x);
            assert_eq!(coordinator.size.width, workers.size.width);
            assert_eq!(effort.origin.x, effort_below.origin.x);
            assert_eq!(effort.size.width, effort_below.size.width);
            assert_eq!(effort.size.width, FIELD_COLUMN);
            assert_eq!(coordinator.size.height, SELECT_H);
            assert_eq!(
                effort.origin.x,
                coordinator.origin.x + coordinator.size.width + FIELD_GAP
            );
        });
    }

    /// The sliders are the shared widget, named with this page's own ids, so a test — or a
    /// click — finds the rail and the thumb by them; and a click on the rail snaps to the
    /// level under the pointer, which is what the launch then passes as `--thinking`.
    #[gpui_kit::test]
    fn the_effort_slider_snaps_to_the_level_under_the_pointer(cx: &mut TestAppContext) {
        let f = open(cx);
        // The catalog's ladder, without its retired rung: low, medium, high, xhigh, max.
        f.set_catalog(cx, &catalog_body());
        f.render(cx);
        let state = f.state(cx);
        let tab = f.tab.clone();
        let rail = ElementId::Name(format!("{COORDINATOR_EFFORT_ID}-rail").into());
        let thumb = ElementId::Name(format!("{COORDINATOR_EFFORT_ID}-thumb").into());
        f.act(cx, |window, cx| {
            assert!(window.find(COORDINATOR_EFFORT_ID).visible(), "the slider");
            assert!(window.find(thumb.clone()).visible(), "its thumb");
            // The rail is a zero-height line, so it is observed but not "visible"; what it
            // is for is the geometry a click is measured against.
            let rail = window.try_find(rail.clone()).expect("its rail");
            assert_eq!(rail.bounds().size.height, px(0.));
            // The middle of the slider is the middle of the rail — the rails are inset 8
            // from each end — and that is the middle rung.
            window.click(COORDINATOR_EFFORT_ID, cx);
        });
        f.act(cx, |_, cx| {
            assert_eq!(
                state.read(cx).launcher.level(Card::Coordinator),
                Some("high")
            );
            assert_eq!(
                tab.read(cx).launch_plan(cx).thinking.as_deref(),
                Some("high")
            );
            // The lanes' slider is its own: it did not move with the other one.
            assert_eq!(state.read(cx).launcher.level(Card::Lanes), Some("medium"));
        });
    }

    /// The keyboard starts on the coordinator's model field — the first control on the
    /// page (§7.1's polish).
    #[gpui_kit::test]
    fn focus_primary_lands_on_the_coordinator_field(cx: &mut TestAppContext) {
        let f = open(cx);
        f.render(cx);
        let state = f.state(cx);
        let tab = f.tab.clone();
        f.act(cx, |window, cx| {
            let expected = state.read(cx).coordinator.read(cx).focus_handle(cx);
            let took = tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
            assert!(took, "the field takes the keyboard");
            assert_eq!(window.focused(cx).as_ref(), Some(&expected));
            // The ring is the box around the field: the element carrying the control's
            // own handle, painted from the handle's focus — which is what the design's
            // `:focus-within` is.
            // The ring itself is paint, and a headless window paints no focus: what the
            // test can pin is that the box the design rings is the one carrying the
            // control's own handle, and that the keyboard is on it.
            window.render_frame(cx);
            let box_ = window.find(ElementId::Name(
                format!("{COORDINATOR_MODEL_ID}-box").into(),
            ));
            assert!(box_.visible());
            let other = window.find(ElementId::Name(format!("{WORKERS_MODEL_ID}-box").into()));
            assert!(other.visible());
            assert_eq!(box_.bounds().origin.x, other.bounds().origin.x);
        });
    }
}
