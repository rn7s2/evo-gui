//! What a binary is, asked the only way this app asks (§13).
//!
//! One process: `<path> --version`, and the first line it prints. Both binaries introduce
//! themselves that way — `evo-swarm 0.1.0`, `evo-agent 0.1.0` — and the line is what a
//! person can compare against what they built, so it is shown whole.
//!
//! Nothing here re-implements what evo does with a path: a path that answers is a path the
//! app can spawn, and the four ways of not answering are the only four things the row
//! needs to distinguish. The name is read in the *output*, never in the file name: `evo` is
//! a symlink to `evo-swarm`, and a copy under any name is still the binary.
//!
//! The process is [`store::cli`]'s bounded run (§9.1), not a bare `Command::output()`: a
//! binary that never exits is a row that never resolves, and the bound — and the quit —
//! stop it like every other read the app starts.

use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use store::cli::{self, CliError};

/// The words for a probe that has been asked and has not answered yet.
pub const CHECKING: &str = "checking…";

/// What the row under one path field says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Check {
    /// The question has been asked; the answer is not back.
    #[default]
    Checking,
    /// The binary introduced itself: the line it printed.
    Ready(String),
    /// Nothing is at that path.
    Missing,
    /// Something is there and this app cannot run it.
    NotExecutable,
    /// It ran and did not introduce itself as the binary the row expects.
    NotEvo,
    /// It ran and never answered: it was still going when its bound ran out, and was
    /// stopped (§9.1).
    TimedOut,
    /// The field is empty, so there is nothing to ask. [`probe`] never answers this: it is
    /// the panel's own state, and why Save is not offered.
    Empty,
}

impl Check {
    /// The line the row shows.
    pub fn text(&self) -> &str {
        match self {
            Check::Checking => CHECKING,
            Check::Ready(line) => line,
            Check::Missing => "not found",
            Check::NotExecutable => "not executable",
            Check::NotEvo => "not an evo binary",
            // The bound's own number, in the row's own register: the same fact the
            // other reads say in a longer sentence, said the way "not found" is.
            Check::TimedOut => "did not answer within 5s",
            Check::Empty => "no path",
        }
    }

    /// Whether the row is a problem: what makes it wear the danger tone.
    pub fn is_problem(&self) -> bool {
        matches!(
            self,
            Check::Missing | Check::NotExecutable | Check::NotEvo | Check::TimedOut | Check::Empty
        )
    }
}

/// Run `<path> --version` and read what it says, within
/// [`store::cli::VERSION_TIMEOUT`].
///
/// Blocking, and deliberately so: it is a process, so the caller runs it off the thread that
/// draws. `name` is the binary the row expects — `evo-swarm` or `evo-agent`.
pub fn probe(path: &Path, name: &str) -> Check {
    probe_within(path, name, cli::VERSION_TIMEOUT)
}

/// [`probe`], giving the binary `limit` before it is stopped.
fn probe_within(path: &Path, name: &str, limit: Duration) -> Check {
    let output = match cli::version_within(path, limit) {
        Ok(output) => output,
        Err(CliError::NotFound { source, .. }) => {
            return match source.kind() {
                ErrorKind::NotFound => Check::Missing,
                // A file that is not marked executable, and a directory: the same answer,
                // since neither is something this app can spawn.
                ErrorKind::PermissionDenied => Check::NotExecutable,
                _ => Check::NotEvo,
            };
        }
        Err(CliError::TimedOut { .. }) => return Check::TimedOut,
        // A run that never printed a document cannot fail as one, so the other two
        // errors are not reachable from here — and "not an evo binary" is the answer
        // this row already has for a program that answered nothing.
        Err(_) => return Check::NotEvo,
    };

    // Either stream: a binary that introduces itself on stderr is still introducing itself.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Check::NotEvo;
    };

    // The name has to *lead*: `evo-agent` is not what `evo-swarm` prints, and a program that
    // merely mentions an evo binary in passing is not one.
    let Some(rest) = line.strip_prefix(name) else {
        return Check::NotEvo;
    };
    if !output.status.success()
        || !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t'))
    {
        return Check::NotEvo;
    }
    Check::Ready(line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, removed on the way out.
    struct Scratch(std::path::PathBuf);

    /// The bound these tests give a hung binary: long enough that a machine running the
    /// whole suite in parallel has run the script's first line, short enough to wait for.
    const SHORT: Duration = Duration::from_secs(2);

    /// A binary that never ends and ignores `SIGTERM`, with a helper of its own — the
    /// shape §9.1 is about — writing down both pids so a test can ask afterwards whether
    /// anything was left behind.
    struct Hung {
        bin: std::path::PathBuf,
        marker: std::path::PathBuf,
    }

    impl Hung {
        /// The pids the script wrote down: its own, and its helper's.
        fn pids(&self) -> Vec<u32> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let text = std::fs::read_to_string(&self.marker).unwrap_or_default();
                let pids: Vec<u32> = text
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect();
                if pids.len() == 2 || std::time::Instant::now() >= deadline {
                    return pids;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// Whether both are gone: the process *and* the helper it held, which is what
        /// "the group was stopped" means for a script shaped like this.
        fn left_nothing(&self) -> bool {
            let pids = self.pids();
            if pids.len() != 2 {
                return false;
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while pids.iter().any(|pid| alive(*pid)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            !pids.iter().any(|pid| alive(*pid))
        }
    }

    /// Whether that pid is still there, asked the kernel through `ps`.
    fn alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .args(["-o", "pid=", "-p", &pid.to_string()])
            .output()
            .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
            .unwrap_or(false)
    }

    impl Scratch {
        fn new() -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "evo-desktop-settings-probe-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        /// A file with these contents and this mode, as a path.
        fn file(&self, name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
            use std::os::unix::fs::PermissionsExt as _;
            let path = self.0.join(name);
            std::fs::write(&path, contents).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            path
        }

        /// A binary that never answers: it traps `TERM`, holds a `sleep` of its own in
        /// the same process group, and loops forever rather than waiting on that helper —
        /// a `wait` would end with it and answer nothing, which is not a hang.
        fn hung(&self, name: &str) -> Hung {
            let marker = self.0.join(format!("{name}-pids"));
            let bin = self.file(
                name,
                &format!(
                    "#!/bin/sh\ntrap '' TERM\necho $$ > \"{marker}\"\nsleep 3600 &\necho $! >> \"{marker}\"\nwhile :; do sleep 1; done\n",
                    marker = marker.display()
                ),
                0o755,
            );
            Hung { bin, marker }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_binary_that_introduces_itself_is_ready_and_its_line_is_kept() {
        let scratch = Scratch::new();
        let script = scratch.file(
            "evo-swarm",
            "#!/bin/sh\n# a stand-in for the real thing\necho 'evo-swarm 9.9.9'\n",
            0o755,
        );
        assert_eq!(
            probe(&script, "evo-swarm"),
            Check::Ready("evo-swarm 9.9.9".to_owned())
        );
        // The line is what the row shows, so it is kept whole rather than parsed.
        assert_eq!(probe(&script, "evo-swarm").text(), "evo-swarm 9.9.9");
    }

    #[test]
    fn the_name_is_read_in_what_it_printed_not_in_what_it_is_called() {
        let scratch = Scratch::new();
        // `/usr/local/bin/evo` is exactly this shape: a symlink to `evo-swarm`, which is
        // still the swarm binary however the path spells it.
        let copied = scratch.file("evo", "#!/bin/sh\necho 'evo-swarm 0.1.0'\n", 0o755);
        assert_eq!(
            probe(&copied, "evo-swarm"),
            Check::Ready("evo-swarm 0.1.0".to_owned())
        );
        // And the same file is not an `evo-agent`.
        assert_eq!(probe(&copied, "evo-agent"), Check::NotEvo);
    }

    #[test]
    fn a_path_with_nothing_at_it_is_not_found() {
        let scratch = Scratch::new();
        assert_eq!(
            probe(&scratch.0.join("nothing-here"), "evo-swarm"),
            Check::Missing
        );
    }

    #[test]
    fn a_file_that_cannot_be_run_says_so() {
        let scratch = Scratch::new();
        let plain = scratch.file("evo-agent", "#!/bin/sh\necho 'evo-agent 0.1.0'\n", 0o644);
        assert_eq!(probe(&plain, "evo-agent"), Check::NotExecutable);
    }

    #[test]
    fn a_program_that_is_not_an_evo_binary_is_named_as_one() {
        let scratch = Scratch::new();
        // Something real, runnable, and not evo: `/bin/echo --version` prints its argument.
        assert_eq!(probe(Path::new("/bin/echo"), "evo-swarm"), Check::NotEvo);
        // Something runnable that answers nothing at all.
        let silent = scratch.file("silent", "#!/bin/sh\nexit 1\n", 0o755);
        assert_eq!(probe(&silent, "evo-swarm"), Check::NotEvo);
        // A binary of evo's *other* half is still not this one.
        let agent = scratch.file("agent", "#!/bin/sh\necho 'evo-agent 0.1.0'\n", 0o755);
        assert_eq!(probe(&agent, "evo-swarm"), Check::NotEvo);
    }

    /// A binary that never ends is a row that never resolves — the read is bounded, and
    /// what it started goes with it (§9.1).
    #[test]
    fn a_binary_that_never_answers_is_stopped_and_says_so() {
        let scratch = Scratch::new();
        // The liveness helper is not vacuously false: this process is alive.
        assert!(
            alive(std::process::id()),
            "a pid that is there reads as there"
        );
        let hung = scratch.hung("evo-swarm");
        // The test's own bound: the shipped one is five seconds, and this asserts the
        // same ladder without spending them.
        assert_eq!(probe_within(&hung.bin, "evo-swarm", SHORT), Check::TimedOut);
        assert!(
            hung.left_nothing(),
            "the hung binary outlived its bound (pids {:?})",
            hung.pids()
        );
    }

    /// The row's words and the shipped bound are one fact: a bound that moved without
    /// them would be a row that lies about what it waited for.
    #[test]
    fn the_rows_words_and_the_bound_are_one_fact() {
        assert_eq!(
            Check::TimedOut.text(),
            format!("did not answer within {}s", cli::VERSION_TIMEOUT.as_secs())
        );
        assert!(
            Check::TimedOut.is_problem(),
            "a binary that did not answer wears the danger tone, like the other three"
        );
    }

    #[test]
    fn the_installed_binaries_answer_as_themselves() {
        // The real thing, when this machine has it: the shape the rows show is the shape
        // `--version` really prints, not one this crate invented.
        for (path, name) in [
            ("/usr/local/bin/evo-swarm", "evo-swarm"),
            ("/usr/local/bin/evo-agent", "evo-agent"),
        ] {
            let path = Path::new(path);
            if !path.exists() {
                continue;
            }
            match probe(path, name) {
                Check::Ready(line) => assert!(line.starts_with(name), "{line}"),
                other => panic!("{name} at {} answered {other:?}", path.display()),
            }
        }
    }
}
