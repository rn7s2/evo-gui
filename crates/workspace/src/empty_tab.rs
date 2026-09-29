//! The empty tab (§7.2): what a new tab shows before a folder is chosen.
//!
//! One centred column — a header, the three choosers with the folder button beside them,
//! then the resumable swarms — and nothing else. The choosers' options, the note under the
//! lanes select and the history rows all come from [`session::Launcher`]; this module owns
//! only what the session model cannot know: the select widgets, the folder dialog, whether
//! the catalog has arrived, and whether the history scan is still running.
//!
//! ```text
//! set_registry / set_model_cache   the model catalog (§9.4)
//! set_history_entries              the scan's rows (§9.5)
//! set_scanning / set_catalog_error the two states with nothing to show yet
//! → TabContentEvent::Launch        the folder plus the choosers' plan (§7.2, §9.6)
//! → TabContentEvent::Resume        a history row, by lane 1's ListEvent subscription
//! ```

use std::path::PathBuf;

use gpui_kit::base::Button;
use gpui_kit::component::list::{List, ListDelegate, ListItem, ListState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    Theme,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, AnyElement, App, Context, ElementId, Entity, FocusHandle, Hsla, IntoElement,
    KeyDownEvent, Pixels, Role, SharedString, Subscription, TestSupportExt as _, WeakEntity,
    Window,
};
use serde_json::Value;
use session::{Choice, ChooserOption, HistoryEntry, LaunchPlan, Launcher, DEFAULT_KEY};
use store::ModelCache;
use swarm_client::{Payload, Registry};

use crate::history::{rows_from_session, HistoryRow};
use crate::tab::{TabContent, TabContentEvent};

/// The launcher's measure (§7.2): one centred column, wide enough for the three rows and
/// the folder button beside them.
const BLOCK_WIDTH: Pixels = px(880.);

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

/// The folder call to action, and how its parts are drawn.
const FOLDER_ID: &str = "select-folder";
const FOLDER_ICON_SIZE: Pixels = px(28.);

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
/// The badge on a row the app had open when it last quit (§9.5).
const OPEN_AT_QUIT_ID: &str = "history-open-at-quit";
const OPEN_AT_QUIT_TEXT: &str = "open at last quit";

/// One option of a chooser, as the Select draws it: the label, a muted detail line under
/// it, and — for a model a lane could not register — the reason it is greyed out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChooserItem {
    /// Stable identity of the option: what the launcher is told was chosen.
    key: SharedString,
    label: SharedString,
    /// The detail under the label: the context window and what else the registry knows, or
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
    /// (§9.4).
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

    /// The chosen lanes model's label (§9.6).
    pub(crate) fn lanes_model(&self, cx: &App) -> SharedString {
        chosen_label(&self.state, Choice::Lanes, cx)
    }

    /// The chosen worker count's label (`Default` leaves it to evo).
    pub(crate) fn workers(&self, cx: &App) -> SharedString {
        chosen_label(&self.state, Choice::Workers, cx)
    }

    /// What the three choosers add up to (§7.2, §9.6).
    pub(crate) fn plan(&self, cx: &App) -> LaunchPlan {
        self.state.read(cx).launcher.plan()
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
    /// The choosers' model: options, the chosen keys, and the history rows (§7.2, §9.5).
    launcher: Launcher,
    coordinator: Entity<SelectState<Vec<ChooserItem>>>,
    lanes: Entity<SelectState<Vec<ChooserItem>>>,
    workers: Entity<SelectState<Vec<ChooserItem>>>,
    /// The home directory the `~` paths are shortened around.
    home: Option<String>,
    /// The catalog has arrived, from the cache or from a probe: until it has, the model
    /// choosers hold nothing but Default.
    catalog: bool,
    /// The catalog could not be read: shown instead of the loading hint.
    catalog_error: Option<String>,
    /// Where a folder pick answers from.
    picker: FolderPicker,
    /// The folder the app expects to be picked — the last one used, say — so the
    /// `swarm.lisp` note can name a real path before the dialog answers. `None` keeps the
    /// generic `<folder>`.
    folder_hint: Option<PathBuf>,
    /// The folder card's own focus handle, so keyboard traversal and the focus ring have
    /// somewhere to land.
    folder_focus: FocusHandle,
    /// DEBUG experiment
    history_focus: FocusHandle,
    /// The tab that owns this state: what a launch is emitted on.
    tab: WeakEntity<TabContent>,
    _subscriptions: Vec<Subscription>,
}

impl EmptyTabState {
    fn new(window: &mut Window, tab: WeakEntity<TabContent>, cx: &mut Context<Self>) -> Self {
        let launcher = Launcher::new();
        let coordinator = chooser_state(&launcher, Choice::Coordinator, window, cx);
        let lanes = chooser_state(&launcher, Choice::Lanes, window, cx);
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
            workers,
            home: std::env::var("HOME").ok(),
            catalog: false,
            catalog_error: None,
            picker: FolderPicker::Dialog,
            folder_hint: None,
            folder_focus: cx.focus_handle(),
            history_focus: cx.focus_handle().tab_stop(true),
            tab,
            _subscriptions: subscriptions,
        }
    }

    /// A chooser committed an option. The launcher is the one who decides whether it means
    /// anything: a rebuilt chooser can drop a choice that no longer exists (§9.4).
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
            cx.notify();
        }
    }

    /// Rebuild the selects from the launcher: the catalog arrived, or the project's worker
    /// count did.
    fn sync_choosers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (which, state) in [
            (Choice::Coordinator, self.coordinator.clone()),
            (Choice::Lanes, self.lanes.clone()),
            (Choice::Workers, self.workers.clone()),
        ] {
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

    /// The model catalog, from the cache or from a live server or probe (§9.4).
    fn set_registry(
        &mut self,
        registry: &Payload<Registry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.catalog = true;
        self.catalog_error = None;
        self.launcher.set_registry(registry.raw());
        self.sync_choosers(window, cx);
        cx.notify();
    }

    /// The catalog the disk cache holds (§9.4's `model-cache.json`).
    ///
    /// The registry is the cache's own either way; what decides which models a lane may
    /// offer is the **kernel api set** the cache remembers, not the registry's `apis` — a
    /// live server's `apis` also carries the APIs its extensions added, while a lane only
    /// has the kernel's own. A cache a live refresh moved forward keeps that set
    /// ([`ModelCache::with_live_registry`]), so the lanes chooser stays exact after one
    /// rather than falling back to uncertain.
    fn set_model_cache(&mut self, cache: &ModelCache, window: &mut Window, cx: &mut Context<Self>) {
        self.catalog = !cache.is_empty();
        self.catalog_error = None;
        if !self.catalog {
            // An empty cache is the same as one that never arrived: Default, and the hint.
            cx.notify();
            return;
        }
        self.launcher.set_registry(&cache.registry);
        self.launcher
            .set_kernel_apis(kernel_apis_value(&cache.kernel_apis).as_ref());
        self.sync_choosers(window, cx);
        cx.notify();
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
    /// wrong, something is still loading, or a lanes model will be written to a file.
    fn caption(&self) -> Option<SharedString> {
        if let Some(error) = &self.catalog_error {
            return Some(SharedString::from(error.clone()));
        }
        if !self.catalog {
            return Some(SharedString::from("Loading models…"));
        }
        let lanes = self.launcher.selected(Choice::Lanes)?;
        if lanes.key == DEFAULT_KEY {
            // Default writes nothing: the swarm keeps the project's own configuration.
            return None;
        }
        // Choosing a folder is what this screen is for, so the note usually names a
        // placeholder; when the app already knows the folder it expects (the last one used,
        // say) the note names that instead (§9.6).
        let folder = match &self.folder_hint {
            Some(folder) => folder.display().to_string(),
            None => "<folder>".to_string(),
        };
        Some(SharedString::from(
            self.launcher
                .lanes_model_note(&folder, self.home.as_deref()),
        ))
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
                    .child(self.chooser_row("Workers", "workers", &self.workers, cx)),
            )
            .child(self.render_folder_button(cx))
    }

    fn render_caption(&self, cx: &Context<Self>) -> impl IntoElement {
        let caption = self.caption();
        let color = match (&self.catalog_error, &caption) {
            (Some(_), _) => cx.theme().danger,
            (None, Some(_)) if !self.catalog => cx.theme().muted_foreground,
            _ => cx.theme().muted_foreground,
        };
        let tooltip = caption.clone();
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
            .children(caption)
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
            .flex_1()
            .h_full()
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

/// The kernel API set a [`ModelCache`] recorded, as the `apis` array
/// [`Launcher::set_kernel_apis`] reads. An empty set is `None`: no probe has said, so the
/// lanes chooser reports uncertainty rather than claiming a model works in a lane.
fn kernel_apis_value(apis: &[String]) -> Option<Value> {
    if apis.is_empty() {
        return None;
    }
    Some(Value::Array(
        apis.iter().cloned().map(Value::String).collect(),
    ))
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

    /// The resumable swarms, newest first (§9.5), with the two states that have no rows.
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

    /// The model catalog (§9.4), from a live server's `/registry`, or from a probe's.
    pub fn set_registry(
        &mut self,
        registry: &Payload<Registry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_registry(registry, window, cx));
        cx.notify();
    }

    /// The catalog the disk cache holds (§9.4), which also carries the kernel api set a
    /// probe learned.
    pub fn set_model_cache(
        &mut self,
        cache: &ModelCache,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_model_cache(cache, window, cx));
        cx.notify();
    }

    /// The catalog could not be learned: say so where the loading hint would be.
    pub fn set_catalog_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_catalog_error(error, cx));
        cx.notify();
    }

    /// The resumable swarms the scan found (§9.5), merged with the app's own recents.
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

    /// The background scan is still walking `~/.evo/sessions` (§9.5): the list says so
    /// instead of claiming there is nothing.
    pub fn set_scanning(&mut self, scanning: bool, cx: &mut Context<Self>) {
        self.history.update(cx, |state, cx| {
            state.delegate_mut().set_scanning(scanning, cx)
        });
        cx.notify();
    }

    /// The scan could not read the sessions directory: the list says why.
    pub fn set_history_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.history
            .update(cx, |state, cx| state.delegate_mut().set_error(error, cx));
        cx.notify();
    }

    /// The folder the app expects to be picked — the last one used, say. Until the dialog
    /// answers, the `swarm.lisp` note names this folder's own file instead of `<folder>`.
    pub fn set_folder_hint(&mut self, folder: Option<PathBuf>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| {
            if state.folder_hint != folder {
                state.folder_hint = folder;
                cx.notify();
            }
        });
        cx.notify();
    }

    /// The `~` the history paths are shortened around, and where the folder dialog starts.
    pub fn set_home(&mut self, home: Option<String>, cx: &mut Context<Self>) {
        let state = self.choosers.state.clone();
        state.update(cx, |state, cx| state.set_home(home, cx));
        cx.notify();
    }

    /// What the three choosers add up to (§7.2, §9.6): the coordinator's `--model`, the
    /// lanes model to write into the folder's `swarm.lisp`, and `--workers`.
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

/// The empty tab's history list (§9.5): the rows, and the states before there are any.
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
        point, size, AnyWindowHandle, Bounds, Focusable as _, TestAppContext, WindowBounds,
        WindowOptions,
    };
    use std::cell::RefCell;
    use std::rc::Rc;
    use store::Root;

    /// A window wide enough for the 880 px block, and tall enough for the whole screen.
    const WINDOW: (f32, f32) = (1200., 800.);

    /// A registry as a `--no-userspace` probe answers it: two models under different wire
    /// APIs, and the kernel's own api set — which is what the lanes chooser measures
    /// against (§9.4).
    const REGISTRY: &str = r#"{
      "version": 1,
      "fetched_at": "2026-09-29T09:25:44Z",
      "kernel_apis": ["anthropic-messages"],
      "registry": {
        "models": [
          {
            "id": "ark-deepseek-v4.1-flash",
            "provider": "aiden",
            "api": "openai-chat",
            "context_window": 200000,
            "vision": false,
            "effort": ["low", "high"]
          },
          {
            "id": "claude-opus-4.5",
            "provider": "anthropic",
            "api": "anthropic-messages",
            "context_window": 1000000,
            "vision": true,
            "effort": ["low", "max"]
          }
        ],
        "apis": ["anthropic-messages"]
      }
    }"#;

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
                        std::sync::Arc::new(crate::SwarmConfig::default()),
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

    /// A model cache on disk, as §9.4's background probe leaves it, loaded the way the app
    /// loads it. The temp directory removes itself.
    struct CacheDir(std::path::PathBuf);

    impl CacheDir {
        fn new(cx: &str) -> CacheDir {
            let dir = std::env::temp_dir().join(format!(
                "evo-desktop-empty-tab-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            std::fs::write(dir.join("model-cache.json"), cx).expect("write cache");
            CacheDir(dir)
        }

        fn cache(&self) -> ModelCache {
            ModelCache::load(&Root::at(self.0.clone()))
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
            when: session::When::Epoch(1_700_000_000 - minutes_ago * 60),
            lanes: Some(4),
            coordinator_model: Some("ark-deepseek-v4.1-flash".to_string()),
            lanes_model: None,
            source: session::HistorySource::Scan,
            open_at_quit: false,
        }
    }

    /// The keys the lanes chooser offers, with whether each is available.
    fn lanes_options(cx: &App, tab: &Entity<TabContent>) -> Vec<(String, bool)> {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .launcher
            .lanes()
            .options
            .iter()
            .map(|option| (option.key.clone(), option.available))
            .collect()
    }

    /// Whether the lanes chooser has not been measured against a kernel api set.
    fn lanes_uncertain(cx: &App, tab: &Entity<TabContent>) -> bool {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .launcher
            .lanes()
            .uncertain
    }

    fn caption_text(cx: &App, tab: &Entity<TabContent>) -> String {
        tab.read(cx)
            .choosers
            .state
            .read(cx)
            .caption()
            .map(|caption| caption.to_string())
            .unwrap_or_default()
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
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_model_cache(&cache.cache(), window, cx)
            });
            window.render_frame(cx);

            // The catalog arrived: the loading hint is gone.
            assert_eq!(caption_text(cx, &f.tab), "");

            // Both models are offered to the coordinator — the aiden one included, since a
            // coordinator runs with the user's own userspace.
            let coordinator: Vec<String> = lanes_options(cx, &f.tab)
                .iter()
                .map(|(key, _)| key.clone())
                .collect();
            assert!(
                coordinator
                    .iter()
                    .any(|key| key.starts_with("claude-opus-4.5")),
                "{coordinator:?}"
            );

            // A lane can only register the kernel's own wire APIs: the registry carries
            // `apis`, so the aiden model is offered and marked unusable (§9.4).
            let lanes = lanes_options(cx, &f.tab);
            let available: Vec<&(String, bool)> =
                lanes.iter().filter(|(key, _)| key != DEFAULT_KEY).collect();
            assert_eq!(available.len(), 2, "{lanes:?}");
            assert!(
                available
                    .iter()
                    .any(|(key, ok)| key.starts_with("claude-opus-4.5") && *ok),
                "{lanes:?}"
            );
            assert!(
                available
                    .iter()
                    .any(|(key, ok)| key.starts_with("ark-deepseek") && !*ok),
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
                .lanes()
                .options
                .iter()
                .find(|option| !option.available)
                .expect("an unavailable lanes model");
            let item = ChooserItem::from(blocked);
            assert!(item.disabled(), "{}", item.label);
            assert!(item.detail.contains("extension API"), "{}", item.detail);
        });
    }

    #[gpui_kit::test]
    fn a_live_registry_refresh_keeps_the_lanes_availability_exact(cx: &mut TestAppContext) {
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        f.act(cx, |window, cx| {
            // A probe's cache names the kernel api set, so the lanes chooser is measured.
            let probe = cache.cache();
            assert!(probe.kernel_apis_known());
            f.tab
                .update(cx, |tab, cx| tab.set_model_cache(&probe, window, cx));
            assert!(!lanes_uncertain(cx, &f.tab));

            // A live server's `/registry` moved the catalog forward (§9.4): it carries the
            // models but not the kernel's own `apis` — a live body's `apis` holds the
            // extensions the server added, which a lane cannot register. The cache kept the
            // probe's set, so the lanes chooser stays exact instead of falling back to
            // uncertain.
            let mut live_body = probe.registry.clone();
            live_body
                .as_object_mut()
                .expect("a registry object")
                .remove("apis");
            let live = probe.clone().with_live_registry(live_body);
            f.tab
                .update(cx, |tab, cx| tab.set_model_cache(&live, window, cx));

            assert!(
                !lanes_uncertain(cx, &f.tab),
                "a live refresh lost the kernel api set"
            );
            let lanes = lanes_options(cx, &f.tab);
            assert!(
                lanes
                    .iter()
                    .any(|(key, ok)| key.starts_with("claude-opus-4.5") && *ok),
                "{lanes:?}"
            );
            assert!(
                lanes
                    .iter()
                    .any(|(key, ok)| key.starts_with("ark-deepseek") && !*ok),
                "{lanes:?}"
            );
        });
    }

    #[gpui_kit::test]
    fn choosing_a_lanes_model_shows_the_file_it_is_written_to_and_lands_in_the_plan(
        cx: &mut TestAppContext,
    ) {
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_model_cache(&cache.cache(), window, cx)
            });
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
            // §9.6: the choice is written into the folder's own file, so the caption names
            // it — with the placeholder folder while none is chosen.
            let caption = caption_text(cx, &f.tab);
            assert!(caption.contains("<folder>/.evo/swarm.lisp"), "{caption}");
            assert!(caption.contains("shared by every tab"), "{caption}");

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
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_model_cache(&cache.cache(), window, cx)
            });
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

            // The rows carry the folder's name, the `~` path and the meta line (§9.5).
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
            Choice::Workers => state.workers.clone(),
        }
    }

    #[gpui_kit::test]
    fn a_chooser_opens_from_space_walks_with_the_arrows_and_closes_on_escape(
        cx: &mut TestAppContext,
    ) {
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        // Every step is its own act: opening and closing go through the select's own
        // actions, which GPUI dispatches on the way out of the update they were queued in.
        let trigger = f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_model_cache(&cache.cache(), window, cx)
            });
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
            f.tab.update(cx, |tab, cx| tab.set_scanning(true, cx));
            window.render_frame(cx);
            assert!(window.find(HISTORY_HINT_ID).visible());

            // The scan could not read the sessions directory: it says why.
            f.tab.update(cx, |tab, cx| tab.set_scanning(false, cx));
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

    /// The tab's own focus handles, in the order they should be reached: the three
    /// choosers, the folder card, then the history list.
    fn focus_order(cx: &App, tab: &Entity<TabContent>) -> Vec<FocusHandle> {
        let state = tab.read(cx).choosers.state.read(cx);
        vec![
            state.coordinator.read(cx).focus_handle(cx),
            state.lanes.read(cx).focus_handle(cx),
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

    #[gpui_kit::test]
    fn the_note_names_the_folder_the_app_expects(cx: &mut TestAppContext) {
        let cache = CacheDir::new(REGISTRY);
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_home(Some("/Users/you".to_string()), cx);
                tab.set_folder_hint(Some(PathBuf::from("/Users/you/coding/foo")), cx);
                tab.set_model_cache(&cache.cache(), window, cx);
            });
            window.render_frame(cx);
            // The note is generic until a lanes model is chosen.
            assert_eq!(caption_text(cx, &f.tab), "");

            f.tab.update(cx, |tab, cx| {
                let lanes = tab.choosers.state.read(cx).lanes.clone();
                lanes.update(cx, |_, cx| {
                    cx.emit(SelectEvent::Confirm(Some(SharedString::from(
                        "claude-opus-4.5@anthropic",
                    ))))
                });
            });
        });

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let caption = caption_text(cx, &f.tab);
            assert!(
                caption.contains("~/coding/foo/.evo/swarm.lisp"),
                "the note should name the folder the app expects: {caption}"
            );
        });
    }

    #[gpui_kit::test]
    fn the_catalog_failure_says_so_where_the_hint_was(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.tab.update(cx, |tab, cx| {
                tab.set_catalog_error(Some("could not start a probe".to_string()), cx)
            });
            window.render_frame(cx);
            assert_eq!(caption_text(cx, &f.tab), "could not start a probe");
        });
    }
}
