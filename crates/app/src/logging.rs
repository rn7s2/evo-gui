//! The app's own log (§2, §6): `~/.evo/desktop/app.log`, one line per event.
//!
//! Lines start with an RFC 3339 **UTC** timestamp (`2026-09-29T09:09:56Z`), so a
//! log read next to a journal never depends on the machine's timezone.
//!
//! A token is never logged. Nothing here can log one — the app never holds a
//! tab's token (the engine reads it from the server's ready file, and the port
//! and token are the engine's own business), and `swarm_client::Token`'s
//! `Debug`/`Display` are both redacted — but the rule is repeated here because
//! this is the file someone would reach for.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use store::paths::Root;
use store::time;

/// The log file's name inside `~/.evo/desktop`.
pub const LOG_NAME: &str = "app.log";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// The app log, shared by the UI thread and every background worker.
#[derive(Clone)]
pub struct AppLog {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl AppLog {
    /// `~/.evo/desktop/app.log`, created if needed.
    ///
    /// A log that cannot be opened is not fatal: the app still runs, and the
    /// reason goes to stderr once rather than stopping a launch.
    pub fn open(root: &Root) -> AppLog {
        let path = root.path().join(LOG_NAME);
        // The very first launch has no `~/.evo/desktop` yet, and the log is
        // opened before anything else creates it.
        if let Some(parent) = path.parent() {
            let _ = store::paths::create_dir_private(parent);
        }
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Some(file),
            Err(error) => {
                eprintln!("evo-desktop: cannot open {}: {error}", path.display());
                None
            }
        };
        AppLog {
            inner: Arc::new(Inner {
                path,
                file: Mutex::new(file),
            }),
        }
    }

    /// The log's path, for a dialog or a bug report.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn line(&self, level: Level, message: impl AsRef<str>) {
        let message = message.as_ref();
        let text = format!(
            "{} {:<5} {}\n",
            time::now_rfc3339(),
            level.as_str(),
            message
        );
        // stderr too: a developer running the binary wants to see it live.
        eprint!("{text}");
        if let Ok(mut guard) = self.inner.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(text.as_bytes());
                let _ = file.flush();
            }
        }
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.line(Level::Info, message);
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.line(Level::Warn, message);
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.line(Level::Error, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_utc_and_names_its_level() {
        let dir = std::env::temp_dir().join(format!("evo-desktop-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = Root::at(&dir);
        let log = AppLog::open(&root);
        log.info("hello");
        log.error("bad");

        let text = std::fs::read_to_string(log.path()).expect("the log was written");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        for line in &lines {
            let stamp = line.split(' ').next().unwrap();
            assert!(stamp.ends_with('Z'), "a UTC timestamp, not local: {line}");
            assert!(stamp.len() == 20, "{line}");
        }
        assert!(lines[0].contains("info"), "{text}");
        assert!(lines[1].contains("error"), "{text}");
        assert!(lines[0].contains("hello"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
