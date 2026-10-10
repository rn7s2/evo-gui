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
//! ## What is not drawn yet
//!
//! The cursor is a block, the width of one cell, when the pane has the keyboard.
//! The mouse is not reported to the shell, and text cannot be selected.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line as Row};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, AbsoluteLength, App, Bounds, Div, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Task, Window,
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
    /// The height of one row as last drawn: what a wheel's pixels are counted
    /// against between frames.
    line: f32,
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
            line: 21.,
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
                if cursor_at == Some(column) || !blank(&cells[Column(column)]) {
                    end = column + 1;
                }
            }

            lines.push(Line::of(cells, end, cursor_at));
        }
        Screen { lines }
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

    /// A click puts the keyboard in the terminal, the way it does in a terminal
    /// window: the click is the request, and there is nowhere else to put it.
    fn clicked(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
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
                let _ = pane.update(cx, |pane, cx| pane.set_size(cols, rows, cx));
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
            .px(PAD_X)
            .py(PAD_Y)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::clicked))
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
    fn of(cells: &alacritty_terminal::grid::Row<Cell>, end: usize, cursor: Option<usize>) -> Line {
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
                .when(run.ink.papered(), |text| text.bg(paper))
                .when(run.ink.underline, |text| text.underline())
                .child(run.text.clone()),
        );
    }
    row
}
