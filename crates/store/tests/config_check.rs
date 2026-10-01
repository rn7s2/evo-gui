//! Reading, validating and saving the four raw-editor files, against a real SBCL.
//!
//! The syntax half needs an SBCL that is really installed (`/opt/homebrew/bin`,
//! `/usr/local/bin`, `PATH`, or `$EVO_SBCL_BIN`). When there is none, the tests
//! that need one say so and pass — a machine without SBCL is a supported state
//! (saving is blocked there), not a broken one. The half of a save that needs no
//! reader is covered by the unit tests in `store::config_file`.
//!
//! Everything here is synthetic text: no real `~/.evo` file is read, and no test
//! writes outside its own temporary directory.

use std::fs;
use std::path::{Path, PathBuf};

use store::config_file::{self, ConfigFile, ConfigScope, SaveError};
use store::lispcheck::{self, CheckError, Checker};

/// A directory of this test's own, removed on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("store-config-check-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn scope(&self) -> ConfigScope {
        ConfigScope::Project(self.0.clone())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The installed SBCL, or `None` on a machine without one.
fn has_sbcl() -> bool {
    match Checker::discover() {
        Ok(_) => true,
        Err(CheckError::NoChecker) => {
            eprintln!("no SBCL on this machine — skipping the reader tests");
            false
        }
        Err(other) => panic!("discovery failed in an unexpected way: {other}"),
    }
}

/// The discovery itself: an installed SBCL, found and runnable.
#[test]
fn discovery_finds_an_installed_sbcl() {
    let Some(path) = lispcheck::find() else {
        eprintln!("no SBCL on this machine — nothing to discover");
        return;
    };
    // A configured `$EVO_SBCL_BIN` is taken as given, right or wrong (the unit
    // tests pin that rule); only a *discovered* path is checked for being there.
    if std::env::var_os(lispcheck::SBCL_BIN_ENV).is_none() {
        assert!(path.exists(), "{} does not exist", path.display());
    }
    Checker::discover().expect("found a path, so discovery succeeds");
}

/// Each of the four files accepts its own real shape, package prefixes and all.
#[test]
fn each_of_the_four_files_accepts_its_own_shape() {
    if !has_sbcl() {
        return;
    }
    let samples = [
        (
            ConfigFile::Init,
            "(in-package :evo.user)\n\
             (evo:register-model \"deepseek-v4-pro\"\n\
             \x20 :provider :deepseek\n\
             \x20 :context-window 1000000 :max-output 192000)\n\
             (evo:set-setting :model \"deepseek-v4-pro\")\n",
        ),
        (
            ConfigFile::Swarm,
            ";;; the coordinator and its lanes\n\
             (evo:set-setting :model \"claude-opus-5\")\n\
             (evo.swarm:set-setting :swarm-workers 4)\n\
             (evo.swarm:in-lanes (evo:set-setting :model \"ark-deepseek-v4.1-flash\"))\n",
        ),
        (
            ConfigFile::Memory,
            "(:id \"mem-1\" :kind :constraint :text \"keep it short\"\n\
             \x20:created-at \"2026-01-01T00:00:00Z\" :updated-at \"2026-01-01T00:00:00Z\")\n\
             (:id \"mem-2\" :kind :fact :text \"synthetic\"\n\
             \x20:created-at \"2026-01-01T00:00:00Z\" :updated-at \"2026-01-01T00:00:00Z\")\n",
        ),
        (
            ConfigFile::Lore,
            "(:id \"lore-1\" :text \"a rule of this project\" :timestamp \"2026-01-01T00:00:00Z\")\n",
        ),
    ];
    for (file, text) in samples {
        config_file::validate(text)
            .unwrap_or_else(|error| panic!("{} should read: {error}", file.file_name()));
    }
    // And an empty file of any of them is fine: nothing configured yet.
    config_file::validate("").expect("an empty file is readable");
    config_file::validate(";;; only a comment\n#| and a block one |#\n")
        .expect("comments alone are readable");
}

/// The malformed forms the gate exists for: dotted lists, dispatch macros,
/// quotes, and untermination.
#[test]
fn malformed_forms_are_rejected() {
    if !has_sbcl() {
        return;
    }
    let rejected = [
        ("(f", "unclosed list"),
        ("(foo))", "an extra close paren"),
        (")\n", "a stray close paren"),
        ("(defun f () \"unterminated", "an unterminated string"),
        (
            "#|unterminated block comment",
            "an unterminated block comment",
        ),
        ("(a .)", "a dot with nothing after it"),
        ("(. a)", "a dot with nothing before it"),
        ("(a . b c)", "a dot with two objects after it"),
        ("#<unreadable>", "an unreadable object"),
        ("#2r9", "a bad radix"),
        ("#1=#1#", "a label around nothing"),
        ("#C(1 2 3)", "a malformed complex"),
    ];
    for (text, what) in rejected {
        match config_file::validate(text) {
            Ok(()) => panic!("{what} ({text:?}) was accepted"),
            Err(CheckError::Syntax(_)) => {}
            Err(other) => panic!("{what} gave {other:?}, not a syntax error"),
        }
    }
    // Valid dispatch and quote forms are not collateral damage.
    for text in [
        "#'car",
        "`(a ,b ,@c)",
        "#(1 2 3)",
        "#\\(",
        "#:uninterned",
        "|a symbol with spaces|",
        "'(a . b)",
    ] {
        config_file::validate(text).unwrap_or_else(|error| panic!("{text:?} should read: {error}"));
    }
}

/// `#.` evaluates its form *when the file is read*. The check reads with
/// `*read-eval*` `nil`, so it is refused — and, the point of the test, it is not
/// run on the way to being refused.
#[test]
fn reader_eval_is_refused_and_never_runs() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("reader-eval");
    let marker = scratch.0.join("executed");
    let text = format!(
        "#.(with-open-file (s \"{}\" :direction :output :if-exists :overwrite \
         :if-does-not-exist :create) (write-string \"ran\" s))\n(defun innocent () :nothing)\n",
        marker.display()
    );

    match config_file::validate(&text) {
        Err(CheckError::Syntax(message)) => {
            assert!(message.contains("READ-EVAL"), "said: {message}");
        }
        other => panic!("expected a syntax error, got {other:?}"),
    }
    assert!(!marker.exists(), "#. was evaluated by the check");
}

/// The check is not a way to run a startup file: `--no-userinit --no-sysinit`
/// keep SBCL from reading `~/.sbclrc`, whatever `$HOME` says.
#[test]
fn a_users_sbclrc_is_not_read() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("sbclrc");
    let marker = scratch.0.join("sbclrc-ran");
    fs::write(
        scratch.0.join(".sbclrc"),
        format!(
            "(with-open-file (s \"{}\" :direction :output :if-exists :overwrite \
             :if-does-not-exist :create) (write-string \"ran\" s))\n",
            marker.display()
        ),
    )
    .unwrap();

    let before = std::env::var_os("HOME");
    std::env::set_var("HOME", &scratch.0);
    let answer = config_file::validate("(a b (c . d) #'(lambda (x) x))\n");
    match before {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    answer.expect("an ordinary form reads");
    assert!(!marker.exists(), "the check ran ~/.sbclrc");
}

/// A syntax error stops a save, and the file keeps exactly what it had.
#[test]
fn an_invalid_text_is_never_written() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("invalid-no-write");
    let path = config_file::path(&scratch.scope(), ConfigFile::Init);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let base = "(evo:set-setting :model \"deepseek-v4-pro\")\n";
    fs::write(&path, base).unwrap();

    let edited = "(evo:set-setting :model \"deepseek-v4-pro\"\n"; // the missing paren
    match config_file::save(&path, edited, Some(base)) {
        Err(SaveError::Check(CheckError::Syntax(_))) => {}
        other => panic!("expected a syntax refusal, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), base);
}

/// A valid edit is written atomically, keeps its permissions, and reads back.
#[test]
fn a_valid_text_is_written_atomically() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("valid-write");
    let path = config_file::path(&scratch.scope(), ConfigFile::Swarm);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let base = ";;; nothing yet\n";
    fs::write(&path, base).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }

    let edited = ";;; two lanes on the flash model\n\
                  (evo.swarm:set-setting :swarm-workers 2)\n\
                  (evo.swarm:in-lanes (evo:set-setting :model \"ark-deepseek-v4.1-flash\"))\n";
    config_file::save(&path, edited, Some(base)).expect("a seen file saves");

    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o640);
    }
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // What was written is what the next read and the next check see.
    let again = config_file::read(&path).unwrap();
    assert_eq!(again.as_deref(), Some(edited));
    config_file::validate(again.as_deref().unwrap()).expect("what was saved reads");
}

/// A missing project file opens as nothing, and saving creates it owner-only.
#[test]
fn a_missing_file_opens_empty_and_saves_into_a_new_one() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("missing-create");
    let path = config_file::path(&scratch.scope(), ConfigFile::Lore);
    assert!(path.ends_with(".evo/lore.sexp"));
    assert_eq!(config_file::read(&path).unwrap(), None);

    let edited =
        "(:id \"lore-1\" :text \"a synthetic rule\" :timestamp \"2026-01-01T00:00:00Z\")\n";
    config_file::save(&path, edited, None).expect("a missing file is created");
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            path.metadata().unwrap().permissions().mode() & 0o777,
            config_file::NEW_FILE_MODE
        );
    }
}

/// A form that reads as a keyword is a form like any other: it must not be
/// mistaken for the end of the text and leave everything behind it unchecked.
#[test]
fn a_form_that_reads_as_eof_does_not_end_the_check() {
    if !has_sbcl() {
        return;
    }
    // `:eof` on its own is readable Lisp, and nothing here refuses it.
    config_file::validate(":eof\n").expect("a bare keyword reads");

    let rejected = [
        ":eof\n(a . b c)\n",
        ":eof\n(defun f ()\n",
        "(a)\n:eof\n)\n",
        ":eof\n#.(+ 1 2)\n",
    ];
    for text in rejected {
        match config_file::validate(text) {
            Err(CheckError::Syntax(_)) => {}
            other => panic!("{text:?} was not refused: {other:?}"),
        }
    }

    // And a save carrying one is refused with it: nothing is written.
    let scratch = Scratch::new("eof-sentinel");
    let path = config_file::path(&scratch.scope(), ConfigFile::Lore);
    let broken = ":eof\n(a . b c)\n";
    match config_file::save(&path, broken, None) {
        Err(SaveError::Check(CheckError::Syntax(_))) => {}
        other => panic!("expected a syntax refusal, got {other:?}"),
    }
    assert_eq!(
        config_file::read(&path).unwrap(),
        None,
        "the file was written"
    );
}

/// `save` validates first, so a good edit of a file somebody else changed is a
/// conflict — and the live writer's version stays.
#[test]
fn a_live_writers_change_is_not_overwritten() {
    if !has_sbcl() {
        return;
    }
    let scratch = Scratch::new("conflict-check");
    let path = config_file::path(&scratch.scope(), ConfigFile::Memory);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let base = "(:id \"mem-1\" :kind :fact :text \"first\")\n";
    fs::write(&path, base).unwrap();

    let live = "(:id \"mem-1\" :kind :fact :text \"first\")\n\
                (:id \"mem-2\" :kind :decision :text \"added while open\")\n";
    fs::write(&path, live).unwrap();

    let edited = "(:id \"mem-1\" :kind :fact :text \"edited in the app\")\n";
    match config_file::save(&path, edited, Some(base)) {
        Err(SaveError::Conflict { .. }) => {}
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), live);
}

/// The path an editor shows is the one evo reads, for both scopes.
#[test]
fn the_resolved_paths_are_the_ones_evo_uses() {
    let scratch = Scratch::new("paths");
    let project = ConfigScope::Project(scratch.0.clone());
    assert_eq!(
        config_file::path(&project, ConfigFile::Memory),
        scratch.0.join(".evo").join("memory.sexp")
    );
    // The global scope is the evo home — `$EVO_HOME` when it is set.
    let before = std::env::var_os("EVO_HOME");
    std::env::set_var("EVO_HOME", scratch.0.join("elsewhere"));
    assert_eq!(
        config_file::path(&ConfigScope::Global, ConfigFile::Init),
        scratch.0.join("elsewhere").join("init.lisp")
    );
    match before {
        Some(value) => std::env::set_var("EVO_HOME", value),
        None => std::env::remove_var("EVO_HOME"),
    }
}

/// A binary that is not there cannot check anything — saving stays blocked.
#[test]
fn a_missing_sbcl_cannot_check() {
    let checker = Checker::at(Path::new("/nonexistent/sbcl-for-tests"));
    match checker.check("(a b)") {
        Err(CheckError::CannotRun { bin, .. }) => {
            assert_eq!(bin, Path::new("/nonexistent/sbcl-for-tests"))
        }
        other => panic!("expected CannotRun, got {other:?}"),
    }
}
