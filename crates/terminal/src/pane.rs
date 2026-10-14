//! The pane: the grid drawn as monospace rows, the keyboard turned into bytes
//! for the shell, and the pty kept in step with the pane's size.
//!
//! ## How a frame is drawn
//!
//! The grid is the shell's own state ([`alacritty_terminal`]'s `Term`), so a
//! frame copies out of it under one lock — one [`Line`] per visible row, split
//! into runs of one colour — and draws the copy. The lock is held for the copy
//! and nothing else: no layout, no painting, and no call back into the pane
//! happens while the reader thread is kept waiting.
//!
//! ## How the pane is measured
//!
//! Nothing tells a view how big it is, so the pane measures itself out of the box
//! its content was laid out in — the shape the workspace measures its own columns
//! with — and hands the result to [`TerminalPane::set_size`]. That is the one
//! place the pty is resized, so a pane the caller sizes by hand and a pane that
//! sizes itself go through the same door.
//!
//! ## The scrollback
//!
//! The grid keeps [`crate::SCROLLBACK`] lines behind the screen. A trackpad walks
//! them to the pixel (a part-line is drawn under the top edge), a notched wheel
//! and Shift with Page Up/Down, Home and End glide there over a few frames, and
//! typing comes back to the live screen. A thumb at the right edge says where the
//! view is while it is not the live screen (`crate::scroll`). On the alternate
//! screen — a pager, an editor — the wheel is arrow keys for the program.
//!
//! ## Selection and the clipboard
//!
//! A left-button drag selects the terminal's own cells, including scrollback;
//! double- and triple-clicks expand to a word and a line. Copy asks the terminal
//! for that selection, so wrapped lines and wide cells follow terminal semantics.
//! Paste writes to the pty, using bracketed paste when the running program asks
//! for it. The mouse is not otherwise reported to the shell.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line as Row, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui_kit::base::input::{Copy, Paste, SelectAll};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, AbsoluteLength, App, Bounds, ClipboardItem, Div, EventEmitter, FocusHandle,
    Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Task, Window,
};

use crate::keys::{self, ScrollKey};
use crate::palette;
use crate::pty::{default_shell, quiet, Cells, PtyEvents, SharedTerm, Shell, Writer};
use crate::scroll::{self, Scrollback};
use crate::{SendBackTab, SendInterrupt, SendTab, TerminalFont, KEY_CONTEXT};

/// The size a terminal starts at before it has been laid out once: what a
/// terminal has always been, and what a shell's first line is wrapped to.
const START: Cells = Cells::new(80, 24);

/// The room between the pane's edges and the grid: enough that the text does
/// not sit on the divider, little enough that a narrow pane keeps its columns.
const PAD_X: Pixels = px(12.);
const PAD_Y: Pixels = px(8.);

/// What a pane tells its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The shell ended (`exit`, `^D`, or killed): there is nothing left to type
    /// into, and the owner should put the pane away.
    Exited,
}

/// The grid, drawn as monospace rows, with a shell on a pty behind it.
pub struct TerminalPane {
    /// The grid the shell writes into, shared with the thread that reads the
    /// pty. Every look at it is a lock, and no lock outlives the look.
    term: SharedTerm,
    /// The shell's own end of the pty, until it is gone.
    shell: Option<Shell>,
    /// What keystrokes and the grid's answers go to. A pane whose shell never
    /// started still has one — it leads nowhere.
    writer: Writer,
    /// The size the grid and the pty were last told, in cells: what a resize is
    /// compared against, so a frame that changed nothing changes nothing.
    size: Cells,
    /// The keyboard's handle on the pane. The pane takes the focus when it is
    /// clicked and gives it up the way any other view does; while it has it, the
    /// cursor is drawn solid.
    focus: FocusHandle,
    /// Why there is no shell, when there is none. The pane says so in place of
    /// the grid rather than showing an empty terminal and no reason.
    failure: Option<String>,
    /// Keeps the wakeup task alive. Dropping it cancels the task, and the
    /// reader thread then finds the channel closed and stops.
    _wakeup: Task<()>,
    /// Where in the scrollback the pane is looking, to the pixel.
    scroll: Scrollback,
    /// The width and height of one cell as last drawn: pointer positions and
    /// wheel pixels are counted against these between frames.
    advance: f32,
    line: f32,
    /// The grid's box in window coordinates, recorded during prepaint so mouse
    /// events can be mapped back to terminal cells.
    grid_bounds: Option<Bounds<Pixels>>,
    /// A left-button drag is extending the terminal's selection.
    selecting: bool,
}

impl EventEmitter<TerminalEvent> for TerminalPane {}

impl TerminalPane {
    /// A terminal in `working_dir`, on the user's own shell.
    ///
    /// `$SHELL` decides which shell that is — the shell a terminal window on this
    /// desktop would open — and `/bin/zsh` is what macOS puts behind it.
    pub fn new(working_dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::bind_keys(cx);
        let size = START;
        let mut failure = None;

        // The pty goes up first: everything else is wired to its write side.
        let (writer, shell) = match Shell::spawn(&default_shell(), &working_dir, size) {
            Ok(shell) => (shell.writer(), Some(shell)),
            Err(error) => {
                failure = Some(format!("{error:#}"));
                (quiet(), None)
            }
        };

        // The parser's own configuration: its scrollback is the history the
        // wheel and the paging keys walk back through (`crate::SCROLLBACK`).
        let term = Arc::new(Mutex::new(Term::new(
            scroll::config(),
            &size,
            PtyEvents::new(Arc::clone(&writer)),
        )));

        // One wakeup at a time is enough for the pane to be drawn (see
        // `crate::pty`), so the channel is a slot rather than a queue.
        let (wakeup, woken) = async_channel::bounded(1);
        let exited = Arc::new(AtomicBool::new(false));
        if let Some(shell) = &shell {
            if let Err(error) = shell.drain(SharedTerm::clone(&term), wakeup, Arc::clone(&exited)) {
                failure = Some(format!("{error:#}"));
            }
        }

        // The shell's bytes, applied to the grid on this thread, one frame per
        // wakeup. The task ends with the pane: the weak handle stops upgrading.
        let _wakeup = cx.spawn(async move |pane, cx| {
            while woken.recv().await.is_ok() {
                let gone = exited.load(Ordering::SeqCst);
                let alive = pane.update(cx, |_, cx| {
                    cx.notify();
                    if gone {
                        cx.emit(TerminalEvent::Exited);
                    }
                });
                if alive.is_err() || gone {
                    return;
                }
            }
        });

        TerminalPane {
            term,
            shell,
            writer,
            size,
            focus: cx.focus_handle(),
            failure,
            _wakeup,
            scroll: Scrollback::default(),
            advance: 8.,
            line: 21.,
            grid_bounds: None,
            selecting: false,
        }
    }

    /// Resize the terminal to `cols` cells across and `rows` down — the pane's
    /// own measurement of itself, or the caller's own sizing of it.
    ///
    /// The grid and the pty are told together, so what the shell wraps to is
    /// always what the pane draws. A size that changes nothing is not a resize:
    /// nothing is told, and no frame is asked for.
    pub fn set_size(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let size = Cells::new(cols, rows);
        if size == self.size {
            return;
        }
        self.size = size;
        {
            let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
            term.resize(size);
        }
        if let Some(shell) = &self.shell {
            // A shell that cannot be told is still a shell: it goes on printing
            // at the size it was started with, and the pane says nothing about
            // it. There is nothing useful to say — the pane cannot fix it.
            let _ = shell.resize(size);
        }
        cx.notify();
    }

    /// What the grid holds right now, copied out of the terminal under one lock.
    ///
    /// While the view sits part-way into a line, the line above the screen is
    /// copied too: it is the row the part-line shows.
    fn screen(&self, window: &Window) -> Screen {
        let term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let selection = term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&term));
        let grid = term.grid();
        let columns = grid.columns();
        let extra = usize::from(self.scroll.frac() > 0.);
        let top = -(grid.display_offset() as i32) - extra as i32;
        let cursor = (self.focus.is_focused(window) && term.mode().contains(TermMode::SHOW_CURSOR))
            .then_some(grid.cursor.point);

        let mut lines = Vec::with_capacity(grid.screen_lines() + extra);
        for row in 0..grid.screen_lines() + extra {
            let line = Row(top + row as i32);
            let cells = &grid[line];
            let cursor_at = cursor
                .filter(|point| point.line == line)
                .map(|point| point.column.0);

            // Where the row stops being worth drawing: a prompt and a command
            // leave the rest of the line blank, and a blank cell with the pane's
            // own background behind it draws nothing. The cursor is kept even on
            // a blank cell — that is where a shell with an empty line puts it.
            let mut end = 0;
            for column in 0..columns {
                let point = GridPoint::new(line, Column(column));
                if cursor_at == Some(column)
                    || selection.is_some_and(|range| range.contains(point))
                    || !blank(&cells[Column(column)])
                {
                    end = column + 1;
                }
            }

            lines.push(Line::of(cells, line, end, cursor_at, selection));
        }
        Screen { lines }
    }

    /// The cell and half-cell under a window position, using the geometry from
    /// the last frame and the grid's current scrollback position.
    fn point_at(
        &self,
        term: &Term<PtyEvents>,
        position: Point<Pixels>,
    ) -> Option<(GridPoint, Side)> {
        let bounds = self.grid_bounds?;
        Some(point_at(
            position,
            bounds,
            self.advance,
            self.line,
            self.scroll.frac(),
            term.grid().display_offset(),
            term.grid().columns(),
            term.grid().screen_lines(),
        ))
    }

    /// Start a simple, semantic (double-click), or whole-line (triple-click)
    /// terminal selection. Shift-click extends the selection already present.
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let Some((point, side)) = self.point_at(&term, event.position) else {
            return;
        };
        if event.modifiers.shift {
            match term.selection.as_mut() {
                Some(selection) => selection.update(point, side),
                None => term.selection = Some(Selection::new(SelectionType::Simple, point, side)),
            }
        } else {
            let kind = match event.click_count {
                1 => SelectionType::Simple,
                2 => SelectionType::Semantic,
                _ => SelectionType::Lines,
            };
            term.selection = Some(Selection::new(kind, point, side));
        }
        drop(term);
        self.selecting = true;
        cx.stop_propagation();
        cx.notify();
    }

    /// Extend a selection while the left button is down.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let Some((point, side)) = self.point_at(&term, event.position) else {
            return;
        };
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
        drop(term);
        cx.stop_propagation();
        cx.notify();
    }

    /// Finish a drag, including one released just outside the pane.
    fn mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        if let Some((point, side)) = self.point_at(&term, event.position) {
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
        }
        drop(term);
        self.selecting = false;
        cx.stop_propagation();
        cx.notify();
    }

    /// Copy the terminal's own selection without trimming it: leading/trailing
    /// spaces and line breaks are part of what was selected.
    fn copy_selection(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .term
            .lock()
            .unwrap_or_else(|gone| gone.into_inner())
            .selection_to_string();
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.stop_propagation();
    }

    /// Paste clipboard text into the pty, honoring the mode requested by the
    /// foreground program.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            cx.stop_propagation();
            return;
        };
        let bracketed = self
            .term
            .lock()
            .unwrap_or_else(|gone| gone.into_inner())
            .mode()
            .contains(TermMode::BRACKETED_PASTE);
        self.send(&paste_bytes(&text, bracketed), cx);
    }

    /// Select the whole retained grid: scrollback and the live screen.
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let grid = term.grid();
        let start = GridPoint::new(grid.topmost_line(), Column(0));
        let end = GridPoint::new(grid.bottommost_line(), grid.last_column());
        let mut selection = Selection::new(SelectionType::Lines, start, Side::Left);
        selection.update(end, Side::Right);
        term.selection = Some(selection);
        drop(term);
        cx.stop_propagation();
        cx.notify();
    }

    /// Clear a visible selection when input returns to the shell.
    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        if term.selection.take().is_some() {
            drop(term);
            cx.notify();
        }
    }

    /// The keyboard is the shell's: what was typed goes to the pty in the shape
    /// the grid's current mode asks for.
    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let mode = *self
            .term
            .lock()
            .unwrap_or_else(|gone| gone.into_inner())
            .mode();
        // The paging keys walk the history — except on the alternate screen,
        // which has none, where they are the program's.
        if !mode.contains(TermMode::ALT_SCREEN) {
            if let Some(key) = keys::scroll(&event.keystroke) {
                self.page(key, cx);
                cx.stop_propagation();
                return;
            }
        }
        let Some(bytes) = keys::bytes(&event.keystroke, mode) else {
            return;
        };
        self.clear_selection(cx);
        self.snap_to_bottom(cx);
        // A keystroke is a handful of bytes and a pty's input buffer is a good
        // deal larger, so typing does not wait on the shell reading it.
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(&bytes);
        }
        // What the terminal took is not also the window's: a bare letter bound to
        // a command would otherwise fire on every word typed into a shell.
        cx.stop_propagation();
    }

    /// Bytes for the shell, from an action a binding took before the key reached
    /// [`Self::key_down`] — Tab and Ctrl-C, which the window would otherwise
    /// spend on something else.
    fn send(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.clear_selection(cx);
        self.snap_to_bottom(cx);
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
        }
        cx.stop_propagation();
    }

    /// Typing goes to the live screen: a pane scrolled back comes down to where
    /// the text is going, the way every terminal does.
    fn snap_to_bottom(&mut self, cx: &mut Context<Self>) {
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        if self.scroll.is_scrolled(&term) || self.scroll.is_animating() {
            self.scroll.snap_to_bottom(&mut term);
            cx.notify();
        }
    }

    /// A paging key: a screenful (less a line, so a line is seen twice and the
    /// eye keeps its place), or all the way to either end — eased, not jumped.
    fn page(&mut self, key: ScrollKey, cx: &mut Context<Self>) {
        let line = self.line;
        let term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let page = term.grid().screen_lines().saturating_sub(1).max(1) as f32 * line;
        match key {
            ScrollKey::PageUp => self.scroll.glide_by(&term, line, page),
            ScrollKey::PageDown => self.scroll.glide_by(&term, line, -page),
            ScrollKey::Top => self.scroll.glide_to(&term, line, f32::MAX),
            ScrollKey::Bottom => self.scroll.glide_to(&term, line, 0.),
        }
        cx.notify();
    }

    /// The wheel and the trackpad. On the main screen they walk the history —
    /// a trackpad's pixels as they come, a notched wheel's lines eased. On the
    /// alternate screen, which has no history, they are arrow keys for the
    /// program that took the screen, if it asked for them.
    fn wheel(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let line = self.line;
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let mode = *term.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            drop(term);
            if !mode.contains(TermMode::ALTERNATE_SCROLL) {
                return;
            }
            let lines = event.delta.pixel_delta(px(line)).y / px(line);
            let count = self.scroll.alternate_lines(lines);
            if count != 0 {
                let key = keys::arrow_key(count > 0, mode);
                let bytes = key.repeat(count.unsigned_abs() as usize);
                if let Ok(mut writer) = self.writer.lock() {
                    let _ = writer.write_all(&bytes);
                }
            }
            cx.stop_propagation();
            return;
        }
        match event.delta {
            ScrollDelta::Pixels(delta) => {
                self.scroll.by_pixels(&mut term, line, f32::from(delta.y))
            }
            ScrollDelta::Lines(delta) => self.scroll.glide_by(&term, line, delta.y * line),
        }
        drop(term);
        cx.stop_propagation();
        cx.notify();
    }

    /// The position as this frame draws it: an animated scroll steps once, and
    /// asks for the next frame while it is still travelling. Returns the
    /// part-line, and the scrollbar's place if the pane is scrolled back.
    fn scroll_frame(&mut self, window: &mut Window, line: f32) -> (f32, Option<Thumb>) {
        let mut term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        self.scroll.settle(&term, line);
        if self.scroll.step(&mut term, line, Instant::now()) {
            window.request_animation_frame();
        }
        let thumb = self.scroll.is_scrolled(&term).then(|| {
            let screen = term.grid().screen_lines() as f32 * line;
            let history = Scrollback::limit(&term, line);
            let total = (screen + history).max(1.);
            let back = self.scroll.position(&term, line);
            Thumb {
                top: (history - back) / total,
                height: screen / total,
            }
        });
        (self.scroll.frac(), thumb)
    }

    /// The rows of the grid, or why there are none.
    fn body(&self, window: &Window, theme: &Theme, metrics: Metrics) -> Vec<Div> {
        let Some(reason) = &self.failure else {
            return self
                .screen(window)
                .lines
                .iter()
                .map(|line| row(line, theme, metrics))
                .collect();
        };
        vec![div()
            .flex_none()
            .text_color(theme.danger)
            .child(reason.clone())]
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (family, font_size) = match cx.try_global::<TerminalFont>() {
            Some(font) if !font.family.trim().is_empty() => (font.family.clone(), px(font.size)),
            Some(font) => (theme.mono_font_family.clone(), px(font.size)),
            None => (theme.mono_font_family.clone(), theme.mono_font_size),
        };
        let metrics = metrics(window, family.clone(), font_size);
        self.advance = f32::from(metrics.advance);
        self.line = f32::from(metrics.line);
        let (frac, thumb) = self.scroll_frame(window, self.line);
        let theme = cx.theme();
        let body = self.body(window, theme, metrics);

        // The grid is as many cells as fit in the box the pane was given: the
        // measure runs here, during prepaint, and hands the answer to `set_size`.
        let measure = {
            let pane = cx.entity().downgrade();
            move |painted: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
                let Some(bounds) = painted.first() else {
                    return;
                };
                let cols = (bounds.size.width / metrics.advance).max(0.) as u16;
                let rows = (bounds.size.height / metrics.line).max(0.) as u16;
                // The pane is borrowed rather than updated in place: this runs
                // during a frame's prepaint, and the pane owns the grid the
                // frame is being drawn from.
                let _ = pane.update(cx, |pane, cx| {
                    pane.grid_bounds = Some(*bounds);
                    pane.set_size(cols, rows, cx);
                });
            }
        };

        div()
            // Measured where the pane is laid out: the box the content was given
            // is the room the grid has, and the grid is as many cells as fit in
            // it. The listener has to be attached before the id, which is what
            // makes the element stateful.
            .on_children_prepainted(measure)
            .id("terminal-pane")
            .size_full()
            .bg(theme.background)
            .cursor_text()
            .track_focus(&self.focus)
            // The hook a key binding needs to say "in a terminal, this key is
            // the shell's" — and, for the chords that must stay the window's,
            // "except this one".
            .key_context(KEY_CONTEXT)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|pane, _: &SendTab, _, cx| pane.send(b"\t", cx)))
            .on_action(cx.listener(|pane, _: &SendBackTab, _, cx| pane.send(b"\x1b[Z", cx)))
            .on_action(cx.listener(|pane, _: &SendInterrupt, _, cx| pane.send(b"\x03", cx)))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::select_all))
            .px(PAD_X)
            .py(PAD_Y)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::wheel))
            .child(
                div()
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .font_family(family)
                    .text_size(font_size)
                    .line_height(metrics.line)
                    .text_color(theme.foreground)
                    .child(
                        // The rows, shifted by the part-line: with one extra row
                        // above the screen, a view `frac` pixels into that row puts
                        // it `line - frac` above the top edge.
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(if frac > 0. { frac - self.line } else { 0. }))
                            .flex()
                            .flex_col()
                            .children(body),
                    )
                    .when_some(thumb, |pane, thumb| {
                        // Where the screen sits in the whole history, while the
                        // pane is looking back: the live screen has no bar.
                        pane.child(
                            div()
                                .absolute()
                                .right_0()
                                .w(px(4.))
                                .top(relative(thumb.top))
                                .h(relative(thumb.height))
                                .min_h(px(16.))
                                .rounded(px(2.))
                                .bg(theme.muted_foreground.opacity(0.45)),
                        )
                    }),
            )
    }
}

/// Map a pointer position to a terminal cell and the nearest side of that cell.
/// Positions in the pane's padding are clamped to the nearest grid edge.
#[allow(clippy::too_many_arguments)]
fn point_at(
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    advance: f32,
    line_height: f32,
    fractional_scroll: f32,
    display_offset: usize,
    columns: usize,
    screen_lines: usize,
) -> (GridPoint, Side) {
    let advance = advance.max(f32::EPSILON);
    let line_height = line_height.max(f32::EPSILON);
    let columns = columns.max(1);
    let screen_lines = screen_lines.max(1);

    let x = f32::from(position.x - bounds.origin.x);
    let column = (x / advance).floor().max(0.) as usize;
    let column = column.min(columns - 1);
    let side = if x <= 0. {
        Side::Left
    } else if x >= columns as f32 * advance || x - column as f32 * advance >= advance / 2. {
        Side::Right
    } else {
        Side::Left
    };

    let extra = i32::from(fractional_scroll > 0.);
    let row_top = if extra == 1 {
        fractional_scroll - line_height
    } else {
        0.
    };
    let y = f32::from(position.y - bounds.origin.y) - row_top;
    let row = (y / line_height).floor().max(0.) as usize;
    let row = row.min(screen_lines + extra as usize - 1);
    let top = -(display_offset as i32) - extra;

    (GridPoint::new(Row(top + row as i32), Column(column)), side)
}

/// Turn clipboard text into exactly what a terminal writes to its pty.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let text = text.replace('\x1b', "");
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// The scrollbar's thumb, as fractions of the pane's height.
#[derive(Clone, Copy, Debug)]
struct Thumb {
    top: f32,
    height: f32,
}

/// What the grid holds, as much of it as the pane draws.
struct Screen {
    lines: Vec<Line>,
}

/// One visible row, as the runs of colour its cells fall into.
struct Line {
    runs: Vec<Run>,
}

impl Line {
    /// The row `cells[..end]` is drawn from, with the block cursor swapped in
    /// where `cursor` says, if it is on this row.
    fn of(
        cells: &alacritty_terminal::grid::Row<Cell>,
        line: Row,
        end: usize,
        cursor: Option<usize>,
        selection: Option<SelectionRange>,
    ) -> Line {
        let mut runs: Vec<Run> = Vec::new();
        for column in 0..end {
            let cell = &cells[Column(column)];
            // A wide character's second cell is a spacer: the glyph itself
            // covers both, so the spacer draws nothing.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let mut ink = Ink::of(cell);
            if cursor == Some(column) {
                ink.reverse();
            }
            ink.selected = selection.is_some_and(|range| {
                let point = GridPoint::new(line, Column(column));
                range.contains(point)
                    || (cell.flags.contains(Flags::WIDE_CHAR)
                        && range.contains(GridPoint::new(line, Column(column + 1))))
            });
            let mut text = String::new();
            text.push(if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            });
            if let Some(combining) = cell.zerowidth() {
                text.extend(combining);
            }
            match runs.last_mut() {
                Some(run) if run.ink == ink => run.text.push_str(&text),
                _ => runs.push(Run { ink, text }),
            }
        }
        Line { runs }
    }
}

/// Cells in one colour, as one string to draw.
struct Run {
    ink: Ink,
    text: String,
}

/// The colours and rules one run of cells share — the whole of what makes two
/// cells draw alike, so runs break exactly where the drawing changes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ink {
    fg: Color,
    bg: Color,
    /// A heavier stroke: drawn as the bright half of the palette (see
    /// [`crate::palette`]).
    bold: bool,
    underline: bool,
    selected: bool,
}

impl Ink {
    fn of(cell: &Cell) -> Ink {
        let (mut fg, mut bg) = (cell.fg, cell.bg);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        Ink {
            fg,
            bg,
            bold: cell.flags.intersects(Flags::BOLD | Flags::DIM_BOLD),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            selected: false,
        }
    }

    /// The same cell with its colours the other way round: the block cursor.
    fn reverse(&mut self) {
        std::mem::swap(&mut self.fg, &mut self.bg);
    }

    /// Whether this run has a colour of its own behind it, or the pane's own
    /// background shows through. Painting the pane's background over itself is
    /// what a run of ordinary text would otherwise do on every frame.
    fn papered(&self) -> bool {
        self.bg != Color::Named(NamedColor::Background)
    }
}

/// A cell that draws nothing: a space, on the pane's own background, with no
/// rule under it.
fn blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell
            .flags
            .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
}

/// One cell of the grid, in pixels.
#[derive(Clone, Copy)]
struct Metrics {
    /// How far the monospace face advances over one character.
    advance: Pixels,
    /// The line box one row of it is drawn in.
    line: Pixels,
}

/// Measure a cell of the grid: `family` at `size`, in the window's own text
/// style.
///
/// The line is the *line box* that style gives this text — the same number the
/// text is drawn in — and not the face's own ascent and descent: the pane tells
/// the shell how many rows fit, so its rows have to be the rows on screen.
fn metrics(window: &Window, family: SharedString, size: Pixels) -> Metrics {
    let mut style = window.text_style();
    style.font_family = family;
    style.font_size = AbsoluteLength::Pixels(size);
    let line = style.line_height_in_pixels(window.rem_size());
    let text = window.text_system();
    let font = text.resolve_font(&style.to_run(1).font);
    Metrics {
        // Every character of a monospace face advances the same, so any
        // character answers for all of them.
        advance: text
            .advance(font, size, '0')
            .map(|glyph| glyph.width)
            // A face with no answer for a digit is a face that cannot draw the
            // grid; the shell's own assumption (0.6 em) is the least wrong
            // guess, and the pane is redrawn with the right one as soon as one
            // answers.
            .unwrap_or(size * 0.6),
        line,
    }
}

/// One row, as a line of the grid: laid out by its cells, not by its text — the
/// face is monospace, so the runs of a row simply follow one another, and the
/// row's height is the line height the pty was told.
fn row(line: &Line, theme: &Theme, metrics: Metrics) -> Div {
    let mut row = div().flex_none().flex().whitespace_nowrap().h(metrics.line);
    for run in &line.runs {
        let ink = palette::ink(run.ink.fg, run.ink.bold, theme);
        let paper = palette::ink(run.ink.bg, false, theme);
        row = row.child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(ink)
                .when(run.ink.selected, |text| text.bg(theme.selection))
                .when(run.ink.papered() && !run.ink.selected, |text| {
                    text.bg(paper)
                })
                .when(run.ink.underline, |text| text.underline())
                .child(run.text.clone()),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{quiet, Cells};
    use alacritty_terminal::grid::Row as GridRow;
    use alacritty_terminal::vte::ansi::Processor;
    use gpui_kit::{point, size};

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(640.), px(480.)))
    }

    #[test]
    fn pointer_positions_map_to_cells_and_sides() {
        let (cell, side) = point_at(point(px(27.), px(61.)), bounds(), 8., 20., 0., 3, 80, 24);
        assert_eq!(cell, GridPoint::new(Row(-1), Column(2)));
        assert_eq!(side, Side::Left);

        let (cell, side) = point_at(point(px(33.), px(61.)), bounds(), 8., 20., 0., 3, 80, 24);
        assert_eq!(cell, GridPoint::new(Row(-1), Column(2)));
        assert_eq!(side, Side::Right);
    }

    #[test]
    fn fractional_scrollback_includes_the_partly_visible_row() {
        let (part, _) = point_at(point(px(10.), px(20.)), bounds(), 8., 20., 5., 3, 80, 24);
        let (whole, _) = point_at(point(px(10.), px(25.)), bounds(), 8., 20., 5., 3, 80, 24);
        assert_eq!(part.line, Row(-4));
        assert_eq!(whole.line, Row(-3));
    }

    #[test]
    fn pointer_positions_outside_the_grid_clamp_to_its_edges() {
        let (top_left, left) = point_at(point(px(0.), px(0.)), bounds(), 8., 20., 0., 0, 80, 24);
        let (bottom_right, right) =
            point_at(point(px(900.), px(900.)), bounds(), 8., 20., 0., 0, 80, 24);
        assert_eq!(top_left, GridPoint::new(Row(0), Column(0)));
        assert_eq!(left, Side::Left);
        assert_eq!(bottom_right, GridPoint::new(Row(23), Column(79)));
        assert_eq!(right, Side::Right);
    }

    #[test]
    fn plain_paste_turns_line_feeds_into_terminal_returns() {
        assert_eq!(paste_bytes("one\ntwo\r\nthree", false), b"one\rtwo\rthree");
    }

    #[test]
    fn bracketed_paste_wraps_text_and_cannot_inject_an_escape_sequence() {
        assert_eq!(
            paste_bytes("one\n\x1b[201~two", true),
            b"\x1b[200~one\n[201~two\x1b[201~"
        );
    }

    #[test]
    fn selection_paints_blank_cells_and_the_wide_character_they_belong_to() {
        let mut cells = GridRow::<Cell>::new(4);
        cells[Column(0)].c = '界';
        cells[Column(0)].flags.insert(Flags::WIDE_CHAR);
        cells[Column(1)].flags.insert(Flags::WIDE_CHAR_SPACER);
        let selection = SelectionRange::new(
            GridPoint::new(Row(0), Column(1)),
            GridPoint::new(Row(0), Column(3)),
            false,
        );

        let line = Line::of(&cells, Row(0), 4, None, Some(selection));
        assert!(line.runs[0].ink.selected);
        assert_eq!(line.runs[0].text, "界  ");
    }

    #[test]
    fn copied_terminal_selection_joins_wrapped_rows() {
        let mut term = Term::new(scroll::config(), &Cells::new(5, 3), PtyEvents::new(quiet()));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"abcdef");
        let mut selection = Selection::new(
            SelectionType::Simple,
            GridPoint::new(Row(0), Column(0)),
            Side::Left,
        );
        selection.update(GridPoint::new(Row(1), Column(0)), Side::Right);
        term.selection = Some(selection);

        assert_eq!(term.selection_to_string().as_deref(), Some("abcdef"));
    }
}
