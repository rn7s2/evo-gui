//! The empty tab: the choosers, the folder button and the history list (§7.2).
//!
//! Every new tab starts here. Choosing a folder moves the tab to
//! [`TabState::Booting`](crate::TabState::Booting); picking a history row opens
//! that swarm in a new tab.

use std::path::PathBuf;

use gpui_kit::component::button::Button;
use gpui_kit::component::list::{List, ListDelegate, ListItem, ListState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Separator, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, px, App, Context, Entity, IntoElement, SharedString, Window};

use crate::history::HistoryRow;
use crate::tab::{TabContent, TabContentEvent};

/// The value every chooser starts at (§7.2): the swarm's own defaults.
pub(crate) const DEFAULT_CHOICE: &str = "Default";

/// The most workers the empty tab offers; the same ceiling the swarm accepts.
pub(crate) const MAX_WORKERS: u32 = 64;

/// One chooser: evo's default, or a concrete value.
type Chooser = Entity<SelectState<Vec<SharedString>>>;

/// The three choosers of the empty tab (§7.2), and nothing else.
pub(crate) struct Choosers {
    coordinator_model: Chooser,
    lanes_model: Chooser,
    workers: Chooser,
}

impl Choosers {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Self {
        Choosers {
            coordinator_model: model_chooser(window, cx),
            lanes_model: model_chooser(window, cx),
            workers: worker_chooser(window, cx),
        }
    }

    /// The coordinator model (`--model`); only passed when it is not `Default`.
    pub(crate) fn coordinator_model(&self, cx: &App) -> SharedString {
        chosen(&self.coordinator_model, cx)
    }

    /// The lanes' default model, recorded in the folder's `swarm.lisp` (§9.6).
    pub(crate) fn lanes_model(&self, cx: &App) -> SharedString {
        chosen(&self.lanes_model, cx)
    }

    /// The worker count (`--workers`); `Default` leaves evo's own in charge.
    pub(crate) fn workers(&self, cx: &App) -> SharedString {
        chosen(&self.workers, cx)
    }
}

impl TabContent {
    pub(crate) fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id("empty-tab")
            .test_support()
            .size_full()
            .p_6()
            .gap_6()
            .child(
                h_flex()
                    .items_center()
                    .gap_8()
                    .child(v_flex().gap_3().flex_1().child(self.render_choosers(cx)))
                    .child(
                        // Spans the three rows: it is their joint outcome.
                        Button::new("select-folder")
                            .label("Select folder…")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| this.pick_folder(window, cx))),
                    ),
            )
            .child(Separator::horizontal())
            .child(self.render_history(cx))
            .into_any_element()
    }

    /// Coordinator model, lanes model and worker count, each starting at
    /// `Default` (§7.2).
    fn render_choosers(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(self.render_chooser(
                "Coordinator model",
                "coordinator-model",
                &self.choosers.coordinator_model,
                cx,
            ))
            .child(
                v_flex()
                    .gap_1()
                    .child(self.render_chooser(
                        "Lanes model",
                        "lanes-model",
                        &self.choosers.lanes_model,
                        cx,
                    ))
                    .child(self.render_swarm_config_note(cx)),
            )
            .child(self.render_chooser(
                "Workers",
                "workers",
                &self.choosers.workers,
                cx,
            ))
    }

    fn render_chooser(
        &self,
        label: &'static str,
        id: &'static str,
        state: &Chooser,
        cx: &App,
    ) -> impl IntoElement {
        h_flex()
            .gap_4()
            .items_center()
            .child(
                div()
                    .w(px(160.))
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Select::new(state)
                    .id(id)
                    .small()
                    .w(px(280.))
                    .accessibility_label(label),
            )
    }

    /// Where the lanes model is recorded: the chosen folder's managed
    /// `swarm.lisp` block. Named next to the chooser because the file belongs
    /// to the folder, not to this tab (§9.6).
    fn render_swarm_config_note(&self, cx: &App) -> impl IntoElement {
        let path = match self.folder() {
            Some(folder) => format!("{}/.evo/swarm.lisp", folder.display()),
            None => "<folder>/.evo/swarm.lisp".to_string(),
        };
        div()
            .id("swarm-config-note")
            .min_w_0()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(path)
    }

    /// The resumable swarms, newest first (§9.5). The background scan that fills
    /// this list is not wired yet, so the rows are placeholders.
    fn render_history(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .text_sm()
                    .font_weight_semibold()
                    .child("History"),
            )
            .child(
                div()
                    .id("history")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(List::new(&self.history)),
            )
    }

    /// Ask the platform for a folder and hand the answer to the workspace
    /// (§7.2).
    ///
    /// The dialog is `rfd`'s async one: it neither blocks the UI thread nor
    /// hops onto one, resolves through the GPUI foreground executor, and
    /// cancelling it leaves the tab empty.
    pub(crate) fn pick_folder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let starting_folder = std::env::var_os("HOME").map(PathBuf::from);
        cx.spawn(async move |this, cx| {
            let mut dialog =
                rfd::AsyncFileDialog::new().set_title("Choose the folder this swarm runs in");
            if let Some(folder) = starting_folder {
                dialog = dialog.set_directory(folder);
            }
            let Some(picked) = dialog.pick_folder().await else {
                return;
            };
            let folder = picked.path().to_path_buf();
            let _ = this.update(cx, |_content, cx| {
                cx.emit(TabContentEvent::FolderPicked(folder))
            });
        })
        .detach();
    }
}

/// A model chooser with only `Default` in it.
///
/// Model ids are never hardcoded (§9.4): the real catalog arrives with the
/// `/registry` cache and the lanes chooser is narrowed to the models a
/// `--no-userspace` lane can register. Until that lands, the honest list is
/// evo's own default.
fn model_chooser(window: &mut Window, cx: &mut App) -> Chooser {
    select_state(vec![DEFAULT_CHOICE.into()], window, cx)
}

/// `Default` plus 1..=[`MAX_WORKERS`] workers (§7.2).
fn worker_chooser(window: &mut Window, cx: &mut App) -> Chooser {
    let mut items = vec![SharedString::from(DEFAULT_CHOICE)];
    items.extend((1..=MAX_WORKERS).map(|workers| SharedString::from(workers.to_string())));
    select_state(items, window, cx)
}

fn select_state(items: Vec<SharedString>, window: &mut Window, cx: &mut App) -> Chooser {
    // `Default` is the first item, so starting on item 0 is the default (§7.2).
    cx.new(|cx| SelectState::new(items, Some(IndexPath::default()), window, cx))
}

fn chosen(state: &Chooser, cx: &App) -> SharedString {
    state
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_else(|| SharedString::from(DEFAULT_CHOICE))
}

/// The empty tab's history list: the rows, and which of them is selected.
pub(crate) struct HistoryList {
    rows: Vec<HistoryRow>,
    selected: Option<IndexPath>,
}

impl HistoryList {
    pub(crate) fn new(rows: Vec<HistoryRow>) -> Self {
        HistoryList {
            rows,
            selected: None,
        }
    }

    pub(crate) fn rows(&self) -> &[HistoryRow] {
        &self.rows
    }

    pub(crate) fn row(&self, row: usize) -> Option<&HistoryRow> {
        self.rows.get(row)
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
        Some(
            ListItem::new(ix)
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(div().child(row.folder_name()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(row.summary()),
                        ),
                )
                .selected(Some(ix) == self.selected),
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
}
