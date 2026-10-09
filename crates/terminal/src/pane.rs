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
//! ## What is not drawn yet
//!
//! The cursor is a block, the width of one cell, when the pane has the keyboard.
//! Nothing scrolls the history, and the mouse is not reported to the shell: a
//! full-screen program that asked for mouse reporting hears nothing. Both are
//! the integration's next step, not a decision made here.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line as Row};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, AbsoluteLength, App, Bounds, Div, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, Render, SharedString, Task, Window,
};

use crate::keys;
use crate::palette;
use crate::pty::{default_shell, quiet, Cells, PtyEvents, SharedTerm, Shell, Writer};

/// The size a terminal starts at before it has been laid out once: what a
/// terminal has always been, and what a shell's first line is wrapped to.
const START: Cells = Cells::new(80, 24);

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
    /// A font family override: when set, the pane draws with this instead of
    /// the theme's monospace. Set from the app's "Terminal Font" setting.
    font_override: Option<SharedString>,
}

impl TerminalPane {
    /// A terminal in `working_dir`, on the user's own shell.
    ///
    /// `$SHELL` decides which shell that is — the shell a terminal window on this
    /// desktop would open — and `/bin/zsh` is what macOS puts behind it.
    pub fn new(working_dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
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

        // The parser's own configuration, its scrollback included: nothing shows
        // the history yet, but a grid that kept none loses a line outright when the
        // pane is made narrower — a re-wrapped line takes a row the screen no
        // longer has, and a grid with nowhere to put it drops the row at the top.
        let term = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &size,
            PtyEvents::new(Arc::clone(&writer)),
        )));

        // One wakeup at a time is enough for the pane to be drawn (see
        // `crate::pty`), so the channel is a slot rather than a queue.
        let (wakeup, woken) = async_channel::bounded(1);
        if let Some(shell) = &shell {
            if let Err(error) = shell.drain(SharedTerm::clone(&term), wakeup) {
                failure = Some(format!("{error:#}"));
            }
        }

        // The shell's bytes, applied to the grid on this thread, one frame per
        // wakeup. The task ends with the pane: the weak handle stops upgrading.
        let _wakeup = cx.spawn(async move |pane, cx| {
            while woken.recv().await.is_ok() {
                if pane.update(cx, |_, cx| cx.notify()).is_err() {
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
            font_override: None,
        }
    }

    /// Set the font family the terminal draws with, overriding the theme's
    /// monospace. An empty string clears the override.
    pub fn set_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        let new = if family.is_empty() {
            None
        } else {
            Some(SharedString::from(family.to_string()))
        };
        if new != self.font_override {
            self.font_override = new;
            cx.notify();
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
    fn screen(&self, window: &Window) -> Screen {
        let term = self.term.lock().unwrap_or_else(|gone| gone.into_inner());
        let grid = term.grid();
        let columns = grid.columns();
        let top = -(grid.display_offset() as i32);
        let cursor = (self.focus.is_focused(window) && term.mode().contains(TermMode::SHOW_CURSOR))
            .then_some(grid.cursor.point);

        let mut lines = Vec::with_capacity(grid.screen_lines());
        for row in 0..grid.screen_lines() {
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
        let Some(bytes) = keys::bytes(&event.keystroke, mode) else {
            return;
        };
        // A keystroke is a handful of bytes and a pty's input buffer is a good
        // deal larger, so typing does not wait on the shell reading it.
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(&bytes);
        }
        // What the terminal took is not also the window's: a bare letter bound to
        // a command would otherwise fire on every word typed into a shell.
        cx.stop_propagation();
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
        let metrics = metrics(window, theme, self.font_override.as_ref());
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
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::clicked))
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .font_family(
                        self.font_override
                            .clone()
                            .unwrap_or_else(|| theme.mono_font_family.clone()),
                    )
                    .text_size(theme.mono_font_size)
                    .line_height(metrics.line)
                    .text_color(theme.foreground)
                    .children(body),
            )
    }
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

/// Measure a cell of the grid: the given font family (or the theme's monospace
/// if `None`), at the theme's monospace size, in the window's own text style.
fn metrics(window: &Window, theme: &Theme, font_override: Option<&SharedString>) -> Metrics {
    let size = theme.mono_font_size;
    let mut style = window.text_style();
    style.font_family = font_override
        .cloned()
        .unwrap_or_else(|| theme.mono_font_family.clone());
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
