//! The window this app opens and the chrome it opens with (§7.1).
//!
//! One window, whose title bar *is* the tab strip: one [`Tab`] per swarm, plus a
//! `+` that always appends an empty one. [`WorkspaceView`] owns the tab set and
//! the selection; everything below the strip belongs to a
//! [`TabContent`](crate::TabContent).

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, linear_color_stop, linear_gradient, point, px, size, AnyElement, App, Background, Bounds,
    Context, ElementId, Entity, FocusHandle, Global, Hsla, IntoElement, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Rgba, ScrollHandle, SharedString, Size, Subscription, Task,
    TestSupportExt as _, Window, WindowBounds, WindowOptions,
};

use std::rc::Rc;

use serde_json::Value;
use session::{HistoryEntry, LaunchPlan};
use store::model_cache::ModelCache;
use tab_engine::EngineHandle;

use crate::history::folder_name;
use crate::launch::{stop_in_background, Launch, SwarmConfig};
use crate::tab::{RegistryHook, TabContent, TabContentEvent, TabId};

/// The app's own name: what the bundle, the menu bar and the About window call it
/// (`scripts/bundle.sh`, `crates/app`). The window title ends with it (§7.1).
const APP_NAME: &str = "Evo Desktop";

/// The size the window opens at when the display has room for it (§7.1).
pub const DEFAULT_WINDOW_SIZE: Size<Pixels> = size(px(1600.), px(1000.));

/// The smallest window that still fits the lane column, a transcript and the
/// composer side by side (§7.3).
pub const MIN_WINDOW_SIZE: Size<Pixels> = size(px(1000.), px(700.));

/// How wide one tab may grow before its label is ellipsized (§7.1).
const TAB_MAX_WIDTH: f32 = 220.;

/// What the kit's title bar keeps for itself on the left before its children
/// start: the traffic lights on macOS, a shorter inset elsewhere
/// (`gpui_component::title_bar` keeps the number private). The strip's own width
/// budget is the window minus this — a strip that ignores it ends up with its `+`
/// under the window's right edge (§7.1).
const TITLE_BAR_LEFT_INSET: f32 = if cfg!(target_os = "macos") { 80. } else { 12. };

/// The `+` at the end of the strip: always present, always the last thing.
const ADD_TAB_ID: &str = "tab-add";

/// The row the strip is drawn in: the tabs' box and the `+`'s box, side by side.
const TAB_STRIP_ID: &str = "tab-strip";

/// The box the tabs scroll inside — the edge they are clipped at, which is the
/// edge the `+` begins at. A tab is never drawn past it (§7.1).
const TAB_STRIP_SCROLL_ID: &str = "tab-strip-scroll";

/// The kit's tab bar inside that box. Its id is only a name: the strip's own
/// element is [`TAB_STRIP_SCROLL_ID`], which is the one with the edge.
const TAB_BAR_ID: &str = "tab-bar";

/// The `+`'s box, which is what keeps a tab's tail off the button.
const ADD_TAB_BOX_ID: &str = "tab-add-box";

/// The title bar's own background, for a box that has to be opaque over it.
///
/// The kit draws it as a vertical gradient — 55% `title_bar` mixed with
/// `background` at the top, `title_bar` at the bottom, in
/// `gpui_component::title_bar`'s `default_title_bar_background`, which is private
/// — so a solid colour would leave a visible patch where the button is. This is
/// that gradient, and the only thing in this file that mirrors the kit's chrome.
fn title_bar_background(cx: &App) -> Background {
    let title_bar = cx.theme().title_bar;
    let background = cx.theme().background.to_rgb();
    let title_bar_rgb = title_bar.to_rgb();
    let mixed = Hsla::from(Rgba {
        r: title_bar_rgb.r * 0.55 + background.r * 0.45,
        g: title_bar_rgb.g * 0.55 + background.g * 0.45,
        b: title_bar_rgb.b * 0.55 + background.b * 0.45,
        a: title_bar_rgb.a * 0.55 + background.a * 0.45,
    });
    linear_gradient(
        180.,
        linear_color_stop(mixed, 0.),
        linear_color_stop(title_bar, 1.),
    )
}

/// The key context the window's own shortcuts are bound in (§7.1).
///
/// Nothing is bound in it any more — the tab strip's shortcuts are bound
/// app-wide (see [`bind_tab_keys`]) — but the window root still carries it, and
/// the composer's own context sits inside it, so a shortcut means the same thing
/// wherever the keyboard happens to be inside the window.
const WORKSPACE_CONTEXT: &str = "Workspace";

gpui_kit::actions!(
    workspace,
    [
        /// Show the tab to the right of this one (⌃⇥, ⌘⇧]).
        SelectNextTab,
        /// Show the tab to the left of this one (⌃⇧⇥, ⌘⇧[).
        SelectPreviousTab,
        /// Show the last tab, however many are open (⌘9).
        SelectLastTab,
    ]
);

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
    WindowOptions {
        window_bounds: Some(initial_window_bounds(cx)),
        window_min_size: Some(MIN_WINDOW_SIZE),
        ..TitleBar::window_options()
    }
}

/// What the window is closed with (§9.8).
///
/// The window has already taken every tab's engine out of its tabs and hands
/// them over: stopping them is the hook's job.
///
/// The hook answers "am I taking this quit over?". `true` vetoes this close —
/// the app is doing the work and closes the window itself
/// (`window.remove_window()`) when it is done, and that close is allowed.
/// `false` lets the window close now, which is what an app that has nothing
/// left to do returns.
pub type QuitHook = Box<dyn Fn(QuitRequest, &mut Window, &mut App) -> bool + 'static>;

/// Everything the app learns about models and past sessions, which every empty
/// tab shows (§9.4, §9.5).
///
/// The app owns the catalog probe and the session scan; the window only carries
/// their results to the tabs that draw them — the ones that exist now and the
/// ones opened afterwards.
#[derive(Clone, Default)]
pub struct LauncherData {
    /// The last `/registry` seen, as it arrived (a live server's, or a probe's).
    pub registry: Option<Value>,
    /// The catalog the disk cache holds, which also carries the kernel api set.
    pub model_cache: Option<ModelCache>,
    /// Why the catalog could not be read, when it could not.
    pub catalog_error: Option<String>,
    /// The session scan is still walking `~/.evo/sessions`.
    pub scanning: bool,
    /// The resumable swarms the scan found (§9.5).
    pub history: Vec<HistoryEntry>,
    /// The clock the rows' relative times read against, and the local offset.
    pub now: i64,
    pub offset_seconds: i32,
    /// Why the scan failed, when it did.
    pub history_error: Option<String>,
    /// Why the swarm cannot be started at all, when it cannot: the `evo_swarm` path
    /// `app.json` names is not a binary that runs (§9.7). The app learns it from the
    /// same `--version` probe the About dialog reads; the empty tabs say it under the
    /// folder card, where a launch would otherwise fail.
    pub swarm_problem: Option<String>,
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
}

/// What [`QuitHook`] is handed.
pub struct QuitRequest {
    /// Every tab's engine, taken out of the tabs, so the app owns stopping them.
    pub engines: Vec<EngineHandle>,
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
    config: Arc<SwarmConfig>,
    /// What the app wants done when the window is closed (§9.8). Without one the
    /// window stops every tab's swarm and then closes.
    quit_hook: Option<QuitHook>,
    /// The catalog and the session list the app pushed, waiting for the tabs that
    /// show them (§9.4, §9.5).
    launcher: LauncherData,
    /// Called with every live tab's `/registry`, so the app can refresh its cache
    /// from a real server (§9.4).
    registry_hook: Option<RegistryHook>,
    /// Tabs whose run ended while another tab was being shown: the strip keeps a
    /// dot on them until they are looked at (§7.1).
    finished: BTreeSet<TabId>,
    /// The title the window has been given, so a redraw does not rename it every
    /// frame (§7.1).
    window_title: Option<SharedString>,
    /// Set once the close hook is registered, so it happens once per window.
    close_hook_installed: bool,
    /// The task waiting for every tab's swarm to stop; `Some` while the window is
    /// stopping them.
    stopping: Option<Task<()>>,
    /// True once nothing is left to wait for and the window may close.
    may_close: bool,
}

impl WorkspaceView {
    /// A window with one empty tab: the app never auto-starts a swarm and never
    /// has an empty window (§7.2, §14.6).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        WorkspaceView::with_config(Arc::new(SwarmConfig::default()), window, cx)
    }

    /// The same window, with the binaries, the app root and the environment its
    /// tabs start their swarms with.
    pub fn with_config(
        config: Arc<SwarmConfig>,
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
            registry_hook: None,
            finished: BTreeSet::new(),
            window_title: None,
            close_hook_installed: false,
            stopping: None,
            may_close: false,
        };
        // The first tab takes the keyboard as it opens (§7.1): without a focus in
        // the frame, a shortcut pressed on a fresh window would go nowhere at all.
        view.open_empty_tab(window, cx);
        view
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

    /// Take every tab's engine, so the caller can stop them all (§9.8).
    ///
    /// The tabs keep what they show; they stop watching and stop typing to their
    /// servers. Each engine's shutdown ladder is the caller's to run — off the UI
    /// thread, with [`stop_in_background`](crate::stop_in_background) or
    /// [`tab_engine::shutdown_all`].
    pub fn take_engines(&mut self, cx: &mut Context<Self>) -> Vec<EngineHandle> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.update(cx, |tab, cx| tab.take_engine(cx)))
            .collect()
    }

    /// What the app does when the window is closed (§9.8). Without a hook the
    /// window stops every tab's swarm and closes itself.
    pub fn set_quit_hook(&mut self, hook: QuitHook) {
        self.quit_hook = Some(hook);
    }

    /// Open a new empty tab and select it — what ⌘T does (§7.1).
    pub fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TabContent> {
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
            .map(|tab| {
                let tab = tab.read(cx);
                TabRecord {
                    window_id: tab.id(),
                    store_id: tab.store_id().cloned(),
                    folder: tab.folder().map(Path::to_path_buf),
                    session: tab.session_path().map(Path::to_path_buf),
                }
            })
            .collect()
    }

    /// What every tab this window opens **from now on** starts its swarm with
    /// (§13): what Settings saved. A tab that is already running keeps the binaries
    /// it started with, which is what the panel's own note says.
    pub fn set_swarm_config(&mut self, config: Arc<SwarmConfig>, cx: &mut Context<Self>) {
        self.config = config;
        cx.notify();
    }

    /// Called with every live tab's `/registry` — the app refreshes its model
    /// cache from a real server with this (§9.4).
    pub fn on_registry(
        &mut self,
        hook: impl Fn(&Value, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let hook: RegistryHook = Rc::new(hook);
        self.registry_hook = Some(hook.clone());
        for tab in &self.tabs {
            let hook = hook.clone();
            tab.update(cx, |tab, _cx| tab.set_registry_hook(Some(hook)));
        }
    }

    pub fn tabs(&self) -> &[Entity<TabContent>] {
        &self.tabs
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The tab whose content the window is showing.
    pub fn selected_tab(&self) -> &Entity<TabContent> {
        &self.tabs[self.selected]
    }

    /// Append an empty tab and select it: what the `+` does (§7.1).
    pub fn open_empty_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabContent> {
        let id = TabId::new(self.next_id);
        self.next_id += 1;
        let config = self.config.clone();
        let tab = cx.new(|cx| TabContent::new(id, config, window, cx));
        let subscription = cx.subscribe_in(
            &tab,
            window,
            |this, tab, event: &TabContentEvent, window, cx| {
                let id = tab.read(cx).id();
                this.on_tab_event(id, event.clone(), window, cx);
            },
        );
        self.subscriptions.push_back((id, subscription));
        self.tabs.push(tab.clone());
        self.selected = self.tabs.len() - 1;
        // The new tab is at the far end, which is exactly where an overflowing
        // strip has to scroll to (§7.1).
        self.strip_scroll.scroll_to_item(self.selected);
        // A tab opened now shows what the app already learned (§9.4, §9.5).
        let launcher = self.launcher.clone();
        tab.update(cx, |tab, cx| tab.set_launcher_data(&launcher, window, cx));
        let registry_hook = self.registry_hook.clone();
        tab.update(cx, |tab, _cx| tab.set_registry_hook(registry_hook));
        // The tab being shown is where the keyboard goes (§7.1). It matters beyond
        // typing: GPUI resolves a keystroke against the *focused* element's place in
        // the frame, so a keyboard left on a tab that is no longer drawn is a
        // keyboard that answers no shortcut at all.
        self.focus_selected(window, cx);
        cx.notify();
        tab
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
        // highlight is never off-screen (§7.1).
        self.strip_scroll.scroll_to_item(index);
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
        let Some(index) = self.tabs.iter().position(|tab| tab.read(cx).id() == id) else {
            return;
        };
        let was_shown = index == self.selected;
        let tab = self.tabs.remove(index);
        self.subscriptions.retain(|(closed, _)| *closed != id);
        if let Some(engine) = tab.update(cx, |tab, cx| tab.take_engine(cx)) {
            // Nothing waits for it: the tab is already gone from the window.
            drop(stop_in_background(vec![engine]));
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

    /// What a tab's content asks the window for; the window owns the tab set, so
    /// the content reports intent instead of acting on it (§7.2).
    fn on_tab_event(
        &mut self,
        id: TabId,
        event: TabContentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            } => {
                // A resumed swarm is its own tab, running in the folder the
                // session came from (§7.2, §14.2).
                let tab = self.open_empty_tab(window, cx);
                tab.update(cx, |content, cx| {
                    content.launch(
                        Launch::Resume {
                            folder,
                            session: session_path,
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

    /// The strip: the tabs, and the `+` that adds one (§7.1).
    ///
    /// The `+` is not the tab bar's *suffix*. A suffix sits at the bar's right
    /// edge while the tabs scroll under it, so a tab whose edge is past the
    /// window's overflows into the button's pixels — and since the button is
    /// drawn after them, what a reader sees is the last visible tab's label with
    /// the `+` on top of it (§7.1, and `docs/screens/09-bad-run-dark.png`, where
    /// it reads `● evo-desktoj +`).
    ///
    /// So the scrolling part gets a box of its own, [clipped](Self::render_tab_scroll)
    /// and sized to end where the button begins: a tab is cut off at that edge
    /// instead of running under it.
    ///
    /// The cap is in pixels — the window, less the title bar's own left padding —
    /// rather than `max_w_full`, because a percentage does not resolve against the
    /// kit's title bar: the row would end up as wide as its content, `+` and all,
    /// which is what once pushed the button off the screen. It is measured afresh
    /// every frame, so a resized window reflows.
    fn render_tab_strip(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let room = window.bounds().size.width - px(TITLE_BAR_LEFT_INSET);
        h_flex()
            .id(TAB_STRIP_ID)
            .min_w_0()
            .h_full()
            .max_w(room)
            .child(self.render_tab_scroll(cx))
            .child(self.render_add_tab_button(cx))
    }

    /// The scrolling part of the strip: the tabs, and nothing else.
    ///
    /// `min_w_0` + `flex_shrink_1` is what makes it give way: with room for every
    /// tab it is as wide as they are (and the `+` sits right after the last one),
    /// and with more tabs than fit it shrinks to what is left of the window and
    /// clips the rest. Scrolling is the bar's own business — it happens inside the
    /// bar, on `track_scroll`, a hair inside this edge — so what this box decides
    /// is only where the tabs stop being drawn.
    fn render_tab_scroll(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(TAB_STRIP_SCROLL_ID)
            .test_support()
            .min_w_0()
            .flex_shrink_1()
            .overflow_x_hidden()
            .child(
                TabBar::new(TAB_BAR_ID)
                    .min_w_0()
                    .max_width(px(TAB_MAX_WIDTH))
                    .track_scroll(&self.strip_scroll)
                    .selected_index(self.selected)
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        this.select_tab(*index, window, cx)
                    }))
                    .children(self.tabs.iter().map(|tab| self.render_tab(tab, cx))),
            )
    }

    /// One tab: the folder's name, the whole path plus the swarm's state on
    /// hover, and its close control (§7.1).
    ///
    /// A middle click closes it, the way a browser's tab does — the same thing the
    /// `×` does, without having to aim at it.
    /// The strip's dot before a tab's name (§7.1): a tiny `success` dot while the
    /// tab's coordinator has a run in flight, and a muted one when a background
    /// tab's run ended since the user last looked at it — a browser's "something
    /// happened here".
    ///
    /// Two element ids rather than one, because *which* dot it is is the whole
    /// question — and the only part of it a picture cannot answer.
    fn render_tab_activity(&self, running: bool, id: TabId, cx: &App) -> Option<AnyElement> {
        let (element, color) = if running {
            (
                ElementId::NamedInteger("tab-running".into(), id.get()),
                cx.theme().success,
            )
        } else if self.finished.contains(&id) {
            (
                ElementId::NamedInteger("tab-finished".into(), id.get()),
                cx.theme().muted_foreground,
            )
        } else {
            return None;
        };
        Some(
            div()
                .id(element)
                .test_support()
                .flex_shrink_0()
                // A fixed 6px: `size_1_5` is a *fraction* of the parent (20%), which
                // made the dot as wide as a fifth of the tab and squeezed the label
                // into an ellipsis.
                .w(px(6.))
                .h(px(6.))
                .rounded_full()
                .bg(color)
                .into_any_element(),
        )
    }

    fn render_tab(&self, tab: &Entity<TabContent>, cx: &mut Context<Self>) -> Tab {
        let content = tab.read(cx);
        let id = content.id();
        let title = content.title();
        let tooltip = content.tooltip();
        let activity = self.render_tab_activity(content.is_running(), id, cx);
        let selected = self
            .tabs
            .get(self.selected)
            .is_some_and(|shown| shown.read(cx).id() == id);
        // The group is the tab's own content, so a pointer anywhere on the tab
        // brings its `×` out — and moving onto the `×` keeps it out, because the
        // pointer is still inside the group.
        let group = SharedString::from(format!("tab-content-{}", id.get()));
        Tab::new()
            .aria_label(title.clone())
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.close_tab(id, window, cx)
                }),
            )
            .child(
                h_flex()
                    .group(group.clone())
                    .gap_1()
                    .min_w_0()
                    .items_center()
                    .when_some(activity, |this, dot| this.child(dot))
                    .child(
                        div()
                            .id(ElementId::NamedInteger("tab-label".into(), id.get()))
                            .test_support()
                            .min_w_0()
                            .truncate()
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .child(title),
                    )
                    .child(self.render_tab_close(id, selected, group, cx)),
            )
    }

    /// A tab's close control: a light `×`, muted until the pointer is on it, on
    /// the tab being shown and on any tab the pointer is over (§7.1).
    ///
    /// The tab's own width never changes: an out-of-sight `×` still occupies its
    /// box, so showing it cannot shuffle the strip — and hovering anywhere on the
    /// tab is what shows it, because the tab's content is the group.
    fn render_tab_close(
        &self,
        id: TabId,
        selected: bool,
        group: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = cx.theme().accent;
        div()
            .id(ElementId::NamedInteger("tab-close".into(), id.get()))
            .test_support()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .size_4()
            .rounded(cx.theme().radius)
            .text_color(cx.theme().muted_foreground)
            .when(!selected, |this| {
                this.opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
            })
            .hover(move |style| style.text_color(accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                // The tab itself selects on click; closing must not.
                cx.stop_propagation();
                this.close_tab(id, window, cx);
            }))
            .child(Icon::new(IconName::Close).xsmall())
            .into_any_element()
    }

    fn render_add_tab_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The strip is wider than the room it has, so the button has tabs behind
        // it: it says where the strip ends. With every tab on screen it sits in
        // open space, and a line there would divide nothing.
        let overflowing = self.strip_scroll.max_offset().x > px(0.);
        h_flex()
            .id(ADD_TAB_BOX_ID)
            .test_support()
            .flex_none()
            .h_full()
            .items_center()
            // Opaque, and the title bar's own colour rather than the strip's:
            // whatever the strip scrolls past this edge stops at the clip, and
            // nothing of it shows through the box.
            .bg(title_bar_background(cx))
            .when(overflowing, |this| {
                this.pl_1()
                    .border_l_1()
                    .border_color(cx.theme().title_bar_border)
            })
            .child(
                Button::new(ADD_TAB_ID)
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New tab")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_empty_tab(window, cx);
                    })),
            )
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

        let engines = self.take_engines(cx);
        if let Some(hook) = self.quit_hook.as_ref() {
            // `true` means the app is closing the window itself, so this close is
            // vetoed; `false` lets it through.
            let app_took_over = hook(QuitRequest { engines }, window, cx);
            self.may_close = !app_took_over;
            return self.may_close;
        }

        let report = stop_in_background(engines);
        self.stopping = Some(cx.spawn_in(window, async move |this, cx| {
            let _ = report.recv().await;
            let _ = this.update_in(cx, |view, window, _cx| {
                view.stopping = None;
                view.may_close = true;
                window.remove_window();
            });
        }));
        false
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_close_hook(window, cx);
        self.sync_window_title(window, cx);
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
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(self.render_tab_strip(window, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.selected_tab().clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::TestAppContext;

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
                            WorkspaceView::with_config(Arc::new(SwarmConfig::default()), window, cx)
                        })
                    },
                )
            })
            .expect("the workspace window");

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            // A second tab, driving a swarm: the state §7.1's rule is about.
            let running = view.update(cx, |view, cx| view.add_tab(window, cx));
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
