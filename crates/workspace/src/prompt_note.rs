//! The note this client gives every session it starts (§1, §7.2): what the
//! transcript renders, in the session's own words.
//!
//! evo knows nothing about this app: a client writes a markdown file, names it on
//! the launch's command line (`--prompt-note`, repeatable), and the session puts
//! it in every system prompt it builds. The text is `transcript`'s
//! ([`transcript::RATEX_NOTE`], the markdown file that ships beside the renderer),
//! the path is `store`'s ([`Root::prompt_note`], under the app's own root), and
//! this module is the two together: the file written where a launch can name it,
//! and which of the two programs can be told about it at all.

use std::io;
use std::path::PathBuf;

use store::launch::Program;
use store::paths::{self, Root};

/// What this client says about itself: the bytes of the note file, as they are
/// written and as a test holds them.
pub const NOTE: &str = transcript::RATEX_NOTE;

/// The note each program's launches pass (`--prompt-note`), or `None` for one that
/// cannot be told about it — a binary that does not take the flag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptNote {
    /// For a single agent's session (`evo-agent serve`).
    pub agent: Option<PathBuf>,
    /// For a swarm (`evo-swarm serve`), which passes the same file on to every
    /// lane it starts.
    pub swarm: Option<PathBuf>,
}

impl PromptNote {
    /// What these binaries can be told, from what they each take (`store::cli`'s
    /// probe of their own usage).
    ///
    /// A swarm is a **pair**: its coordinator takes the flag and hands it to every
    /// lane it starts, and a lane is the agent's binary — so a swarm gets the note
    /// only when both take it, while one agent needs only its own. Half of a pair
    /// that does not know the flag is a lane that would refuse to start.
    pub fn for_binaries(root: &Root, agent_takes: bool, swarm_takes: bool) -> PromptNote {
        let path = root.prompt_note();
        PromptNote {
            agent: agent_takes.then(|| path.clone()),
            swarm: (agent_takes && swarm_takes).then_some(path),
        }
    }

    /// What one program's launch passes.
    pub fn for_program(&self, program: Program) -> Option<PathBuf> {
        match program {
            Program::Agent => self.agent.clone(),
            Program::Swarm => self.swarm.clone(),
        }
    }

    /// Whether no session at all can be told about this client.
    pub fn is_empty(&self) -> bool {
        self.agent.is_none() && self.swarm.is_none()
    }
}

/// Write the note to [`Root::prompt_note`], and answer the path.
///
/// Written rather than rewritten: the text is compiled in, so a call that finds the
/// file already holding it touches nothing — which is what keeps a reader's own
/// idea of the file's mtime meaningful, and the file readable by a session whose
/// supervisor restarts hours later. The write itself is atomic
/// ([`paths::write_atomic`]): a reader sees the whole note or the one before it,
/// never half of one.
pub fn write(root: &Root) -> io::Result<PathBuf> {
    let path = root.prompt_note();
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no directory for {}", path.display()),
        )
    })?;
    root.ensure()?;
    paths::create_dir_private(dir)?;
    if std::fs::read_to_string(&path).is_ok_and(|held| held == NOTE) {
        return Ok(path);
    }
    paths::write_atomic(&path, NOTE.as_bytes(), paths::FILE_MODE)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> Root {
        let path =
            std::env::temp_dir().join(format!("workspace-note-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Root::at(path)
    }

    /// The file a launch names holds exactly the note the renderer ships, at the
    /// path the flag is given, created owner-only.
    #[test]
    fn the_file_holds_the_note_and_nothing_else() {
        let root = root("content");
        let path = write(&root).unwrap();
        assert_eq!(path, root.prompt_note());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NOTE);
        assert!(NOTE.starts_with("## Math in this client"), "{NOTE}");
        let _ = std::fs::remove_dir_all(root.path());
    }

    /// A second call with the file already right is no write at all: the mtime a
    /// reader compares against is left alone.
    #[test]
    fn a_note_that_is_already_right_is_not_written_again() {
        let root = root("unchanged");
        let path = write(&root).unwrap();
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        let again = write(&root).unwrap();
        assert_eq!(again, path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            first,
            "the same note is not a rewrite"
        );
        // A file that says something else is replaced by the note.
        std::fs::write(&path, "something a person edited\n").unwrap();
        write(&root).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), NOTE);
        let _ = std::fs::remove_dir_all(root.path());
    }

    /// A note that cannot be written is an error, not a panic, and never a half
    /// file: a launch takes the flag back out rather than asking a session for a
    /// path that is not there.
    #[test]
    fn a_note_that_cannot_be_written_is_an_error() {
        let root = root("planted");
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(root.path().join(paths::PROMPT_NOTES_DIR), "not a directory").unwrap();
        assert!(write(&root).is_err(), "a file where the directory goes");
        let _ = std::fs::remove_dir_all(root.path());
    }

    /// The pair rule: a swarm passes the note only when its coordinator *and* the
    /// binary its lanes run take the flag.
    #[test]
    fn a_swarm_needs_both_binaries_and_one_agent_only_its_own() {
        let root = root("pair");
        let path = root.prompt_note();

        let both = PromptNote::for_binaries(&root, true, true);
        assert_eq!(both.agent.as_ref(), Some(&path));
        assert_eq!(both.swarm.as_ref(), Some(&path));
        assert!(!both.is_empty());
        assert_eq!(both.for_program(Program::Agent), Some(path.clone()));
        assert_eq!(both.for_program(Program::Swarm), Some(path.clone()));

        // A new `evo-agent` beside an `evo-swarm` that predates the flag: one
        // agent can be told, a swarm cannot — its lanes are the very binary the
        // coordinator would pass the flag on to.
        let agent_only = PromptNote::for_binaries(&root, true, false);
        assert_eq!(agent_only.for_program(Program::Agent), Some(path));
        assert_eq!(agent_only.for_program(Program::Swarm), None);

        // Neither: nothing is written, and no launch is told anything.
        let neither = PromptNote::for_binaries(&root, false, false);
        assert!(neither.is_empty());
        assert_eq!(neither.for_program(Program::Agent), None);
        assert_eq!(neither.for_program(Program::Swarm), None);
        let _ = std::fs::remove_dir_all(root.path());
    }
}
