//! The command line: what this server binds, what it serves, and which binaries
//! it drives.
//!
//! Hand-rolled rather than a parser crate: there are eight flags, every one of
//! them is a single value, and the one rule that matters — a non-loopback bind
//! requires a token — is easier to state here than to configure.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

/// What this server listens on and drives. Everything is decided once, at
/// startup, and never changes while it runs.
#[derive(Clone, Debug)]
pub struct Config {
    /// The address to bind. Loopback unless a person asked for otherwise, in
    /// which case [`Config::token`] is `Some` — the two are decided together.
    pub bind: IpAddr,
    /// `0` asks the OS for a free port.
    pub port: u16,
    /// The frontend's static bundle: `index.html` and the assets beside it.
    pub dist: PathBuf,
    /// Where tabs live (`tabs/<id>/`), the same root the desktop app uses.
    pub root: PathBuf,
    /// The token every `/api` request must carry, or `None` on loopback.
    pub token: Option<String>,
    /// The `evo-agent` a single-agent tab runs, and the one a swarm's lanes run.
    pub agent_bin: PathBuf,
    /// The `evo-swarm` a swarm tab runs.
    pub swarm_bin: PathBuf,
    /// `--prompt-note`: a markdown file every session is told about, so the
    /// browser's renderer is described to the model. Optional.
    pub prompt_note: Option<PathBuf>,
}

/// What the arguments said to do.
#[derive(Debug)]
pub enum Outcome {
    Run(Box<Config>),
    Help,
}

pub const USAGE: &str = "\
evo-web — serve the browser UI and drive evo on this machine

USAGE:
  evo-web [OPTIONS]

OPTIONS:
  --bind ADDR        address to listen on (default 127.0.0.1, env EVO_WEB_BIND).
                     Anything that is not loopback REQUIRES --auth-token.
  --port N           port to listen on (default 8787, env EVO_WEB_PORT).
                     0 asks the OS for a free one; the chosen one is printed.
  --dist DIR         the frontend bundle to serve (default dist, env EVO_WEB_DIST)
  --root DIR         where tabs live (default ~/.evo/desktop, env EVO_WEB_ROOT)
  --auth-token T     require this token on /api (env EVO_WEB_TOKEN)
  --evo-agent PATH   the evo-agent to run (default $EVO_AGENT_BIN, else
                     /usr/local/bin/evo-agent)
  --evo-swarm PATH   the evo-swarm to run (default $EVO_SWARM_BIN, else
                     /usr/local/bin/evo-swarm)
  --prompt-note PATH markdown every session this server starts is told about
  -h, --help         this text

The bearer token of a serve process never leaves this server: the browser talks
to /api and this process adds the token on the way out.
";

/// Parse the arguments, reading the environment for anything not given.
///
/// `getenv` is a parameter rather than `std::env::var` so that the rules — the
/// loopback one above all — can be tested without touching this process's own
/// environment.
pub fn from_args<I>(args: I, getenv: &dyn Fn(&str) -> Option<String>) -> Result<Outcome, String>
where
    I: IntoIterator<Item = String>,
{
    let mut bind: Option<String> = None;
    let mut port: Option<String> = None;
    let mut dist: Option<String> = None;
    let mut root: Option<String> = None;
    let mut token: Option<String> = None;
    let mut agent: Option<String> = None;
    let mut swarm: Option<String> = None;
    let mut note: Option<String> = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Outcome::Help),
            "--bind" => bind = Some(value()?),
            "--port" => port = Some(value()?),
            "--dist" => dist = Some(value()?),
            "--root" => root = Some(value()?),
            "--auth-token" => token = Some(value()?),
            "--evo-agent" => agent = Some(value()?),
            "--evo-swarm" => swarm = Some(value()?),
            "--prompt-note" => note = Some(value()?),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }

    let bind = bind
        .or_else(|| getenv("EVO_WEB_BIND"))
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    let bind = parse_bind(&bind)?;
    let port = match port.or_else(|| getenv("EVO_WEB_PORT")) {
        Some(text) => text
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("--port {text:?} is not a port number"))?,
        None => 8787,
    };
    let dist = dist
        .or_else(|| getenv("EVO_WEB_DIST"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("dist"));
    let root = root
        .or_else(|| getenv("EVO_WEB_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(store::paths::default_root_dir);
    let token = token
        .or_else(|| getenv("EVO_WEB_TOKEN"))
        .filter(|t| !t.is_empty());
    let note = note
        .or_else(|| getenv("EVO_WEB_PROMPT_NOTE"))
        .filter(|t| !t.is_empty())
        .map(PathBuf::from);

    // The one rule that is a security decision rather than a preference. A
    // non-loopback bind is reachable by anything on the network, and this server
    // spawns processes and stops them; without a token that is an open door, so
    // the door stays shut until somebody hands over a token.
    if !swarm_client::is_loopback(&bind.to_string()) && token.is_none() {
        return Err(format!(
            "refusing to bind {bind}: a non-loopback address is reachable from the \
             network, and this server starts and stops processes. Pass --auth-token \
             (or set EVO_WEB_TOKEN), or bind 127.0.0.1."
        ));
    }

    // A note that is not there is a session that refuses to start (evo reads the
    // path at parse time), so this is a startup error rather than a surprise per
    // tab.
    if let Some(path) = &note {
        if !path.is_file() {
            return Err(format!("--prompt-note {} is not a file", path.display()));
        }
    }

    let agent_bin = agent
        .map(PathBuf::from)
        .unwrap_or_else(store::cli::agent_bin);
    let swarm_bin = swarm
        .map(PathBuf::from)
        .unwrap_or_else(store::cli::swarm_bin);

    Ok(Outcome::Run(Box::new(Config {
        bind,
        port,
        dist,
        root,
        token,
        agent_bin,
        swarm_bin,
        prompt_note: note,
    })))
}

/// An address literal, or `localhost`, which means loopback.
fn parse_bind(text: &str) -> Result<IpAddr, String> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    text.parse::<IpAddr>().map_err(|_| {
        format!("--bind {text:?} is not an IP address (use 127.0.0.1, ::1 or 0.0.0.0)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn parse(args: &[&str]) -> Result<Outcome, String> {
        from_args(args.iter().map(|a| (*a).to_owned()), &no_env)
    }

    fn config(args: &[&str]) -> Config {
        match parse(args).expect("parses") {
            Outcome::Run(cfg) => *cfg,
            Outcome::Help => panic!("not help"),
        }
    }

    #[test]
    fn the_defaults_are_loopback_and_a_port() {
        let cfg = config(&[]);
        assert_eq!(cfg.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(cfg.port, 8787);
        assert_eq!(cfg.dist, PathBuf::from("dist"));
        assert!(cfg.token.is_none());
        assert_eq!(cfg.agent_bin, PathBuf::from(store::cli::INSTALLED_AGENT));
        assert_eq!(cfg.swarm_bin, PathBuf::from(store::cli::INSTALLED_SWARM));
    }

    #[test]
    fn every_flag_is_read() {
        let cfg = config(&[
            "--bind",
            "::1",
            "--port",
            "0",
            "--dist",
            "/tmp/bundle",
            "--root",
            "/tmp/root",
            "--auth-token",
            "s3cret",
            "--evo-agent",
            "/opt/agent",
            "--evo-swarm",
            "/opt/swarm",
        ]);
        assert!(cfg.bind.is_loopback());
        assert_eq!(cfg.port, 0);
        assert_eq!(cfg.dist, PathBuf::from("/tmp/bundle"));
        assert_eq!(cfg.root, PathBuf::from("/tmp/root"));
        assert_eq!(cfg.token.as_deref(), Some("s3cret"));
        assert_eq!(cfg.agent_bin, PathBuf::from("/opt/agent"));
        assert_eq!(cfg.swarm_bin, PathBuf::from("/opt/swarm"));
    }

    /// The security rule: a reachable bind without a token is refused outright.
    #[test]
    fn a_non_loopback_bind_without_a_token_is_refused() {
        let err = parse(&["--bind", "0.0.0.0"]).unwrap_err();
        assert!(err.contains("--auth-token"), "{err}");
        let err = parse(&["--bind", "192.168.1.5"]).unwrap_err();
        assert!(err.contains("refusing to bind"), "{err}");
        // With a token it is allowed, and the token is the one that was given.
        assert_eq!(
            config(&["--bind", "0.0.0.0", "--auth-token", "t"])
                .token
                .as_deref(),
            Some("t")
        );
        // Loopback needs nothing.
        assert!(parse(&["--bind", "127.0.0.1"]).is_ok());
        assert!(parse(&["--bind", "::1"]).is_ok());
    }

    #[test]
    fn the_environment_is_the_fallback() {
        let env = |name: &str| match name {
            "EVO_WEB_BIND" => Some("0.0.0.0".to_owned()),
            "EVO_WEB_PORT" => Some("9000".to_owned()),
            "EVO_WEB_TOKEN" => Some("from-env".to_owned()),
            "EVO_WEB_DIST" => Some("/srv/dist".to_owned()),
            _ => None,
        };
        let Outcome::Run(cfg) = from_args(Vec::new(), &env).expect("parses") else {
            panic!("not help");
        };
        assert_eq!(cfg.bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(cfg.port, 9000);
        assert_eq!(cfg.token.as_deref(), Some("from-env"));
        assert_eq!(cfg.dist, PathBuf::from("/srv/dist"));
    }

    #[test]
    fn a_flag_beats_the_environment() {
        let env = |name: &str| (name == "EVO_WEB_PORT").then(|| "9000".to_owned());
        let Outcome::Run(cfg) =
            from_args(["--port".to_owned(), "1234".to_owned()], &env).expect("parses")
        else {
            panic!("not help");
        };
        assert_eq!(cfg.port, 1234);
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        assert!(parse(&["--port", "http"])
            .unwrap_err()
            .contains("not a port"));
        assert!(parse(&["--bind", "example.com"])
            .unwrap_err()
            .contains("not an IP"));
        assert!(parse(&["--dist"]).unwrap_err().contains("needs a value"));
        assert!(parse(&["--wat"]).unwrap_err().contains("unknown argument"));
        assert!(parse(&["--prompt-note", "/nope/nope.md"])
            .unwrap_err()
            .contains("is not a file"));
    }

    #[test]
    fn help_is_not_a_run() {
        assert!(matches!(parse(&["--help"]).unwrap(), Outcome::Help));
    }
}
