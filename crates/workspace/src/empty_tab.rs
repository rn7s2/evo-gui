//! The empty tab (§7.2): what a new tab shows before a folder is chosen.
//!
//! One centred column — a header, the three choosers with the folder button beside them,
//! then the resumable swarms — and nothing else. The choosers' options, the note under the
//! lanes select and the history rows all come from [`session::Launcher`]; this module owns
//! only what the session model cannot know: the select widgets, the folder dialog, whether
//! the catalog has arrived, and whether the history scan is still running.
//!
//! ```text
//! set_catalog                      the /catalog body (§5.6): the cache, or a server
//! set_history_entries              the session index's rows (§2)
//! set_history_loading / set_catalog_error  the two states with nothing to show yet
//! → TabContentEvent::Launch        the folder plus the choosers' plan (§7.2, §1)
//! → TabContentEvent::Resume        a history row, by lane 1's ListEvent subscription
//! ```

use std::path::PathBuf;

use gpui_kit::base::Button;
use gpui_kit::component::list::{List, ListDelegate, ListItem, ListState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Colorize as _, Icon, IconName, IndexPath, Sizable as _,
    StyledExt as _, Theme,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, AnyElement, App, Context, ElementId, Entity, FocusHandle, Focusable as _,
    Hsla, IntoElement, KeyDownEvent, Pixels, Role, SharedString, Subscription, TestSupportExt as _,
    WeakEntity, Window,
};
use serde_json::Value;
use session::{Choice, ChooserOption, HistoryEntry, LaunchPlan, Launcher, DEFAULT_KEY};
use store::catalog::{CheckReport, Problem, ProblemTarget};
use store::cli;

use crate::history::{rows_from_session, HistoryRow};
use crate::tab::{TabContent, TabContentEvent};

/// The launcher's measure (§7.2): one centred column, wide enough for the three rows and
/// the folder button beside them.
const BLOCK_WIDTH: Pixels = px(880.);

gpui_kit::actions!(
    workspace,
    [
        /// Open the app's Settings panel — what the "evo-swarm not found" line under the
        /// folder card does (§9.7).
        ///
        /// The panel and the paths in it are the *app's*; the workspace only knows that the
        /// person asked for it. So the empty tab, which is what draws the line, declares the
        /// action, and the app answers it (`evo_desktop`'s Settings… handler).
        OpenSettings,
    ]
);

/// How far down the block starts, as a fraction of the window: the top of a 1000 px window
/// is not where the eye should land.
const BLOCK_TOP: f32 = 0.12;

/// A chooser row: a fixed label column, then the select.
const LABEL_WIDTH: Pixels = px(150.);
const SELECT_WIDTH: Pixels = px(420.);
/// The popup is wider than the trigger because an option carries its detail line.
const MENU_WIDTH: Pixels = px(460.);

/// The gap between the chooser rows, and between them and the folder button.
const ROW_GAP: Pixels = px(16.);

/// The caption under the lanes chooser. It reserves **two** lines whether or not it has
/// something to say, so the three rows never shift when a lanes model is chosen and the note
/// — a folder's path plus what it means — wraps instead of truncating. It starts where the
/// selects do, not under the labels. A minimum rather than a fixed height, so a theme with a
/// taller line never clips the second line.
const CAPTION_LINES: usize = 2;
const CAPTION_HEIGHT: Pixels = px(40.);
/// [`LABEL_WIDTH`] plus the row's own gap: the selects' left edge.
const CAPTION_INDENT: Pixels = px(166.);
const CAPTION_ID: &str = "lanes-caption";

/// The check's problem lines under the choosers (§9): one calm line each, and a click opens
/// the chooser the line is about. There are none when the launch is fine.
const PROBLEMS_ID: &str = "check-problems";
const PROBLEM_ID: &str = "check-problem";

/// What the caption says when the catalog could not be fetched at all (§5.6): one sentence
/// about what the tab still does, because the server's own words — `http 500: The value
/// "Bearer …"` — are evidence, not a message. They go in the line's tooltip and in
/// `app.log`; the choosers stay usable on Default, so a swarm can still be started.
const CATALOG_FAILED: &str = "Couldn't load the model list — Default models will be used.";
/// … and when the catalog could not be fetched but the last one is still in the choosers.
const CATALOG_STALE: &str = "Couldn't refresh the model list — using the last one it loaded.";

/// The caption's warning tone, for the one line that is not the page's quiet grey.
///
/// The kit's `warning` is a bright amber: 13:1 on the dark theme's near-black, but 1.9:1 on
/// the light theme's white — a 12px line nobody can read. The light theme darkens that same
/// hue until it clears AA (`#EAB308` → `#8D6C05`, 4.9:1); the tone stays amber, which is what
/// it means here: not an error, something to notice.
fn warning_ink(theme: &Theme) -> Hsla {
    if theme.is_dark() {
        theme.warning
    } else {
        theme.warning.darken(0.4)
    }
}

/// The caption's own shape: the one line it shows, the tone it wears, and the longer version
/// of itself for the hover.
struct Caption {
    text: SharedString,
    tone: CaptionTone,
    /// The words behind the line, when it is a summary of something longer: the catalog
    /// command's own error.
    detail: Option<SharedString>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptionTone {
    /// Nothing to say yet: the catalog is on its way.
    Loading,
    /// Something went wrong that the tab copes with (§5.6) — quiet amber, never red.
    Warning,
    /// Every lane will run the model chosen here (§1).
    Note,
}

/// The folder call to action, and how its parts are drawn.
const FOLDER_ID: &str = "select-folder";
const FOLDER_ICON_SIZE: Pixels = px(28.);

/// The line under the folder card when nothing can be launched at all: the swarm binary
/// `app.json` names is not one that runs (§9.7), so the tab says it here instead of leaving
/// it to the first launch to find out. It is also the id a test clicks to open Settings.
const SWARM_MISSING_ID: &str = "swarm-missing";
/// Two lines of it, then the tooltip: the line names a path, and a path can be long.
const SWARM_MISSING_LINES: usize = 2;

/// The history region and its states.
const HISTORY_ID: &str = "history";
const HISTORY_ROW_ID: &str = "history-row";
const HISTORY_HINT_ID: &str = "history-hint";
/// What the history section is called, for a screen reader: the list's items name
/// themselves, but the list around them has no name of its own.
const HISTORY_LABEL: &str = "Resumable swarms";
/// A history row reads like a document: a small folder glyph, then the folder's own name as
/// the title, with the path and the facts under it.
const ROW_ICON_SIZE: Pixels = px(14.);
const ROW_TITLE_SIZE: Pixels = px(15.);
/// The badge on a row the app had open when it last quit (§2).
const OPEN_AT_QUIT_ID: &str = "history-open-at-quit";
const OPEN_AT_QUIT_TEXT: &str = "open at last quit";

/// One option of a chooser, as the Select draws it: the label, a muted detail line under
/// it, and — for a model a lane could not register — the reason it is greyed out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChooserItem {
    /// Stable identity of the option: what the launcher is told was chosen.
    key: SharedString,
    label: SharedString,
    /// The detail under the label: the context window and what else the catalog knows, or
    /// the reason the option cannot be used.
    detail: SharedString,
    available: bool,
}

impl From<&ChooserOption> for ChooserItem {
    fn from(option: &ChooserOption) -> Self {
        ChooserItem {
            key: SharedString::from(option.key.clone()),
            label: SharedString::from(option.label.clone()),
            detail: SharedString::from(match &option.unavailable_reason {
                Some(reason) => reason.clone(),
                None => option.detail.clone(),
            }),
            available: option.available,
        }
    }
}

impl SelectItem for ChooserItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.key
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query) || self.detail.to_lowercase().contains(&query)
    }

    /// A lanes model a lane cannot register is shown, not hidden: seeing why is the point
    /// (§5.6).
    fn disabled(&self) -> bool {
        !self.available
    }

    fn render(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let detail_color = if self.available {
            cx.theme().muted_foreground
        } else {
            cx.theme().warning
        };
        let detail = self.detail.clone();
        v_flex()
            .gap_0p5()
            .py_0p5()
            .child(div().child(self.label.clone()))
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

/// The empty tab's model and the widgets over it.
///
/// It is an entity of its own so the selects' own events can update the model: a click in a
/// chooser moves [`session::Launcher`]'s selection, which is what the caption and the launch
/// plan read.
pub(crate) struct Choosers {
    state: Entity<EmptyTabState>,
}

impl Choosers {
    /// The three choosers need the window they will be rendered in, and the tab they will
    /// report a launch to.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<TabContent>) -> Self {
        let tab = cx.weak_entity();
        Choosers {
            state: cx.new(|cx| EmptyTabState::new(window, tab, cx)),
        }
    }

    /// The chosen coordinator model's label (`Default` untouched) — the tab strip's own
    /// language, and what §3's `--model` would take.
    pub(crate) fn coordinator_model(&self, cx: &App) -> SharedString {
        chosen_label(&self.state, Choice::Coordinator, cx)
    }

    /// The chosen lanes model's label.
    pub(crate) fn lanes_model(&self, cx: &App) -> SharedString {
        chosen_label(&self.state, Choice::Lanes, cx)
    }

    /// The chosen worker count's label (`Default` leaves it to evo).
    pub(crate) fn workers(&self, cx: &App) -> SharedString {
        chosen_label(&self.state, Choice::Workers, cx)
    }

    /// What the choosers add up to (§7.2, §1).
    pub(crate) fn plan(&self, cx: &App) -> LaunchPlan {
        self.state.read(cx).launcher.plan()
    }

    /// Put the keyboard where the empty tab begins: the coordinator chooser — the
    /// first tab stop of the three rows — or, if that chooser cannot take the
    /// focus, the folder card beside them.
    ///
    /// The window is the owner's: selecting an empty tab lands the keyboard in it
    /// (§7.1's polish), which is the window's own `TabContent::focus_primary` to
    /// call. Answers whether anything took the focus; a window that takes no focus
    /// at all (a capture, a window on its way out) answers `false` and is left
    /// alone.
    ///
    /// The seam is what the window calls: `TabContent::focus_primary` routes an
    /// empty tab's keyboard here (§7.1's polish).
    pub fn focus_primary(&self, window: &mut Window, cx: &mut App) -> bool {
        // Read out of the state first: focusing borrows the window and the app, and
        // the state is borrowed through both.
        let (chooser, folder) = {
            let state = self.state.read(cx);
            (state.coordinator.clone(), state.folder_focus.clone())
        };
        let primary = chooser.read(cx).focus_handle(cx);
        window.focus(&primary, cx);
        if window.focused(cx).as_ref() == Some(&primary) {
            return true;
        }
        // The first chooser would not have the keyboard: the folder card is the next
        // thing on the tab that can.
        window.focus(&folder, cx);
        window.focused(cx).as_ref() == Some(&folder)
    }
}

fn chosen_label(state: &Entity<EmptyTabState>, which: Choice, cx: &App) -> SharedString {
    state
        .read(cx)
        .launcher
        .selected(which)
        .map(|option| SharedString::from(option.label.clone()))
        .unwrap_or_else(|| SharedString::from(DEFAULT_KEY))
}

struct EmptyTabState {
    /// The choosers' model: options, the chosen keys, and the history rows (§7.2, §2).
    launcher: Launcher,
    coordinator: Entity<SelectState<Vec<ChooserItem>>>,
    lanes: Entity<SelectState<Vec<ChooserItem>>>,
    lane_thinking: Entity<SelectState<Vec<ChooserItem>>>,
    workers: Entity<SelectState<Vec<ChooserItem>>>,
    /// The home directory the `~` paths are shortened around.
    home: Option<String>,
    /// The catalog has arrived, from the cache or from a server: until it has, the model
    /// choosers hold nothing but Default.
    catalog: bool,
    /// The catalog could not be read: shown instead of the loading hint.
    catalog_error: Option<String>,
    /// The swarm binary cannot be run at all (§9.7): the line under the folder card. The
    /// app's words, so the tab does not have to know what a `--version` is.
    swarm_problem: Option<String>,
    /// The `evo-swarm` a check runs. The app's own path from Settings, so the check is
    /// about the swarm this app would really spawn.
    swarm_bin: PathBuf,
    /// What the last check found wrong with the launch the choosers describe (§9). One line
    /// each, in evo's own words; empty when the launch is fine.
    problems: Vec<Problem>,
    /// Bumped on every check asked: an answer that is no longer the newest is dropped before
    /// it is applied.
    check_revision: u64,
    /// Where a check's answer comes from.
    check_probe: CheckProbe,
    /// Where a folder pick answers from.
    picker: FolderPicker,
    /// The folder card's own focus handle, so keyboard traversal and the focus ring have
    /// somewhere to land.
    folder_focus: FocusHandle,
    history_focus: FocusHandle,
    /// The tab that owns this state: what a launch is emitted on.
    tab: WeakEntity<TabContent>,
    _subscriptions: Vec<Subscription>,
}

/// Where a check's answer comes from: the swarm binary, or an answer already known.
#[derive(Clone, Default)]
enum CheckProbe {
    /// Run it: `evo-swarm check --json`, on the app's own executor.
    #[default]
    Command,
    /// Answer from this list, starting no process. For tests, which must not wait on a
    /// swarm binary.
    Fixed(Vec<Problem>),
}

impl EmptyTabState {
    fn new(window: &mut Window, tab: WeakEntity<TabContent>, cx: &mut Context<Self>) -> Self {
        let launcher = Launcher::new();
        let coordinator = chooser_state(&launcher, Choice::Coordinator, window, cx);
        let lanes = chooser_state(&launcher, Choice::Lanes, window, cx);
        let lane_thinking = chooser_state(&launcher, Choice::LaneThinking, window, cx);
        let workers = chooser_state(&launcher, Choice::Workers, window, cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &coordinator,
                window,
                |this, _, event: &SelectEvent<Vec<ChooserItem>>, window, cx| {
                    this.on_choose(Choice::Coordinator, event, window, cx)
                },
            ),
            cx.subscribe_in(
                &lanes,
                window,
                |this, _, event: &SelectEvent<Vec<ChooserItem>>, window, cx| {
                    this.on_choose(Choice::Lanes, event, window, cx)
                },
            ),
            cx.subscribe_in(
                &lane_thinking,
                window,
                |this, _, event: &SelectEvent<Vec<ChooserItem>>, window, cx| {
                    this.on_choose(Choice::LaneThinking, event, window, cx)
                },
            ),
            cx.subscribe_in(
                &workers,
                window,
                |this, _, event: &SelectEvent<Vec<ChooserItem>>, window, cx| {
                    this.on_choose(Choice::Workers, event, window, cx)
                },
            ),
        ];
        EmptyTabState {
            launcher,
            coordinator,
            lanes,
            lane_thinking,
            workers,
            home: std::env::var("HOME").ok(),
            catalog: false,
            catalog_error: None,
            swarm_problem: None,
            // The app hands its own path in as soon as it can; until then this is where the
            // installed binary is (§1).
            swarm_bin: cli::swarm_bin(),
            problems: Vec::new(),
            check_revision: 0,
            check_probe: CheckProbe::default(),
            picker: FolderPicker::Dialog,
            folder_focus: cx.focus_handle(),
            history_focus: cx.focus_handle().tab_stop(true),
            tab,
            _subscriptions: subscriptions,
        }
    }

    /// The select widget of one chooser row: what a click on a problem line focuses.
    fn select(&self, which: Choice) -> Entity<SelectState<Vec<ChooserItem>>> {
        match which {
            Choice::Coordinator => self.coordinator.clone(),
            Choice::Lanes => self.lanes.clone(),
            Choice::LaneThinking => self.lane_thinking.clone(),
            Choice::Workers => self.workers.clone(),
        }
    }

    /// A chooser committed an option. The launcher is the one who decides whether it means
    /// anything: a rebuilt chooser can drop a choice that no longer exists.
    fn on_choose(
        &mut self,
        which: Choice,
        event: &SelectEvent<Vec<ChooserItem>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(key)) = event else {
            return;
        };
        if self.launcher.select(which, key) {
            // A different launch is a different answer: ask again (§9).
            self.run_check(cx);
            cx.notify();
        }
    }

    /// Rebuild the selects from the launcher: the catalog arrived, or the project's worker
    /// count did.
    fn sync_choosers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for which in [
            Choice::Coordinator,
            Choice::Lanes,
            Choice::LaneThinking,
            Choice::Workers,
        ] {
            let state = self.select(which);
            let items: Vec<ChooserItem> = self
                .launcher
                .chooser(which)
                .options
                .iter()
                .map(ChooserItem::from)
                .collect();
            let selected = self
                .launcher
                .chooser(which)
                .index_of(self.launcher.selected_key(which))
                .map(|row| IndexPath::default().row(row));
            state.update(cx, |state, cx| {
                state.set_items(items, window, cx);
                state.set_selected_index(selected, window, cx);
            });
        }
    }

    /// The catalog: the `/catalog` body (§5.6) the disk cache holds, or the one the running
    /// server just answered with.
    ///
    /// The body carries everything the choosers need — the models with their `ready`/`reason`,
    /// and the `lanes.models` list with evo's own `ok`/`reason` for each registration a lane
    /// may run — so nothing here has to compare API sets.
    fn set_catalog(&mut self, catalog: &Value, window: &mut Window, cx: &mut Context<Self>) {
        self.catalog = true;
        self.catalog_error = None;
        self.launcher.set_catalog(catalog);
        self.sync_choosers(window, cx);
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

    /// What a check found, without running one: the tab's answer for a test.
    #[allow(dead_code)]
    fn set_problems(&mut self, problems: Vec<Problem>, cx: &mut Context<Self>) {
        self.check_probe = CheckProbe::Fixed(problems);
        self.run_check(cx);
    }

    /// Ask `evo-swarm check --json` about the launch the choosers describe (§9), off the
    /// thread that draws: are the models resolvable, can a lane reach its API, is the key
    /// there. The answer is the choosers' state — the lines this tab shows under them.
    ///
    /// A check that cannot run at all (no binary, a crash) is not a problem with the launch:
    /// the tab then shows nothing, and the line under the folder card is the app's own
    /// answer to a binary that does not run (§9.7).
    fn run_check(&mut self, cx: &mut Context<Self>) {
        let spec = crate::launch::check_spec(&self.launcher.plan());
        self.check_revision += 1;
        let revision = self.check_revision;

        if let CheckProbe::Fixed(problems) = self.check_probe.clone() {
            self.settle(&problems, revision, cx);
            return;
        }

        let bin = self.swarm_bin.clone();
        let argv = spec.check_argv();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            // A process, but not on the thread that draws: the app's own executor runs it.
            let problems = cx
                .background_executor()
                .spawn(async move {
                    match cli::run_json(&bin, &argv) {
                        Ok(body) => CheckReport::from_json(&body).problems,
                        Err(_) => Vec::new(),
                    }
                })
                .await;
            let _ = this.update(cx, |state, cx| state.settle(&problems, revision, cx));
        })
        .detach();
    }

    /// Take a check's answer, if it is still the answer to the newest question asked.
    fn settle(&mut self, problems: &[Problem], revision: u64, cx: &mut Context<Self>) {
        if self.check_revision != revision || self.problems == problems {
            return;
        }
        self.problems = problems.to_vec();
        cx.notify();
    }

    /// A click on a problem line: the chooser the line is about takes the keyboard — so the
    /// arrows are already on it — or, for anything about the machine rather than the launch,
    /// the app's Settings panel opens.
    fn open_target(&mut self, target: ProblemTarget, window: &mut Window, cx: &mut Context<Self>) {
        // A problem about the machine the swarm would run on — a binary that is not there —
        // is not about anything on this screen: the app's Settings panel is where it is
        // fixed (§13, §9.7).
        let Some(which) = (match target {
            ProblemTarget::Model => Some(Choice::Coordinator),
            ProblemTarget::LaneModel => Some(Choice::Lanes),
            ProblemTarget::Thinking => Some(Choice::LaneThinking),
            ProblemTarget::Workers => Some(Choice::Workers),
            ProblemTarget::Other => None,
        }) else {
            window.dispatch_action(Box::new(OpenSettings), cx);
            return;
        };
        let handle = self.select(which).read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn set_catalog_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.catalog_error = error.filter(|error| !error.trim().is_empty());
        cx.notify();
    }

    /// The swarm binary cannot be run (§9.7): the line under the folder card, or `None`
    /// once Settings points at one that runs.
    fn set_swarm_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        self.swarm_problem = problem.filter(|problem| !problem.trim().is_empty());
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
    /// The dialog is `rfd`'s async one: the UI thread neither blocks on it nor waits for it,
    /// its answer arrives on the GPUI foreground executor, and cancelling it leaves the tab
    /// empty.
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

    /// The caption under the lanes chooser: the one place the empty tab says something went
    /// wrong, something is still loading, or what a chosen lanes model will do.
    fn caption(&self) -> Option<Caption> {
        if let Some(error) = &self.catalog_error {
            // The server's own words are evidence, not a message: they go in the hover (and
            // in `app.log`, where the app wrote them) and the line says what the tab does
            // about it — nothing, which is why it still works: every chooser is on Default,
            // and the folder card opens a swarm from there.
            let text = if self.catalog {
                // A cache was in use, so the last catalog is still in the choosers.
                CATALOG_STALE
            } else {
                CATALOG_FAILED
            };
            return Some(Caption {
                text: SharedString::from(text),
                tone: CaptionTone::Warning,
                detail: Some(SharedString::from(error.clone())),
            });
        }
        if !self.catalog {
            return Some(Caption {
                text: SharedString::from("Loading models…"),
                tone: CaptionTone::Loading,
                detail: None,
            });
        }
        let lanes = self.launcher.selected(Choice::Lanes)?;
        if lanes.key == DEFAULT_KEY {
            // Default passes no flag: every lane runs what the coordinator runs, or what the
            // project's own `swarm.lisp` says (§1).
            return None;
        }
        // A lane's model is a launch flag now — the app never writes the project's
        // `swarm.lisp` — so the note says what the flag means (§1, F3).
        Some(Caption {
            text: SharedString::from(format!("Every lane runs {} (--lane-model)", lanes.key)),
            tone: CaptionTone::Note,
            detail: None,
        })
    }

    fn header(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_lg().font_semibold().child("New swarm"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Pick models, then a folder"),
            )
    }

    /// One chooser row: the label, then the select.
    fn chooser_row(
        &self,
        label: &'static str,
        id: &'static str,
        state: &Entity<SelectState<Vec<ChooserItem>>>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_4()
            // Space opens the menu, the way Enter and the arrows already do (`gpui-base` binds
            // those in the select's own key context, and nothing binds Space): the key arrives
            // here from the select's trigger, which is inside this row.
            .on_key_down(cx.listener(Self::chooser_key))
            .child(
                div()
                    .w(LABEL_WIDTH)
                    .flex_none()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Select::new(state)
                    .id(id)
                    .w(SELECT_WIDTH)
                    .menu_width(MENU_WIDTH)
                    .accessibility_label(label),
            )
    }

    /// The chooser's own keys. Only Space is missing from the kit's bindings: the arrows
    /// open a closed select, `enter` opens it, and `escape` closes it without the tab going
    /// anywhere — all of those are the select's own actions, dispatched to the focused
    /// control. Space is the one key a user is as likely to try, so it takes the same path
    /// the other two do: the select's `Confirm` action, which opens the menu on the value it
    /// already has instead of moving the highlight.
    fn chooser_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "space" {
            return;
        }
        // The focused select opens itself; this row only knows the key was pressed inside it.
        window.dispatch_action(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            cx,
        );
        cx.stop_propagation();
    }

    fn render_choosers(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_stretch()
            .gap_6()
            .child(
                v_flex()
                    // The chooser column, and the button takes exactly what is left of the
                    // block beside it.
                    .w(px(586.))
                    .flex_none()
                    .gap(ROW_GAP)
                    .child(self.chooser_row(
                        "Coordinator model",
                        "coordinator-model",
                        &self.coordinator,
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(self.chooser_row("Lanes model", "lanes-model", &self.lanes, cx))
                            .child(self.render_caption(cx)),
                    )
                    .child(self.chooser_row(
                        "Lane thinking",
                        "lane-thinking",
                        &self.lane_thinking,
                        cx,
                    ))
                    .child(self.chooser_row("Workers", "workers", &self.workers, cx))
                    .when_some(self.render_problems(cx), |rows, problems| {
                        rows.child(problems)
                    }),
            )
            .child(
                v_flex()
                    // The folder card's column: the card, and under it the one line that
                    // says nothing can be launched from any of this yet (§9.7).
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(self.render_folder_button(cx))
                    .when_some(
                        self.swarm_problem.clone().map(SharedString::from),
                        |column, problem| column.child(self.render_swarm_problem(problem, cx)),
                    ),
            )
    }

    /// What `evo-swarm check --json` found wrong with this launch (§9): one calm line each,
    /// and a click opens the chooser the line is about. Nothing at all when the launch is
    /// fine, which is the usual case.
    fn render_problems(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.problems.is_empty() {
            return None;
        }
        let ink = warning_ink(cx.theme());
        let lines: Vec<AnyElement> =
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
                        .text_xs()
                        .text_color(ink)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        // What a screen reader hears, and what the hover shows: the message
                        // evo wrote, one line.
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
        Some(
            v_flex()
                .id(PROBLEMS_ID)
                .test_support()
                .ml(CAPTION_INDENT)
                .gap_1()
                .children(lines)
                .into_any_element(),
        )
    }

    /// The line under the folder card when the swarm binary cannot be run at all (§9.7).
    ///
    /// Nothing starts until the path in Settings points at a real `evo-swarm`, and this is
    /// the screen the person is on when they would otherwise find that out by picking a
    /// folder. So it wears the caption's own warning tone, says the path, and opens Settings
    /// when it is clicked — the panel is the app's, and the action it answers is declared
    /// above ([`OpenSettings`]).
    fn render_swarm_problem(&self, problem: SharedString, cx: &Context<Self>) -> impl IntoElement {
        // The line is elided to two lines, so what it says in full is what it hovers.
        let hovered = problem.clone();
        div()
            .id(SWARM_MISSING_ID)
            .test_support()
            .w_full()
            .min_w_0()
            .text_xs()
            .text_color(warning_ink(cx.theme()))
            .line_clamp(SWARM_MISSING_LINES)
            .text_ellipsis()
            .cursor_pointer()
            .hover(|style| style.underline())
            // What a screen reader hears: the line itself, which the visible one may have
            // had to cut short.
            .aria_label(problem.clone())
            .tooltip(move |window, cx| {
                Tooltip::new(hovered.clone())
                    .max_w(px(460.))
                    .build(window, cx)
            })
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(OpenSettings), cx);
            })
            .child(problem)
    }

    fn render_caption(&self, cx: &Context<Self>) -> impl IntoElement {
        let caption = self.caption();
        // The one tone that is not the page's quiet grey: a catalog that could not be read
        // is worth noticing, and it is not a mistake the person made — amber, not red.
        let color = match caption.as_ref().map(|caption| caption.tone) {
            Some(CaptionTone::Warning) => warning_ink(cx.theme()),
            _ => cx.theme().muted_foreground,
        };
        // What the hover says: the server's own error where there is one, the line itself
        // otherwise — a note that had to be elided still has to be readable in full.
        let tooltip = caption.as_ref().map(|caption| {
            caption
                .detail
                .clone()
                .unwrap_or_else(|| caption.text.clone())
        });
        div()
            .id(CAPTION_ID)
            .test_support()
            .ml(CAPTION_INDENT)
            .min_w_0()
            .min_h(CAPTION_HEIGHT)
            .flex_none()
            // Two lines, then an ellipsis: `line_clamp` alone would simply cut the second
            // line mid-word at the box edge, so the caption asks for the overflow ellipsis
            // too — that is the pair GPUI renders as a clamped, ellipsized block.
            .line_clamp(CAPTION_LINES)
            .text_ellipsis()
            .text_xs()
            .text_color(color)
            .when_some(tooltip, |line, tooltip| {
                line.tooltip(move |window, cx| {
                    Tooltip::new(tooltip.clone())
                        .max_w(px(460.))
                        .build(window, cx)
                })
            })
            .children(caption.map(|caption| caption.text))
    }

    /// The folder call to action (§7.2): the three choosers add up to one decision, so it is
    /// drawn as the block they lead to — a quiet card, an icon, the label and what picking a
    /// folder means, exactly as tall as the rows beside it.
    fn render_folder_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let surface = theme.secondary;
        let surface_hover = card_hover_fill(theme);
        let border = theme.border;
        let accent = theme.primary;
        let ring = theme.ring;
        // The icon is the one spot of colour on the screen, so it takes the theme's blue-ish
        // info tone: a near-black or near-white `primary` would be a slab (light) or the
        // whole card in the foreground colour (dark).
        let icon_color = theme.info;
        Button::new(FOLDER_ID)
            .track_focus(&self.folder_focus)
            .accessibility_label("Select folder…")
            // The card is the column's fill: it takes the height the chooser rows beside it
            // give, less whatever the line under it needs (§9.7).
            .w_full()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(border)
            .bg(surface)
            .hover(move |style| style.bg(surface_hover).border_color(accent))
            // Keyboard focus strengthens the surface the same way and takes the theme's
            // focus ring — a tab stop should read as focus, not as a second hover.
            .focus_visible(move |style| style.border_color(ring).bg(surface_hover))
            .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx)))
            .child(
                Icon::new(IconName::Folder)
                    .with_size(FOLDER_ICON_SIZE)
                    .text_color(icon_color),
            )
            .child(div().text_sm().font_medium().child("Select folder…"))
            .child(
                div()
                    .text_xs()
                    .text_color(card_hint_color(theme))
                    .child("The swarm starts in the folder you pick"),
            )
    }
}

impl Render for EmptyTabState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_6()
            .child(self.header(cx))
            .child(self.render_choosers(cx))
    }
}

/// How much of the card's own foreground the hint line keeps: a step down from the label, so
/// the two lines still read as a label and a hint.
const CARD_HINT_STRENGTH: f32 = 0.75;

/// The folder card's hint line. The card is filled with `secondary`, so its text is that
/// tone's own foreground — but at full strength the hint would weigh the same as the label
/// above it, and the muted grey is too faint to read on a filled card.
fn card_hint_color(theme: &Theme) -> Hsla {
    theme.secondary_foreground.opacity(CARD_HINT_STRENGTH)
}

/// The folder card's fill under the pointer. Hover strengthens the surface as well as the
/// border, and the light theme's `secondary_hover` is the very tone `secondary` already is
/// (both neutral-200), so the next stronger neutral is what actually moves the fill there.
/// The dark theme's own hover tone does.
fn card_hover_fill(theme: &Theme) -> Hsla {
    if theme.is_dark() {
        theme.secondary_hover
    } else {
        theme.secondary_active
    }
}

/// A chooser's select, built from the launcher's options.
fn chooser_state(
    launcher: &Launcher,
    which: Choice,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SelectState<Vec<ChooserItem>>> {
    let items: Vec<ChooserItem> = launcher
        .chooser(which)
        .options
        .iter()
        .map(ChooserItem::from)
        .collect();
    // Default is the first option, so a fresh tab starts on it (§7.2).
    cx.new(|cx| SelectState::new(items, Some(IndexPath::default()), window, cx))
}

impl TabContent {
    /// The empty tab (§7.2): the launcher block, then the resumable swarms.
    pub(crate) fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("empty-tab")
            .test_support()
            .size_full()
            .px_6()
            .items_center()
            // A tenth of the window keeps the block off the title bar, and the block then
            // fills what is left so the history list scrolls inside it.
            .child(div().h(relative(BLOCK_TOP)).flex_none())
            .child(
                v_flex()
                    .id("empty-tab-block")
                    .test_support()
                    .w_full()
                    .max_w(BLOCK_WIDTH)
                    .flex_1()
                    .min_h_0()
                    .gap_6()
                    .child(self.choosers.state.clone())
                    .child(self.render_history(cx)),
            )
            .into_any_element()
    }

    /// The history list's own keys: the List draws and handles its items, but it does not
    /// take part in tab traversal, so the frame around it holds the focus (§7.2's keyboard
    /// order) and the arrows and Enter are handled here — the same two things a click does.
    fn history_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.history.read(cx).delegate().rows().len();
        let current = self.history.read(cx).selected_index();
        match event.keystroke.key.as_str() {
            // Home and End are the ends of the list, the same way an arrow is one step of it.
            "home" | "end" | "down" | "up" => {
                if rows == 0 {
                    return;
                }
                let key = event.keystroke.key.as_str();
                let row = match (key, current) {
                    ("home", _) => 0,
                    ("end", _) => rows - 1,
                    (_, Some(ix)) => {
                        let step: isize = if key == "down" { 1 } else { -1 };
                        ix.row.saturating_add_signed(step).min(rows - 1)
                    }
                    // Nothing selected yet: an arrow starts at the top, or the bottom when
                    // it points up.
                    (_, None) if key == "up" => rows - 1,
                    (_, None) => 0,
                };
                self.history.update(cx, |state, cx| {
                    state.set_selected_index(Some(IndexPath::default().row(row)), window, cx)
                });
            }
            "enter" => {
                let Some(row) = current.and_then(|ix| {
                    self.history
                        .read(cx)
                        .delegate()
                        .row(ix.row)
                        .map(|row| (row.session_path.clone(), row.folder.clone()))
                }) else {
                    return;
                };
                cx.emit(TabContentEvent::Resume {
                    session_path: row.0,
                    folder: row.1,
                });
            }
            _ => {}
        }
    }

    /// The resumable swarms, newest first (§2), with the two states that have no rows.
    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.history.read(cx).delegate();
        let rows = state.rows().len();
        let count = match (rows, state.scanning()) {
            (0, true) => SharedString::default(),
            (0, false) => SharedString::default(),
            (1, _) => SharedString::from("1 resumable"),
            (n, _) => SharedString::from(format!("{n} resumable")),
        };
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("History"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    ),
            )
            .child(
                div()
                    .id(HISTORY_ID)
                    .test_support()
                    // The list's own focus handle, tracked here as well: the List draws and
                    // handles keys, but it does not register itself as a tab stop, so
                    // tabbing would never reach it (§7.2's keyboard order).
                    .track_focus(&self.choosers.state.read(cx).history_focus)
                    .tab_stop(true)
                    // The frame is what has the focus, so the frame is what carries the
                    // section's name: the List inside names its own items but not itself.
                    .role(Role::Group)
                    .aria_label(HISTORY_LABEL)
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    // The frame is the tab stop, so the frame is what shows the keyboard
                    // focus: the same hairline ring the folder card carries. Only the
                    // keyboard draws it — a pointer that lands on a row is not the list
                    // saying it is ready for the arrows.
                    .border_1()
                    .border_color(cx.theme().transparent)
                    .focus_visible({
                        let ring = cx.theme().ring;
                        move |style| style.border_color(ring)
                    })
                    .on_key_down(cx.listener(Self::history_key))
                    .child(List::new(&self.history)),
            )
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

    /// The catalog could not be learned: say so where the loading hint would be.
    pub fn set_catalog_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_catalog_error(error, cx));
        cx.notify();
    }

    /// The swarm binary cannot be run at all (§9.7): the line under the folder card, which
    /// opens Settings when it is clicked.
    pub fn set_swarm_problem(&mut self, problem: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_swarm_problem(problem, cx));
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
        state.update(cx, |state, cx| {
            state
                .launcher
                .set_history(entries, now, offset_seconds, home.as_deref());
            if home.is_some() {
                state.home = home;
            }
            cx.notify();
        });
        let rows = rows_from_session(state.read(cx).launcher.history());
        self.history
            .update(cx, |state, cx| state.delegate_mut().set_rows(rows, cx));
        cx.notify();
    }

    /// The session index is still being fetched (§2): the list says so instead of claiming
    /// there is nothing.
    pub fn set_history_loading(&mut self, scanning: bool, cx: &mut Context<Self>) {
        self.history.update(cx, |state, cx| {
            state.delegate_mut().set_scanning(scanning, cx)
        });
        cx.notify();
    }

    /// The session index could not be read: the list says why.
    pub fn set_history_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.history
            .update(cx, |state, cx| state.delegate_mut().set_error(error, cx));
        cx.notify();
    }

    /// The `~` the history paths are shortened around, and where the folder dialog starts.
    pub fn set_home(&mut self, home: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_home(home, cx));
        cx.notify();
    }

    /// What the choosers add up to (§7.2, §1): the coordinator's `--model`, the lanes'
    /// `--lane-model` and `--lane-thinking`, and `--workers`.
    pub fn launch_plan(&self, cx: &App) -> LaunchPlan {
        self.choosers.plan(cx)
    }

    /// A folder is chosen: hand the window the launch it asked for — the models and the
    /// worker count are fixed when the swarm starts (§7.2). The window starts the swarm;
    /// this only reports the intent.
    pub(crate) fn emit_launch(
        &mut self,
        folder: PathBuf,
        plan: LaunchPlan,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TabContentEvent::Launch { folder, plan });
    }

    /// [`TabContent::emit_launch`] with the plan read from the choosers, for a caller that
    /// is not inside this tab's own update.
    pub(crate) fn request_launch(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let plan = self.launch_plan(cx);
        self.emit_launch(folder, plan, cx);
    }

    /// A folder pick a test injects, in place of the platform dialog. `None` is a cancelled
    /// dialog: the tab stays empty. Test-only, hence the allow.
    #[allow(dead_code)]
    pub(crate) fn set_folder_picker(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| {
            state.set_picker(FolderPicker::Fixed(folder), cx)
        });
    }
}

/// The empty tab's history list (§2): the rows, and the states before there are any.
pub(crate) struct HistoryList {
    rows: Vec<HistoryRow>,
    /// The background scan is still running.
    scanning: bool,
    /// The scan could not read the sessions directory at all.
    error: Option<String>,
    selected: Option<IndexPath>,
}

impl HistoryList {
    pub(crate) fn new(rows: Vec<HistoryRow>) -> Self {
        HistoryList {
            rows,
            scanning: false,
            error: None,
            selected: None,
        }
    }

    pub(crate) fn rows(&self) -> &[HistoryRow] {
        &self.rows
    }

    pub(crate) fn row(&self, row: usize) -> Option<&HistoryRow> {
        self.rows.get(row)
    }

    pub(crate) fn scanning(&self) -> bool {
        self.scanning
    }

    fn set_rows(&mut self, rows: Vec<HistoryRow>, cx: &mut Context<ListState<Self>>) {
        if self.rows == rows {
            return;
        }
        self.rows = rows;
        // The rows are a different set now, so a remembered row index means nothing.
        self.selected = None;
        cx.notify();
    }

    fn set_scanning(&mut self, scanning: bool, cx: &mut Context<ListState<Self>>) {
        if self.scanning != scanning {
            self.scanning = scanning;
            cx.notify();
        }
    }

    fn set_error(&mut self, error: Option<String>, cx: &mut Context<ListState<Self>>) {
        let error = error.filter(|error| !error.trim().is_empty());
        if self.error != error {
            self.error = error;
            cx.notify();
        }
    }
}

impl ListDelegate for HistoryList {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let row = self.rows.get(ix.row)?;
        let selected = Some(ix) == self.selected;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // The path and the facts are the row's substance — which folder, how many lanes, how
        // long ago, which model — so they take the theme's secondary text tone. The muted grey
        // is lighter than that: on the light theme's white page it reads as fine print.
        let facts = theme.tab_foreground;
        // The pill is filled with the theme's secondary tone, so it takes that tone's own
        // foreground: the muted grey is the page's secondary text, and at 11 px on a filled
        // chip it reads as a smudge.
        let badge_face = theme.secondary;
        let badge_text = theme.secondary_foreground;
        let radius = theme.radius;
        let tooltip = row.tooltip.clone();
        // The session the app had open when it last quit wears a pill, so it reads as a
        // fact about this row rather than as part of its name.
        let badge = row.open_at_quit.then(|| {
            div()
                .id(ElementId::NamedInteger(
                    OPEN_AT_QUIT_ID.into(),
                    ix.row as u64,
                ))
                .test_support()
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded(radius)
                .bg(badge_face)
                .text_color(badge_text)
                // A badge, not a word: a notch under the meta's own size, the way the agent
                // list's row badges are set.
                .text_size(px(11.))
                .child(OPEN_AT_QUIT_TEXT)
                .into_any_element()
        });
        Some(
            ListItem::new(ElementId::NamedInteger(
                HISTORY_ROW_ID.into(),
                ix.row as u64,
            ))
            // An 8 px pill, like the rest of the app's rows, with a document's own height:
            // 8 px above and below the two lines (the list item's own padding is narrower).
            .rounded(px(8.))
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    // A small folder glyph leads the row in the muted colour, so the list
                    // reads as a list of folders rather than of bare names.
                    .child(
                        Icon::new(IconName::Folder)
                            .with_size(ROW_ICON_SIZE)
                            .flex_none()
                            .text_color(muted),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_shrink(1.)
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ROW_TITLE_SIZE)
                                            .font_medium()
                                            .child(row.title.clone()),
                                    )
                                    .when_some(badge, |line, badge| line.child(badge)),
                            )
                            .child(
                                // The path and the facts share the second line. The path
                                // gives way first: it ellipsizes and the `·` separator sits
                                // right after it, with the same space on both sides as the
                                // ones inside the meta. The lane count and the time never
                                // give way — only the model at the very end may ellipsize.
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_shrink(1.)
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(facts)
                                            .child(row.subtitle.clone()),
                                    )
                                    .child(div().flex_none().text_xs().text_color(facts).child("·"))
                                    .child(
                                        div()
                                            .flex_none()
                                            .max_w(px(420.))
                                            .truncate()
                                            .text_xs()
                                            .text_color(facts)
                                            .child(row.meta.clone()),
                                    ),
                            ),
                    ),
            )
            .selected(selected)
            // The pill is a fact about the row, not decoration: a screen reader hears it too,
            // in the one place the row says it.
            .accessibility_label(match row.open_at_quit {
                true => format!(
                    "{} {} {} {}",
                    row.title, row.subtitle, OPEN_AT_QUIT_TEXT, row.meta
                ),
                false => format!("{} {} {}", row.title, row.subtitle, row.meta),
            })
            .tooltip(move |window, cx| {
                Tooltip::new(tooltip.clone())
                    .max_w(px(460.))
                    .build(window, cx)
            }),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
        cx.notify();
    }

    /// Nothing to list: say which nothing it is — the scan is still running, the scan
    /// failed, or there is genuinely nothing to resume.
    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (line, color) = match (&self.error, self.scanning) {
            (Some(error), _) => (error.clone(), cx.theme().danger),
            (None, true) => ("Scanning sessions…".to_string(), muted),
            (None, false) => ("No resumable swarms yet".to_string(), muted),
        };
        h_flex()
            .id(HISTORY_HINT_ID)
            .test_support()
            .w_full()
            .py_4()
            .gap_2()
            .items_center()
            .text_sm()
            .text_color(color)
            .when(self.error.is_none() && self.scanning, |row| {
                row.child(Spinner::new().small())
            })
            .child(line)
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
    use store::Root;

    /// A window wide enough for the 880 px block, and tall enough for the whole screen.
    const WINDOW: (f32, f32) = (1200., 800.);

    /// A `/catalog` body as `evo-swarm catalog --json` prints it (§5.6): two models a lane
    /// may run, and one whose `lanes.models` entry says it may not.
    fn catalog_body() -> Value {
        serde_json::json!({
            "models": [
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "name": "DeepSeek V4.1",
                 "api": "ark-chat", "context_window": 200000,
                 "reasoning": false, "images": false, "ready": true, "reason": null},
                {"id": "claude-opus-4.5", "provider": "anthropic", "name": "Claude Opus 4.5",
                 "api": "anthropic-messages", "context_window": 1000000,
                 "reasoning": true, "images": true, "ready": true, "reason": null}
            ],
            "providers": [{"name": "aiden", "api": "ark-chat", "has_key": true, "key_env": null}],
            "default_model": {"id": "ark-deepseek-v4.1-flash", "provider": "aiden"},
            "thinking_levels": ["off", "low", "medium", "high"],
            "languages": [{"code": "en", "name": "English"}],
            "lanes": {"models": [
                {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "ok": false,
                 "reason": "ark-chat is not an api a lane has"},
                {"id": "claude-opus-4.5", "provider": "anthropic", "ok": true, "reason": null}
            ]},
            "warnings": []
        })
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
                        std::sync::Arc::new(crate::LaunchEnv::default()),
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

    /// A `model-cache.json` on disk, as the app's own `catalog --json` fetch leaves it,
    /// loaded the way the app loads it. The temp directory removes itself.
    struct CacheDir(std::path::PathBuf);

    impl CacheDir {
        fn new(catalog: &Value) -> CacheDir {
            let dir = std::env::temp_dir().join(format!(
                "evo-desktop-empty-tab-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            let cache = serde_json::json!({
                "version": 2,
                "fetched_at": "2026-09-30T09:25:44Z",
                "program": "evo-swarm",
                "catalog": catalog,
            });
            std::fs::write(dir.join("model-cache.json"), cache.to_string()).expect("write cache");
            CacheDir(dir)
        }

        /// The body the app hands the tab: what `ModelCache::load` read back.
        fn catalog(&self) -> Value {
            store::ModelCache::load(&Root::at(self.0.clone()))
                .raw()
                .clone()
        }
    }

    impl Drop for CacheDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn history_entry(session: &str, folder: &str, minutes_ago: i64) -> HistoryEntry {
        HistoryEntry {
            session_path: session.to_string(),
            folder: folder.to_string(),
            title: String::new(),
            when: Some(1_700_000_000 - minutes_ago * 60),
            lanes: Some(4),
            coordinator_model: Some("ark-deepseek-v4.1-flash".to_string()),
            lanes_model: None,
            source: session::HistorySource::Index,
            open_at_quit: false,
        }
    }

    /// The keys one chooser offers, with whether each is available.
    fn options(cx: &App, tab: &Entity<TabContent>, which: Choice) -> Vec<(String, bool)> {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .launcher
            .chooser(which)
            .options
            .iter()
            .map(|option| (option.key.clone(), option.available))
            .collect()
    }

    /// The problem lines the last check left, as the tab shows them.
    fn problem_lines(cx: &App, tab: &Entity<TabContent>) -> Vec<String> {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .problems
            .iter()
            .map(|problem| problem.line())
            .collect()
    }

    fn caption_text(cx: &App, tab: &Entity<TabContent>) -> String {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .caption()
            .map(|caption| caption.text.to_string())
            .unwrap_or_default()
    }

    /// What the caption's hover carries, when the line is a summary of something longer.
    fn caption_detail(cx: &App, tab: &Entity<TabContent>) -> Option<String> {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .caption()
            .and_then(|caption| caption.detail)
            .map(|detail| detail.to_string())
    }

    /// Whether the caption wears the quiet warning tone rather than the page's muted grey.
    fn caption_is_warning(cx: &App, tab: &Entity<TabContent>) -> bool {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .caption()
            .is_some_and(|caption| caption.tone == CaptionTone::Warning)
    }

    #[gpui_kit::test]
    fn a_fresh_tab_is_default_everywhere_and_says_it_is_loading(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            // Every chooser starts on Default, which passes nothing to the swarm (§7.2).
            assert_eq!(f.tab.read(cx).coordinator_model(cx).as_ref(), "Default");
            assert_eq!(f.tab.read(cx).lanes_model(cx).as_ref(), "Default");
            assert_eq!(f.tab.read(cx).workers(cx).as_ref(), "Default");
            assert_eq!(f.tab.read(cx).launch_plan(cx), LaunchPlan::default());

            // Before the catalog is known, the choosers hold Default alone and the caption
            // says why.
            assert_eq!(caption_text(cx, &f.tab), "Loading models…");
            assert!(window.find(CAPTION_ID).visible());

            // Nothing has been scanned yet, so the history says so rather than showing
            // placeholder rows.
            assert!(f.tab.read(cx).history_rows(cx).is_empty());
            assert!(window.find(HISTORY_HINT_ID).visible());
        });
    }

    #[gpui_kit::test]
    fn the_cached_catalog_fills_the_choosers_and_the_lanes_availability(cx: &mut TestAppContext) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            window.render_frame(cx);

            // The catalog arrived: the loading hint is gone, and both models are offered to
            // the coordinator — a coordinator runs with the user's own userspace, so the
            // catalog's own `ready` is the only answer it needs.
            assert_eq!(caption_text(cx, &f.tab), "");
            let coordinator = options(cx, &f.tab, Choice::Coordinator);
            assert_eq!(coordinator.len(), 3, "Default plus two models");
            assert!(
                coordinator
                    .iter()
                    .any(|(key, ok)| key == "claude-opus-4.5@anthropic" && *ok),
                "{coordinator:?}"
            );
            assert!(
                coordinator
                    .iter()
                    .any(|(key, ok)| key == "ark-deepseek-v4.1-flash@aiden" && *ok),
                "{coordinator:?}"
            );

            // The lanes chooser offers the same registrations and greys out exactly the one
            // `lanes.models` says a lane cannot run (§5.6) — evo's own words are the reason.
            let lanes = options(cx, &f.tab, Choice::Lanes);
            let offered: Vec<&(String, bool)> =
                lanes.iter().filter(|(key, _)| key != DEFAULT_KEY).collect();
            assert_eq!(offered.len(), 2, "{lanes:?}");
            assert!(
                offered
                    .iter()
                    .any(|(key, ok)| key == "claude-opus-4.5@anthropic" && *ok),
                "{lanes:?}"
            );
            assert!(
                offered
                    .iter()
                    .any(|(key, ok)| key == "ark-deepseek-v4.1-flash@aiden" && !*ok),
                "{lanes:?}"
            );

            // ... and an unavailable option is one the menu cannot commit.
            let blocked = f
                .tab
                .read(cx)
                .choosers
                .state
                .read(cx)
                .launcher
                .chooser(Choice::Lanes)
                .options
                .iter()
                .find(|option| !option.available)
                .expect("an unavailable lanes model");
            let item = ChooserItem::from(blocked);
            assert!(item.disabled(), "{}", item.label);
            assert!(
                item.detail.contains("not an api a lane has"),
                "{}",
                item.detail
            );

            // The thinking row is the catalog's own levels, Default first (§1's
            // `--lane-thinking`).
            let thinking: Vec<String> = options(cx, &f.tab, Choice::LaneThinking)
                .iter()
                .map(|(key, _)| key.clone())
                .collect();
            assert_eq!(
                thinking,
                vec!["default", "off", "low", "medium", "high"],
                "the catalog's levels, Default first"
            );
        });
    }

    /// The catalog is the only answer now: the body a running server answers `GET /catalog`
    /// with is read exactly like the cached one, and a body that changes its mind about a
    /// lane changes the chooser with it.
    #[gpui_kit::test]
    fn a_live_catalog_refresh_answers_the_same_way(cx: &mut TestAppContext) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            assert!(
                !options(cx, &f.tab, Choice::Lanes)
                    .iter()
                    .find(|(key, _)| key == "ark-deepseek-v4.1-flash@aiden")
                    .expect("the aiden model")
                    .1,
                "a lane cannot run it, and the catalog said so"
            );

            // The server now says a lane can: the same registration turns available, with no
            // API set to compare and nothing remembered between the two bodies.
            let mut live = catalog_body();
            live["lanes"]["models"][0]["ok"] = Value::Bool(true);
            live["lanes"]["models"][0]["reason"] = Value::Null;
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&live, window, cx));
            assert!(
                options(cx, &f.tab, Choice::Lanes)
                    .iter()
                    .find(|(key, _)| key == "ark-deepseek-v4.1-flash@aiden")
                    .expect("the aiden model")
                    .1,
                "the catalog's newest answer is the one that counts"
            );
        });
    }

    #[gpui_kit::test]
    fn choosing_a_lanes_model_says_every_lane_runs_it_and_lands_in_the_plan(
        cx: &mut TestAppContext,
    ) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            window.render_frame(cx);

            // The chooser commits the way the menu does: the select emits its Confirm.
            let key = SharedString::from("claude-opus-4.5@anthropic");
            f.tab.update(cx, |tab, cx| {
                let lanes = tab.choosers.state.read(cx).lanes.clone();
                lanes.update(cx, |lanes, cx| {
                    let index = lanes.selected_index(cx);
                    let _ = index;
                    cx.emit(SelectEvent::Confirm(Some(key.clone())));
                });
            });
            window.render_frame(cx);
        });

        // The choice reaches the launcher when the update that emitted it returns, so the
        // caption is read in its own update.
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // §1: the choice is a launch flag, not a file this app writes — the caption says
            // what every lane will run.
            let caption = caption_text(cx, &f.tab);
            assert_eq!(
                caption,
                "Every lane runs claude-opus-4.5@anthropic (--lane-model)"
            );

            // ... and it is what the launch plan carries.
            let plan = f.tab.read(cx).launch_plan(cx);
            assert_eq!(
                plan.lanes_model,
                Some(("claude-opus-4.5".to_string(), "anthropic".to_string()))
            );
            assert_eq!(plan.model, None);
            assert_eq!(plan.workers, None);
        });
    }

    #[gpui_kit::test]
    fn a_chosen_folder_launches_with_the_plan(cx: &mut TestAppContext) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            f.tab.update(cx, |tab, cx| {
                tab.choosers.state.update(cx, |state, cx| {
                    state.launcher.select(Choice::Workers, "6");
                    cx.notify();
                });
            });
            f.tab.update(cx, |tab, cx| {
                tab.set_folder_picker(Some(PathBuf::from("/Users/you/coding/evo-gui")), cx)
            });
            window.render_frame(cx);
            window.click("select-folder", cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Launch {
                folder: PathBuf::from("/Users/you/coding/evo-gui"),
                plan: LaunchPlan {
                    model: None,
                    lanes_model: None,
                    lane_thinking: None,
                    workers: Some(6),
                },
            }]
        );
    }

    #[gpui_kit::test]
    fn a_cancelled_folder_pick_leaves_the_tab_where_it_was(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| tab.set_folder_picker(None, cx));
            window.render_frame(cx);
            window.click("select-folder", cx);
        });

        assert!(f.events().is_empty());
        f.act(cx, |_, cx| {
            assert_eq!(f.tab.read(cx).state(), &crate::tab::TabState::Empty)
        });
    }

    #[gpui_kit::test]
    fn a_history_row_resumes_its_session_in_its_folder(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let entries = vec![
                history_entry(
                    "/Users/you/.evo/sessions/a/1.sexp",
                    "/Users/you/coding/foo",
                    5,
                ),
                history_entry(
                    "/Users/you/.evo/sessions/b/2.sexp",
                    "/Users/you/coding/bar",
                    90,
                ),
            ];
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(&entries, 1_700_000_000, 0, Some("/Users/you"), cx)
            });
            window.render_frame(cx);

            // The rows carry the folder's name, the `~` path and the meta line (§2).
            assert_eq!(f.tab.read(cx).history_rows(cx).len(), 2);
            assert_eq!(f.tab.read(cx).history_rows(cx)[0].title, "foo");
            assert_eq!(f.tab.read(cx).history_rows(cx)[0].subtitle, "~/coding/foo");

            window.click(ElementId::NamedInteger(HISTORY_ROW_ID.into(), 0), cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Resume {
                session_path: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                folder: PathBuf::from("/Users/you/coding/foo"),
            }]
        );
    }

    #[gpui_kit::test]
    fn only_a_row_the_app_had_open_wears_the_badge(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            // The app's own recents know the tab was open when it last quit; the scan cannot.
            let mut opened = history_entry(
                "/Users/you/.evo/sessions/a/1.sexp",
                "/Users/you/coding/foo",
                5,
            );
            opened.open_at_quit = true;
            opened.source = session::HistorySource::Recent;
            let scanned = history_entry(
                "/Users/you/.evo/sessions/b/2.sexp",
                "/Users/you/coding/bar",
                90,
            );
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(
                    &[opened, scanned],
                    1_700_000_000,
                    0,
                    Some("/Users/you"),
                    cx,
                )
            });
            window.render_frame(cx);

            assert!(window
                .find(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), 0))
                .visible());
            // The row it sits on is still a row: the badge did not replace its own id.
            assert!(window
                .find(ElementId::NamedInteger(HISTORY_ROW_ID.into(), 0))
                .visible());
            assert!(
                window
                    .try_find(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), 1))
                    .is_none(),
                "a session the scan alone found cannot say it was open at quit"
            );
            // A badge is decoration on the row, not a thing of its own: the pill takes no
            // click, so the click lands on the row under it and opens the session.
            window.click(ElementId::NamedInteger(OPEN_AT_QUIT_ID.into(), 0), cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Resume {
                session_path: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                folder: PathBuf::from("/Users/you/coding/foo"),
            }]
        );
    }

    /// A chooser's select, by which one it is: the tests reach them through the tab's own
    /// state, the way the render does.
    fn chooser(f: &Fixture, cx: &App, which: Choice) -> Entity<SelectState<Vec<ChooserItem>>> {
        let state = f.tab.read(cx).choosers.state.clone();
        let state = state.read(cx);
        match which {
            Choice::Coordinator => state.coordinator.clone(),
            Choice::Lanes => state.lanes.clone(),
            Choice::LaneThinking => state.lane_thinking.clone(),
            Choice::Workers => state.workers.clone(),
        }
    }

    #[gpui_kit::test]
    fn a_chooser_opens_from_space_walks_with_the_arrows_and_closes_on_escape(
        cx: &mut TestAppContext,
    ) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        // Every step is its own act: opening and closing go through the select's own
        // actions, which GPUI dispatches on the way out of the update they were queued in.
        let trigger = f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            let chooser = chooser(&f, cx, Choice::Coordinator);
            let trigger = chooser.read(cx).focus_handle(cx);
            window.focus(&trigger, cx);
            window.render_frame(cx);
            trigger
        });
        assert_eq!(
            f.act(cx, |window, cx| window.focused(cx)),
            Some(trigger.clone())
        );

        f.act(cx, |window, cx| {
            window.press("space", cx);
            window.render_frame(cx);
        });
        // Space opens the menu the way Enter and the arrows do: the options take the focus,
        // and the trigger gives it up.
        assert_ne!(
            f.act(cx, |window, cx| window.focused(cx)),
            Some(trigger.clone()),
            "space must open the chooser: the options have the focus"
        );

        // Escape closes it and hands the trigger back — and the tab is still the tab: no tab
        // went away, no tab was opened, and nothing was committed.
        f.act(cx, |window, cx| {
            window.press("escape", cx);
            window.render_frame(cx);
        });
        assert_eq!(
            f.act(cx, |window, cx| window.focused(cx)),
            Some(trigger.clone()),
            "escape gives the chooser back its trigger"
        );
        assert!(f.act(cx, |window, _| window.find("empty-tab").visible()));
        assert!(f.events().is_empty(), "a closed chooser launches nothing");
        cx.update(|cx| {
            assert_eq!(f.tab.read(cx).coordinator_model(cx).as_ref(), "Default");
        });

        // Opened again with Space, the arrows walk the menu and Enter commits what they land
        // on: one step down is the first real model, not Default.
        f.act(cx, |window, cx| {
            window.press("space", cx);
            window.render_frame(cx);
        });
        f.act(cx, |window, cx| {
            window.press("down", cx);
            window.render_frame(cx);
        });
        f.act(cx, |window, cx| {
            window.press("enter", cx);
            window.render_frame(cx);
        });

        let (committed, expected) = cx.update(|cx| {
            let state = f.tab.read(cx).choosers.state.read(cx);
            let expected = state.launcher.chooser(Choice::Coordinator).options[1]
                .label
                .clone();
            let committed = f.tab.read(cx).coordinator_model(cx).to_string();
            (committed, expected)
        });
        assert_eq!(
            committed, expected,
            "enter committed the option the arrows walked to"
        );
    }

    #[gpui_kit::test]
    fn enter_and_the_arrows_open_a_chooser_the_way_the_kit_binds_them(cx: &mut TestAppContext) {
        // Space is the one key the tab has to supply: the select's own key context already
        // binds Enter and the arrows to opening, and Escape to closing. This pins that, so a
        // kit upgrade that dropped one of them would fail here rather than in a capture.
        let f = open(cx);
        for key in ["enter", "down", "up"] {
            let trigger = f.act(cx, |window, cx| {
                let chooser = chooser(&f, cx, Choice::Coordinator);
                let trigger = chooser.read(cx).focus_handle(cx);
                window.focus(&trigger, cx);
                window.render_frame(cx);
                trigger
            });
            f.act(cx, |window, cx| {
                window.press(key, cx);
                window.render_frame(cx);
            });
            assert_ne!(
                f.act(cx, |window, cx| window.focused(cx)),
                Some(trigger.clone()),
                "{key} opens the chooser"
            );
            f.act(cx, |window, cx| {
                window.press("escape", cx);
                window.render_frame(cx);
            });
            assert_eq!(
                f.act(cx, |window, cx| window.focused(cx)),
                Some(trigger),
                "escape closes what {key} opened, without leaving the tab"
            );
            assert!(f.act(cx, |window, _| window.find("empty-tab").visible()));
        }
    }

    /// Selecting an empty tab puts the keyboard where the tab begins: the
    /// coordinator chooser, the first of its three rows (§7.1).
    #[gpui_kit::test]
    fn focus_primary_lands_the_keyboard_on_the_first_chooser(cx: &mut TestAppContext) {
        let f = open(cx);
        // Rendered first, the way a tab the window is showing is.
        f.act(cx, |window, cx| window.render_frame(cx));

        let (took, focused, trigger) = f.act(cx, |window, cx| {
            let took = f
                .tab
                .update(cx, |tab, cx| tab.choosers.focus_primary(window, cx));
            let trigger = chooser(&f, cx, Choice::Coordinator);
            let trigger = trigger.read(cx).focus_handle(cx);
            (took, window.focused(cx), trigger)
        });
        assert!(took, "the empty tab takes the keyboard");
        assert_eq!(
            focused,
            Some(trigger.clone()),
            "…and the coordinator chooser is what has it"
        );

        // The focus is this frame's, not a handle remembered from another: the key
        // the tab supplies for its choosers opens the menu from where the keyboard
        // now is.
        f.act(cx, |window, cx| {
            window.press("space", cx);
            window.render_frame(cx);
        });
        assert_ne!(
            f.act(cx, |window, cx| window.focused(cx)),
            Some(trigger),
            "space opened the chooser the keyboard landed on"
        );
    }

    /// The call is idempotent, and a window that takes no focus at all is left
    /// alone rather than half-focused.
    #[gpui_kit::test]
    fn focus_primary_answers_false_when_the_window_refuses_the_keyboard(cx: &mut TestAppContext) {
        let f = open(cx);
        let (first, landed, again, still) = f.act(cx, |window, cx| {
            let first = f
                .tab
                .update(cx, |tab, cx| tab.choosers.focus_primary(window, cx));
            let landed = window.focused(cx);
            let again = f
                .tab
                .update(cx, |tab, cx| tab.choosers.focus_primary(window, cx));
            (first, landed, again, window.focused(cx))
        });
        assert!(first && again, "both calls take the focus");
        assert_eq!(landed, still, "and the second lands on the same control");

        let refused = f.act(cx, |window, cx| {
            window.disable_focus(cx);
            let took = f
                .tab
                .update(cx, |tab, cx| tab.choosers.focus_primary(window, cx));
            (took, window.focused(cx))
        });
        assert!(!refused.0, "a window taking no focus answers false");
        assert_eq!(refused.1, None, "and nothing holds the keyboard");
    }

    /// The row the history list has highlighted, for the keyboard tests below.
    fn selected_history_row(cx: &App, tab: &Entity<TabContent>) -> Option<usize> {
        tab.read(cx)
            .history
            .read(cx)
            .selected_index()
            .map(|ix| ix.row)
    }

    #[gpui_kit::test]
    fn the_history_list_walks_with_the_arrows_and_the_ends(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let entries = ["a", "b", "c"]
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    history_entry(
                        &format!("/Users/you/.evo/sessions/{name}/1.sexp"),
                        &format!("/Users/you/coding/{name}"),
                        (i as i64 + 1) * 30,
                    )
                })
                .collect::<Vec<_>>();
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(&entries, 1_700_000_000, 0, Some("/Users/you"), cx)
            });
            let list = f.tab.read(cx).choosers.state.read(cx).history_focus.clone();
            window.focus(&list, cx);
            window.render_frame(cx);

            // Nothing selected yet, so Down starts at the top and Up starts at the bottom.
            assert_eq!(selected_history_row(cx, &f.tab), None);
            window.press("up", cx);
            assert_eq!(selected_history_row(cx, &f.tab), Some(2));
            window.press("home", cx);
            assert_eq!(selected_history_row(cx, &f.tab), Some(0));
            window.press("down", cx);
            assert_eq!(selected_history_row(cx, &f.tab), Some(1));
            window.press("end", cx);
            assert_eq!(selected_history_row(cx, &f.tab), Some(2));
            // The ends hold: another Down does not walk past the last row.
            window.press("down", cx);
            assert_eq!(selected_history_row(cx, &f.tab), Some(2));
        });
    }

    #[gpui_kit::test]
    fn the_badge_is_part_of_what_the_row_says(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let mut opened = history_entry(
                "/Users/you/.evo/sessions/a/1.sexp",
                "/Users/you/coding/foo",
                5,
            );
            opened.open_at_quit = true;
            let scanned = history_entry(
                "/Users/you/.evo/sessions/b/2.sexp",
                "/Users/you/coding/bar",
                90,
            );
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(
                    &[opened, scanned],
                    1_700_000_000,
                    0,
                    Some("/Users/you"),
                    cx,
                )
            });
            window.render_frame(cx);

            let said = |ix: u64| {
                window
                    .find(ElementId::NamedInteger(HISTORY_ROW_ID.into(), ix))
                    .label()
                    .unwrap_or_default()
                    .to_string()
            };
            let opened_said = said(0);
            let scanned_said = said(1);
            // The pill is the only place the row says this, so the name has to carry it.
            assert!(
                opened_said.contains(OPEN_AT_QUIT_TEXT),
                "the row's name must say what the pill says: {opened_said}"
            );
            assert!(
                !scanned_said.contains(OPEN_AT_QUIT_TEXT),
                "a row the scan alone found must not claim it: {scanned_said}"
            );
            // ... and the row still names itself: title, path and facts.
            assert!(opened_said.contains("foo"), "{opened_said}");
            assert!(opened_said.contains("~/coding/foo"), "{opened_said}");
        });
    }

    #[gpui_kit::test]
    fn the_history_list_says_which_nothing_it_is(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // Nothing yet.
            assert!(window.find(HISTORY_HINT_ID).visible());

            // The scan is running: it says so, with a spinner.
            f.tab
                .update(cx, |tab, cx| tab.set_history_loading(true, cx));
            window.render_frame(cx);
            assert!(window.find(HISTORY_HINT_ID).visible());

            // The scan could not read the sessions directory: it says why.
            f.tab
                .update(cx, |tab, cx| tab.set_history_loading(false, cx));
            f.tab.update(cx, |tab, cx| {
                tab.set_history_error(Some("~/.evo/sessions is not readable".to_string()), cx)
            });
            window.render_frame(cx);
            assert!(window.find(HISTORY_HINT_ID).visible());

            // And a real row replaces the hint.
            let entries = vec![history_entry(
                "/Users/you/.evo/sessions/a/1.sexp",
                "/Users/you/coding/foo",
                5,
            )];
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(&entries, 1_700_000_000, 0, Some("/Users/you"), cx)
            });
            f.tab.update(cx, |tab, cx| tab.set_history_error(None, cx));
            window.render_frame(cx);
            assert!(window.try_find(HISTORY_HINT_ID).is_none());
            assert!(window
                .find(ElementId::NamedInteger(HISTORY_ROW_ID.into(), 0))
                .visible());
        });
    }

    /// A focus handle's identity, for a test that has to compare them: `FocusHandle` is
    /// `PartialEq` but has no printable name.
    fn focus_name(handle: &FocusHandle) -> String {
        format!("{handle:?}")
    }

    /// The tab's own focus handles, in the order they should be reached: the four
    /// choosers, the folder card, then the history list.
    fn focus_order(cx: &App, tab: &Entity<TabContent>) -> Vec<FocusHandle> {
        let state = tab.read(cx).choosers.state.read(cx);
        vec![
            state.coordinator.read(cx).focus_handle(cx),
            state.lanes.read(cx).focus_handle(cx),
            state.lane_thinking.read(cx).focus_handle(cx),
            state.workers.read(cx).focus_handle(cx),
            state.folder_focus.clone(),
            // The frame around the list, not the list's own handle: the List does not take
            // part in tab traversal, so the frame holds the focus and forwards the keys.
            state.history_focus.clone(),
        ]
    }

    #[gpui_kit::test]
    fn tab_reaches_the_choosers_then_the_folder_card_then_the_history(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let entries = vec![history_entry(
                "/Users/you/.evo/sessions/a/1.sexp",
                "/Users/you/coding/foo",
                5,
            )];
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(&entries, 1_700_000_000, 0, Some("/Users/you"), cx)
            });
            window.render_frame(cx);

            // Walk the window's tab stops, and note where each of the tab's own controls is
            // reached. `focus_next` is what the platform's Tab key does (gpui keeps Tab out
            // of the action path); other things are focusable too — the tab strip — so only
            // the order matters.
            let expected = focus_order(cx, &f.tab);
            let mut seen: Vec<String> = Vec::new();
            for _ in 0..30 {
                window.focus_next(cx);
                if let Some(focused) = window.focused(cx) {
                    let name = focus_name(&focused);
                    if !seen.contains(&name) {
                        seen.push(name);
                    }
                }
            }
            let reached: Vec<usize> = expected
                .iter()
                .map(|handle| {
                    let name = focus_name(handle);
                    seen.iter()
                        .position(|seen| *seen == name)
                        .unwrap_or_else(|| panic!("never focused: {name} of {seen:?}"))
                })
                .collect();
            assert!(
                reached.windows(2).all(|pair| pair[0] < pair[1]),
                "focus order is {reached:?}, expected the choosers, the card and then the list: {seen:?}"
            );
        });
    }

    #[gpui_kit::test]
    fn enter_on_the_folder_card_launches(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_folder_picker(Some(PathBuf::from("/Users/you/coding/foo")), cx)
            });
            let card = f.tab.read(cx).choosers.state.read(cx).folder_focus.clone();
            window.focus(&card, cx);
            window.render_frame(cx);
            window.press("enter", cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Launch {
                folder: PathBuf::from("/Users/you/coding/foo"),
                plan: LaunchPlan::default(),
            }]
        );
    }

    #[gpui_kit::test]
    fn enter_on_the_history_list_resumes_the_selected_row(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let entries = vec![
                history_entry(
                    "/Users/you/.evo/sessions/a/1.sexp",
                    "/Users/you/coding/foo",
                    5,
                ),
                history_entry(
                    "/Users/you/.evo/sessions/b/2.sexp",
                    "/Users/you/coding/bar",
                    90,
                ),
            ];
            f.tab.update(cx, |tab, cx| {
                tab.set_history_entries(&entries, 1_700_000_000, 0, Some("/Users/you"), cx)
            });
            let list = f.tab.read(cx).choosers.state.read(cx).history_focus.clone();
            window.focus(&list, cx);
            window.render_frame(cx);

            // Down selects a row, Enter opens it — the keyboard route to the same event a
            // click emits, on the frame Tab actually lands on.
            window.press("down", cx);
            window.press("enter", cx);
        });

        assert_eq!(
            f.events(),
            vec![TabContentEvent::Resume {
                session_path: PathBuf::from("/Users/you/.evo/sessions/a/1.sexp"),
                folder: PathBuf::from("/Users/you/coding/foo"),
            }]
        );
    }

    /// §9: what `evo-swarm check --json` found wrong is said under the choosers, one calm
    /// line each, and a click on a line opens the chooser it is about. Nothing is said when
    /// the launch is fine.
    #[gpui_kit::test]
    fn the_checks_problems_are_lines_that_open_their_chooser(cx: &mut TestAppContext) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab
                .update(cx, |tab, cx| tab.set_catalog(&cache.catalog(), window, cx));
            window.render_frame(cx);
            // A launch on Default has nothing wrong with it.
            assert!(problem_lines(cx, &f.tab).is_empty());
            assert!(window.try_find(PROBLEMS_ID).is_none());

            // The check's answer, as evo's own `check --json` gives it: two problems, one
            // about the coordinator's model and one about the lanes'.
            f.tab.update(cx, |tab, cx| {
                tab.choosers.state.update(cx, |state, cx| {
                    state.set_problems(
                        vec![
                            Problem {
                                code: "model_not_ready".to_string(),
                                message: "claude-opus-4.5 has no credential".to_string(),
                            },
                            Problem {
                                code: "lane_model_not_ready".to_string(),
                                message: "a lane cannot register\nthe aiden model".to_string(),
                            },
                        ],
                        cx,
                    )
                })
            });
            window.render_frame(cx);
            assert_eq!(
                problem_lines(cx, &f.tab),
                vec![
                    "claude-opus-4.5 has no credential".to_string(),
                    "a lane cannot register the aiden model".to_string(),
                ],
                "one line each, the message's own newline folded away"
            );
            assert!(window.find(PROBLEMS_ID).visible());

            // The keyboard starts on the folder card: not on any chooser.
            window.focus(
                &f.tab.read(cx).choosers.state.read(cx).folder_focus.clone(),
                cx,
            );
            window.click(ElementId::NamedInteger(PROBLEM_ID.into(), 1), cx);
        });

        // The click is the lanes' line, so the lanes chooser is what has the keyboard —
        // not the coordinator's, which is the row above it.
        let lanes = f.act(cx, |window, cx| window.focused(cx));
        assert_eq!(
            lanes,
            f.act(cx, |_, cx| chooser(&f, cx, Choice::Lanes)
                .read(cx)
                .focus_handle(cx)),
            "the lanes line opens the lanes chooser"
        );

        f.act(cx, |window, cx| {
            // A clean check takes the lines away again.
            f.tab.update(cx, |tab, cx| {
                tab.choosers
                    .state
                    .update(cx, |state, cx| state.set_problems(Vec::new(), cx))
            });
            window.render_frame(cx);
            assert!(window.try_find(PROBLEMS_ID).is_none());
        });
    }

    #[gpui_kit::test]
    fn a_catalog_failure_reads_as_one_sentence_and_keeps_the_servers_words_for_the_hover(
        cx: &mut TestAppContext,
    ) {
        // The error a failing catalog fetch leaves behind: the command's own words, a
        // bearer token and all. Under a chooser it reads as a broken screen.
        let raw = r#"http 500: The value "Bearer sk-live-9f3c…" is not a model"#;
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_catalog_error(Some(raw.to_string()), cx)
            });
            window.render_frame(cx);

            let line = caption_text(cx, &f.tab);
            assert_eq!(
                line, "Couldn't load the model list — Default models will be used.",
                "one sentence about what the tab does now"
            );
            assert!(
                !line.contains("Bearer") && !line.contains("500"),
                "not the server's error dump: {line}"
            );
            assert!(
                caption_is_warning(cx, &f.tab),
                "and it wears the warning tone, not the danger one"
            );
            assert_eq!(
                caption_detail(cx, &f.tab).as_deref(),
                Some(raw),
                "the server's own words are what the hover carries"
            );
            // The tab still works: every chooser is usable on Default, and the folder card
            // is what starts the swarm.
            assert_eq!(f.tab.read(cx).coordinator_model(cx).as_ref(), "Default");
            assert_eq!(f.tab.read(cx).lanes_model(cx).as_ref(), "Default");
            assert_eq!(f.tab.read(cx).workers(cx).as_ref(), "Default");
            assert!(window.find(FOLDER_ID).visible());
        });
    }

    /// A fetch can fail while the last catalog is still in the choosers: then the line must
    /// not claim the list is gone when it is on the screen.
    #[gpui_kit::test]
    fn a_failed_refresh_says_the_last_catalog_is_still_in_use(cx: &mut TestAppContext) {
        let cache = CacheDir::new(&catalog_body());
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_catalog(&cache.catalog(), window, cx);
                tab.set_catalog_error(Some("evo-swarm catalog --json exited 1".to_string()), cx);
            });
            window.render_frame(cx);
            assert_eq!(
                caption_text(cx, &f.tab),
                "Couldn't refresh the model list — using the last one it loaded."
            );
            assert!(
                options(cx, &f.tab, Choice::Lanes).len() > 1,
                "the models the cache brought are still in the chooser"
            );
        });
    }

    /// The swarm binary cannot be run at all: the tab says it under the folder card, before
    /// anything is picked, and the line is the way to the panel that fixes it (§9.7).
    #[gpui_kit::test]
    fn a_swarm_binary_that_cannot_be_run_is_said_under_the_folder_card(cx: &mut TestAppContext) {
        let line = "evo-swarm not found at /usr/local/bin/evo-swarm — fix it in Settings…";
        // What the app's own handler would do with the action; the app is not in this test,
        // so the test is the one that answers it.
        let asked = Rc::new(RefCell::new(0usize));
        let answered = asked.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &OpenSettings, _cx: &mut App| {
                *answered.borrow_mut() += 1;
            });
        });
        let f = open(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // Nothing is claimed while the binary is fine: the line is the app's to send.
            assert!(window.try_find(SWARM_MISSING_ID).is_none());

            f.tab.update(cx, |tab, cx| {
                tab.set_swarm_problem(Some(line.to_string()), cx)
            });
            window.render_frame(cx);

            let drawn = window.find(SWARM_MISSING_ID);
            assert!(drawn.visible(), "the line is on the screen");
            assert_eq!(
                drawn.label(),
                Some(line),
                "and it is the app's own sentence, path and all"
            );
            // The card it belongs to is still the screen's call to action.
            assert!(window.find(FOLDER_ID).visible());

            // Clicking it opens Settings — the workspace dispatches the action and the app
            // answers it, which is the seam between the two crates.
            window.click(SWARM_MISSING_ID, cx);
        });
        // A window's own action dispatch is deferred to the end of the effect cycle
        // (`Window::dispatch_action`), so what the click asked for lands here.
        cx.run_until_parked();
        assert_eq!(*asked.borrow(), 1, "the click asked for Settings");
        assert!(
            !f.events()
                .iter()
                .any(|event| matches!(event, TabContentEvent::Launch { .. })),
            "the line is not the folder card: nothing was launched"
        );

        // And it goes away when the path is fixed in Settings.
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| tab.set_swarm_problem(None, cx));
            window.render_frame(cx);
            assert!(
                window.try_find(SWARM_MISSING_ID).is_none(),
                "a binary that runs is no line"
            );
        });
    }
}
