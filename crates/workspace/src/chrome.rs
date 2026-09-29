//! The window this app opens and the chrome it opens with (§7.1).
//!
//! One window, whose title bar *is* the tab strip: one [`Tab`] per swarm, plus a
//! `+` that always appends an empty one. [`WorkspaceView`] owns the tab set and
//! the selection; everything below the strip belongs to a
//! [`TabContent`](crate::TabContent).

use std::collections::VecDeque;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, IconName, Sizable as _, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, point, px, size, App, Bounds, Context, ElementId, Entity, IntoElement, Pixels,
    ScrollHandle, Size, Subscription, TestSupportExt as _, Window, WindowBounds, WindowOptions,
};

use crate::tab::{TabContent, TabContentEvent, TabId};

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
}

impl WorkspaceView {
    /// A window with one empty tab: the app never auto-starts a swarm and never
    /// has an empty window (§7.2, §14.6).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = WorkspaceView {
            tabs: Vec::new(),
            selected: 0,
            next_id: 0,
            strip_scroll: ScrollHandle::default(),
            subscriptions: VecDeque::new(),
        };
        view.open_empty_tab(window, cx);
        view
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
        let tab = cx.new(|cx| TabContent::new(id, window, cx));
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

    /// Close `id`, which shuts its swarm down once the client wiring lands (§3,
    /// §14.5). Closing the last tab leaves a fresh empty one rather than an
    /// empty window (§7.2).
    pub fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.read(cx).id() == id) else {
            return;
        };
        self.tabs.remove(index);
        self.subscriptions.retain(|(closed, _)| *closed != id);
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
            TabContentEvent::FolderPicked(folder) => {
                tab.update(cx, |content, cx| content.begin_boot(folder, cx));
            }
            TabContentEvent::RetryRequested(_) => {
                tab.update(cx, |content, cx| content.retry(cx));
            }
            TabContentEvent::HistoryRowActivated(row) => {
                // Resuming the recorded session comes with the client wiring;
                // opening the row's folder is what the tab means today (§9.5).
                let tab = self.open_empty_tab(window, cx);
                tab.update(cx, |content, cx| content.begin_boot(row.folder, cx));
            }
        }
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("tab-strip")
            // Filling the bar is what lets the tabs inside scroll once they
            // overflow it (§7.1).
            .w_full()
            .min_w_0()
            .max_width(px(TAB_MAX_WIDTH))
            .track_scroll(&self.strip_scroll)
            .selected_index(self.selected)
            .on_click(cx.listener(|this, index: &usize, _window, cx| this.select_tab(*index, cx)))
            .suffix(self.render_add_tab_button(cx))
            .children(self.tabs.iter().map(|tab| self.render_tab(tab, cx)))
    }

    /// One tab: the folder's name, the whole path plus the swarm's state on
    /// hover, and a close button (§7.1).
    fn render_tab(&self, tab: &Entity<TabContent>, cx: &mut Context<Self>) -> Tab {
        let content = tab.read(cx);
        let id = content.id();
        let title = content.title();
        let tooltip = content.tooltip();
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
            .suffix(
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
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
