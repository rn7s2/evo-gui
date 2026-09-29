//! What a binary is, asked the only way this app asks (§13).
//!
//! One process: `<path> --version`, and the first line it prints. Both binaries introduce
//! themselves that way — `evo-swarm 0.1.0`, `evo-agent 0.1.0` — and the line is what a
//! person can compare against what they built, so it is shown whole.
//!
//! Nothing here re-implements what evo does with a path: a path that answers is a path the
//! app can spawn, and the three ways of not answering are the only three things the row
//! needs to distinguish. The name is read in the *output*, never in the file name: `evo` is
//! a symlink to `evo-swarm`, and a copy under any name is still the binary.

use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

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
            Check::Empty => "no path",
        }
    }

    /// Whether the row is a problem: what makes it wear the danger tone.
    pub fn is_problem(&self) -> bool {
        matches!(
            self,
            Check::Missing | Check::NotExecutable | Check::NotEvo | Check::Empty
        )
    }
}

/// Run `<path> --version` and read what it says.
///
/// Blocking, and deliberately so: it is a process, so the caller runs it off the thread that
/// draws. `name` is the binary the row expects — `evo-swarm` or `evo-agent`.
pub fn probe(path: &Path, name: &str) -> Check {
    let output = match Command::new(path).arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            return match error.kind() {
                ErrorKind::NotFound => Check::Missing,
                // A file that is not marked executable, and a directory: the same answer,
                // since neither is something this app can spawn.
                ErrorKind::PermissionDenied => Check::NotExecutable,
                _ => Check::NotEvo,
            };
        }
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
