//! The tab page (§7.3) and the screens around it: a swarm starting, one that
//! never came up, and one being stopped.
//!
//! The page is the layout its parts fill: the agent column on the left, the
//! selected agent's transcript and todos in the middle, the coordinator's
//! composer on the right. All three are real crates: `agent_list`, `transcript`
//! and `composer`; this module is the frame they hang in, the folder line under
//! the list, and the header that names the agent being shown.

use std::path::Path;
use std::rc::Rc;

use agent_list::{activity_status, status_color};
use gpui_kit::base::{InteractiveElementExt as _, ResizeHandleRenderer};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    h_flex, h_resizable, resizable_panel, resize_handle_appearance, v_flex, ActiveTheme as _,
    IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, AnyElement, App, Context, IntoElement, Pixels, SharedString, TestSupportExt as _,
};
use session::{lane_task_label, AgentKey, LaneStatus, TabModel};
use transcript::TodoPanel;

use crate::panes::side_of;
use crate::tab::{Notice, NoticeTone, TabContent, TabContentEvent, TabState};
use store::app_state::{CENTER_MIN, LEFT_MAX, LEFT_MIN, RIGHT_MAX, RIGHT_MIN};

/// The middle column's header: its own padding, one line of text and the hairline
/// under it. The composer column pads by this, so the input begins on the same
/// line as the transcript's first row rather than over the header (§7.3). The
/// assertion in `chrome`'s tests is what keeps the two in step.
pub(crate) const HEADER_HEIGHT: Pixels = px(35.);

/// The status line's element id. It carries the whole line as its tooltip and as
/// its accessible name (§7.3).
pub const READOUT_LINE_ID: &str = "status-readout";

/// The status line's size: the TUI's dim status line, small enough to stay one
/// line however long the line is.
const READOUT_SIZE: Pixels = px(12.);

/// What the status line's tooltip wraps to: the middle column's own minimum, so
/// the whole line is read in a card the width of the column it belongs to.
const READOUT_TOOLTIP_WIDTH: Pixels = px(CENTER_MIN);

/// How many characters the folder line under the agent list may take before it is
/// elided in the middle: about what fits under a 260 px column at 11 px text.
const FOLDER_LINE_CHARS: usize = 34;

/// How many characters a path may take on the boot and failure screens, where the
/// whole window is the measure.
const SCREEN_PATH_CHARS: usize = 88;

impl TabContent {
    /// The content for the tab's current state.
    pub(crate) fn render_for_state(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            TabState::Empty => self.render_empty(cx),
            TabState::Booting { folder } => self.render_booting(folder, cx),
            TabState::Running { folder } => self.render_page(folder, cx),
            TabState::Failed {
                folder,
                message,
                log_tail,
                was_up,
                tab_dir,
            } => self.render_failed(
                folder,
                message.clone(),
                log_tail,
                *was_up,
                tab_dir.as_deref(),
                cx,
            ),
            TabState::Stopping { .. } => self.render_stopping(cx),
        }
    }

    /// While the swarm starts: what is starting, and where (§3).
    fn render_booting(&self, folder: &Path, cx: &App) -> AnyElement {
        centered_region(
            "starting swarm…",
            Some(path_text(
                "boot-folder",
                folder,
                SCREEN_PATH_CHARS,
                true,
                cx,
            )),
            "the swarm's log tail appears here if it fails to come up",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
            cx,
        )
    }

    /// While §3's ladder runs on the tab's way out (§9.8).
    fn render_stopping(&self, cx: &App) -> AnyElement {
        centered_region(
            "stopping the swarm…",
            None,
            "the server is being asked to shut down, then signalled if it does not",
            Some(Spinner::new().xsmall().color(cx.theme().muted_foreground)),
            cx,
        )
    }

    /// A failed swarm: the tail of its log, and a way to try again (§9.7).
    ///
    /// Two failures wear this screen, and `was_up` is which: a swarm that never
    /// answered `/health`, and one that was up and went away. The second is not
    /// a boot that failed — the session it was writing is still there, and Retry
    /// resumes it — so it says so.
    fn render_failed(
        &self,
        folder: &Path,
        message: Option<String>,
        log_tail: &str,
        was_up: bool,
        tab_dir: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folder = folder.to_path_buf();
        let retry = folder.clone();
        let show_log = !log_tail.trim().is_empty()
            && message.as_deref().map(str::trim) != Some(log_tail.trim());
        // The evidence, as the screen shows it: the same lines, with the long
        // paths a swarm writes read back as places (see `shorten_log`).
        let log = shorten_log(log_tail, tab_dir);
        v_flex()
            .id("boot-failure")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(div().font_semibold().child(SharedString::from(if was_up {
                "The swarm is gone"
            } else {
                "Could not start a swarm"
            })))
            .child(path_text(
                "failure-folder",
                &folder,
                SCREEN_PATH_CHARS,
                true,
                cx,
            ))
            .when_some(message.clone(), |this, message| {
                // The engine's own one-line reason, ahead of the evidence (§9.7).
                this.child(
                    div()
                        .id("boot-failure-reason")
                        .test_support()
                        .aria_label(message.clone())
                        .max_w(px(720.))
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(message)),
                )
            })
            // A swarm that had been up has no boot reason: nothing failed to
            // start. It still owes the reader the two things they will ask —
            // what happened, and what Retry does with their session (§9.7).
            .when(was_up && message.is_none(), |this| {
                this.child(
                    div()
                        .id("failure-gone-note")
                        .test_support()
                        .max_w(px(720.))
                        .min_w_0()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(
                            "The server exited on its own. Retry resumes the session it was writing.",
                        )),
                )
            })
            // The engine repeats its reason as the log tail when the process
            // never wrote a line, so the box only appears when it carries
            // something the line above does not (§9.7).
            .when(show_log, |this| {
                this.child(
                    // The log is the evidence: its own box, monospace, and only as
                    // tall as it needs to be before it scrolls (§9.7). Lines are
                    // not rewrapped — a stack trace and a long command line are
                    // read as they were written — so the box scrolls sideways
                    // too, and carries the whole tail for a screen reader.
                    div()
                        .id("boot-log-tail")
                        .test_support()
                        .aria_label(log.clone())
                        .w_full()
                        .max_w(px(720.))
                        .max_h(px(320.))
                        .overflow_x_scroll()
                        .overflow_y_scroll()
                        .p_3()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().muted_foreground)
                        .child(div().whitespace_nowrap().child(log)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("tab-retry")
                            .label("Retry")
                            .primary()
                            .on_click(cx.listener(move |_this, _, _, cx| {
                                cx.emit(TabContentEvent::RetryRequested(retry.clone()))
                            })),
                    )
                    .child(
                        Button::new("tab-failure-close")
                            .label("Close")
                            .ghost()
                            .on_click(cx.listener(|_this, _, _, cx| {
                                cx.emit(TabContentEvent::CloseRequested)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The tab page: agents, transcript and todos, composer (§7.3).
    ///
    /// Three columns with a draggable split between them (§7.3), drawn by the
    /// kit's resizable panels: the two side columns take the widths the window
    /// remembers, the middle one takes what is left. Every tab's page shares one
    /// [`panes::Panes`](crate::panes) state — the three columns are the same three
    /// columns in every tab, so a split dragged in one tab is dragged in all of
    /// them — and the window hears where the drag ended so it can write it down.
    fn render_page(&self, folder: &Path, cx: &mut Context<Self>) -> AnyElement {
        let panes = self.panes();
        div()
            .id("tab-page")
            .test_support()
            .size_full()
            .child(self.render_columns(folder, panes, cx))
            .into_any_element()
    }

    /// The three columns themselves, in the group that lets the splits between
    /// them be dragged (§7.3).
    fn render_columns(
        &self,
        folder: &Path,
        panes: store::app_state::Panes,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_resizable("tab-columns")
            .when_some(self.pane_state.clone(), |group, state| {
                group.with_state(&state)
            })
            .with_handle_appearance(self.pane_hands(cx))
            .child(
                resizable_panel()
                    .size(px(panes.left))
                    .size_range(px(LEFT_MIN)..px(LEFT_MAX))
                    .flex_none()
                    .child(self.render_agent_column(folder, cx)),
            )
            .child(
                resizable_panel()
                    .size_range(px(CENTER_MIN)..Pixels::MAX)
                    .child(self.render_center_column(cx)),
            )
            .child(
                resizable_panel()
                    .size(px(panes.right))
                    .size_range(px(RIGHT_MIN)..px(RIGHT_MAX))
                    .flex_none()
                    .child(self.render_composer_column(cx)),
            )
    }

    /// The dividers between the three columns (§7.3): the kit's own hairline and
    /// the pill it grows on hover, wrapped in the one gesture it has no room for —
    /// a double-click that puts that side back to the width it starts at.
    fn pane_hands(&self, cx: &Context<Self>) -> ResizeHandleRenderer {
        let kit = resize_handle_appearance();
        let tab = cx.entity().downgrade();
        let state = self.pane_state.clone();
        Rc::new(move |handle, window, cx| {
            let side_state = state.clone();
            let side_tab = tab.clone();
            // What the kit draws: the hairline between the columns, and the pill
            // it grows when the pointer is over it (§7.3).
            let painted = kit(handle, window, cx);
            Some(
                div()
                    // Only what a double-click needs: the divider itself is the
                    // kit's, and a keystroke must not land on this.
                    .id("pane-handle")
                    .h_full()
                    .w(px(1.))
                    // The kit sets the cursor on the band it drags; this is the
                    // one pixel of it that is drawn, and the pointer reads it.
                    .cursor_col_resize()
                    .on_double_click(move |event, _, cx| {
                        // Which split this is: the kit draws both with the same
                        // element and tells neither of them its own name, so the
                        // pointer's place against the two splits' own places is
                        // what says it (§7.3).
                        let sizes = side_state
                            .as_ref()
                            .map(|state| state.read(cx).sizes().to_vec())
                            .unwrap_or_default();
                        let side = side_of(event.position().x.as_f32(), &sizes);
                        if let Some(tab) = side_tab.upgrade() {
                            // The window owns the widths (§7.3); a tab only says
                            // what was asked of it.
                            tab.update(cx, |_, cx| cx.emit(TabContentEvent::ResetPane(side)));
                        }
                    })
                    .child(painted.unwrap_or_else(|| {
                        div()
                            .h_full()
                            .w(px(1.))
                            .bg(cx.theme().border)
                            .into_any_element()
                    }))
                    .into_any_element(),
            )
        })
    }

    /// The agent column: `agent_list`'s own rows, with the folder the swarm runs
    /// in pinned under them (§7.3).
    ///
    /// The list takes the room; the folder line keeps its own single row at the
    /// bottom, so a path can never push the lanes around.
    fn render_agent_column(&self, folder: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("agent-column")
            .test_support()
            // The panel the column sits in decides how wide it is (§7.3); the
            // column fills whatever it is given, and the divider between them is
            // the panel's own handle, not a border here.
            .w_full()
            .h_full()
            .child(div().flex_1().min_h_0().child(self.agents.clone()))
            .child(folder_line(folder, cx))
    }

    /// The slim line above the transcript: whose transcript this is, what that
    /// agent is doing, and the task it was given (§7.3).
    ///
    /// The colours are the left column's own (`agent_list`'s status table), so the
    /// header and the row agree at a glance. On the right sits the one control the
    /// transcript needs: showing the thinking the rows already carry.
    fn render_agent_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let agent = self.selected_agent();
        let name = match agent {
            AgentKey::Coordinator => SharedString::from("main"),
            AgentKey::Lane(n) => SharedString::from(format!("Lane {n}")),
        };
        // The model is where the status and the task come from; without one (a
        // page whose swarm is not there to ask) the header says who is shown and
        // nothing more.
        let model = self.model();
        let status = match (agent, model) {
            (AgentKey::Coordinator, Some(model)) => activity_status(model.activity()),
            (AgentKey::Lane(n), Some(model)) => model
                .lane_rows()
                .iter()
                .find(|lane| lane.n as u32 == n)
                .map(|lane| lane.status)
                .unwrap_or(LaneStatus::Idle),
            _ => LaneStatus::Idle,
        };
        let task = match (agent, model) {
            (AgentKey::Lane(n), Some(model)) => model
                .lane_rows()
                .iter()
                .find(|lane| lane.n as u32 == n)
                .and_then(|lane| lane.task.as_deref())
                .filter(|task| !task.is_empty())
                .map(lane_task_label),
            _ => None,
        };
        h_flex()
            .id("transcript-header")
            .test_support()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_color(status_color(status, cx.theme()))
                    .child(SharedString::from(status.glyph().to_string())),
            )
            .child(div().font_medium().child(name))
            .child(div().flex_1().min_w_0().when_some(task, |this, task| {
                // One line, elided: a task is a sentence, and the transcript
                // below is where the whole of it can be read.
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(task)),
                )
            }))
            .when_some(self.thinking_toggle(cx), |this, toggle| this.child(toggle))
            .into_any_element()
    }

    /// The quiet control the header carries while the shown agent has thinking
    /// text to reveal (§7.3).
    ///
    /// It is the *view's* own state: switching agents shows each transcript the
    /// way its reader left it.
    fn thinking_toggle(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.transcripts.get(&self.selected_agent())?.clone();
        let (has_thinking, showing) = {
            let view = view.read(cx);
            (view.has_thinking(cx), view.is_showing_thinking(cx))
        };
        if !has_thinking {
            return None;
        }
        Some(
            Button::new("transcript-thinking")
                .label(if showing {
                    "Hide thinking"
                } else {
                    "Show thinking"
                })
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |_this, _, _window, cx| {
                    view.update(cx, |view, cx| view.toggle_thinking(cx));
                }))
                .into_any_element(),
        )
    }

    /// The selected agent's transcript, with its todos along the bottom (§7.3).
    fn render_center_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected_agent();
        let view = self.transcripts.get(&selected).cloned();
        let todos = view
            .as_ref()
            .map(|view| view.read(cx).todos().to_vec())
            .unwrap_or_default();
        v_flex()
            .id("transcript-column")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_agent_header(cx))
            .when(self.is_reconnecting(), |this| {
                this.child(reconnect_badge(cx))
            })
            // The view says what an agent with nothing to show means, and for
            // which agent (§7.3).
            .child(div().flex_1().min_h_0().children(view))
            // The panel renders nothing while the agent has no todos, so this
            // row disappears rather than leaving a gap (§7.3).
            .child(TodoPanel::new(&todos))
            // Under everything else, the one line that says what the agent being
            // shown is working with (§7.3).
            .child(status_line(status_text(self.model()), cx))
    }

    /// The coordinator's composer: the input, and the readout and the single
    /// action button on one line beneath it (§7.3).
    ///
    /// A refused POST says so here — above the input that caused it, in the
    /// server's own words, and gone on its own (§4, §9.2).
    fn render_composer_column(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .id("composer-column")
            .test_support()
            .w_full()
            .h_full()
            // The header's own height at the top, so what is below it starts on
            // the line the transcript starts on (§7.3).
            .px_4()
            .pt(HEADER_HEIGHT)
            .pb_4()
            .gap_2()
            .when_some(self.notice(), |this, notice| {
                this.child(notice_line(notice, cx))
            })
            .child(
                div()
                    .id("composer-body")
                    .test_support()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(self.composer.clone()),
            )
    }
}

/// What the status line under the transcript says: the readout of the agent
/// being shown, or the UI's own words while nothing about it is known (§7.3).
///
/// The coordinator's line is made of `/state` and `/registry`; a lane's of its
/// own transcript and usage. A lane selected before anything about it has been
/// read has no line at all — which is a fact about that agent, not an empty
/// space, and is said in the same muted voice.
fn status_text(model: Option<&TabModel>) -> SharedString {
    model
        .and_then(|model| model.selected_readout_text())
        .map(SharedString::from)
        .unwrap_or_else(|| SharedString::from("no metrics yet"))
}

/// The status line itself: one muted line at the foot of the middle column, with
/// a hairline above it (§7.3).
///
/// The line is as long as the readout is; the column decides how much of it is
/// drawn. What is not drawn is still *said*: the whole line is the element's
/// accessible name, and its tooltip, wrapped rather than run off the screen.
fn status_line(text: SharedString, cx: &App) -> impl IntoElement {
    let tooltip = text.clone();
    div()
        .id(READOUT_LINE_ID)
        .test_support()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .truncate()
        .px_4()
        .pt_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_size(READOUT_SIZE)
        .text_color(cx.theme().muted_foreground)
        .aria_label(text.clone())
        .tooltip(move |window, cx| {
            let line = tooltip.clone();
            Tooltip::element(move |_, _| div().w(READOUT_TOOLTIP_WIDTH).child(line.clone()))
                .build(window, cx)
        })
        .child(text)
}

/// The folder the swarm runs in: one line under the agent list, `~`-shortened
/// and elided in the middle, with the whole path on hover (§7.3).
fn folder_line(folder: &Path, cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(path_text(
            "agent-folder",
            folder,
            FOLDER_LINE_CHARS,
            true,
            cx,
        ))
}

/// A path as one line of text: elided in the middle to LIMIT characters, with the
/// whole of it on hover.
///
/// `muted` is the smaller, quieter form the boot and failure screens use.
fn path_text(id: &'static str, folder: &Path, limit: usize, muted: bool, cx: &App) -> AnyElement {
    let full = folder.display().to_string();
    let shown = shorten_path(&full, limit);
    div()
        .id(id)
        .test_support()
        .text_xs()
        .min_w_0()
        .truncate()
        .when(muted, |this| this.text_color(cx.theme().muted_foreground))
        .child(SharedString::from(shown))
        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
        .into_any_element()
}

/// A path as `~` where it is the home directory: the form a person reads.
fn shorten_path(path: &str, limit: usize) -> String {
    let shortened = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    };
    elide_middle(&shortened, limit)
}

/// `head…tail`: both ends of a path are worth keeping, because the tail names the
/// folder and the head says where it is.
fn elide_middle(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit || limit < 6 {
        return text.to_string();
    }
    let head = limit / 3;
    let tail = limit - 1 - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// A log tail as the failure screen shows it (§9.7).
///
/// A server's log is mostly paths, and all of them are long: the tab's own
/// directory is in `--token-file`, the home or temporary directory a session or a
/// probe was made in is on half the lines. Read as `…/T/swarm-client-fixture-71353-…/
/// app/tabs/1a6c242b-…/token` that is a wall of temporary directories; read as
/// `<tab>/token` it is a place. The log **file** keeps its own lines — this is
/// what one screen shows of them.
fn shorten_log(tail: &str, tab_dir: Option<&Path>) -> String {
    let mut log = tail.to_string();
    if let Some(dir) = tab_dir {
        log = shorten_prefix(&log, &dir.to_string_lossy(), "<tab>");
    }
    if let Some(home) = env_path("HOME") {
        log = shorten_prefix(&log, &home, "~");
    }
    if let Some(tmp) = env_path("TMPDIR") {
        log = shorten_prefix(&log, &tmp, "…");
    }
    log
}

/// A directory from the environment — empty or `/` would shorten the whole
/// filesystem into a single character.
fn env_path(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty() && value != "/")
}

/// Replace `prefix` with SHORT where it **is** a path: at the start of one, so
/// `/Users/me` is not read out of `/Users/me2/x`, and with a separator (or the end
/// of the text) after it, so a directory named `/tmp/evolved` is not the tab's
/// `/tmp/evo`.
fn shorten_prefix(text: &str, prefix: &str, short: &str) -> String {
    if prefix.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let (before, from) = rest.split_at(at);
        let after = &from[prefix.len()..];
        let starts_a_path = before.chars().next_back().is_none_or(|c| !in_a_path(c));
        let ends_a_path = after.chars().next().is_none_or(|c| c == '/');
        out.push_str(before);
        out.push_str(if starts_a_path && ends_a_path {
            short
        } else {
            prefix
        });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether a character could be part of the path a prefix was found inside, and
/// so whether that prefix is really the start of one.
fn in_a_path(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+')
}

/// A transient line above the composer: the server's own words, dim when the
/// answer was `409 not now` and in the danger colour for a failure (§4, §9.2).
///
/// The two tones are two elements, so a test can tell them apart. A line that had
/// to be paraphrased — a POST the server never answered, `503` — carries the raw
/// error text on hover, so nothing is lost by saying it in plain words.
fn notice_line(notice: &Notice, cx: &App) -> AnyElement {
    let detail = notice.detail.clone();
    h_flex()
        .id(notice.tone.element_id())
        .test_support()
        .aria_label(notice.text.clone())
        .w_full()
        .min_w_0()
        .px_2()
        .py_1()
        .rounded(cx.theme().radius)
        .text_xs()
        .bg(cx.theme().muted)
        .text_color(match notice.tone {
            NoticeTone::Dim => cx.theme().muted_foreground,
            NoticeTone::Error => cx.theme().danger,
        })
        .when_some(detail, |this, detail| {
            this.tooltip(move |window, cx| {
                Tooltip::new(detail.clone())
                    .max_w(px(520.))
                    .build(window, cx)
            })
        })
        .child(div().min_w_0().truncate().child(notice.text.clone()))
        .into_any_element()
}

/// The badge a dropped stream shows while it retries (§9.7).
fn reconnect_badge(cx: &App) -> impl IntoElement {
    h_flex()
        .id("reconnecting")
        .test_support()
        .w_full()
        .px_4()
        .py_1()
        .gap_2()
        .items_center()
        .bg(cx.theme().muted)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(IconName::Loader)
        .child("reconnecting…")
}

/// A centered message region, used while a tab has nothing to show yet.
fn centered_region(
    headline: &str,
    detail: Option<AnyElement>,
    note: &str,
    spinner: Option<Spinner>,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id("booting")
        .test_support()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .when_some(spinner, |this, spinner| this.child(spinner))
        .child(
            div()
                .font_semibold()
                .child(SharedString::from(headline.to_string())),
        )
        .when_some(detail, |this, detail| this.child(detail))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(note.to_string())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;

    /// A path is elided in the middle: both ends survive, and the tail — the part
    /// that names the folder — gets the longer half.
    #[test]
    fn a_long_path_keeps_both_ends() {
        let path = "/var/folders/5d/bgm58_l51j52vqz4v7jrdfch0000gn/T/fixture/proj";
        let shown = elide_middle(path, 30);
        assert_eq!(shown.chars().count(), 30);
        assert!(shown.starts_with("/var/"), "{shown}");
        assert!(shown.ends_with("re/proj"), "{shown}");
        assert!(shown.contains('…'), "{shown}");

        // Short enough to fit: nothing is taken away.
        assert_eq!(elide_middle("/tmp/proj", 30), "/tmp/proj");
        assert_eq!(elide_middle("", 30), "");
    }

    /// `~` is what a person reads for their own home directory, and only there.
    #[test]
    fn the_home_directory_is_a_tilde() {
        let home = std::env::var("HOME").expect("a home directory");
        assert_eq!(
            shorten_path(&format!("{home}/work/proj"), 80),
            "~/work/proj"
        );
        assert_eq!(shorten_path("/var/tmp/proj", 80), "/var/tmp/proj");
    }

    /// §9.7: the tail a failure screen shows is the log's lines with the paths a
    /// swarm writes read back as places — its own directory, the home directory,
    /// the temporary one — and nothing else about the lines touched.
    #[test]
    fn the_log_tail_names_places_not_directories() {
        // Nothing above this directory can be shortened, so what the assertions
        // below see is this rule and no other.
        let tab_dir = Path::new("/evo-fixture/tabs/1a6c242b-ba23-4e6b");
        let tail = "\
evo-swarm serve: listening on http://127.0.0.1:52658/ (token in /evo-fixture/tabs/1a6c242b-ba23-4e6b/token)
evo-swarm: restarting serve --workers 2 --port 52658 --token-file /evo-fixture/tabs/1a6c242b-ba23-4e6b/token --resume (attempt 1)
evo-swarm: another tab's file /evo-fixture/tabs/1a6c242b-ba23-4e6bff/token is not this one's
evo-swarm: no directory here: /evo-fixture/tabs/1a6c242b-ba23-4e6bbackup
still here";
        let shown = shorten_log(tail, Some(tab_dir));

        assert!(
            shown.contains("(token in <tab>/token)"),
            "a path into the tab's own directory reads as the tab: {shown}"
        );
        assert!(
            shown.contains("--token-file <tab>/token --resume"),
            "the command line the swarm logged keeps its shape: {shown}"
        );
        assert!(
            shown.contains("/evo-fixture/tabs/1a6c242b-ba23-4e6bff/token"),
            "a *different* directory that merely starts with the same characters is left alone: {shown}"
        );
        assert!(
            shown.contains("/evo-fixture/tabs/1a6c242b-ba23-4e6bbackup"),
            "and so is one that carries on from it: {shown}"
        );
        assert!(shown.ends_with("still here"), "no line is lost: {shown}");

        // Without a directory to shorten against — a failure that never got as
        // far as one — the lines are shown as they are.
        assert_eq!(
            shorten_log(tail, None).lines().count(),
            tail.lines().count()
        );
    }

    /// `status_line`'s own view: a column of a fixed width with the line in it.
    struct LineView(SharedString, Pixels);

    impl gpui_kit::Render for LineView {
        fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(self.1).child(status_line(self.0.clone(), cx))
        }
    }

    /// §7.3: the line is one line in a column that may be narrower than it is —
    /// and what is not drawn is still read. The whole line is the element's
    /// accessible name, which is what a reader who cannot see the ellipsis (and a
    /// test) is meant to read.
    #[gpui_kit::test]
    fn a_status_line_wider_than_its_column_keeps_all_of_itself_as_its_name(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let line: SharedString =
            "ark-deepseek-v4.1-flash \u{b7} max \u{b7} ctx 48k/936k (5%) \u{b7} 97% cached \u{b7} \
             goal a1b2c3d4 (active) 12k/50k \u{b7} 0123456789 0123456789 0123456789 0123456789"
                .into();
        cx.update(gpui_kit::init);
        let (_view, cx) = cx.add_window_view(|_, _| LineView(line.clone(), px(200.)));
        cx.update(|window, cx| window.render_frame(cx));

        let element = cx.update(|window, _| window.find(READOUT_LINE_ID));
        assert!(element.visible(), "the line is drawn");
        assert_eq!(
            element.label(),
            Some(line.as_ref()),
            "the whole line, drawn or not, is what the element is called"
        );
        assert!(
            element.bounds().size.width <= px(200.),
            "and it stays inside the column it was given: {:?}",
            element.bounds()
        );
        // One line of 12 px text, its padding and the hairline — not two.
        assert!(
            element.bounds().size.height <= px(36.),
            "one line high, however long the line is: {:?}",
            element.bounds()
        );
    }

    /// §7.3: what the line says is the readout of the agent being *shown*, and a
    /// selected agent nothing is known about is a fact the line states rather
    /// than an empty row.
    #[test]
    fn the_status_line_says_what_is_known_and_says_when_nothing_is() {
        assert_eq!(
            status_text(None),
            "no metrics yet",
            "no swarm to ask, nothing to say"
        );

        let mut model = TabModel::new();
        model.on_state(&serde_json::json!({
            "status": "idle",
            "model": "ark-deepseek-v4.1-flash",
            "thinking": "max",
            "context_tokens": 48_000,
            "context_window": 936_000,
        }));
        let line = status_text(Some(&model));
        assert!(
            line.contains("ark-deepseek-v4.1-flash") && line.contains("ctx 48k/936k"),
            "the coordinator's own line: {line}"
        );

        // A lane selected before anything about it has been read: the same words
        // as a page with no model at all (§7.3).
        model.select(AgentKey::Lane(1));
        assert_eq!(status_text(Some(&model)), "no metrics yet");
    }

    /// The home and temporary directories are places too, wherever the log names
    /// them; a prefix is only shortened where it starts a path.
    #[test]
    fn the_home_and_temporary_directories_are_places_in_a_log_too() {
        let home = env_path("HOME").expect("a home directory");
        let tail = format!(
            "evo-swarm: sessions in {home}/.evo/sessions/proj\n\
             evo-swarm: not a path: x{home}/x\n\
             evo-swarm: integer /2"
        );
        let shown = shorten_log(&tail, None);
        assert!(
            shown.contains("sessions in ~/.evo/sessions/proj"),
            "{shown}"
        );
        assert!(shown.contains(&format!("x{home}/x")), "{shown}");
        assert!(shown.ends_with("integer /2"), "{shown}");

        if let Some(tmp) = env_path("TMPDIR") {
            let shown = shorten_log(&format!("evo-swarm: probe in {tmp}/evo-probe\n"), None);
            assert!(shown.contains("probe in …/evo-probe"), "{shown}");
        }
    }
}
