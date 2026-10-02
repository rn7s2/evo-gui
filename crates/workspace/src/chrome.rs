//! The window this app opens and the chrome it opens with (§7.1).
//!
//! One window, whose title bar *is* the tab strip ([`crate::tab_strip`]): one tab
//! per swarm, plus a `+` that adds one — and never a second empty one, since a
//! strip has one New Swarm tab at a time (§7.1, [`WorkspaceView::add_tab`]).
//! [`WorkspaceView`] owns
//! the tab set, the selection and what the pointer is on; everything below the
//! strip belongs to a [`TabContent`](crate::TabContent).

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Sizable as _, TitleBar};
use gpui_kit::component::{ResizablePanelEvent, ResizableState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, point, px, size, AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Global, IntoElement, KeyBinding, KeyUpEvent, ModifiersChangedEvent, Pixels, ScrollHandle,
    SharedString, Size, Subscription, Task, TestSupportExt as _, Window, WindowBounds,
    WindowOptions,
};

use serde_json::Value;
use session::{HistoryEntry, LaunchPlan};
use store::app_state::Panes;
use tab_engine::EngineHandle;

use crate::history::folder_name;
use crate::launch::{Launch, LaunchEnv};
use crate::panes;
use crate::tab::{TabContent, TabContentEvent, TabId};

/// The app's own name: what the bundle, the menu bar and the About window call it
/// (`scripts/bundle.sh`, `crates/app`). The window title ends with it (§7.1).
const APP_NAME: &str = "Evo Desktop";

/// The size the window opens at when the display has room for it (§7.1).
pub const DEFAULT_WINDOW_SIZE: Size<Pixels> = size(px(1600.), px(1000.));

/// The smallest window that still fits the lane column, a transcript and the
/// composer side by side (§7.3).
pub const MIN_WINDOW_SIZE: Size<Pixels> = size(px(1000.), px(700.));

/// The page's widths, as the panels ended up: the first panel's and the last
/// panel's, with the middle column left to take what remains (§7.3).
///
/// `None` until the panels have laid out once — before that there is no page to
/// measure, and nothing to remember.
fn panes_from_sizes(sizes: &[Pixels]) -> Option<Panes> {
    let [left, _] = sizes else {
        return None;
    };
    Some(Panes {
        left: left.as_f32(),
    })
}

/// The key context the window's own shortcuts are bound in (§7.1).
///
/// Nothing is bound in it any more — the tab strip's shortcuts are bound
/// app-wide (see [`bind_tab_keys`]) — but the window root still carries it, and
/// the composer's own context sits inside it, so a shortcut means the same thing
/// wherever the keyboard happens to be inside the window.
const WORKSPACE_CONTEXT: &str = "Workspace";

/// How often a closed tab looks whether its swarm has exited yet.
const TERMINATE_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// How long ⌘Q has to be held before it means quit (§9.8).
///
/// A keyboard quit is a mistake away from a mouse click's worth of damage — six
/// swarms and their transcripts — so the shortcut asks for a hold, the way a
/// browser's does. The menu item is the deliberate path: it quits at once.
pub const QUIT_HOLD: Duration = Duration::from_secs(1);

/// The toast that says what a held ⌘Q is waiting for (§9.8).
const HOLD_TOAST_TEXT: &str = "Hold ⌘Q to Quit";
const HOLD_TOAST_ID: &str = "quit-hold-toast";

/// What the window says while the app's sessions are being stopped (§9.8): one
/// session, or several. A session, not a swarm: a tab may hold either program, and the
/// screen is about the processes, which are the same thing either way.
const QUIT_SCREEN_ID: &str = "quit-screen";
const QUIT_SCREEN_LABEL_ID: &str = "quit-screen-label";
const QUIT_ONE_TEXT: &str = "Terminating session.";
const QUIT_MANY_TEXT: &str = "Terminating sessions.";

gpui_kit::actions!(
    workspace,
    [
        /// Show the tab to the right of this one (⌃⇥, ⌘⇧]).
        SelectNextTab,
        /// Show the tab to the left of this one (⌃⇧⇥, ⌘⇧[).
        SelectPreviousTab,
        /// Show the last tab, however many are open (⌘9).
        SelectLastTab,
        /// ⌘Q, held: quit once the hold is complete (§9.8).
        HoldToQuit,
    ]
);

/// ⌘Q was held long enough: the window's half of the quit (§9.8).
///
/// The key has to be watched where the keys are — the window — because a *hold*
/// is a key-down and a key-up, and the menu item's own action only ever fires
/// once. The app subscribes to this and runs the quit sequence; a ⌘Q tapped, or
/// released before the hold is over, never gets here.
#[derive(Clone, Debug)]
pub struct QuitHeld;

/// Show the tab at this index (0-based) — what ⌘1…⌘8 are.
///
/// A number rather than eight near-identical actions: the eight bindings differ
/// only in which tab they name. A menu can name it too
/// (`workspace::SelectTab`), which is why it is an action and not a key handler.
#[derive(Clone, PartialEq, Eq, Debug, gpui_kit::Action)]
#[action(namespace = workspace, no_json)]
pub struct SelectTab(pub usize);

/// Set once the tab keys are in the keymap: the keymap belongs to the app, and a
/// second window must not stack a second copy of every binding onto it.
struct TabKeysBound;
impl Global for TabKeysBound {}

/// Bind the window's tab shortcuts (§7.1) — once per app, whatever opens the
/// first window.
///
/// They are bound **app-wide**, with no key context, for two reasons:
///
/// * macOS draws a menu item's key equivalent from the app's keymap, and the
///   Window menu's items are these actions. A binding that exists only inside a
///   window's context is not in that keymap when the menu bar is built — and the
///   menu bar is built once, at install, before any window exists — so the menu
///   would show a bare title. This is why the app calls this function before it
///   sets its menus, and not only when a window opens.
/// * the shortcuts mean the same thing wherever the keyboard is: the composer's
///   text field binds none of ⌃⇥, ⌃⇧⇥, ⌘1…⌘9 or ⌘⇧[/⌘⇧], so a global binding
///   changes no text-editing behaviour while it reaches the strip from anywhere
///   in the window.
pub fn bind_tab_keys(cx: &mut App) {
    if cx.has_global::<TabKeysBound>() {
        return;
    }
    cx.set_global(TabKeysBound);
    let mut keys = vec![
        KeyBinding::new("ctrl-tab", SelectNextTab, None),
        KeyBinding::new("ctrl-shift-tab", SelectPreviousTab, None),
        KeyBinding::new("cmd-shift-]", SelectNextTab, None),
        KeyBinding::new("cmd-shift-[", SelectPreviousTab, None),
        KeyBinding::new("cmd-9", SelectLastTab, None),
    ];
    // ⌘1…⌘8 is the tab with that number; with fewer tabs than the number, the
    // shortcut does nothing rather than wrapping (§7.1).
    keys.extend(
        (0..8).map(|index| KeyBinding::new(&format!("cmd-{}", index + 1), SelectTab(index), None)),
    );
    cx.bind_keys(keys);
}

/// The bounds the window opens at: [`DEFAULT_WINDOW_SIZE`], clamped to the
/// display's work area — the visible area, without the dock or the menu bar —
/// and centered in it.
pub fn initial_window_bounds(cx: &App) -> WindowBounds {
    match cx.primary_display().map(|display| display.visible_bounds()) {
        Some(work_area) => WindowBounds::Windowed(fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area)),
        // Headless or display-less: take the default size rather than fail.
        None => WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), DEFAULT_WINDOW_SIZE)),
    }
}

/// `requested` centered in `work_area`, shrunk when the work area is smaller.
///
/// Never returns bounds that stick out of the work area, so a small display
/// still shows the whole window.
pub fn fit_to_work_area(requested: Size<Pixels>, work_area: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = requested.min(&work_area.size);
    let center = work_area.center();
    let offset = size / 2.0;
    Bounds::new(
        point(center.x - offset.width, center.y - offset.height),
        size,
    )
}

/// The options the app's window opens with.
///
/// `TitleBar::window_options()` is the base because the title bar is the tab
/// strip: it lets the title bar own dragging and double-clicking instead of the
/// system (§7.1).
pub fn window_options(cx: &App) -> WindowOptions {
    let mut options = WindowOptions {
        window_bounds: Some(initial_window_bounds(cx)),
        window_min_size: Some(MIN_WINDOW_SIZE),
        ..TitleBar::window_options()
    };
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.traffic_light_position = Some(traffic_light_position());
    }
    options
}

/// AppKit's frame for each traffic light button: 14pt square, the circle
/// centered in it.
const TRAFFIC_BUTTON: f32 = 14.;

/// Where the traffic lights sit: vertically centered in the tab strip.
///
/// `TitleBar`'s own `(9, 9)` centers them in its 34px bar, which leaves them
/// 4px high in the 42px strip. The x stays at AppKit's 9.
pub fn traffic_light_position() -> gpui_kit::Point<Pixels> {
    point(
        px(9.),
        px((store::design::STRIP_HEIGHT - TRAFFIC_BUTTON) / 2.),
    )
}

/// What the window is closed with (§9.8).
///
/// The window hands over every swarm it still has running — the live tabs' own
/// and the ones a closed tab is still waiting for — and stopping them is the
/// hook's job.
///
/// The hook answers "am I taking this quit over?". `true` vetoes this close: the
/// app keeps the window up as the screen its swarms' exit is shown on, and ends
/// the process itself when they have gone. `false` lets the window close now,
/// which is what an app that has nothing left to do returns.
pub type QuitHook = Box<dyn Fn(QuitRequest, &mut Window, &mut App) -> bool + 'static>;

/// Everything the app learns about models and past sessions, which every empty
/// tab shows (§9.4, §9.5).
///
/// The app owns the catalog probe and the session scan; the window only carries
/// their results to the tabs that draw them — the ones that exist now and the
/// ones opened afterwards.
#[derive(Clone, Default)]
pub struct LauncherData {
    /// The catalog body (`evo-swarm catalog --json`, or a server's `GET /catalog`),
    /// as it arrived: the model choosers and the launch check read it (§5.6).
    pub catalog: Option<Value>,
    /// Why the catalog could not be read, when it could not.
    pub catalog_error: Option<String>,
    /// The session index is still being read.
    pub history_loading: bool,
    /// The resumable swarms the index lists (§2).
    pub history: Vec<HistoryEntry>,
    /// The clock the rows' relative times read against, and the local offset.
    pub now: i64,
    pub offset_seconds: i32,
    /// Why the index could not be read, when it could not.
    pub history_error: Option<String>,
    /// `$HOME`, for shortening the paths in the rows.
    pub home: Option<String>,
}

/// One tab as an app stores it (§6, §9.8).
///
/// `window_id` is the id this window knows the tab by — stable while it is open,
/// never reused. `store_id` is the `tabs/<id>/` directory its swarm writes to,
/// which is what a stored set has to keep so the directory can be found again;
/// it is `None` until the tab starts a swarm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabRecord {
    pub window_id: TabId,
    pub store_id: Option<store::paths::TabId>,
    pub folder: Option<PathBuf>,
    pub session: Option<PathBuf>,
    /// Whether this tab is a swarm or one `evo-agent` (§7.2): the app writes it into its
    /// own record of the session, which is what a history row reads when the session
    /// index cannot say it.
    pub swarm: bool,
}

/// What [`QuitHook`] is handed.
pub struct QuitRequest {
    /// Every swarm this window still has running, shared: the app stops them and
    /// waits for them to go (§9.8). The tabs keep their own handles — the window
    /// stays on screen, covered by the quitting screen, while they go.
    pub swarms: Vec<Rc<EngineHandle>>,
}

/// The window's root view: the tab strip in the title bar, over the selected
/// tab's content (§7.1).
pub struct WorkspaceView {
    tabs: Vec<Entity<TabContent>>,
    /// Index into `tabs`, always valid: there is at least one tab.
    selected: usize,
    /// The window's own focus: what holds the keyboard when the shown tab has
    /// nothing to type into — and what makes the point of a keystroke the window
    /// rather than nowhere, so the shortcuts land on the tab strip (§7.1).
    root_focus: FocusHandle,
    next_id: u64,
    strip_scroll: ScrollHandle,
    /// One subscription per tab, dropped with it so a closed tab stops sending.
    subscriptions: VecDeque<(TabId, Subscription)>,
    /// What every tab of this window starts its swarms with (§3).
    config: Arc<LaunchEnv>,
    /// What the app wants done when the window is closed (§9.8). Without one the
    /// window stops every tab's swarm and then closes.
    quit_hook: Option<QuitHook>,
    /// The catalog and the session list the app pushed, waiting for the tabs that
    /// show them (§9.4, §9.5).
    launcher: LauncherData,
    /// Tabs whose run ended while another tab was being shown: the strip keeps a
    /// dot on them until they are looked at (§7.1).
    finished: BTreeSet<TabId>,
    /// The tab the pointer is on, if any.
    ///
    /// The strip needs to *know* this, not merely style itself: the design's
    /// `:has(+ .tab:hover)` hides the divider of the tab beside the one being
    /// pointed at, and a tab's own outward corner is drawn by its neighbour. Both
    /// are rules about a tab's next-door tab, which no element's own hover style
    /// can express.
    hovered: Option<usize>,
    /// The close button the pointer is on, if any.
    ///
    /// The `×` is `currentColor` in the design — the tab ink at rest, the
    /// foreground while its button is pointed at — and a glyph is painted rather
    /// than styled, so which ink to paint it in is state here, as the hovered tab
    /// is.
    close_hovered: Option<TabId>,
    /// A left button is down on the strip's own pixels (not on a tab): a move
    /// while it is held drags the window, which is what the kit's title bar did
    /// before the strip drew itself.
    strip_drag: bool,
    /// The title the window has been given, so a redraw does not rename it every
    /// frame (§7.1).
    window_title: Option<SharedString>,
    /// Set once the close hook is registered, so it happens once per window.
    close_hook_installed: bool,
    /// The task waiting for every tab's swarm to stop; `Some` while the window is
    /// stopping them.
    stopping: Option<Task<()>>,
    /// The swarms a closed tab is still waiting for (§7.1): one entry per tab on
    /// the strip that is terminating. A quit waits for these too — the engine is
    /// what holds the session, and the app may not exit under a swarm that is
    /// still running (§9.8).
    terminating: Vec<(TabId, Rc<EngineHandle>)>,
    /// ⌘Q is down and the hold is still being timed (§9.8): the toast is up while
    /// this is true. The flag is what a repeated key-down is answered with, so a
    /// hold that is repeating under the key does not restart its own clock.
    holding_quit: bool,
    /// The task timing that hold, held so the timer outlives the keystroke.
    quit_hold: Option<Task<()>>,
    /// The app is quitting and this many swarms are still stopping (§9.8): the
    /// window covers itself with the quitting screen until the last one is gone.
    quitting_swarms: Option<usize>,
    /// Where the keyboard goes while that screen is up: the page under it belongs
    /// to a session on its way out and must not take a keystroke (§9.8).
    quit_focus: FocusHandle,
    /// True once nothing is left to wait for and the window may close.
    may_close: bool,
    /// The tab page's side columns (§7.3): one pair of widths for the whole
    /// window, shared by every tab's page and remembered in `app.json`.
    panes: Panes,
    /// The drag machinery behind those widths (`gpui_base`'s resizable panels).
    /// One state for every page, so a split dragged in one tab is dragged in all
    /// of them — the pages are the same three columns.
    pane_state: Entity<ResizableState>,
    /// Whether the strip still owes the shown tab a reveal (§7.1). See
    /// [`WorkspaceView::reveal_selected_tab`].
    strip_reveal: bool,
    /// How far the strip can scroll, which is its content against its room: the
    /// shape of it. A strip that has changed under the tab being shown is
    /// noticed through this even when nothing said so (§7.1) — a title, a dot, a
    /// tab added, a window resized.
    strip_content: Option<gpui_kit::Point<Pixels>>,
    /// The state's own report that a drag (or a programmatic resize) is over.
    /// `Some` for the life of the window: it is made in [`WorkspaceView::with_config`].
    pane_resized: Option<Subscription>,
}

impl WorkspaceView {
    /// A window with one empty tab: the app never auto-starts a swarm and never
    /// has an empty window (§7.2, §14.6).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        WorkspaceView::with_config(Arc::new(LaunchEnv::default()), window, cx)
    }

    /// The same window, with the binaries, the app root and the environment its
    /// tabs start their swarms with.
    pub fn with_config(
        config: Arc<LaunchEnv>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        bind_tab_keys(cx);
        let mut view = WorkspaceView {
            tabs: Vec::new(),
            selected: 0,
            root_focus: cx.focus_handle(),
            next_id: 0,
            strip_scroll: ScrollHandle::default(),
            subscriptions: VecDeque::new(),
            config,
            quit_hook: None,
            launcher: LauncherData::default(),
            finished: BTreeSet::new(),
            hovered: None,
            close_hovered: None,
            strip_drag: false,
            window_title: None,
            close_hook_installed: false,
            stopping: None,
            terminating: Vec::new(),
            holding_quit: false,
            quit_hold: None,
            quitting_swarms: None,
            quit_focus: cx.focus_handle(),
            may_close: false,
            strip_reveal: false,
            strip_content: None,
            panes: Panes::default(),
            pane_state: cx.new(|_| ResizableState::default()),
            pane_resized: None,
        };
        // The columns the app was left with (§7.3), before any page is built.
        let stored = store::app_state::AppState::load(&view.config.root);
        view.panes = panes::fit(stored.panes, window.bounds().size.width.into());
        // Where a resized split ends up: the panels tell the state, the state
        // tells the window, and the window is what remembers it (§7.3).
        view.pane_resized = Some(cx.subscribe_in(
            &view.pane_state,
            window,
            |this, state, _: &ResizablePanelEvent, window, cx| {
                this.panes_resized(state, window, cx);
            },
        ));
        // The first tab takes the keyboard as it opens (§7.1): without a focus in
        // the frame, a shortcut pressed on a fresh window would go nowhere at all.
        view.open_empty_tab(window, cx);
        view
    }

    /// The widths, against the window they are drawn in (§7.3).
    ///
    /// A window narrowed under a pair of wide columns would otherwise leave the
    /// transcript with nothing: the sides come in, and the panels are told, so
    /// what is on screen is what is remembered. A window wide again leaves them
    /// where they were — a resize is not a reason to grow a column the reader
    /// sized by hand.
    fn fit_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fitted = panes::fit(self.panes, window.bounds().size.width.into());
        if fitted == self.panes {
            return;
        }
        self.set_panes(fitted, cx);
        let state = self.pane_state.clone();
        state.update(cx, |state, cx| {
            state.resize_panel(0, px(fitted.left), window, cx)
        });
    }

    /// A split was dragged — or something moved one: take the widths the panels
    /// ended at, fit them to the window, hand them to every tab's page and write
    /// them down (§7.3).
    fn panes_resized(
        &mut self,
        state: &Entity<ResizableState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sizes = state.read(cx).sizes().to_vec();
        let Some(panes) = panes_from_sizes(&sizes) else {
            return;
        };
        let panes = panes::fit(panes, window.bounds().size.width.into());
        self.set_panes(panes, cx);
    }

    /// The widths, everywhere they are needed: the pages, the app file.
    fn set_panes(&mut self, panes: Panes, cx: &mut Context<Self>) {
        if self.panes == panes {
            return;
        }
        self.panes = panes;
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| tab.set_panes(panes, cx));
        }
        self.remember_panes();
        cx.notify();
    }

    /// `app.json`'s own pair of widths, in place: the file is read, changed and
    /// written back whole, the way the app's other remembered facts are (§6).
    fn remember_panes(&self) {
        let mut state = store::app_state::AppState::load(&self.config.root);
        if state.panes == self.panes {
            return;
        }
        state.panes = self.panes;
        let _ = state.save(&self.config.root);
    }

    /// The width the window is showing its tab pages' agent column at (§7.3).
    ///
    /// One for the window, remembered in `app.json`: the two columns are the same
    /// two in every tab.
    pub fn panes(&self) -> Panes {
        self.panes
    }

    /// How many tabs the window is showing.
    pub fn open_tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// The title the window was last given (§7.1): the app's name, or the folder of
    /// the tab being shown.
    ///
    /// Readable here because the platform's own title is not readable back — a
    /// headless test window answers `""` to `Window::window_title` even right after
    /// it was set, which would leave the naming untested.
    pub fn window_title(&self) -> &str {
        self.window_title.as_deref().unwrap_or(APP_NAME)
    }

    /// Every swarm this window still has running (§9.8): a live tab's own engine,
    /// and the engines of the tabs a close is still waiting for.
    ///
    /// The handles are shared — a tab's page holds its own, and a terminating tab
    /// has handed its to the close that is watching it — so a caller stops them
    /// where they are ([`EngineHandle::shutdown`] takes `&self`) rather than taking
    /// them away from a window that is still on screen.
    pub fn swarms(&self, cx: &App) -> Vec<Rc<EngineHandle>> {
        let mut swarms: Vec<Rc<EngineHandle>> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.read(cx).engine())
            .collect();
        swarms.extend(self.terminating.iter().map(|(_, engine)| engine.clone()));
        swarms
    }

    /// What the app does when the window is closed (§9.8). Without a hook the
    /// window stops every tab's swarm and closes itself.
    pub fn set_quit_hook(&mut self, hook: QuitHook) {
        self.quit_hook = Some(hook);
    }

    /// The app is quitting, and the window may not go while an editor here holds
    /// changes that are not on disk (§9.8): the app asks this first, and decides what
    /// to do with the answer.
    ///
    /// Both kinds count — the Settings tab's own editors, and the project settings a
    /// running tab is showing or has shown.
    pub fn has_dirty_settings(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.read(cx).is_settings_dirty(cx))
    }

    /// Throw away every unsaved settings change in this window — what the app's quit
    /// runs when the person would rather lose them than stay (§9.8).
    ///
    /// Putting a draft back is an input's own work, and an input lives in a window,
    /// which the app's quit has in hand. A page that is *writing* keeps what it is
    /// writing: nothing here throws away an edit that is already on its way to disk.
    pub fn discard_settings_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in self.tabs.clone() {
            tab.update(cx, |tab, cx| tab.discard_settings_changes(window, cx));
        }
        cx.notify();
    }

    /// Whether any tab in this window is writing a settings file right now — the
    /// other half of the quit's question (§9.8): a write that is in flight cannot be
    /// called back, so the app waits for it rather than exiting under it.
    pub fn has_saving_settings(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.read(cx).is_settings_saving(cx))
    }

    /// Show the first tab an exit would have to wait for, with its editor in front
    /// and the keyboard in it — what the app's quit runs when the person would rather
    /// keep what they were writing (§9.8). Answers whether there was such a tab.
    ///
    /// A write in flight comes first: it is the one thing that cannot be called back,
    /// and the page is where its own status line says so. A tab holding a draft comes
    /// next — that one *can* be answered for, which is what the quit is asking about.
    /// Both are what [`Self::has_saving_settings`] and [`Self::has_dirty_settings`]
    /// report; a close question that is already up stays up, because it belongs to
    /// the person.
    pub fn focus_dirty_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut chosen = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            let tab = tab.read(cx);
            if tab.is_settings_saving(cx) {
                chosen = Some(index);
                break;
            }
            if chosen.is_none() && tab.is_settings_dirty(cx) {
                chosen = Some(index);
            }
        }
        let Some(index) = chosen else {
            return false;
        };
        let tab = self.tabs[index].clone();
        tab.update(cx, |tab, cx| tab.reveal_dirty_settings(window, cx));
        self.select_tab(index, window, cx);
        true
    }

    /// Open a tab — what ⌘T, the Window menu's New Tab and the strip's `+` do
    /// (§7.1).
    ///
    /// There is at most one New Swarm tab at a time (§7.1): asked for a tab while
    /// an empty one is on the strip, this shows that one and hands it the keyboard
    /// — what clicking it would do — rather than stacking up empty tabs. A tab
    /// whose launch failed, or one that is booting, is not empty: it is a swarm
    /// with something to say, and [`Self::open_empty_tab`] is what it asks for.
    pub fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TabContent> {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.read(cx).state() == &crate::tab::TabState::Empty)
        {
            let tab = self.tabs[index].clone();
            self.select_tab(index, window, cx);
            return tab;
        }
        self.open_empty_tab(window, cx)
    }

    /// Close the tab being shown — what ⌘W does (§7.1). The last tab is never
    /// closed: the window keeps a fresh empty one instead (§7.2).
    pub fn close_selected_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.tabs[self.selected].read(cx).id();
        self.close_tab(id, window, cx);
    }

    /// The catalog and the session list the app has learned, applied to every tab
    /// (and to every empty tab opened later) (§9.4, §9.5).
    pub fn set_launcher_data(
        &mut self,
        data: LauncherData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.launcher = data;
        let data = self.launcher.clone();
        for tab in &self.tabs {
            let data = data.clone();
            tab.update(cx, |tab, cx| tab.set_launcher_data(&data, window, cx));
        }
    }

    /// The tabs as an app persists them: strip order, one [`TabRecord`] per tab,
    /// and `selected_index()` says which one is showing (§9.8).
    ///
    /// `folder`, `store_id` and `session` are `None` only while a tab has never
    /// started a swarm; `session` alone stays `None` until that swarm has answered
    /// `/state`, which is the path `Launch::Resume` takes (§9.5).
    pub fn tab_records(&self, cx: &App) -> Vec<TabRecord> {
        self.tabs
            .iter()
            // The Settings tab is not one of them: it holds no folder, no swarm and
            // no session, and a stored set that named it would promise a resume that
            // cannot come back (§6, §9.8). It is always the last tab, so what is left
            // here stands in the strip's own order.
            .filter(|tab| tab.read(cx).state() != &crate::tab::TabState::Settings)
            .map(|tab| {
                let tab = tab.read(cx);
                TabRecord {
                    window_id: tab.id(),
                    store_id: tab.store_id().cloned(),
                    folder: tab.folder().map(Path::to_path_buf),
                    // A tab closed while its swarm was still exiting was closed by the
                    // person: its session is not one that was open at quit (§9.5).
                    session: (!tab.is_terminating())
                        .then(|| tab.session_path().map(Path::to_path_buf))
                        .flatten(),
                    swarm: tab.swarm(cx),
                }
            })
            .collect()
    }

    /// What every tab this window opens **from now on** starts its swarm with
    /// (§13): what Settings saved. A tab that is already running keeps the binaries
    /// it started with, which is what the panel's own note says.
    pub fn set_launch_env(&mut self, config: Arc<LaunchEnv>, cx: &mut Context<Self>) {
        self.config = config;
        // A tab that has started nothing yet is still the empty tab, and its check
        // runs the binary Settings now names (§9, §13): a path fixed in the panel
        // has to take effect on the tab the person is looking at. A tab that has
        // started keeps what it started with — the panel's own note (§13).
        let swarm = self.config.swarm_bin.clone();
        let agent = self.config.agent_bin.clone();
        for tab in &self.tabs {
            tab.update(cx, |tab, cx| {
                if tab.state() == &crate::tab::TabState::Empty {
                    tab.set_swarm_bin(swarm.clone(), cx);
                    tab.set_agent_bin(agent.clone(), cx);
                }
            });
        }
        cx.notify();
    }

    pub fn tabs(&self) -> &[Entity<TabContent>] {
        &self.tabs
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The tab whose content the window is showing.
    /// The tab the pointer is on, for the strip ([`crate::tab_strip`]).
    pub(crate) fn hovered_tab(&self) -> Option<usize> {
        self.hovered
    }

    /// The pointer moved on or off the tab at `index`.
    ///
    /// Only a change repaints: moving within one tab's pixels is not news to
    /// anyone, and a tab set can be large.
    ///
    /// Leaving is "clear it if it is still mine": the pointer can move straight
    /// from one tab onto the next, and the two listeners fire in whatever order
    /// the elements are walked — an unconditional clear would wipe the fill off
    /// the tab the pointer has already arrived on.
    pub(crate) fn set_hovered_tab(&mut self, index: usize, over: bool, cx: &mut Context<Self>) {
        let next = if over {
            Some(index)
        } else if self.hovered == Some(index) {
            None
        } else {
            return;
        };
        if self.hovered != next {
            self.hovered = next;
            cx.notify();
        }
    }

    /// The close button the pointer is on, for the strip ([`crate::tab_strip`]).
    pub(crate) fn close_hovered(&self) -> Option<TabId> {
        self.close_hovered
    }

    /// The pointer moved on or off the close button of the tab with this id: the
    /// `×` is drawn in the foreground while it is pointed at, and in the tab ink
    /// otherwise. Clearing works as [`Self::set_hovered_tab`]'s does, and for the
    /// same reason — one `×` is a mouse move away from the next.
    pub(crate) fn set_close_hovered(&mut self, id: TabId, over: bool, cx: &mut Context<Self>) {
        let next = if over {
            Some(id)
        } else if self.close_hovered == Some(id) {
            None
        } else {
            return;
        };
        if self.close_hovered != next {
            self.close_hovered = next;
            cx.notify();
        }
    }

    /// Whether a left button is held on the strip's own pixels.
    pub(crate) fn strip_dragging(&self) -> bool {
        self.strip_drag
    }

    pub(crate) fn set_strip_dragging(&mut self, dragging: bool) {
        self.strip_drag = dragging;
    }

    /// Whether this tab's run ended while another tab was being shown: the strip
    /// keeps its ring at full strength until the tab is looked at (§7.1).
    pub(crate) fn has_unseen_finish(&self, id: TabId) -> bool {
        self.finished.contains(&id)
    }

    /// The strip's scroll: what shows the tab being shown.
    pub(crate) fn strip_scroll(&self) -> &ScrollHandle {
        &self.strip_scroll
    }

    pub fn selected_tab(&self) -> &Entity<TabContent> {
        &self.tabs[self.selected]
    }

    /// Append an empty tab and select it, whatever is already on the strip: the
    /// tab a resume opens in, the fresh one the last close leaves, a capture's
    /// extra tab.
    ///
    /// What a person's "new tab" does instead is [`Self::add_tab`], which is this
    /// only while there is no empty tab to show (§7.1).
    pub fn open_empty_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabContent> {
        let id = TabId::new(self.next_id);
        self.next_id += 1;
        let config = self.config.clone();
        let tab = cx.new(|cx| TabContent::new(id, config, window, cx));
        let at = self.place_tab(tab.clone(), window, cx);
        // The new tab is at the far end of the swarm tabs — past every other one,
        // which is where an overflowing strip has to scroll to (§7.1) — and adding
        // one changes the width every tab before it has. (The Settings tab, when
        // there is one, stays past *it*: it is the strip's last tab always.)
        self.select_tab(at, window, cx);
        self.ask_reveal(cx);
        // A tab opened now has the columns the window is showing (§7.3), and the
        // one drag state every page in the window shares.
        let pane_state = self.pane_state.clone();
        let panes = self.panes;
        tab.update(cx, |tab, cx| {
            tab.set_pane_state(pane_state, cx);
            tab.set_panes(panes, cx);
        });
        // A tab opened now shows what the app already learned (§9.4, §9.5). The
        // workers card's own switch is *not* the window's to hand over: a new page
        // starts on a swarm, whatever another tab's switch says (§7.2).
        let launcher = self.launcher.clone();
        tab.update(cx, |tab, cx| {
            tab.set_launcher_data(&launcher, window, cx);
        });
        // The tab being shown is where the keyboard goes (§7.1): `select_tab` above
        // moved it, and it matters beyond typing — GPUI resolves a keystroke against
        // the *focused* element's place in the frame, so a keyboard left on a tab
        // that is no longer drawn is a keyboard that answers no shortcut at all.
        cx.notify();
        tab
    }

    /// The window's Settings tab — what the gear at the far right of the strip does
    /// (§7.1).
    ///
    /// There is at most one. Asking for it while it is already open shows the one
    /// that is there rather than making a second, and it is always the strip's last
    /// tab: a stored tab set has no Settings in it ([`Self::tab_records`]), so the
    /// strip order with the Settings tab left out is exactly the order stored, and
    /// [`Self::selected_index`] addresses both of them the same way (§9.8).
    ///
    /// It drives no swarm: nothing about it is spawned, waited on or written down
    /// anywhere but the window.
    pub fn open_settings_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabContent> {
        if let Some(index) = self.settings_index(cx) {
            let tab = self.tabs[index].clone();
            self.select_tab(index, window, cx);
            return tab;
        }
        let id = TabId::new(self.next_id);
        self.next_id += 1;
        let config = self.config.clone();
        let tab = cx.new(|cx| TabContent::new_settings(id, config, window, cx));
        let at = self.place_tab(tab.clone(), window, cx);
        self.select_tab(at, window, cx);
        self.ask_reveal(cx);
        cx.notify();
        tab
    }

    /// Put a tab on the strip without showing it: watch its events, and leave the
    /// Settings tab last. Answers where on the strip it went.
    ///
    /// A new tab goes *before* the Settings tab, never after it — that is what keeps
    /// "the Settings tab is last" true for the life of the window, and with it the
    /// one-to-one reading between the strip and the stored tab set.
    fn place_tab(
        &mut self,
        tab: Entity<TabContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let at = self.settings_index(cx).unwrap_or(self.tabs.len());
        let id = tab.read(cx).id();
        let subscription = cx.subscribe_in(
            &tab,
            window,
            |this, tab, event: &TabContentEvent, window, cx| {
                let id = tab.read(cx).id();
                this.on_tab_event(id, event.clone(), window, cx);
            },
        );
        self.subscriptions.push_back((id, subscription));
        self.tabs.insert(at, tab);
        // Everything from the insertion point on has moved along one, the shown tab
        // included: it has to keep being the tab it was.
        if self.selected >= at {
            self.selected += 1;
        }
        at
    }

    /// Where the Settings tab is on the strip, while there is one (§7.1).
    fn settings_index(&self, cx: &App) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.read(cx).state() == &crate::tab::TabState::Settings)
    }

    /// Ask the strip to show the whole of the tab being shown (§7.1).
    ///
    /// A reveal is not a one-off. The strip's geometry moves under whoever is
    /// looking at it — a tab's dot appears when its coordinator starts working,
    /// a folder arrives with a title, the window changes width — and the offset
    /// that showed the tab a moment ago can leave it half past the strip's edge,
    /// where its label is cut mid-word instead of elided.
    fn ask_reveal(&mut self, cx: &mut Context<Self>) {
        self.strip_reveal = true;
        cx.notify();
    }

    /// Show the shown tab, once (§7.1).
    ///
    /// `ScrollHandle` answers a reveal against the layout the frame *before* the
    /// one it is asked in left behind, so the ask is kept until a frame runs:
    /// asking at the moment the strip changes shape — a title arriving, a dot
    /// appearing, a window resize — is answered from the shape that is gone, and
    /// the tab is left where the change pushed it, half past the strip's edge,
    /// its label cut mid-word instead of elided. Waited for a frame, the answer
    /// is computed from the strip as it is.
    ///
    /// The first tab is the one case a reveal gets wrong: it brings the tab's own
    /// left edge to the strip's edge, which is a corner's room past the start, and
    /// the room the first tab's outward corner is drawn in is scrolled off with
    /// it. The strip's start is where that tab is shown whole, so that is what it
    /// asks for.
    fn reveal_selected_tab(&mut self, _cx: &mut Context<Self>) {
        if !self.strip_reveal {
            return;
        }
        self.strip_reveal = false;
        if self.selected == 0 {
            self.strip_scroll.set_offset(point(px(0.), px(0.)));
            return;
        }
        self.strip_scroll.scroll_to_item(self.selected);
    }

    /// Show the tab at `index`, and put the keyboard where that tab's work starts
    /// (§7.1): the composer of a tab driving a swarm, so a tab picked to be typed
    /// into takes the typing.
    ///
    /// Out of range does nothing, which is what a `⌘8` with three tabs open means.
    pub fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        // A strip with more tabs than fit scrolls to the one being shown, so the
        // highlight is never off-screen — and the whole of it: a tab revealed by
        // half is a label cut mid-word (§7.1).
        self.ask_reveal(cx);
        if index != self.selected {
            self.selected = index;
            cx.notify();
        }
        self.focus_selected(window, cx);
    }

    /// The keyboard follows the tab: whatever is shown is where typing goes (§7.1,
    /// §7.2).
    ///
    /// A tab with nothing to type into yet — an empty one, until its own first
    /// chooser is the seam for that — hands the keyboard back to the window rather
    /// than leaving it in a composer nobody can see: a keystroke nobody hears is
    /// worse than one the window handles.
    fn focus_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs[self.selected].clone();
        // Looking at a tab is what clears the dot a finish left on it (§7.1).
        if self.finished.remove(&tab.read(cx).id()) {
            cx.notify();
        }
        let taken = tab.update(cx, |tab, cx| tab.focus_primary(window, cx));
        if !taken {
            window.focus(&self.root_focus, cx);
        }
    }

    /// The window's own title, for Mission Control and ⌘` (§7.1): the folder of the
    /// tab being shown, or the app's name alone on an empty tab.
    ///
    /// Kept current from `render` — the selection, a chosen folder and a closed tab
    /// all end in a redraw — and compared against what the window was last given,
    /// so a redraw is not a rename.
    fn sync_window_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = match self.tabs[self.selected].read(cx).folder() {
            Some(folder) => SharedString::from(format!("{} — {}", folder_name(folder), APP_NAME)),
            None => SharedString::from(APP_NAME),
        };
        if self.window_title.as_ref() != Some(&title) {
            window.set_window_title(&title);
            self.window_title = Some(title);
        }
    }

    /// ⌃⇥ and ⌘⇧]: the next tab, round the end (§7.1).
    fn on_select_next_tab(
        &mut self,
        _: &SelectNextTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_tab(1, window, cx);
    }

    /// ⌃⇧⇥ and ⌘⇧[: the previous tab, round the start (§7.1).
    fn on_select_previous_tab(
        &mut self,
        _: &SelectPreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_tab(-1, window, cx);
    }

    /// ⌘9: the last tab, whatever the count (a browser's rule, and the reason ⌘9
    /// is not `SelectTab(8)`).
    fn on_select_last_tab(
        &mut self,
        _: &SelectLastTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_tab(self.tabs.len().saturating_sub(1), window, cx);
    }

    /// ⌘1…⌘8: the tab with that number.
    fn on_select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(action.0, window, cx);
    }

    /// One step along the strip, wrapping: with one tab open there is nowhere to
    /// go, and the shortcut does nothing rather than reopening the same tab.
    fn step_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        if count < 2 {
            return;
        }
        let next = (self.selected as isize + step).rem_euclid(count as isize) as usize;
        self.select_tab(next, window, cx);
    }

    /// Close `id`: the tab goes at once, and its swarm runs §3's ladder on a
    /// thread of its own (§14.5). Closing the last tab leaves a fresh empty one
    /// rather than an empty window (§7.2).
    pub fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.read(cx).id() == id)
            .cloned()
        else {
            return;
        };
        if tab.read(cx).is_terminating() {
            // Already on its way out: a second × has nothing more to do.
            return;
        }
        // A save that is in flight is not something to walk away from. The write is
        // on the background executor with a copy of the text, and dropping the editor
        // under it — or answering a discard that could not undo it — is how a file
        // changes with nobody watching. The close waits for it to land, and the page
        // that is writing comes to the front while it does: its own status line says
        // `Saving…`, which is the feedback — a press that looked like nothing
        // happened would be worse than the wait.
        if tab.read(cx).is_settings_saving(cx) {
            tab.update(cx, |tab, cx| tab.reveal_dirty_settings(window, cx));
            if let Some(index) = self.tabs.iter().position(|other| other.read(cx).id() == id) {
                self.select_tab(index, window, cx);
            }
            return;
        }
        // An editor with changes that are not on disk is asked about first: the tab
        // puts the Discard / Keep Editing question up and stays where it is until one
        // of them is answered (§7.1, §13).
        if tab.read(cx).is_settings_dirty(cx) && !tab.read(cx).is_closing_prompted() {
            tab.update(cx, |tab, cx| {
                tab.ask_close(cx);
            });
            return;
        }
        // The swarm is told to stop now (stdin EOF), and the tab stays on the strip,
        // frozen under "Terminating session.", until it has exited — so the session it
        // was writing is free before anything can open it again (§7.1, §8).
        let Some(engine) = tab.update(cx, |tab, cx| tab.terminate(window, cx)) else {
            // No swarm behind it (a New Swarm page, a boot that never started a
            // process): nothing to wait for.
            self.remove_tab(id, window, cx);
            return;
        };
        // The window keeps its own hold on the engine while the tab is on its way
        // out: a quit asks [`WorkspaceView::swarms`] what is still running, and this
        // one is still running after its tab has stopped showing it (§9.8).
        self.terminating.push((id, engine.clone()));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            // The engine thread ends after the server's own ladder has run; its
            // mailbox closing is the sign. Polled on a timer, so no other thread
            // ever wakes the UI's executor.
            while engine.is_running() {
                cx.background_executor().timer(TERMINATE_POLL).await;
            }
            drop(engine);
            let _ = this.update_in(cx, |view, window, cx| view.remove_tab(id, window, cx));
        })
        .detach();
    }

    /// Take a tab off the strip, with nothing left to stop.
    fn remove_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.read(cx).id() == id) else {
            return;
        };
        let was_shown = index == self.selected;
        let tab = self.tabs.remove(index);
        self.subscriptions.retain(|(closed, _)| *closed != id);
        // The engine this close was watching is not the window's to hold any more:
        // the tab that was running it is gone, and so is the watch.
        self.terminating.retain(|(closing, _)| *closing != id);
        if self.close_hovered == Some(id) {
            // The button the pointer was on went with the tab.
            self.close_hovered = None;
        }
        if let Some(engine) = tab.update(cx, |tab, cx| tab.take_engine(cx)) {
            // Only a tab whose swarm started after its close began still has one.
            drop(engine);
        }
        if self.tabs.is_empty() {
            self.open_empty_tab(window, cx);
            return;
        }
        // The tab that took the closed one's place, or the new last tab.
        self.selected = self.selected.min(self.tabs.len() - 1);
        if was_shown {
            // The keyboard does not go with the closed tab (§7.1): the tab that
            // took its place is what is being looked at now.
            self.focus_selected(window, cx);
        }
        cx.notify();
    }

    /// The app is quitting: cover the window with the quitting screen while the
    /// swarms it named are stopped (§9.8).
    ///
    /// The app owns the wait and the exit — this is the window's half, and it
    /// stays up (and takes every click and keystroke) until the process ends.
    /// Called once per quit: a second call with the same count is nothing new.
    pub fn show_quitting(&mut self, swarms: usize, cx: &mut Context<Self>) {
        if self.quitting_swarms == Some(swarms) {
            return;
        }
        self.quitting_swarms = Some(swarms);
        cx.notify();
    }

    /// How many swarms the quitting screen says are being terminated, while it is
    /// up; `None` when the app is not quitting (§9.8).
    pub fn quitting_swarms(&self) -> Option<usize> {
        self.quitting_swarms
    }

    /// Whether ⌘Q is down and its hold is still being timed: what the toast says
    /// (§9.8).
    pub fn is_holding_quit(&self) -> bool {
        self.holding_quit
    }

    /// ⌘Q went down (§9.8): put the toast up and start timing the hold.
    ///
    /// The key repeats while it is held, and every repeat is answered by the hold
    /// that is already being timed — restarting the clock on each repeat is
    /// exactly what a hold must not do.
    fn on_hold_to_quit(&mut self, _: &HoldToQuit, window: &mut Window, cx: &mut Context<Self>) {
        if self.holding_quit || self.quitting_swarms.is_some() {
            return;
        }
        self.holding_quit = true;
        self.quit_hold = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(QUIT_HOLD).await;
            let _ = this.update_in(cx, |view, _window, cx| view.hold_ran_out(cx));
        }));
        cx.notify();
    }

    /// The hold ran out: if ⌘Q is still down, this is the quit (§9.8).
    fn hold_ran_out(&mut self, cx: &mut Context<Self>) {
        if !self.holding_quit {
            return;
        }
        self.holding_quit = false;
        cx.notify();
        cx.emit(QuitHeld);
    }

    /// Any key came up: a hold in progress is over, and so is the toast (§9.8).
    /// The other half of the pair — ⌘ released with Q still down — arrives as a
    /// modifier change, below.
    fn on_key_up(&mut self, _: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.release_quit_hold(cx);
    }

    /// The modifiers changed: ⌘ up ends a hold that was waiting on it (§9.8).
    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.platform {
            self.release_quit_hold(cx);
        }
    }

    /// Let a hold go: nothing quits, and the toast comes down (§9.8).
    fn release_quit_hold(&mut self, cx: &mut Context<Self>) {
        if !self.holding_quit {
            return;
        }
        self.holding_quit = false;
        cx.notify();
    }

    /// What a tab's content asks the window for; the window owns the tab set, so
    /// the content reports intent instead of acting on it (§7.2).
    fn on_tab_event(
        &mut self,
        id: TabId,
        event: TabContentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every event a tab can send is one that may have changed how wide it
        // is — its title, its dot, whether it has a swarm at all — and the strip
        // reveals the shown tab against those widths (§7.1).
        self.ask_reveal(cx);
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.read(cx).id() == id)
            .cloned()
        else {
            return;
        };
        match event {
            TabContentEvent::Launch { folder, plan } => tab.update(cx, |content, cx| {
                content.launch(Launch::New { folder, plan }, window, cx)
            }),
            TabContentEvent::FolderPicked(folder) => tab.update(cx, |content, cx| {
                content.launch(
                    Launch::New {
                        folder,
                        plan: LaunchPlan::default(),
                    },
                    window,
                    cx,
                )
            }),
            TabContentEvent::Resume {
                session_path,
                folder,
                swarm,
            } => {
                // A session another tab already has open is shown, not started a
                // second time: two swarms on one journal would fork it (§7.1).
                if let Some(index) = self.tabs.iter().position(|other| {
                    other.read(cx).id() != id && other.read(cx).holds_session(&session_path)
                }) {
                    self.select_tab(index, window, cx);
                    return;
                }
                // The New Swarm page the row was picked on becomes the resumed
                // swarm, as a folder pick does: a resume is that page's answer,
                // not a second tab beside it. Only a tab that is not an empty
                // page (none today, but a resume can be asked for from anywhere)
                // gets a tab of its own.
                let tab = if tab.read(cx).state() == &crate::tab::TabState::Empty {
                    tab
                } else {
                    self.open_empty_tab(window, cx)
                };
                // The session index keeps a cwd as `…/project/`; the tab shows
                // and remembers the folder without that trailing slash.
                let folder = match folder.to_str().map(|f| f.trim_end_matches('/')) {
                    Some(trimmed) if !trimmed.is_empty() => PathBuf::from(trimmed),
                    _ => folder,
                };
                tab.update(cx, |content, cx| {
                    content.launch(
                        Launch::Resume {
                            folder,
                            session: session_path,
                            swarm,
                        },
                        window,
                        cx,
                    )
                });
            }
            TabContentEvent::RetryRequested(_) => {
                tab.update(cx, |content, cx| content.retry(window, cx))
            }
            TabContentEvent::CloseRequested => {
                self.close_tab(id, window, cx);
            }
            TabContentEvent::RunFinished => {
                // The run ended on a tab nobody was looking at — the strip keeps
                // that fact, with a dot, until the tab is shown. One that ends on
                // the tab being shown is watched, and leaves nothing (§7.1).
                if self.tabs[self.selected].read(cx).id() != id && self.finished.insert(id) {
                    cx.notify();
                }
            }
            TabContentEvent::ResetPane => {
                // A double-clicked split goes back to the width it starts at
                // (§7.3). The panel is what moves; the window hears it and writes
                // the result down, the way it does for a drag.
                let state = self.pane_state.clone();
                state.update(cx, |state, cx| {
                    state.resize_panel(0, px(store::app_state::LEFT_DEFAULT), window, cx)
                });
            }
            TabContentEvent::ScreenChanged => {
                // The screen changed under the keyboard. GPUI resolves a keystroke
                // against the focused element's place in the frame, so a keyboard
                // left on the screen that just went away answers nothing at all —
                // not even ⌘1. It goes where the new screen starts: the composer of
                // a page, the window for a boot, a failure or an empty tab's own
                // first chooser (§7.1). A background tab's screen is not this
                // window's keyboard, so only the shown one is moved.
                if self.tabs[self.selected].read(cx).id() == id {
                    self.focus_selected(window, cx);
                }
            }
        }
    }

    /// Register what happens when the user closes the window (§9.8). Once per
    /// window, from its first render, because it needs the view's own handle.
    fn install_close_hook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_hook_installed {
            return;
        }
        self.close_hook_installed = true;
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let Some(view) = this.upgrade() else {
                return true;
            };
            view.update(cx, |view, cx| view.should_close(window, cx))
        });
    }

    /// The window is being closed: hand the tabs over, let the app decide, or
    /// stop every swarm and close (§9.8).
    ///
    /// Never blocks: a hook is called on the UI thread and is expected to start
    /// work of its own, and the default path waits for the ladders on a task.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.may_close {
            return true;
        }
        if self.stopping.is_some() {
            // Already stopping every tab; the close it asks for is allowed.
            return false;
        }

        let swarms = self.swarms(cx);
        if let Some(hook) = self.quit_hook.as_ref() {
            // `true` means the app is closing the window itself, so this close is
            // vetoed; `false` lets it through. The app vetoes it while its swarms
            // stop: the window is the screen that wait is shown on (§9.8).
            let app_took_over = hook(QuitRequest { swarms }, window, cx);
            self.may_close = !app_took_over;
            return self.may_close;
        }

        // The child's stdin closes when the last handle to it goes; the ladder
        // itself runs on the engine's thread, so the window can close now. The
        // tabs are dropped with the window, and their own handles with them.
        drop(swarms);
        self.stopping = None;
        true
    }
}

impl EventEmitter<QuitHeld> for WorkspaceView {}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_close_hook(window, cx);
        self.sync_window_title(window, cx);
        self.fit_panes(window, cx);
        // The strip's shape is what its tabs take — and the window is part of
        // what they take. When that shape changes, the offset that showed the
        // tab being shown may no longer show it, and nothing has to have said
        // so: a dot appearing as a run starts changes it without an event, and
        // a window resize changes it in the same way (§7.1).
        let content = self.strip_scroll.max_offset();
        if self.strip_content != Some(content) {
            self.strip_content = Some(content);
            self.ask_reveal(cx);
        }
        self.reveal_selected_tab(cx);
        // What the window is showing: the strip over the shown tab's page, under
        // whichever layer is up — the hold toast while ⌘Q is held, or the quitting
        // screen while the app's swarms stop (§9.8).
        let page = v_flex()
            .size_full()
            .child(crate::tab_strip::strip(self, window, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.selected_tab().clone()),
            )
            .into_any_element();
        let layer = match self.quitting_swarms {
            Some(swarms) => {
                // The keyboard leaves the page with the screen: nothing under it
                // is still taking keystrokes (§9.8). One frame is what this takes
                // — the screen is focused before it is drawn, and focusing it
                // again is a no-op.
                if !self.quit_focus.is_focused(window) {
                    window.focus(&self.quit_focus, cx);
                }
                Some(self.quit_screen(swarms, cx))
            }
            None if self.holding_quit => Some(hold_toast(cx)),
            None => None,
        };
        v_flex()
            .id("workspace")
            .test_support()
            // The window root is where the tab strip's actions are handled
            // (§7.1), and the focus handle is what puts the keyboard somewhere
            // at all: a fresh window has nothing else holding it. The actions
            // themselves are bound app-wide (see `bind_tab_keys`), so `⌘2` or
            // `⌃⇥` reaches the strip from anywhere inside the window — the
            // composer included — and the context here is what the composer's
            // own context nests inside.
            .track_focus(&self.root_focus)
            .key_context(WORKSPACE_CONTEXT)
            .on_action(cx.listener(Self::on_select_next_tab))
            .on_action(cx.listener(Self::on_select_previous_tab))
            .on_action(cx.listener(Self::on_select_last_tab))
            .on_action(cx.listener(Self::on_select_tab))
            // ⌘Q is the one action whose *release* matters (§9.8): the hold is
            // timed from the key-down the keymap sends here, and it is over the
            // moment either the key or the modifier comes up.
            .on_action(cx.listener(Self::on_hold_to_quit))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(page)
            .when_some(layer, |this, layer| this.child(layer))
    }
}

/// The layer a held ⌘Q puts up: a small card in the middle of the window saying
/// what the key is waiting for (§9.8).
///
/// It takes no clicks: it is a hint about a key, not a dialog, and the window
/// under it is still the window — the hold is the decision, and nothing else is
/// being asked.
fn hold_toast(cx: &App) -> AnyElement {
    let theme = cx.theme();
    // Near the top, under the strip, where nothing of the page's own sits (the
    // empty transcript's note is at the middle): a floating hint, as Chrome's is.
    div()
        .absolute()
        .top(px(store::design::STRIP_HEIGHT + 72.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id(HOLD_TOAST_ID)
                .test_support()
                .px(px(16.))
                .py(px(10.))
                .rounded(px(10.))
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .text_size(px(13.))
                .text_color(theme.popover_foreground)
                .aria_label(HOLD_TOAST_TEXT)
                .child(HOLD_TOAST_TEXT),
        )
        .into_any_element()
}

/// The layer the app's quit puts up: what is happening, over everything, until
/// the process ends (§9.8).
///
/// It takes every click and keystroke — the page under it belongs to a session
/// on its way out, and nothing there should still answer — and it says how many
/// swarms are being terminated, since that is the thing being waited for. The
/// inside of a [`WorkspaceView`].
impl WorkspaceView {
    fn quit_screen(&self, swarms: usize, cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .id(QUIT_SCREEN_ID)
            .test_support()
            .track_focus(&self.quit_focus)
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme.background.opacity(0.92))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().small().color(theme.muted_foreground))
            .child(
                div()
                    .id(QUIT_SCREEN_LABEL_ID)
                    .test_support()
                    .text_size(px(13.))
                    .text_color(theme.foreground)
                    .aria_label(if swarms == 1 {
                        QUIT_ONE_TEXT
                    } else {
                        QUIT_MANY_TEXT
                    })
                    .child(if swarms == 1 {
                        QUIT_ONE_TEXT
                    } else {
                        QUIT_MANY_TEXT
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        ElementId, InputEvent as _, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Point, TestAppContext, VisualTestContext,
    };

    /// The traffic lights' 14pt buttons sit with equal room above and below
    /// inside the strip.
    #[test]
    fn traffic_lights_are_centered_in_the_strip() {
        let top = f32::from(traffic_light_position().y);
        let bottom = store::design::STRIP_HEIGHT - top - TRAFFIC_BUTTON;
        assert_eq!(top, bottom);
        assert_eq!(top, 14.);
    }

    /// A window of a given size, built by `build` — the production entry point,
    /// at the size this test cares about.
    fn window_with(
        cx: &mut TestAppContext,
        window_size: (f32, f32),
        build: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::Context<WorkspaceView>) -> WorkspaceView
            + 'static,
    ) -> (gpui_kit::AnyWindowHandle, Entity<WorkspaceView>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: size(px(window_size.0), px(window_size.1)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| build(window, cx)),
            )
            .expect("the workspace window")
        })
    }

    /// A window on the New Swarm page: one empty tab, over an app root of the
    /// test's own — where the workers card's own switch is drawn (§7.2).
    fn empty_page_window(
        cx: &mut TestAppContext,
        root: std::path::PathBuf,
        window_size: (f32, f32),
    ) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::with_config(
                Arc::new(LaunchEnv {
                    root: store::paths::Root::at(root),
                    ..LaunchEnv::default()
                }),
                window,
                cx,
            )
        });
        cx.simulate_resize(size(px(window_size.0), px(window_size.1)));
        cx.update(|window, cx| window.render_frame(cx));
        (view, cx)
    }

    /// A window with a page on it: one tab, driving a swarm in a folder that does
    /// not have to exist, over an app root of the test's own (§7.3).
    ///
    /// The page is where the side columns live, and it only exists for a tab that
    /// has a swarm — which a unit test can say without starting one.
    fn page_window(
        cx: &mut TestAppContext,
        root: std::path::PathBuf,
        window_size: (f32, f32),
    ) -> (Entity<WorkspaceView>, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::with_config(
                Arc::new(LaunchEnv {
                    root: store::paths::Root::at(root),
                    ..LaunchEnv::default()
                }),
                window,
                cx,
            )
        });
        cx.simulate_resize(size(px(window_size.0), px(window_size.1)));
        view.update(cx, |view, cx| {
            let tab = view.selected_tab().clone();
            tab.update(cx, |tab, _| {
                tab.state = crate::TabState::Running {
                    folder: PathBuf::from("/tmp/proj"),
                }
            });
        });
        cx.update(|window, cx| window.render_frame(cx));
        (view, cx)
    }

    /// The tab at `index` (`ids[index]` is the tab's own id) — its box and the
    /// label inside it — has to lie inside the clipped strip, two frames after the
    /// reveal was asked for: the request is applied against the layout of the frame
    /// before it.
    fn revealed(
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
        ids: &[u64],
        index: usize,
        what: &str,
    ) {
        // A reveal is asked for in one frame and answered in the next: render
        // as the app would, and then look.
        window.render_frame(cx);
        window.render_frame(cx);
        let strip = window.find("tab-strip-scroll").bounds();
        let tab = window
            .find(ElementId::NamedInteger("tab".into(), ids[index]))
            .bounds();
        let label = window
            .find(ElementId::NamedInteger("tab-label".into(), ids[index]))
            .bounds();
        assert!(
            tab.left() >= strip.left() - px(1.) && tab.right() <= strip.right() + px(1.),
            "{what}: tab {index} is not fully inside the strip: {tab:?} against {strip:?}"
        );
        assert!(
            label.left() >= strip.left() - px(1.) && label.right() <= strip.right() + px(1.),
            "{what}: the label of tab {index} runs past the strip: {label:?} against {strip:?}"
        );
    }

    /// The widths the window is showing, from outside it.
    fn view_panes(cx: &mut VisualTestContext, view: &Entity<WorkspaceView>) -> Panes {
        let view = view.clone();
        cx.update(|_, cx| view.read(cx).panes())
    }

    /// Drag a divider from `from` to `to`: press, move past the threshold the
    /// drag needs, then to where the split belongs, and let go (§7.3).
    fn drag_split(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            from + point(px(6.), px(0.)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| window.render_frame(cx));
    }

    /// Double-click a divider, the way a pointer does: two clicks, the second of
    /// which is what the gesture is made of.
    fn double_click_split(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.update(|window, cx| {
            for count in 1..=2 {
                window.dispatch_event(
                    MouseDownEvent {
                        position: at,
                        modifiers: Modifiers::none(),
                        button: MouseButton::Left,
                        click_count: count,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        position: at,
                        modifiers: Modifiers::none(),
                        button: MouseButton::Left,
                        click_count: count,
                    }
                    .to_platform_input(),
                    cx,
                );
            }
            window.render_frame(cx);
        });
    }

    /// An app root of this test's own, empty.
    fn test_root(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("workspace-panes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a root to work in");
        dir
    }

    /// The middle of the page, which is where the dividers run from top to bottom
    /// and where a drag has room to move.
    fn page_middle(window: &mut gpui_kit::Window) -> gpui_kit::Pixels {
        window.find("tab-page").bounds().center().y
    }

    /// The page's left divider: the boundary between the agent column and the
    /// transcript, which is where the agent column ends.
    fn left_divider(window: &mut gpui_kit::Window) -> gpui_kit::Point<gpui_kit::Pixels> {
        let column = window.find("agent-column").bounds();
        point(column.right(), page_middle(window))
    }

    /// §7.1: the tab being shown must be *visible* — all of it, not just its
    /// first pixel. With more tabs than the window has room for, the strip
    /// scrolls the shown tab in; the tab's own box (and the label inside it)
    /// has to end up inside the clipped strip, or the label is cut mid-word by
    /// the window's edge instead of being elided where it stops.
    #[gpui_kit::test]
    fn the_tab_being_shown_is_scrolled_fully_into_the_strip(cx: &mut TestAppContext) {
        let home =
            std::env::temp_dir().join(format!("workspace-strip-shown-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("a home to work in");
        let root = home.clone();

        let mut ids: Vec<u64> = Vec::new();
        let (handle, view) = window_with(cx, (1280., 800.), move |window, cx| {
            WorkspaceView::with_config(
                Arc::new(LaunchEnv {
                    swarm_bin: PathBuf::from("/nonexistent/evo-swarm"),
                    root: store::paths::Root::at(root),
                    ..LaunchEnv::default()
                }),
                window,
                cx,
            )
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |view, cx| {
                for index in 0..15 {
                    // Short to start with: 15 short-titled tabs fit the window,
                    // so nothing has to be scrolled until the names arrive —
                    // which is the shape the strip is caught in.
                    let folder = home.join(format!("p{index}"));
                    std::fs::create_dir_all(&folder).expect("a folder to work in");
                    let tab = if index == 0 {
                        view.selected_tab().clone()
                    } else {
                        view.open_empty_tab(window, cx)
                    };
                    ids.push(tab.read(cx).id().get());
                    tab.update(cx, |tab, cx| {
                        tab.launch(
                            Launch::New {
                                folder,
                                plan: LaunchPlan::default(),
                            },
                            window,
                            cx,
                        )
                    });
                }
            });
            window.render_frame(cx);
        })
        .unwrap();

        // The phases are their own turns: what a tab emits is delivered when the
        // update it was emitted in returns, and what the strip does about it
        // needs a frame after that.
        for index in [0usize, 7, 14] {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| view.select_tab(index, window, cx));
            })
            .unwrap();
            cx.update_window(handle, |_, window, cx| {
                revealed(window, cx, &ids, index, "after selecting");
            })
            .unwrap();

            // Now the strip changes shape *under* the offset that revealed the
            // tab: every tab before it widens — a title arriving, a dot, anything
            // that lands after the reveal — and the shown tab is pushed towards
            // the edge by all of them at once (§7.1). No event says a thing about
            // it, which is the whole difficulty: a reveal asked for once, at the
            // moment of the change, leaves the tab half past the edge, its label
            // cut mid-word rather than elided where it stops.
            cx.update_window(handle, |_, _window, cx| {
                view.update(cx, |view, cx| {
                    for (n, tab) in view.tabs().iter().take(index).enumerate() {
                        tab.update(cx, |tab, _| {
                            // A tab is named after the folder it runs in, and the
                            // name is most of its width: the names arriving is the
                            // strip growing under the tab being shown.
                            tab.state = crate::TabState::Running {
                                folder: home.join(format!(
                                    "evo-desktop-visual-review-a-longer-context-name-{n}"
                                )),
                            };
                        });
                    }
                    // Nobody announces the change: the tab being shown is what
                    // the window draws, and drawing it again is what the app does
                    // when a run starts and a dot appears on the strip. What the
                    // window has to work with is the strip's own shape.
                    let shown = view.tabs()[index].clone();
                    shown.update(cx, |_, cx| cx.notify());
                });
            })
            .unwrap();
            cx.update_window(handle, |_, window, cx| {
                revealed(window, cx, &ids, index, "after the tabs before it grew");
            })
            .unwrap();
        }
    }

    /// §7.1: the `×` is `currentColor` in the design — the tab ink until the
    /// pointer is on the button, the foreground while it is. A glyph is painted
    /// rather than styled, so the strip draws it from this state; what a test can
    /// hold is that the state follows the pointer, from one `×` straight to the
    /// next, and that a closed tab drops it.
    #[gpui_kit::test]
    fn the_close_button_reports_the_pointer_on_it(cx: &mut TestAppContext) {
        let (handle, view) = window_with(cx, (1280., 800.), WorkspaceView::new);
        let first = cx.update(|cx| view.read(cx).selected_tab().read(cx).id());
        let second = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| view.open_empty_tab(window, cx))
                    .read(cx)
                    .id()
            })
            .unwrap();
        let close = |id: TabId| ElementId::NamedInteger("tab-close".into(), id.get());

        /// Move the pointer to `at` the way the platform does, and let the frame
        /// after it carry whatever the move said.
        fn move_to(window: &mut gpui_kit::Window, at: Point<Pixels>, cx: &mut gpui_kit::App) {
            window.dispatch_event(
                MouseMoveEvent {
                    position: at,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        }

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).close_hovered(),
                None,
                "nothing is pointed at yet"
            );

            // The `×` of the tab being shown.
            let shown = window.find(close(second)).bounds().center();
            move_to(window, shown, cx);
            assert_eq!(
                view.read(cx).close_hovered(),
                Some(second),
                "the pointer is on the ×"
            );

            // Straight from one `×` onto the next, which is one mouse move: the
            // two listeners are walked in whatever order the elements are, and
            // the ink has to end up on the one the pointer is actually on. The
            // other tab's `×` is out of sight (its own tab is not being shown —
            // and it is still there, so it still hears the pointer).
            let other = window.find(close(first)).bounds().center();
            move_to(window, other, cx);
            assert_eq!(
                view.read(cx).close_hovered(),
                Some(first),
                "the × the pointer moved onto"
            );

            // Off the buttons, onto the shown tab's own label: the ink goes back
            // to the tab's own.
            let label = window
                .find(ElementId::NamedInteger("tab-label".into(), second.get()))
                .bounds()
                .center();
            move_to(window, label, cx);
            assert_eq!(view.read(cx).close_hovered(), None);

            // Closing the tab whose `×` is pointed at takes the button with it.
            move_to(window, shown, cx);
            assert_eq!(view.read(cx).close_hovered(), Some(second));
            view.update(cx, |view, cx| view.close_selected_tab(window, cx));
            assert_eq!(
                view.read(cx).close_hovered(),
                None,
                "the button went with the tab"
            );
        })
        .unwrap();
    }

    /// §7.1: which tab the pointer is on is state on the view — the dividers and
    /// the outward corners are rules about a tab's next-door tab, which no
    /// element's own hover style can express. Moving straight from one tab onto
    /// the next is one mouse move, and the two listeners fire in whatever order
    /// the elements are walked: the state has to end up on the tab the pointer is
    /// actually on.
    #[gpui_kit::test]
    fn the_hovered_tab_follows_the_pointer_between_tabs(cx: &mut TestAppContext) {
        let (handle, view) = window_with(cx, (1280., 800.), WorkspaceView::new);
        let first = cx.update(|cx| view.read(cx).selected_tab().read(cx).id());
        let second = cx
            .update_window(handle, |_, window, cx| {
                view.update(cx, |view, cx| view.open_empty_tab(window, cx))
                    .read(cx)
                    .id()
            })
            .unwrap();
        let label = |id: TabId| ElementId::NamedInteger("tab-label".into(), id.get());

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(view.read(cx).hovered_tab(), None, "no tab is pointed at");

            for (index, id) in [(0usize, first), (1, second), (0, first)] {
                let at = window.find(label(id)).bounds().center();
                window.dispatch_event(
                    MouseMoveEvent {
                        position: at,
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
                assert_eq!(
                    view.read(cx).hovered_tab(),
                    Some(index),
                    "the pointer moved onto tab {index}"
                );
            }
        })
        .unwrap();
    }

    /// §7.3: the conversation is one column, top to bottom — the header band, the
    /// transcript, and the composer's box at the foot of it. Everything the reader
    /// works with is in the column the transcript is in, and the input sits under the
    /// transcript rather than off in a column of its own.
    #[gpui_kit::test]
    fn the_composer_sits_at_the_foot_of_the_conversation(cx: &mut TestAppContext) {
        let (_view, cx) = page_window(cx, test_root("align"), (1280., 800.));
        cx.update(|window, cx| window.render_frame(cx));

        let header = cx.update(|window, _| window.find("transcript-header").bounds());
        let column = cx.update(|window, _| window.find("conversation-column").bounds());
        let composer = cx.update(|window, _| window.find("composer").bounds());
        let box_ = cx.update(|window, _| window.find("composer-box").bounds());
        assert_eq!(
            header.top(),
            column.top(),
            "the header is the column's first row: {header:?} against {column:?}"
        );
        assert_eq!(
            header.size.height,
            px(store::design::HEADER_HEIGHT),
            "the band is the design's own height"
        );
        assert_eq!(
            composer.bottom(),
            column.bottom(),
            "the box is the column's last row: {composer:?} against {column:?}"
        );
        assert!(
            composer.top() > header.bottom(),
            "the input is under the transcript, not over it: {composer:?} against {header:?}"
        );
        assert!(
            (composer.size.width - column.size.width).abs() <= px(1.),
            "and it is the column's own width: {composer:?} against {column:?}"
        );

        // The box on the reading measure, with the input inside it and the action at
        // the foot of it.
        assert_eq!(
            box_.size.width,
            px(store::design::MEASURE - 2. * store::design::INSET),
            "the box is the measure, less the page's insets: {box_:?}"
        );
        assert!(
            box_.top() == composer.top() + px(4.),
            "the dock's own 4px above the box: {box_:?} against {composer:?}"
        );
        let button = cx.update(|window, _| window.find(composer::BUTTON_ID).bounds());
        assert!(
            button.bottom() <= box_.bottom() && button.top() >= box_.top(),
            "the action is inside the box: {button:?} against {box_:?}"
        );
        assert_eq!(button.size.height, px(28.), "one control high: {button:?}");

        // The status line the page used to draw is gone: its own words are the chips
        // on the box's foot row now.
        cx.update(|window, _| {
            assert!(
                window.try_find("status-readout").is_none(),
                "the readout line is not drawn under the transcript any more"
            );
        });
    }

    /// §7.3: the split between the agent column and the transcript is draggable,
    /// and the room comes out of the middle column — the side columns are what
    /// the reader sized, and dragging one is no reason to move the other.
    #[gpui_kit::test]
    fn a_dragged_split_moves_that_column_and_takes_the_room_from_the_middle(
        cx: &mut TestAppContext,
    ) {
        let root = test_root("drag");
        let (view, cx) = page_window(cx, root.clone(), (1280., 800.));

        let column = cx.update(|window, _| window.find("agent-column").bounds().size.width);
        let transcript =
            cx.update(|window, _| window.find("conversation-column").bounds().size.width);
        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from + point(px(60.), px(0.)));

        let column_after = cx.update(|window, _| window.find("agent-column").bounds().size.width);
        let transcript_after =
            cx.update(|window, _| window.find("conversation-column").bounds().size.width);
        assert!(
            (column_after.as_f32() - column.as_f32() - 60.).abs() <= 2.,
            "the agent column follows the pointer: {column:?} then {column_after:?}"
        );
        assert!(
            (transcript.as_f32() - transcript_after.as_f32() - 60.).abs() <= 2.,
            "the transcript gives up what the column took: {transcript:?} then {transcript_after:?}"
        );

        let panes = view_panes(cx, &view);
        assert!(
            (panes.left - 320.).abs() < 2.,
            "260 points, 60 points more: {panes:?}"
        );

        // The window wrote the width down (§6): it is the app's own, so it
        // outlives the window that dragged it.
        let saved = store::app_state::AppState::load(&store::paths::Root::at(root)).panes;
        assert!(
            (saved.left - panes.left).abs() < 0.01,
            "app.json remembers the split: {saved:?} against {panes:?}"
        );
    }

    /// §7.2: the workers card's own switch is one *page's*, for one launch — flipping
    /// it is not the window's business, not another tab's, and not `app.json`'s.
    ///
    /// The three things that are *not* the switch:
    ///
    /// * **the window.** It is not a value beside the column widths; the page owns it.
    /// * **another tab.** A page opened after a flip opens on a swarm, which is what
    ///   every page opens on.
    /// * **the file.** Nothing about it is written down: `app.json` is byte for byte
    ///   what it was, and a switch is one launch's own.
    #[gpui_kit::test]
    fn the_workers_switch_belongs_to_one_page_and_one_launch(cx: &mut TestAppContext) {
        let root = test_root("use-swarm");
        let (view, cx) = empty_page_window(cx, root.clone(), (1280., 800.));
        let root = store::paths::Root::at(root);
        let switch = crate::empty_tab::SWARM_SWITCH_ID;
        assert_eq!(
            cx.update(|window, _| window.find(switch).checked()),
            Some(true),
            "a page opens on a swarm"
        );
        // A file to leave alone: the app's own, written before anything is flipped.
        let mut saved = store::app_state::AppState::load(&root);
        saved.zoom = 1.5;
        saved.save(&root).unwrap();
        let before = std::fs::read_to_string(root.app_json()).unwrap();

        cx.update(|window, cx| window.click(switch, cx));
        cx.update(|window, cx| window.render_frame(cx));

        let page = cx.update(|_, cx| view.read(cx).selected_tab().clone());
        assert!(
            !cx.update(|_, cx| page.read(cx).swarm(cx)),
            "the page that was flipped starts one agent now"
        );
        // And its words follow: a session, not a swarm — on the page and on the strip
        // alike, because one program is one page and one name (§7.2).
        let headline = cx.update(|window, _| {
            window
                .find(crate::empty_tab::HEADLINE_ID)
                .label()
                .map(str::to_owned)
        });
        assert_eq!(
            headline.as_deref(),
            Some("New Session"),
            "the page's own headline says what this page is"
        );
        assert_eq!(
            cx.update(|_, cx| page.read(cx).title(cx).to_string()),
            "New Session",
            "and so does the tab"
        );

        // And the launch this page would make is one `evo-agent` — the *program*, not a
        // flag: the window starts what the page's own plan says (§7.2, §1).
        assert_eq!(
            program_of(cx, &page),
            Some(store::launch::Program::Agent),
            "the switch off is one `evo-agent`, not a swarm"
        );

        // Another tab is another launch: it opens on a swarm, and the switch drawn on
        // it says so.
        let second =
            cx.update(|window, cx| view.update(cx, |view, cx| view.open_empty_tab(window, cx)));
        assert!(
            cx.update(|_, cx| second.read(cx).swarm(cx)),
            "a page opened afterwards opens on a swarm"
        );
        assert_eq!(
            program_of(cx, &second),
            Some(store::launch::Program::Swarm),
            "and its launch is `evo-swarm`"
        );
        assert_eq!(
            cx.update(|window, cx| {
                window.render_frame(cx);
                window.find(switch).checked()
            }),
            Some(true),
            "drawn where it is, saying what *this* page holds"
        );
        // And the flipped page is still the flipped page.
        assert!(
            !cx.update(|_, cx| page.read(cx).swarm(cx)),
            "the first page kept its own answer"
        );

        // Nothing was written down: not the switch, not the page it was flipped on.
        assert_eq!(
            std::fs::read_to_string(root.app_json()).unwrap(),
            before,
            "app.json is untouched by a switch"
        );
        assert!(!before.contains("use_swarm"), "and never carried one: {before}");
    }

    /// The program a page's own launch would be: the plan its controls add up to, put
    /// through the same `launch_spec` the window starts one with.
    fn program_of(
        cx: &mut VisualTestContext,
        page: &Entity<crate::TabContent>,
    ) -> Option<store::launch::Program> {
        let plan = cx.update(|_, cx| page.read(cx).launch_plan(cx));
        // The tab directory only names the launch's ready file; the program is the
        // plan's own (`crate::launch::launch_spec`).
        crate::launch::launch_spec(
            &LaunchEnv::default(),
            &Launch::New {
                folder: PathBuf::from("/tmp/proj"),
                plan,
            },
            &store::paths::TabId::new(),
        )
        .program
    }

    /// §7.3: a split stops where the page says it does. A column has a range, and
    /// a drag past it is a drag that has already done all it can.
    #[gpui_kit::test]
    fn a_split_stops_at_the_widths_the_page_allows(cx: &mut TestAppContext) {
        let (view, cx) = page_window(cx, test_root("clamp"), (1280., 800.));

        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from + point(px(600.), px(0.)));
        let left = view_panes(cx, &view).left;
        assert!(
            (left - store::app_state::LEFT_MAX).abs() < 1.,
            "the agent column stops at its widest: {left}"
        );

        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from - point(px(600.), px(0.)));
        let left = view_panes(cx, &view).left;
        assert!(
            (left - store::app_state::LEFT_MIN).abs() < 1.,
            "and at its narrowest: {left}"
        );

        // The conversation is never squeezed out of the page: at the widest the
        // window allows, it still has its own room (§7.3).
        let middle = cx.update(|window, _| window.find("conversation-column").bounds().size.width);
        assert!(
            middle.as_f32() >= store::app_state::CENTER_MIN - 1.,
            "the transcript keeps its minimum: {middle:?}"
        );
    }

    /// §7.3: a double-click on the split puts the agent column back to the width
    /// it starts at.
    #[gpui_kit::test]
    fn double_clicking_the_split_puts_the_column_back(cx: &mut TestAppContext) {
        let (view, cx) = page_window(cx, test_root("reset"), (1280., 800.));

        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from + point(px(60.), px(0.)));
        assert!(
            view_panes(cx, &view).left > 300.,
            "the split was dragged away from its default first: {:?}",
            view_panes(cx, &view)
        );

        let at = cx.update(|window, _| left_divider(window));
        double_click_split(cx, at);

        let panes = view_panes(cx, &view);
        assert!(
            (panes.left - store::app_state::LEFT_DEFAULT).abs() < 1.,
            "the split that was double-clicked is back at its default: {panes:?}"
        );
    }

    /// §7.3: the two columns are the window's, not a tab's. A split dragged while
    /// one tab is shown is the split the next tab opens with.
    /// §7.3, §13: a running tab whose project settings hold a draft asks before the
    /// tab goes — the swarm is not stopped, and the page is not dropped, under an
    /// edit nobody has answered for.
    /// §9.8: the quit's "show me what I would lose" goes to a write in flight before
    /// a draft — the one thing that cannot be called back — and it takes the page to
    /// the document that holds it, not to whichever document was in front.
    #[gpui_kit::test]
    fn focusing_what_a_quit_would_wait_for_prefers_the_write(cx: &mut TestAppContext) {
        crate::test_evo_home();
        let (view, cx) = page_window(cx, test_root("focus-dirty"), (1280., 800.));
        cx.update(|window, cx| window.render_frame(cx));

        // A draft in the running tab's project settings.
        cx.update(|window, cx| {
            window.click("project-row", cx);
            window.render_frame(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.input("(a)", cx);
            window.render_frame(cx);
        });
        assert!(
            cx.update(|_, cx| view.read(cx).tabs()[0].read(cx).is_settings_dirty(cx)),
            "the project's page holds a draft"
        );

        // And a settings tab whose document is on its way to disk.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_settings_tab(window, cx);
            });
            window.render_frame(cx);
            window.input("(b)", cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let page = view.read(cx).tabs()[1]
                .read(cx)
                .settings_editor()
                .cloned()
                .expect("the settings tab has its editors");
            page.update(cx, |page, cx| page.save_selected(window, cx));
            assert!(page.read(cx).is_saving(), "the write is in flight");
            assert!(
                view.read(cx).has_saving_settings(cx),
                "and the window knows something is being written"
            );

            // Away to the other tab, then the quit's own question.
            view.update(cx, |view, cx| view.select_tab(0, window, cx));
            let shown = view.update(cx, |view, cx| view.focus_dirty_settings(window, cx));
            assert!(shown, "there is something a quit would wait for");
            assert_eq!(
                view.read(cx).selected_index(),
                1,
                "the write in flight is what a quit would wait for"
            );
            assert!(
                view.read(cx).tabs()[1].read(cx).settings_editor().is_some(),
                "and the page it is writing is the one in front"
            );
        });
    }

    #[gpui_kit::test]
    fn a_project_tab_with_a_draft_asks_before_it_goes(cx: &mut TestAppContext) {
        let (view, cx) = page_window(cx, test_root("project-guard"), (1280., 800.));
        cx.update(|window, cx| window.render_frame(cx));

        // The folder row is the way in, and the press puts the keyboard in the page:
        // what is typed lands there as a draft. The page reads its four documents
        // once, on the background executor — a draft is measured against what was
        // read, so the read is let to land before anything is typed.
        cx.update(|window, cx| {
            window.click("project-row", cx);
            window.render_frame(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.input("(a)", cx);
            window.render_frame(cx);
        });
        let (id, tab) = cx.update(|_, cx| {
            let tab = view.read(cx).tabs()[0].clone();
            let id = tab.read(cx).id();
            (id, tab)
        });
        assert!(
            cx.update(|_, cx| tab.read(cx).is_settings_dirty(cx)),
            "the press and the typing left the project's settings dirty"
        );

        // The close asks, and asks with the project's settings still in front.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_tab(id, window, cx));
            window.render_frame(cx);
            assert!(
                window.try_find("settings-close-prompt").is_some(),
                "the tab asks before it goes"
            );
        });
        assert_eq!(
            cx.update(|_, cx| view.read(cx).tabs().len()),
            1,
            "and it has not gone while the question is up"
        );

        // Keep Editing: the tab stays, the page stays, the draft stays.
        cx.update(|window, cx| {
            window.click("settings-close-keep", cx);
            window.render_frame(cx);
            assert!(window.try_find("settings-close-prompt").is_none());
        });
        assert!(cx.update(|_, cx| tab.read(cx).project_settings_shown()));
        assert!(cx.update(|_, cx| tab.read(cx).is_settings_dirty(cx)));

        // Discard: the drafts go, and so does the tab.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.close_tab(id, window, cx));
            window.render_frame(cx);
            window.click("settings-close-discard", cx);
            window.render_frame(cx);
        });
        // The close travels as the tab's own event, which the window hears on its
        // next effect cycle.
        cx.run_until_parked();
        cx.update(|window, cx| window.render_frame(cx));
        assert_eq!(
            cx.update(|_, cx| view.read(cx).tabs().len()),
            1,
            "the last tab leaves a fresh New Swarm page, not an empty window"
        );
        assert_eq!(
            cx.update(|_, cx| view.read(cx).selected_tab().read(cx).state().clone()),
            crate::TabState::Empty
        );
    }

    #[gpui_kit::test]
    fn every_tab_shows_the_same_two_columns(cx: &mut TestAppContext) {
        let (view, cx) = page_window(cx, test_root("shared"), (1280., 800.));

        let second = cx.update(|window, cx| {
            let tab = view.update(cx, |view, cx| view.add_tab(window, cx));
            tab.update(cx, |tab, _| {
                tab.state = crate::TabState::Running {
                    folder: PathBuf::from("/tmp/other"),
                };
            });
            tab
        });
        cx.update(|window, cx| window.render_frame(cx));
        assert_eq!(cx.update(|_, cx| view.read(cx).selected_index()), 1);

        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from + point(px(60.), px(0.)));
        let dragged = view_panes(cx, &view);
        assert!(dragged.left > 300., "the drag landed: {dragged:?}");

        // Back to the first tab: its page is drawn with the widths the drag left,
        // whatever it was showing when they changed.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.select_tab(0, window, cx));
            window.render_frame(cx);
        });
        let column = cx.update(|window, _| window.find("agent-column").bounds().size.width);
        assert!(
            (column.as_f32() - dragged.left).abs() < 2.,
            "the tab that was not being looked at has the same column: {column:?} against {dragged:?}"
        );
        assert_eq!(cx.update(|_, cx| second.read(cx).panes()), dragged);
    }

    /// §7.3: a window narrowed under a column someone widened still shows a page
    /// — the agent column comes in so the conversation keeps its room, rather than
    /// the transcript being squeezed to nothing.
    ///
    /// A window narrower than the smallest the app opens, because the two columns
    /// are the page's whole width: the widest column and the conversation's
    /// minimum fit that smallest window with room to spare, so nothing narrower
    /// than it can force the column in.
    #[gpui_kit::test]
    fn a_narrowed_window_brings_the_agent_column_in(cx: &mut TestAppContext) {
        let root = test_root("small");
        let (view, cx) = page_window(cx, root.clone(), (1600., 900.));

        let from = cx.update(|window, _| left_divider(window));
        drag_split(cx, from, from + point(px(600.), px(0.)));
        let dragged = view_panes(cx, &view);
        assert!(
            (dragged.left - store::app_state::LEFT_MAX).abs() < 1.,
            "the widest column the window can hold: {dragged:?}"
        );

        cx.simulate_resize(size(px(820.), px(700.)));
        cx.update(|window, cx| window.render_frame(cx));

        let panes = view_panes(cx, &view);
        assert!(
            panes.left < dragged.left,
            "the column came in for the narrow window: {panes:?}"
        );
        assert!(
            panes.left + store::app_state::CENTER_MIN <= 820. + 1.,
            "and leaves the conversation its room: {panes:?}"
        );
        assert!(
            panes.left >= store::app_state::LEFT_MIN,
            "and did not go below its own minimum: {panes:?}"
        );
        let column = cx.update(|window, _| window.find("agent-column").bounds().size.width);
        assert!(
            (column.as_f32() - panes.left).abs() < 2.,
            "the page is drawn at the fitted width: {column:?} against {panes:?}"
        );
        let conversation =
            cx.update(|window, _| window.find("conversation-column").bounds().size.width);
        assert!(
            conversation.as_f32() >= store::app_state::CENTER_MIN - 1.,
            "the conversation kept its room: {conversation:?}"
        );

        // And the window remembered what it had to do (§6): the width it could not
        // hold is not what the next window opens with.
        let saved = store::app_state::AppState::load(&store::paths::Root::at(root)).panes;
        assert!(
            saved.left + store::app_state::CENTER_MIN <= 820. + 1.,
            "app.json kept the fitted width: {saved:?}"
        );
    }

    /// §7.1: the keyboard follows the tab. A tab that drives a swarm is one
    /// someone means to type into, so selecting it puts the caret in its
    /// composer — and the tab shortcuts still work with the caret in that field,
    /// which is where it will be most of the time.
    #[gpui_kit::test]
    fn selecting_a_tab_puts_the_caret_in_its_composer(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, view) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: point(px(0.), px(0.)),
                            size: size(px(1280.), px(800.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        cx.new(|cx| {
                            WorkspaceView::with_config(Arc::new(LaunchEnv::default()), window, cx)
                        })
                    },
                )
            })
            .expect("the workspace window");

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            // A second tab, driving a swarm: the state §7.1's rule is about.
            let running = view.update(cx, |view, cx| view.open_empty_tab(window, cx));
            running.update(cx, |tab, _cx| {
                tab.state = crate::TabState::Running {
                    folder: PathBuf::from("/tmp/proj"),
                }
            });
            window.render_frame(cx);

            // Where the caret should end up: the composer's own input, asked for
            // by the composer, so the assertion does not lean on the code under
            // test.
            let composer = running.read(cx).composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
            let caret = window.focused(cx).expect("the caret is in the composer");
            assert_eq!(view.read(cx).selected_index(), 1);

            // Away to the empty tab: nothing there types, so the window takes the
            // keyboard back rather than leaving it in a field nobody can see.
            view.update(cx, |view, cx| view.select_tab(0, window, cx));
            window.render_frame(cx);
            assert_eq!(view.read(cx).selected_index(), 0);
            assert_ne!(
                window.focused(cx),
                Some(caret.clone()),
                "an empty tab has no composer to type into"
            );
            assert!(
                window.focused(cx).is_some(),
                "and the window itself holds the keyboard"
            );

            // Back by shortcut: ⌘2 selects the tab and hands its composer the caret.
            window.press("cmd-2", cx);
            assert_eq!(view.read(cx).selected_index(), 1);
            assert_eq!(window.focused(cx), Some(caret.clone()));

            // The shortcuts still work from inside that field: ⌘1 is a tab, not a
            // character the composer swallows.
            window.press("cmd-1", cx);
            assert_eq!(view.read(cx).selected_index(), 0);

            // ⌃⇥ too — a tab character is not what it means in this window.
            window.press("ctrl-tab", cx);
            assert_eq!(view.read(cx).selected_index(), 1);
        })
        .unwrap();
    }

    #[test]
    fn a_roomy_work_area_keeps_the_default_size_centered() {
        let work_area = Bounds::new(point(px(0.), px(25.)), size(px(2560.), px(1400.)));
        let bounds = fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area);

        assert_eq!(bounds.size, DEFAULT_WINDOW_SIZE);
        assert_eq!(bounds.center(), work_area.center());
    }

    #[test]
    fn a_small_work_area_shrinks_the_window_inside_it() {
        // A 13" display: smaller than the size we ask for.
        let work_area = Bounds::new(point(px(0.), px(25.)), size(px(1440.), px(875.)));
        let bounds = fit_to_work_area(DEFAULT_WINDOW_SIZE, work_area);

        assert_eq!(bounds.size, work_area.size);
        assert!(work_area.contains(&bounds.origin));
        assert!(bounds.right() <= work_area.right());
        assert!(bounds.bottom() <= work_area.bottom());
    }
}
