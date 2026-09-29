//! The window this app opens and the chrome it opens with (§7.1).
//!
//! One window, whose title bar *is* the tab strip: one [`Tab`] per swarm, plus a
//! `+` that always appends an empty one. [`WorkspaceView`] owns the tab set and
//! the selection; everything below the strip belongs to a
//! [`TabContent`](crate::TabContent).

use std::collections::VecDeque;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, IconName, Sizable as _, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, point, px, size, App, Bounds, Context, ElementId, Entity, IntoElement, Pixels,
    ScrollHandle, Size, Subscription, Task, TestSupportExt as _, Window, WindowBounds,
    WindowOptions,
};

use std::rc::Rc;

use serde_json::Value;
use session::{HistoryEntry, LaunchPlan};
use store::model_cache::ModelCache;
use tab_engine::EngineHandle;

use crate::launch::{stop_in_background, Launch, SwarmConfig};
use crate::tab::{RegistryHook, TabContent, TabContentEvent, TabId};

/// The size the window opens at when the display has room for it (§7.1).
pub const DEFAULT_WINDOW_SIZE: Size<Pixels> = size(px(1600.), px(1000.));

/// The smallest window that still fits the lane column, a transcript and the
/// composer side by side (§7.3).
pub const MIN_WINDOW_SIZE: Size<Pixels> = size(px(1000.), px(700.));

/// How wide one tab may grow before its label is ellipsized (§7.1).
const TAB_MAX_WIDTH: f32 = 220.;

/// The `+` at the end of the strip: always present, always the last thing.
const ADD_TAB_ID: &str = "tab-add";

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
/// them over: stopping them is the hook's job. Returning `true` lets the window
/// close; `false` vetoes this close — the hook then closes the window itself
/// (`window.remove_window()`) once it is done, and that close is allowed.
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
    /// `$HOME`, for shortening the paths in the rows.
    pub home: Option<String>,
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
        let mut view = WorkspaceView {
            tabs: Vec::new(),
            selected: 0,
            next_id: 0,
            strip_scroll: ScrollHandle::default(),
            subscriptions: VecDeque::new(),
            config,
            quit_hook: None,
            launcher: LauncherData::default(),
            registry_hook: None,
            close_hook_installed: false,
            stopping: None,
            may_close: false,
        };
        view.open_empty_tab(window, cx);
        view
    }

    /// How many tabs the window is showing.
    pub fn open_tab_count(&self) -> usize {
        self.tabs.len()
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
        // A tab opened now shows what the app already learned (§9.4, §9.5).
        let launcher = self.launcher.clone();
        tab.update(cx, |tab, cx| tab.set_launcher_data(&launcher, window, cx));
        let registry_hook = self.registry_hook.clone();
        tab.update(cx, |tab, _cx| tab.set_registry_hook(registry_hook));
        cx.notify();
        tab
    }

    /// Show the tab at `index` (§7.1).
    pub fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() && index != self.selected {
            self.selected = index;
            cx.notify();
        }
    }

    /// Close `id`: the tab goes at once, and its swarm runs §3's ladder on a
    /// thread of its own (§14.5). Closing the last tab leaves a fresh empty one
    /// rather than an empty window (§7.2).
    pub fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.read(cx).id() == id) else {
            return;
        };
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
        }
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("tab-strip")
            // The bar takes the width its tabs need, so the `+` sits right after
            // the last one; capped at the window, so that once the tabs overflow
            // it is the strip that scrolls and the `+` stays visible (§7.1).
            .max_w_full()
            .max_width(px(TAB_MAX_WIDTH))
            .track_scroll(&self.strip_scroll)
            .selected_index(self.selected)
            .on_click(cx.listener(|this, index: &usize, _window, cx| this.select_tab(*index, cx)))
            .suffix(self.render_add_tab_button(cx))
            .children(self.tabs.iter().map(|tab| self.render_tab(tab, cx)))
    }

    /// One tab: the folder's name, the whole path plus the swarm's state on
    /// hover, and — on the tab being shown — a close button (§7.1).
    fn render_tab(&self, tab: &Entity<TabContent>, cx: &mut Context<Self>) -> Tab {
        let content = tab.read(cx);
        let id = content.id();
        let title = content.title();
        let tooltip = content.tooltip();
        let selected = self
            .tabs
            .get(self.selected)
            .is_some_and(|shown| shown.read(cx).id() == id);
        Tab::new()
            .aria_label(title.clone())
            .child(
                div()
                    .id(ElementId::NamedInteger("tab-label".into(), id.get()))
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .child(title),
            )
            // Only the tab being shown carries its close button: an × on every
            // tab is noise, and an invisible one would still be clickable.
            .when(selected, |tab| {
                tab.suffix(
                    Button::new(ElementId::NamedInteger("tab-close".into(), id.get()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::CircleX)
                        .tooltip("Close tab")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // The tab itself selects on click; closing must not.
                            cx.stop_propagation();
                            this.close_tab(id, window, cx);
                        })),
                )
            })
    }

    fn render_add_tab_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new(ADD_TAB_ID)
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .tooltip("New tab")
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_empty_tab(window, cx);
            }))
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
            self.may_close = !hook(QuitRequest { engines }, window, cx);
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
        v_flex()
            .id("workspace")
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(self.render_tab_strip(cx)))
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
