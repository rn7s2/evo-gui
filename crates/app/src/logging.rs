//! The app's own log (§2, §6): `~/.evo/desktop/app.log`, one line per event.
//!
//! **A release build writes none of it**: [`AppLog::open`] returns the disabled
//! logger there, which touches neither the directory nor the file, and nothing
//! reaches stderr either. Debug builds — `cargo run`, tests, examples, the
//! probe — behave as they always have. The gate is that one `cfg!`, so no call
//! site moves.
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

/// The one gate: debug builds write the log (and echo it to stderr), a release
/// build writes nothing at all. The log is a developer's tool — the app a
/// person runs ships no `app.log`, opens no file and owns no directory for it.
/// Keeping it here means no call site has to know, and [`AppLog`]'s API stays
/// the same in both builds.
pub const ENABLED: bool = cfg!(debug_assertions);

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
    /// False for the disabled logger [`AppLog::disabled`] makes: `line` returns
    /// at once, so nothing reaches the file or stderr.
    enabled: bool,
}

impl AppLog {
    /// `~/.evo/desktop/app.log`, created if needed.
    ///
    /// A log that cannot be opened is not fatal: the app still runs, and the
    /// reason goes to stderr once rather than stopping a launch. A release
    /// build does not get this far — it opens the disabled logger, which does
    /// not even make the directory.
    pub fn open(root: &Root) -> AppLog {
        let path = root.path().join(LOG_NAME);
        if !ENABLED {
            return AppLog::disabled(path);
        }
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
                enabled: true,
            }),
        }
    }

    /// A logger that writes nothing and echoes nothing: what [`AppLog::open`]
    /// hands back in a release build. It keeps the path, so a dialog that names
    /// the log still has one to name.
    fn disabled(path: PathBuf) -> AppLog {
        AppLog {
            inner: Arc::new(Inner {
                path,
                file: Mutex::new(None),
                enabled: false,
            }),
        }
    }

    /// The log's path, for a dialog or a bug report.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn line(&self, level: Level, message: impl AsRef<str>) {
        if !self.inner.enabled {
            return;
        }
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

    /// Tests run in a debug build, so the gate is open: `open` hands back the
    /// writing logger, and its lines land in the file.
    #[test]
    fn a_debug_build_opens_the_writing_logger() {
        let dir = std::env::temp_dir().join(format!("evo-desktop-log-on-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = AppLog::open(&Root::at(&dir));
        assert!(log.inner.enabled, "cfg!(debug_assertions) is the gate");
        log.info("written");

        assert!(log.path().is_file(), "a debug build's log is a real file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The closed gate, built by hand (a test cannot be a release binary): the
    /// disabled logger makes no file, no directory, and no stderr line.
    #[test]
    fn the_disabled_logger_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("evo-desktop-log-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("desktop").join(LOG_NAME);
        let log = AppLog::disabled(path.clone());
        log.info("silent");
        log.warn("silent");
        log.error("silent");

        assert_eq!(log.path(), path, "a dialog still has a path to name");
        assert!(!path.exists(), "no log file: {}", path.display());
        assert!(!path.parent().unwrap().exists(), "not even the directory");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
