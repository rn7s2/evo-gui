//! The argv of one launch (§1) — a pure function, so both a spawn and a test can
//! read it.
//!
//! Nothing here picks a port, waits for the ready file, or holds a pipe: the
//! client that spawns does that (it owns the child's stdin and reads
//! `--ready-file`). This decides **what** the child is told:
//!
//! ```text
//! evo-swarm serve --ready-file PATH [--watch-stdin] [--port N]
//!                [--resume PATH] [--model ID@PROVIDER] [--thinking L]
//!                [--workers N] [--lane-model ID@PROVIDER] [--lane-thinking L]
//! ```
//!
//! Two rules the redesign turns on:
//!
//! * `--model` / `--lane-model` name **one registration** (`ID@PROVIDER`). A
//!   bare id keeps evo's own first-registration rule.
//! * A resume passes the *exact* journal path it was resumed from — never a bare
//!   `--resume`, which would resolve to the newest journal in the cwd and can
//!   come back on another tab's session (§1, E1).

use std::path::{Path, PathBuf};

use crate::catalog::ModelRef;
use crate::cli::{agent_bin, swarm_bin};
use crate::paths::Root;

/// Which binary runs, and therefore which flags exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// `evo-agent serve`: no lanes, so no lane flags.
    Agent,
    /// `evo-swarm serve`: the coordinator plus its pool of lanes.
    Swarm,
}

impl Program {
    pub fn binary(&self) -> PathBuf {
        match self {
            Program::Agent => agent_bin(),
            Program::Swarm => swarm_bin(),
        }
    }
}

/// One launch, as the flags it becomes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Which binary, and which flags.
    pub program: Option<Program>,
    /// The child's working directory (the folder the tab runs in). Not an
    /// argument — a spawn's cwd — but part of one launch, so it lives here.
    pub folder: PathBuf,
    /// `--ready-file`: where the child writes its port, url and token once it is
    /// listening (§1). Required by the contract; a launch without one is a
    /// launch the app cannot attach to.
    pub ready_file: Option<PathBuf>,
    /// `--watch-stdin`: the parent holds the pipe, and EOF shuts the child down.
    pub watch_stdin: bool,
    /// `--resume` with the exact journal path.
    pub resume: Option<PathBuf>,
    /// `--model ID@PROVIDER` (the coordinator's).
    pub model: Option<ModelRef>,
    /// `--thinking LEVEL`.
    pub thinking: Option<String>,
    /// `--lane-model ID@PROVIDER`, the model every lane registers.
    pub lane_model: Option<ModelRef>,
    /// `--lane-thinking LEVEL`.
    pub lane_thinking: Option<String>,
    /// `--workers N`.
    pub workers: Option<u16>,
    /// `--port N`; `Some(0)` asks the child to pick one and report it in the
    /// ready file.
    pub port: Option<u16>,
    /// `--no-userspace`: no extensions, so exactly the kernel's own api set —
    /// what a probe of the model catalog used to need.
    pub no_userspace: bool,
}

impl LaunchSpec {
    /// A new swarm in `folder`.
    pub fn new(program: Program, folder: impl Into<PathBuf>) -> LaunchSpec {
        LaunchSpec {
            program: Some(program),
            folder: folder.into(),
            ..LaunchSpec::default()
        }
    }

    /// `serve` and its flags, in the order §1 writes them.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec!["serve".to_owned()];
        // A launch without a ready file is one the app cannot attach to — the flag
        // is required (§1) — but building an argv is not the place to panic: the
        // spawn that waits for the file is where that failure surfaces.
        if let Some(path) = &self.ready_file {
            argv.push("--ready-file".to_owned());
            argv.push(path.display().to_string());
        }
        if self.watch_stdin {
            argv.push("--watch-stdin".to_owned());
        }
        if let Some(port) = self.port {
            argv.push("--port".to_owned());
            argv.push(port.to_string());
        }
        if self.no_userspace {
            argv.push("--no-userspace".to_owned());
        }
        if let Some(resume) = &self.resume {
            argv.push("--resume".to_owned());
            argv.push(resume.display().to_string());
        }
        if let Some(model) = &self.model {
            argv.push("--model".to_owned());
            argv.push(model.spec());
        }
        if let Some(level) = level(&self.thinking) {
            argv.push("--thinking".to_owned());
            argv.push(level);
        }
        if self.program == Some(Program::Swarm) {
            if let Some(workers) = self.workers {
                argv.push("--workers".to_owned());
                argv.push(workers.to_string());
            }
            if let Some(model) = &self.lane_model {
                argv.push("--lane-model".to_owned());
                argv.push(model.spec());
            }
            if let Some(level) = level(&self.lane_thinking) {
                argv.push("--lane-thinking".to_owned());
                argv.push(level);
            }
        }
        argv
    }

    /// `evo-swarm check --json` for **this** launch: the same models, the same
    /// worker count, so its answer is about what would actually be spawned.
    pub fn check_argv(&self) -> Vec<String> {
        let mut argv = vec!["check".to_owned(), "--json".to_owned()];
        if let Some(workers) = self.workers {
            argv.push("--workers".to_owned());
            argv.push(workers.to_string());
        }
        if let Some(model) = &self.model {
            argv.push("--model".to_owned());
            argv.push(model.spec());
        }
        if let Some(model) = &self.lane_model {
            argv.push("--lane-model".to_owned());
            argv.push(model.spec());
        }
        argv
    }

    /// The path of the ready file this launch waits for, under a tab's own
    /// directory (`tabs/<id>/ready.json`, §1).
    pub fn ready_file_in(root: &Root, id: &crate::paths::TabId) -> PathBuf {
        root.tab_ready(id)
    }

    /// The child's working directory.
    pub fn folder(&self) -> &Path {
        &self.folder
    }
}

/// A thinking level, or `None` for the empty string — an empty flag value is
/// never passed.
fn level(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TabId;

    fn spec(program: Program) -> LaunchSpec {
        LaunchSpec {
            program: Some(program),
            folder: PathBuf::from("/coding/foo"),
            ready_file: Some(PathBuf::from("/Users/x/.evo/desktop/tabs/t1/ready.json")),
            watch_stdin: true,
            resume: None,
            model: None,
            thinking: None,
            lane_model: None,
            lane_thinking: None,
            workers: None,
            port: Some(0),
            no_userspace: false,
        }
    }

    #[test]
    fn a_plain_launch_is_serve_ready_file_and_stdin() {
        assert_eq!(
            spec(Program::Swarm).argv(),
            vec![
                "serve",
                "--ready-file",
                "/Users/x/.evo/desktop/tabs/t1/ready.json",
                "--watch-stdin",
                "--port",
                "0"
            ]
        );
        // The agent takes the same head, and no lane flags.
        assert_eq!(spec(Program::Agent).argv()[0], "serve");
        assert!(!spec(Program::Agent)
            .argv()
            .iter()
            .any(|a| a.starts_with("--lane")));
    }

    #[test]
    fn a_model_is_named_by_id_and_provider() {
        let mut launch = spec(Program::Swarm);
        launch.model = Some(ModelRef::new("claude-opus-5", Some("anthropic")));
        launch.thinking = Some("High".to_owned());
        launch.lane_model = Some(ModelRef::new("ark-deepseek-v4.1-flash", Some("aiden")));
        launch.lane_thinking = Some("medium".to_owned());
        launch.workers = Some(4);
        assert_eq!(
            launch.argv(),
            vec![
                "serve",
                "--ready-file",
                "/Users/x/.evo/desktop/tabs/t1/ready.json",
                "--watch-stdin",
                "--port",
                "0",
                "--model",
                "claude-opus-5@anthropic",
                "--thinking",
                "high",
                "--workers",
                "4",
                "--lane-model",
                "ark-deepseek-v4.1-flash@aiden",
                "--lane-thinking",
                "medium"
            ]
        );
    }

    #[test]
    fn a_bare_model_id_is_passed_through_as_a_bare_id() {
        let mut launch = spec(Program::Swarm);
        launch.model = Some(ModelRef::new("bare", None::<String>));
        let argv = launch.argv();
        let at = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[at + 1], "bare");
    }

    #[test]
    fn a_resume_is_the_exact_path_never_a_bare_flag() {
        let mut launch = spec(Program::Swarm);
        launch.resume = Some(PathBuf::from(
            "/Users/x/.evo/sessions/-Users-x-foo/20260929T010000Z_aaaa1111.sexp",
        ));
        let argv = launch.argv();
        let at = argv.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(
            argv[at + 1],
            "/Users/x/.evo/sessions/-Users-x-foo/20260929T010000Z_aaaa1111.sexp"
        );
        assert!(!argv.iter().any(|a| a == "--resume" && a.is_empty()));
    }

    #[test]
    fn lane_flags_belong_to_the_swarm_only() {
        let mut launch = spec(Program::Agent);
        launch.lane_model = Some(ModelRef::new("lane", Some("aiden")));
        launch.lane_thinking = Some("low".to_owned());
        launch.workers = Some(3);
        let argv = launch.argv();
        assert!(!argv.iter().any(|a| a.starts_with("--lane")));
        assert!(!argv.iter().any(|a| a == "--workers"));
    }

    #[test]
    fn an_empty_thinking_level_is_no_flag_at_all() {
        let mut launch = spec(Program::Swarm);
        launch.thinking = Some("   ".to_owned());
        launch.lane_thinking = Some(String::new());
        let argv = launch.argv();
        assert!(!argv.iter().any(|a| a == "--thinking"));
        assert!(!argv.iter().any(|a| a == "--lane-thinking"));
    }

    #[test]
    fn the_check_argv_asks_about_this_launch() {
        let mut launch = spec(Program::Swarm);
        launch.model = Some(ModelRef::new("m", Some("aiden")));
        launch.lane_model = Some(ModelRef::new("l", Some("aiden")));
        launch.workers = Some(2);
        assert_eq!(
            launch.check_argv(),
            vec![
                "check", "--json", "--workers", "2", "--model", "m@aiden", "--lane-model",
                "l@aiden"
            ]
        );
        // A plain launch still asks.
        assert_eq!(spec(Program::Swarm).check_argv(), vec!["check", "--json"]);
    }

    #[test]
    fn the_ready_file_lives_in_the_tabs_own_directory() {
        let root = Root::at("/tmp/root");
        let id = TabId::parse("t1").unwrap();
        assert_eq!(
            LaunchSpec::ready_file_in(&root, &id),
            PathBuf::from("/tmp/root/tabs/t1/ready.json")
        );
    }
}
