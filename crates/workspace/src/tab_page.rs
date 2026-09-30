//! The tab page and the screens around it: a swarm starting, one that never came up,
//! and one being stopped.
//!
//! The page is two columns (the design's `.workspace`): the swarm's agents on the
//! left, and on the right the selected agent's conversation — the header, the
//! transcript, and the composer's box at the foot of it. The split keeps the design's
//! limits (180–480 for the agent column, 420 for the conversation), and a double-click
//! puts it back where it starts.
//!
//! The parts are real crates — `agent_list`, `transcript` (`crates/widgets`' chips and
//! dots are the composer's and the list's own) — and this module is the frame they hang
//! in: the two bands, the folder line under the list, and the pages that are not the
//! page.

use std::path::Path;
use std::rc::Rc;

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
    div, px, AnyElement, App, Context, IntoElement, MouseButton, MouseDownEvent, Pixels,
    SharedString, TestSupportExt as _, Window,
};
use session::AgentKey;

use crate::tab::{Notice, NoticeTone, TabContent, TabContentEvent, TabState};
use store::app_state::{CENTER_MIN, LEFT_MAX, LEFT_MIN};
use store::design;
use widgets::paint;

/// How many characters the folder line under the agent list may take before it is
/// elided in the middle: about what fits under a 260 px column at 11 px text.
const FOLDER_LINE_CHARS: usize = 34;

/// How many characters a path may take on the boot and failure screens, where the
/// whole window is the measure.
const SCREEN_PATH_CHARS: usize = 88;

impl TabContent {
    /// The content for the tab's current state.
    pub(crate) fn render_for_state(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.state {
            TabState::Empty => self.render_empty(cx),
            TabState::Booting { folder } => self.render_booting(folder, cx),
            TabState::Running { folder } => self.render_page(folder, window, cx),
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

    /// The tab page: the swarm's lanes, and the selected agent's conversation
    /// (§7.3).
    ///
    /// Two columns with a draggable split between them, drawn by the kit's resizable
    /// panels: the agent column takes the width the window remembers, the
    /// conversation takes what is left. Every tab's page shares one
    /// [`panes::Panes`](crate::panes) state — the two columns are the same two in
    /// every tab, so a split dragged in one tab is dragged in all of them — and the
    /// window hears where the drag ended so it can write it down.
    fn render_page(
        &self,
        folder: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panes = crate::panes::fit(self.panes(), f32::from(window.bounds().size.width));
        div()
            .id("tab-page")
            .test_support()
            .size_full()
            // The design's `pointerdown` on the document, which folds an open drawer
            // on a press anywhere but the composer's box. This is the page's own
            // whole surface — the lanes, the band, the transcript, the space the box
            // sits in — so a press in any of them reaches here, and the box answers
            // for itself: it is the composer that knows where its box was painted.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    let composer = this.composer.clone();
                    composer.update(cx, |composer, cx| {
                        composer.close_drawer_at(event.position, cx)
                    });
                }),
            )
            .child(self.render_columns(folder, panes, window, cx))
            .into_any_element()
    }

    /// The two columns themselves, in the group that lets the split between them be
    /// dragged.
    ///
    /// The agent column may be dragged out to `LEFT_MAX`, but never so far that the
    /// conversation is squeezed below `CENTER_MIN`: the design's own
    /// `Math.min(LEFT_MAX, width - MAIN_MIN)`, which is what makes the split depend on
    /// the window rather than on the file.
    fn render_columns(
        &self,
        folder: &Path,
        panes: store::app_state::Panes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width = f32::from(window.bounds().size.width);
        let widest = (width - CENTER_MIN).clamp(LEFT_MIN, LEFT_MAX);
        h_resizable("tab-columns")
            .when_some(self.pane_state.clone(), |group, state| {
                group.with_state(&state)
            })
            .with_handle_appearance(self.pane_hands(cx))
            .child(
                resizable_panel()
                    .size(px(panes.left))
                    .size_range(px(LEFT_MIN)..px(widest))
                    .flex_none()
                    .child(self.render_lane_column(folder, cx)),
            )
            .child(
                resizable_panel()
                    .size_range(px(CENTER_MIN)..Pixels::MAX)
                    .child(self.render_conversation_column(window, cx)),
            )
    }

    /// The split between the two columns: the kit's own hairline and the pill it grows
    /// on hover, on press and while it is dragged — the design's divider, which is that
    /// same state machine — wrapped in the one gesture it has no room for: a
    /// double-click that puts the column back to the width it starts at.
    fn pane_hands(&self, cx: &Context<Self>) -> ResizeHandleRenderer {
        let kit = resize_handle_appearance();
        let tab = cx.entity().downgrade();
        Rc::new(move |handle, window, cx| {
            let tab = tab.clone();
            // The design's `.workspace.resizing *{cursor:col-resize}`: while the split
            // is being dragged the pointer is usually off the nine-pixel band, where
            // nothing under it would say what the drag is doing, so the drag itself
            // carries the cursor.
            if handle.state().is_active() {
                // `col-resize`, the same cursor the band itself wears while hovered.
                cx.set_active_drag_cursor_style(gpui_kit::CursorStyle::ResizeColumn, window);
            }
            let painted = kit(handle, window, cx);
            Some(
                div()
                    // Only what a double-click needs: the divider itself is the kit's,
                    // and the cursor while it is dragged is the band's own.
                    .id("pane-handle")
                    .h_full()
                    .w(px(1.))
                    .cursor_col_resize()
                    .on_double_click(move |_, _, cx| {
                        if let Some(tab) = tab.upgrade() {
                            // The window owns the width (§7.3); a tab only says what was
                            // asked of it.
                            tab.update(cx, |_, cx| cx.emit(TabContentEvent::ResetPane));
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

    /// The lane column: `agent_list`'s own band and rows, with the folder the swarm
    /// runs in pinned under them.
    ///
    /// The list takes the room; the folder line keeps its own single row at the bottom,
    /// so a path can never push the lanes around.
    fn render_lane_column(&self, folder: &Path, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = design::palette(cx.theme().mode.is_dark());
        v_flex()
            .id("agent-column")
            .test_support()
            // The panel decides how wide the column is; the column fills whatever it is
            // given, and the divider between them is the panel's own handle.
            .w_full()
            .h_full()
            .bg(paint::color(palette.sidebar))
            .child(div().flex_1().min_h_0().child(self.agents.clone()))
            .child(folder_line(folder, cx))
    }

    /// The band over the conversation (`.ws-head`): whose transcript this is, what it
    /// was told to do, and the one control the transcript needs.
    ///
    /// It is the lane column's band, at the same height and on the same surface, so the
    /// rule under the two runs straight across the page.
    fn render_agent_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = design::palette(cx.theme().mode.is_dark());
        let agent = self.selected_agent();
        let name = match agent {
            AgentKey::Coordinator => SharedString::from("main"),
            AgentKey::Lane(n) => SharedString::from(format!("lane {n}")),
        };
        let task = self.header_task(agent);
        h_flex()
            .id("transcript-header")
            .test_support()
            .w_full()
            .flex_none()
            .h(px(design::HEADER_HEIGHT))
            .items_center()
            .gap(px(8.))
            .px(px(design::INSET))
            .bg(paint::color(palette.sidebar))
            .border_b_1()
            .border_color(paint::color(palette.border))
            .text_color(paint::color(palette.fg))
            .child(
                // `.ws-head{font-size:14px}` with `.ws-agent-name{font-weight:500}`.
                div().font_medium().text_size(px(14.)).child(name),
            )
            .child(
                // One line, elided: a task is a sentence, and the transcript below is
                // where the whole of it is read.
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(paint::color(palette.muted_fg))
                    .child(task),
            )
            .when_some(self.thinking_toggle(cx), |this, toggle| this.child(toggle))
            .into_any_element()
    }

    /// What the band says the agent is doing — the design's own rule: `main` is the
    /// coordinator, and a lane says the task it was given while it works, where it is
    /// otherwise, and why it is down when it is.
    fn header_task(&self, agent: AgentKey) -> SharedString {
        let AgentKey::Lane(n) = agent else {
            return SharedString::from("coordinator");
        };
        let model = self.model();
        if let Some(reason) = model.and_then(|model| model.lane_down_reason(n)) {
            return SharedString::from(reason);
        }
        let row = model.and_then(|model| model.lane_rows().iter().find(|row| row.n == n).cloned());
        match row {
            Some(row) if row.is_busy() => row
                .task_label()
                .map(SharedString::from)
                .unwrap_or_else(|| SharedString::from(row.status.word())),
            Some(row) => SharedString::from(row.status.word()),
            // Nothing has been read about this lane yet: silence rather than a guess.
            None => SharedString::from(""),
        }
    }

    /// The quiet control the header carries while the shown agent has thinking text to
    /// reveal.
    ///
    /// It is the *view's* own state: switching agents shows each transcript the way its
    /// reader left it.
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

    /// The conversation: the band, the transcript, and the composer's box at the foot
    /// of the column (§7.3).
    ///
    /// Everything the reader works with is one column, top to bottom, in the order they
    /// are used; nothing the page is *for* lives off to the side where a window has to
    /// be wide enough to reach it. The box owns its own furniture — the todo strip, the
    /// drawers a chip folds out, the input and the chips — and the page hands it the
    /// height of the pane it has to live in.
    fn render_conversation_column(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = design::palette(cx.theme().mode.is_dark());
        let selected = self.selected_agent();
        let view = self.transcripts.get(&selected).cloned();
        // What the input may grow to half of: the conversation column, which is the
        // window under the tab strip (`AutoTextarea.tsx` measures this pane too).
        let pane = px(f32::from(window.bounds().size.height) - design::STRIP_HEIGHT);
        self.composer
            .update(cx, |composer, cx| composer.set_pane_height(pane, cx));
        v_flex()
            .id("conversation-column")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(paint::color(palette.bg))
            .child(self.render_agent_header(cx))
            .when(self.is_reconnecting(), |this| {
                this.child(reconnect_badge(cx))
            })
            // A refused op says so here, directly over the box that caused it.
            .when_some(self.notice(), |this, notice| {
                this.child(
                    div()
                        .px(px(design::INSET))
                        .pb(px(6.))
                        .child(notice_line(notice, cx)),
                )
            })
            // The view says what an agent with nothing to show means, and for which
            // agent.
            .child(div().flex_1().min_h_0().children(view))
            .child(self.composer.clone())
    }
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

/// A path as `~` where it is the home directory, and with whole leading
/// components dropped while it is still longer than the limit.
///
/// A folder line has one line to say where the swarm runs. Cutting the middle out
/// of the path (`/var/folder…-88368-screens/project`) keeps the temporary root
/// nobody asked about and swallows the names a person is looking for; dropping
/// leading components whole keeps the tail, which is the part that names the
/// place: `…/project`, or `~/…/gui-model/crates/workspace/src` for a directory
/// under the home directory, whose `~` is kept however deep it goes.
fn shorten_path(path: &str, limit: usize) -> String {
    let shortened = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match path.strip_prefix(&home) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        _ => path.to_string(),
    };
    if shortened.chars().count() <= limit {
        return shortened;
    }
    // The root is kept: `~` says the folder is in the home directory, `/` that it
    // is somewhere absolute. What goes is a stretch of directories under it, one
    // whole component at a time, longest tail first — so a name is never cut in
    // half and the tail, which names the place, is the last thing to go.
    let (root, rest) = match shortened.strip_prefix('~') {
        Some(rest) => ("~", rest),
        None => ("", shortened.as_str()),
    };
    let head = if root.is_empty() {
        "…".to_string()
    } else {
        format!("{root}/…")
    };
    for slash in rest.match_indices('/').map(|(at, _)| at) {
        let candidate = format!("{head}{}", &rest[slash..]);
        if candidate.chars().count() <= limit {
            return candidate;
        }
    }
    // Every component above the last one dropped and the name itself still too
    // long: the only thing left to take away is the middle of that one name.
    let last = match rest.rfind('/') {
        Some(at) => &rest[at..],
        None => rest,
    };
    let head_len = head.chars().count();
    format!(
        "{head}{}",
        elide_middle(last, limit.saturating_sub(head_len))
    )
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

    /// A folder line too long for its one line loses whole leading components, not
    /// the middle of a name: what is left names the place the swarm runs in, and
    /// nothing above it is worth a character.
    #[test]
    fn a_folder_line_drops_leading_components_whole() {
        let long = "/var/folders/5d/bgm58_l51j52vqz4v7jrdfch0000gn/T/swarm-client-fixture-88368-screens/project";
        let shown = shorten_path(long, FOLDER_LINE_CHARS);
        assert_eq!(
            shown, "…/project",
            "nothing above the folder it runs in fits, and the folder is what is kept"
        );

        // As many whole components as fit, and no more.
        let mixed = "/var/folders/5d/x/T/tmp-1/run/project";
        let shown = shorten_path(mixed, FOLDER_LINE_CHARS);
        assert_eq!(shown, "…/folders/5d/x/T/tmp-1/run/project");
        assert!(shown.chars().count() <= FOLDER_LINE_CHARS);

        // A repository under the home directory still shows the path to it, as far
        // as it fits.
        let home = std::env::var("HOME").expect("a home directory");
        let deep = format!("{home}/coding/evo/wt/gui-model/crates/workspace/src");
        let shown = shorten_path(&deep, FOLDER_LINE_CHARS);
        assert!(shown.starts_with("~/"), "{shown}");
        assert!(shown.ends_with("src"), "{shown}");
        assert!(shown.chars().count() <= FOLDER_LINE_CHARS);

        // One name longer than the line: the middle of it goes, both ends stay.
        let uuid = "/tmp/0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9-0a1b2c3d-4e5f-6071";
        let shown = shorten_path(uuid, 20);
        assert_eq!(shown.chars().count(), 20);
        assert!(shown.starts_with("…/0a1b"), "{shown}");
        assert!(shown.ends_with("6071"), "{shown}");
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
