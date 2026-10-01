//! Syntax checking for the Settings raw editors: one SBCL, reading the text.
//!
//! A person editing `init.lisp`, `swarm.lisp`, `memory.sexp` or `lore.sexp` by
//! hand gets a typo caught before it is written: the write is refused unless the
//! text reads as Lisp. The check runs a real SBCL — the reader evo itself is
//! built on, so it agrees with what a boot will accept — and reads the text
//! **without ever evaluating it**.
//!
//! ```text
//! sbcl --noinform --no-userinit --no-sysinit --non-interactive
//!      --disable-debugger --eval <CHECKER>
//!      <the text on stdin>
//! ```
//!
//! Every flag is load-bearing: `--no-userinit --no-sysinit` keep SBCL from
//! evaluating `~/.sbclrc` first (`--script` implies both, `--eval` does not);
//! `--noinform` keeps the banner off stdout, which is `/dev/null` for the whole
//! call because the checker never writes there; `--non-interactive
//! --disable-debugger` make a condition that escapes the checker end the process
//! rather than drop into a debugger; and `--eval <CHECKER>` keeps the checker a
//! fixed string compiled into this binary — the text under check never reaches
//! argv or a temporary file, it goes in on stdin and is only ever read.
//!
//! The child is started in a process group of its own, and a check that runs out
//! of time kills that group rather than the one process: an SBCL behind a wrapper
//! script leaves children that inherit the pipes, and one of those alive would
//! hold the call past its deadline.
//!
//! Inside the checker `*read-eval*` is `nil`, so `#.` (sharpsign-dot, which
//! evaluates its form **at read time**) is refused rather than a way to run code,
//! and `*read-suppress*` is `nil` as well: bound to `t` the SBCL reader accepts
//! text it otherwise rejects — `(a . b c)`, `(a .)`, `#<unreadable>`, `#2r9`,
//! `#1=#1#`, `#C(1 2 3)` all read as valid — which is exactly the failure this
//! gate exists to prevent. The package problem `*read-suppress*` looked like the
//! answer to (`evo:register-model` in a bare SBCL) is solved the other way: an
//! unknown package raises `package-error`, and the checker invokes the reader's
//! own `continue` restart, which reads that one symbol without a package and
//! carries on. Structure is still validated in full while `evo:foo` is not a
//! false positive.
//!
//! Two read-time requirements of a *loaded* image are refused with it, because
//! the checker is a bare SBCL with nobody's extensions loaded: `#S(struct …)`
//! needs its type to exist and `#A(…)` needs a type specifier that resolves.
//! Both are vanishingly rare in a config file, and being wrong in this direction
//! only costs a person a save. Everything else evo's own reader accepts — every
//! package prefix, quote and dispatch macro, dotted pairs, vectors, bit vectors,
//! pathnames, character names, `|escaped symbols|` — reads here.
//!
//! A missing SBCL is not worked around with a weaker parser: [`Checker::discover`]
//! fails, saving is blocked, and [`CheckError::NoChecker`] says what to install.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The environment variable that names the SBCL to use, like `EVO_AGENT_BIN`
/// names the agent. An empty value is not an override.
pub const SBCL_BIN_ENV: &str = "EVO_SBCL_BIN";

/// Where an SBCL usually is when it is not on `PATH`: Homebrew on Apple silicon,
/// Homebrew on Intel / a manual install, MacPorts, and the system directory.
const KNOWN_DIRS: [&str; 4] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/usr/bin",
];

/// How long the check may take before the process is killed. A one-shot SBCL
/// reads the whole text in about 20 ms; this is a hang guard, not a budget.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The most of the checker's stderr we keep, as in [`crate::cli`]: enough to name
/// the mistake, not enough to fill a log.
const STDERR_CAP: usize = 4096;

/// The prefix the checker puts on a reader error, so a failure to read is told
/// apart from a failure to run.
const SYNTAX_PREFIX: &str = "SYNTAX: ";

/// The checker itself, one form, read and evaluated by SBCL from `--eval`.
///
/// It writes nothing to stdout, and on an unreadable text it writes one
/// `SYNTAX: …` line to stderr and exits 1. `package-error` is a portable
/// condition class (`SB-INT:SIMPLE-READER-PACKAGE-ERROR` is a subtype of it) and
/// its `continue` restart is the reader's own "read this symbol without a
/// package" answer.
pub const CHECKER: &str = r#"(progn
  (handler-case
      (handler-bind
          ((package-error
             (lambda (e)
               (declare (ignore e))
               (let ((r (find-restart 'continue)))
                 (when r (invoke-restart r))))))
        (let ((*read-eval* nil)
              (*read-suppress* nil)
              (*package* (find-package :cl-user))
              ;; Uninterned, because `:eof` is an ordinary keyword a file can
              ;; contain: reading one would end the loop and leave the rest of
              ;; the file unread, which is a false pass. No form can be `eq` to
              ;; something with no home package.
              (eof '#:eof))
          (loop for form = (read *standard-input* nil eof)
                until (eq form eof))))
    (error (e)
      (format *error-output* "SYNTAX: ~a~%" e)
      (sb-ext:exit :code 1)))
  (sb-ext:exit :code 0))
"#;

/// Why a text could not be checked.
#[derive(Debug)]
pub enum CheckError {
    /// No SBCL is installed and none is configured. Nothing here replaces it:
    /// saving stays blocked until there is one.
    NoChecker,
    /// The SBCL at this path could not be started.
    CannotRun { bin: PathBuf, source: io::Error },
    /// It read the text and refused it — the reader's own words.
    Syntax(String),
    /// It did not finish in the time allowed, and was killed.
    Timeout { seconds: u64 },
    /// It failed for a reason that is not a reader error (a crash, a broken
    /// SBCL), so the text is unverified and must not be written either.
    Failed { code: Option<i32>, message: String },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::NoChecker => write!(
                f,
                "no SBCL found to check the file — install SBCL (for example `brew install sbcl`) \
                 or set {SBCL_BIN_ENV}; saving is blocked until then"
            ),
            CheckError::CannotRun { bin, source } => {
                write!(f, "could not run the SBCL at {}: {source}", bin.display())
            }
            CheckError::Syntax(message) => f.write_str(message),
            CheckError::Timeout { seconds } => {
                write!(f, "the SBCL check did not finish within {seconds}s")
            }
            CheckError::Failed { code, message } => match code {
                Some(code) => write!(f, "the SBCL check failed (exit {code}): {message}"),
                None => write!(f, "the SBCL check failed: {message}"),
            },
        }
    }
}

impl std::error::Error for CheckError {}

/// The one SBCL this app will check with.
///
/// Blocking: [`Checker::check`] runs a process, so the caller runs it off the
/// thread that draws.
#[derive(Clone, Debug)]
pub struct Checker {
    bin: PathBuf,
    timeout: Duration,
}

impl Checker {
    /// The SBCL found by [`find`], or [`CheckError::NoChecker`].
    pub fn discover() -> Result<Checker, CheckError> {
        find().map(Checker::at).ok_or(CheckError::NoChecker)
    }

    /// This particular binary — `app.json`'s value, or a test's own stand-in.
    pub fn at(bin: impl Into<PathBuf>) -> Checker {
        Checker {
            bin: bin.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// How long a check may take. For tests, and for a machine where SBCL is
    /// slow to start.
    pub fn with_timeout(mut self, timeout: Duration) -> Checker {
        self.timeout = timeout;
        self
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Read `text` as Lisp, without evaluating it. `Ok(())` is the only answer
    /// that lets a save go ahead.
    ///
    /// Bounded: [`Checker::timeout`] covers the whole exchange — the text going
    /// in, the answer coming back, and the process finishing — and what has not
    /// finished when it runs out is killed, group and all. `Ok` also means the
    /// checker read every byte it was given: a process that exits happily having
    /// ignored its stdin is not a pass.
    pub fn check(&self, text: &str) -> Result<(), CheckError> {
        let mut command = Command::new(&self.bin);
        command
            .args([
                "--noinform",
                "--no-userinit",
                "--no-sysinit",
                "--non-interactive",
                "--disable-debugger",
                "--eval",
                CHECKER,
            ])
            .stdin(Stdio::piped())
            // The checker says nothing on stdout, so it goes nowhere: one pipe
            // fewer to drain, and nothing it could say can be held in memory.
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // Its own process group, so [`kill_group`] can take a wrapper's children
        // with it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| CheckError::CannotRun {
            bin: self.bin.clone(),
            source,
        })?;

        let pid = child.id();
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        // One supervisor owns every blocking wait — the text going in, the
        // answer, the child's own exit — and sends one complete result, so the
        // deadline below is over all of them and not just the first to finish.
        // Nothing on the caller's path waits for the child.
        let bytes = text.as_bytes().to_vec();
        let (tx, rx) = mpsc::channel();
        let supervisor = std::thread::spawn(move || {
            // The text goes in on its own thread and the pipe closes behind it:
            // the checker stops at EOF. Its own thread, because a text larger
            // than a pipe buffer would otherwise block before the child had read
            // a byte of it.
            let writer = std::thread::spawn(move || {
                let written = stdin.write_all(&bytes).and_then(|()| stdin.flush()).is_ok();
                drop(stdin);
                written
            });

            // The answer is read as it arrives and capped as it is read: the
            // child is never blocked by a full pipe, and this process never
            // holds more than the tail it could show.
            let err = drain_tail(&mut stderr, STDERR_CAP);
            let status = child.wait();
            let written = writer.join().unwrap_or(false);
            let _ = tx.send((err, status, written));
        });

        let (err, status, written) = match rx.recv_timeout(self.timeout) {
            Ok(answer) => answer,
            Err(_) => {
                // Out of time. The whole group goes, so nothing a wrapper script
                // left behind holds the pipes open — and the supervisor is
                // dropped rather than joined: it reaps the child and ends by
                // itself once the kill lands.
                kill_group(pid);
                drop(supervisor);
                return Err(CheckError::Timeout {
                    seconds: self.timeout.as_secs(),
                });
            }
        };

        let stderr = tail(&err);
        match status {
            // It read the text and said nothing: readable.
            Ok(status) if status.success() && written => Ok(()),
            // It exited happily without reading what it was given, so nothing
            // was checked — a check that never happened is not one that passed.
            Ok(status) if status.success() => Err(CheckError::Failed {
                code: None,
                message: "the checker did not read the text it was given".to_owned(),
            }),
            Ok(status) => Err(classify(status.code(), &stderr)),
            Err(source) => Err(CheckError::Failed {
                code: None,
                message: source.to_string(),
            }),
        }
    }
}

/// Kill every process in the group `pid` leads, itself included.
///
/// The child is spawned as its own group leader (`process_group(0)`), so a
/// negative pid reaches it and the children a wrapper script left behind — the
/// ones that would hold its pipes open. `kill` is declared here rather than
/// taking a `libc` dependency for one call; `SIGKILL` is 9 wherever this runs.
#[cfg(unix)]
fn kill_group(pid: u32) {
    const SIGKILL: i32 = 9;
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: a signal to a process group this process just created; the pid is
    // never this process's own, and the return value is not used.
    unsafe {
        kill(-(pid as i32), SIGKILL);
    }
}

/// Nothing to signal by pid where there are no signals (the like of
/// `config_file`'s `set_mode`): the check gives up and leaves the child to end on
/// its own.
#[cfg(not(unix))]
fn kill_group(_: u32) {}

/// Read `reader` to the end, keeping only the last `keep` bytes.
///
/// The whole stream is consumed — a child that writes more than a pipe holds
/// would block forever if it were not — but only the tail is kept, so what this
/// process holds has a ceiling whatever the child says.
fn drain_tail(reader: &mut impl Read, keep: usize) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                kept.extend_from_slice(&buffer[..read]);
                if kept.len() > keep {
                    let excess = kept.len() - keep;
                    kept.drain(..excess);
                }
            }
        }
    }
    kept
}

/// Turn a non-zero exit into the answer the editor shows: the reader's own line
/// when the checker wrote one, and its first line of output otherwise.
fn classify(code: Option<i32>, stderr: &str) -> CheckError {
    if let Some(line) = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix(SYNTAX_PREFIX))
    {
        return CheckError::Syntax(line.trim().to_owned());
    }
    let message = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_owned();
    CheckError::Failed { code, message }
}

/// The SBCL this app should check with: `$EVO_SBCL_BIN`, else `sbcl` on `PATH`,
/// else the directories it is usually installed in.
pub fn find() -> Option<PathBuf> {
    if let Some(configured) = configured_bin() {
        return Some(configured);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(KNOWN_DIRS.iter().map(PathBuf::from));
    search(&dirs)
}

/// A value someone set, taken as-is even when it does not run: an explicit
/// choice that is wrong should say so, not fall back to another SBCL.
fn configured_bin() -> Option<PathBuf> {
    match std::env::var_os(SBCL_BIN_ENV) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// The first `sbcl` in these directories that is a runnable file.
fn search(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join("sbcl"))
        .find(|candidate| is_runnable(candidate))
}

fn is_runnable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The last [`STDERR_CAP`] bytes of output, on a character boundary, trailing
/// whitespace trimmed.
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end();
    if text.len() <= STDERR_CAP {
        return text.to_owned();
    }
    let start = text.len() - STDERR_CAP;
    let start = (start..text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

/// A path that is not this process's own, for the tests below.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_binary_wins_even_when_it_is_wrong() {
        let before = std::env::var_os(SBCL_BIN_ENV);
        std::env::set_var(SBCL_BIN_ENV, "/nonexistent/sbcl-from-a-setting");
        assert_eq!(
            find(),
            Some(PathBuf::from("/nonexistent/sbcl-from-a-setting"))
        );
        std::env::set_var(SBCL_BIN_ENV, "");
        assert_ne!(find(), Some(PathBuf::from("")));
        std::env::remove_var(SBCL_BIN_ENV);
        assert!(configured_bin().is_none());
        if let Some(value) = before {
            std::env::set_var(SBCL_BIN_ENV, value);
        }
    }

    #[test]
    fn search_skips_what_is_not_a_runnable_file() {
        let dir = std::env::temp_dir().join(format!("store-lispcheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Nothing there.
        assert_eq!(search(std::slice::from_ref(&dir)), None);

        // A directory named `sbcl` is not a binary.
        std::fs::create_dir_all(dir.join("sbcl")).unwrap();
        assert_eq!(search(std::slice::from_ref(&dir)), None);
        std::fs::remove_dir_all(dir.join("sbcl")).unwrap();

        // A file without the execute bit is not runnable.
        std::fs::write(dir.join("sbcl"), "not executable").unwrap();
        assert_eq!(search(std::slice::from_ref(&dir)), None);

        // With it, it is.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.join("sbcl"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            assert_eq!(search(std::slice::from_ref(&dir)), Some(dir.join("sbcl")));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_binary_that_is_not_there_cannot_be_run() {
        let checker = Checker::at("/nonexistent/sbcl");
        match checker.check("(a b)") {
            Err(CheckError::CannotRun { bin, .. }) => {
                assert_eq!(bin, PathBuf::from("/nonexistent/sbcl"))
            }
            other => panic!("expected CannotRun, got {other:?}"),
        }
    }

    #[test]
    fn missing_checker_says_what_to_install() {
        let message = CheckError::NoChecker.to_string();
        assert!(message.contains("SBCL"), "{message}");
        assert!(message.contains(SBCL_BIN_ENV), "{message}");
    }

    #[test]
    fn a_stub_that_writes_a_syntax_line_is_a_syntax_error() {
        let stub = stub(
            "syntax",
            "#!/bin/sh\necho 'SYNTAX: unmatched close parenthesis' >&2\nexit 1\n",
        );
        assert_eq!(
            Checker::at(&stub).check(")").unwrap_err().to_string(),
            "unmatched close parenthesis"
        );
        cleanup(&stub);
    }

    #[test]
    fn a_stub_that_fails_another_way_is_not_a_syntax_error() {
        let stub = stub("crash", "#!/bin/sh\necho 'SBCL is broken' >&2\nexit 3\n");
        match Checker::at(&stub).check("(a)") {
            Err(CheckError::Failed { code, message }) => {
                assert_eq!(code, Some(3));
                assert_eq!(message, "SBCL is broken");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        cleanup(&stub);
    }

    #[test]
    fn a_stub_that_hangs_is_killed_and_times_out() {
        let stub = stub("hang", "#!/bin/sh\nsleep 30\n");
        let started = std::time::Instant::now();
        let error = Checker::at(&stub)
            .with_timeout(Duration::from_millis(200))
            .check("(a)")
            .unwrap_err();
        assert!(matches!(error, CheckError::Timeout { .. }), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        cleanup(&stub);
    }

    #[test]
    fn a_timeout_is_not_held_by_what_the_killed_process_left_behind() {
        // The two sleeps hold the pipes after the script itself is killed, so a
        // call that waited for its workers would sit here for the whole five
        // seconds instead of returning at its deadline.
        let stub = stub("orphan", "#!/bin/sh\nsleep 5 &\nsleep 5\n");
        let started = std::time::Instant::now();
        let error = Checker::at(&stub)
            .with_timeout(Duration::from_millis(200))
            .check("(a)")
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(matches!(error, CheckError::Timeout { .. }), "{error:?}");
        assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
        cleanup(&stub);
    }

    #[test]
    fn a_child_that_closes_stderr_and_lingers_still_hits_the_deadline() {
        // The answer is in at once — the pipe is closed — but the process has
        // not exited, so the deadline has to cover that too, not only the read.
        let stub = stub("closed-stderr", "#!/bin/sh\nexec 2>&-\nsleep 5\n");
        let started = std::time::Instant::now();
        let error = Checker::at(&stub)
            .with_timeout(Duration::from_millis(200))
            .check("(a)")
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(matches!(error, CheckError::Timeout { .. }), "{error:?}");
        assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
        cleanup(&stub);
    }

    #[test]
    fn a_child_that_never_reads_the_text_is_not_a_pass() {
        // Bigger than a pipe holds, so the writer is still writing when the
        // child walks away: a happy exit that read ten bytes of a 200 KB text
        // has checked nothing, and must not read as one.
        let text = "(a)".repeat(50_000);
        let partial = stub("partial-read", "#!/bin/sh\nhead -c 10 >/dev/null\nexit 0\n");
        let error = Checker::at(&partial).check(&text).unwrap_err();
        assert!(error.to_string().contains("did not read"), "{error:?}");

        // A refusal is still taken from the child's own words, whether or not it
        // read the whole text.
        let refused = stub(
            "partial-read-1",
            "#!/bin/sh\nhead -c 10 >/dev/null\necho 'SYNTAX: stopped early' >&2\nexit 1\n",
        );
        assert_eq!(
            Checker::at(&refused).check(&text).unwrap_err().to_string(),
            "stopped early"
        );
        cleanup(&partial);
        cleanup(&refused);
    }

    #[test]
    fn a_stub_that_floods_stderr_is_still_answered() {
        // More than a pipe holds. Reading one stream at a time deadlocks here:
        // the child blocks writing stderr while this side blocks on the other
        // pipe, and the answer it did give is never seen.
        let flood = stub(
            "stderr-flood",
            "#!/bin/sh\nyes e | head -c 200000 >&2\necho 'SYNTAX: read to the end' >&2\nexit 1\n",
        );
        assert_eq!(
            Checker::at(&flood).check("(a)").unwrap_err().to_string(),
            "read to the end"
        );
        cleanup(&flood);

        let flood_ok = stub(
            "stderr-flood-ok",
            "#!/bin/sh\nyes e | head -c 200000 >&2\nexit 0\n",
        );
        Checker::at(&flood_ok)
            .check("(a)")
            .expect("a flood is not a failure");
        cleanup(&flood_ok);
    }

    #[test]
    fn a_flood_of_output_keeps_only_its_tail() {
        let mut bytes = vec![b'x'; 50_000];
        bytes.extend_from_slice(b"THE-END");
        let kept = drain_tail(&mut std::io::Cursor::new(bytes), 7);
        assert_eq!(kept, b"THE-END".to_vec());

        // Less than the cap is kept whole, as `tail` expects.
        let short = b"one line\n".to_vec();
        assert_eq!(
            drain_tail(&mut std::io::Cursor::new(short.clone()), STDERR_CAP),
            short
        );
    }

    #[test]
    fn the_install_locations_are_searched_after_the_path() {
        // Not a claim about this machine: the list itself, so that a machine
        // without `sbcl` on `PATH` still finds Homebrew's.
        assert!(KNOWN_DIRS.contains(&"/opt/homebrew/bin"));
        assert!(KNOWN_DIRS.contains(&"/usr/local/bin"));
    }

    #[test]
    fn stderr_is_capped_at_its_tail() {
        let long = "e".repeat(STDERR_CAP * 2);
        assert_eq!(tail(long.as_bytes()).len(), STDERR_CAP);
    }

    fn stub(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path =
            std::env::temp_dir().join(format!("store-lispcheck-{}-{name}.sh", std::process::id()));
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}
