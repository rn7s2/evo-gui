//! The shell behind the grid: the pty it runs on, the thread that drains it,
//! and the grid the bytes land in.
//!
//! Reading a pty blocks, so it happens on a thread of its own. That thread owns
//! nothing else: it parses what it read into the [`SharedTerm`] under the lock
//! the pane also takes to draw it, and then wakes the pane through a channel.
//! It never touches GPUI state — the pane is woken, and the pane does the
//! drawing on the UI thread.
//!
//! The wakeups are coalesced: the channel holds one, and a send that finds it
//! full is dropped. Nothing is lost by that, because the grid already holds
//! everything read up to that point — a queued wakeup draws the newest grid,
//! not the one that was current when it was queued.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Term, MIN_COLUMNS, MIN_SCREEN_LINES};
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{Context as _, Result};
use async_channel::Sender;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

/// How much of the shell's output is taken at a time.
const CHUNK: usize = 16 * 1024;

/// What the shell is told its terminal is. `xterm-256color` is the name every
/// program in a unix userland knows, and the one whose escape sequences the
/// grid parses.
const TERM: &str = "xterm-256color";

/// The grid, shared between the thread that feeds it and the pane that draws it.
pub(crate) type SharedTerm = Arc<Mutex<Term<PtyEvents>>>;

/// The pty's write side: keystrokes, pastes, and the grid's own answers.
pub(crate) type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

/// A grid's size in cells, as the terminal and the pty both want it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cells {
    pub cols: u16,
    pub rows: u16,
}

impl Cells {
    /// A size in cells, at least as large as a terminal is allowed to be: a
    /// pty of no columns would divide by zero inside the parser, and a grid of
    /// no lines has nowhere to put the cursor.
    pub(crate) const fn new(cols: u16, rows: u16) -> Self {
        Cells {
            cols: if cols < MIN_COLUMNS as u16 {
                MIN_COLUMNS as u16
            } else {
                cols
            },
            rows: if rows < MIN_SCREEN_LINES as u16 {
                MIN_SCREEN_LINES as u16
            } else {
                rows
            },
        }
    }
}

impl Dimensions for Cells {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Whatever the pty is asked for, when there is no shell on one: a pane whose
/// shell failed to start still has a grid, and the grid still answers programs
/// that are not there.
pub(crate) fn quiet() -> Writer {
    Arc::new(Mutex::new(Box::new(std::io::sink())))
}

/// The shell a pane starts when it is given only a directory: `$SHELL`, else the
/// shell a terminal on this system opens by default.
pub(crate) fn default_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

/// What the grid asks of the shell while its bytes are being parsed.
pub(crate) struct PtyEvents {
    writer: Writer,
}

impl PtyEvents {
    pub(crate) fn new(writer: Writer) -> Self {
        PtyEvents { writer }
    }
}

impl EventListener for PtyEvents {
    fn send_event(&self, event: Event) {
        // A program's question about its terminal (`ESC[c` and the like) is
        // answered where it was asked — on the reader thread, which is the only
        // thread that may hold the grid's lock. Everything else the parser
        // raises is about a window the terminal does not have yet: a title,
        // the bell, the clipboard. None of it is drawn, so none of it is sent.
        if let Event::PtyWrite(text) = event {
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(text.as_bytes());
            }
        }
    }
}

/// A shell running on a pseudo-terminal of its own.
pub(crate) struct Shell {
    /// The master end: the pane's handle on the terminal, and how its size is
    /// set.
    master: Box<dyn MasterPty + Send>,
    /// The write side, shared with the grid (which answers device queries) and
    /// with the pane (which types into it).
    writer: Writer,
    /// The shell itself. Held so that the pane can end it: nothing else reaps
    /// it, and a shell outliving its pane would keep the pty and the process
    /// group alive for the rest of the session.
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Shell {
    /// Start `shell` on a new pty of `size` cells, in `cwd`.
    pub(crate) fn spawn(shell: &Path, cwd: &Path, size: Cells) -> Result<Shell> {
        let pty = native_pty_system()
            .openpty(size_of(size))
            .context("no pseudo-terminal")?;

        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        command.env("TERM", TERM);
        // The app inherits the environment of whatever launched it, and a launch
        // from Terminal.app carries its identity: macOS's /etc/zshrc then runs
        // Terminal's own session save/restore in this pane ("Restored session:").
        // This pane is not Terminal.app, and says so.
        command.env("TERM_PROGRAM", "EvoDesktop");
        for variable in [
            "TERM_PROGRAM_VERSION",
            "TERM_SESSION_ID",
            "SHELL_SESSION_ID",
        ] {
            command.env_remove(variable);
        }
        let child = pty
            .slave
            .spawn_command(command)
            .with_context(|| format!("cannot start {} in {}", shell.display(), cwd.display()))?;
        // The child holds the slave end now. Keeping our copy would hold the
        // pty open past the shell's exit, and the pane would then wait for an
        // EOF that can no longer come.
        drop(pty.slave);

        let writer = Arc::new(Mutex::new(pty.master.take_writer()?));
        Ok(Shell {
            master: pty.master,
            writer,
            child,
        })
    }

    /// The write side of the pty, for whoever types into it.
    pub(crate) fn writer(&self) -> Writer {
        Arc::clone(&self.writer)
    }

    /// Tell the kernel — and through it the shell, which gets `SIGWINCH` — how
    /// big the terminal is now.
    pub(crate) fn resize(&self, size: Cells) -> Result<()> {
        self.master.resize(size_of(size))
    }

    /// Start the thread that drains the shell's output into `term`, waking
    /// `wakeup` after every read.
    ///
    /// The thread is not joined, and does not need to be: it ends when the pty
    /// goes (the pane ends the shell as it goes) or when the pane drops the
    /// receiving end of `wakeup`, whichever comes first.
    ///
    /// When the shell is gone (its end of the pty closed), `exited` is set and one
    /// last wakeup is sent — blocking for the slot, so it cannot be coalesced away
    /// — and the pane learns its shell has ended.
    pub(crate) fn drain(
        &self,
        term: SharedTerm,
        wakeup: Sender<()>,
        exited: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut reader = self.master.try_clone_reader()?;
        thread::Builder::new()
            .name("terminal-pty".to_owned())
            .spawn(move || {
                let mut processor: Processor = Processor::new();
                let mut chunk = [0u8; CHUNK];
                loop {
                    let read = match reader.read(&mut chunk) {
                        // The shell exited, or the pty went with the pane:
                        // there is nothing left to read and nobody to read it.
                        Ok(0) | Err(_) => {
                            exited.store(true, Ordering::SeqCst);
                            let _ = wakeup.send_blocking(());
                            return;
                        }
                        Ok(read) => read,
                    };
                    {
                        let mut grid = term.lock().unwrap_or_else(|gone| gone.into_inner());
                        processor.advance(&mut *grid, &chunk[..read]);
                    }
                    if wakeup.try_send(()).is_err() && wakeup.is_closed() {
                        return;
                    }
                }
            })
            .context("no thread for the shell")?;
        Ok(())
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // Closing the master would send the shell a `SIGHUP`; ending it here is
        // the same end and it also reaps the child, which nothing else will —
        // an unwaited child stays a zombie for as long as this app runs.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A grid size as the pty wants it. The pixel size is left to the kernel: only
/// a terminal that draws images from the shell's own report needs it.
fn size_of(size: Cells) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
