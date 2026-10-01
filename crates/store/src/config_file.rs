//! The four user files Settings edits by hand: where they are, and how they are
//! read and written without losing anybody's work.
//!
//! evo is configured by Lisp and by sexpr streams, and these are its own files
//! (`docs/extension-api.md`, `docs/swarm.md`, `src/core-ext/memory.lisp`,
//! `src/kernel/lore.lisp`):
//!
//! ```text
//! ~/.evo/init.lisp        <cwd>/.evo/init.lisp        models, providers, settings
//! ~/.evo/swarm.lisp       <cwd>/.evo/swarm.lisp       the coordinator and its lanes
//! ~/.evo/memory.sexp      <cwd>/.evo/memory.sexp      curated memory, one form per entry
//! ~/.evo/lore.sexp        <cwd>/.evo/lore.sexp        durable lore, one form per entry
//! ```
//!
//! The whole surface is five things:
//!
//! ```text
//! path(scope, file) -> PathBuf                  where a file is
//! read(path)        -> Result<Option<String>>   its text, or None when it is not there
//! validate(text)    -> Result<()>               is it readable Lisp? (a real SBCL)
//! write(path, text, base) -> Result<()>         the conflict check and the atomic write
//! save(path, text, base)  -> Result<()>         validate + write
//! ```
//!
//! [`ConfigScope::Global`] is the home evo itself would use (`$EVO_HOME` when it
//! is set, else `~/.evo`, see [`crate::paths::evo_dir`]); [`ConfigScope::Project`]
//! carries the folder a tab runs in and `.evo` is appended to it. A project scope
//! is always a folder somebody chose — this module never guesses one.
//!
//! ## `base`, and how far a save goes to not overwrite a live file
//!
//! `memory.sexp` and `lore.sexp` are written by a running session while a person
//! may have them open, so a save is only allowed to replace the file it was shown:
//! `base` is the text [`read`] returned (`None` when the file did not exist), and
//! [`write`] refuses unless the file on disk is still exactly that — including
//! when it appeared or vanished in between. The refusal is
//! [`SaveError::Conflict`], and nothing is written.
//!
//! [`write`] stages the replacement first and checks the target a second time
//! immediately before the rename, so the window it cannot cover is one syscall
//! wide rather than as long as an fsync. A writer that lands inside that window is
//! still lost — a plain file offers no way to make that impossible without the
//! other writer's cooperation, and this app does not pretend otherwise.
//!
//! ## Symlinks
//!
//! `~/.evo/init.lisp` pointing into a dotfiles repository is a symlink, and
//! writing through one (or renaming a plain file over it) does something the
//! author did not ask for, in a place they did not name. Reading and writing both
//! stop with [`SaveError::Symlink`] / [`ConfigError::Symlink`] instead of
//! following or replacing one.
//!
//! The **final component is the whole subject**: `~/.evo` itself being a symlink
//! into a dotfiles repository is a layout evo follows, so it is followed here too.
//! Only the file the path names is refused, never the directories above it.
//!
//! On Unix the refusal is the open itself (`O_NOFOLLOW`), not one look followed by
//! another: there is no window between "the path is not a symlink" and "the file
//! is opened" for a link to be swapped into, and the text — and the comparison a
//! save makes — comes from the opened handle.
//!
//! ## What a save writes
//!
//! [`crate::paths::write_atomic`]: a private scratch file created with
//! `create_new` (so a planted scratch name is never followed), fsync, rename — a
//! reader never sees half a file and a crash leaves the previous one intact. The
//! scratch file is given its final permissions *before* the rename, so nothing
//! can fail once the new file is in place. An existing file keeps the permissions
//! it had; one this creates is owner-only ([`NEW_FILE_MODE`]). Nothing here checks
//! the *shape* of what was written (an entry's keys, a setting's type):
//! [`validate`] is syntax and says so.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::lispcheck::{self, CheckError};
use crate::paths;

/// Permissions for a file this module creates: owner-only, like everything else
/// the app writes.
pub const NEW_FILE_MODE: u32 = 0o600;

/// Which evo home a config file lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigScope {
    /// The evo home every session on this machine shares: `$EVO_HOME` when it is
    /// set and non-empty, else `~/.evo`.
    Global,
    /// One project's own `.evo/`: the folder a tab runs in, as it was chosen.
    Project(PathBuf),
}

impl ConfigScope {
    /// The directory the four files live in (`.evo` is appended to a project
    /// folder).
    pub fn dir(&self) -> PathBuf {
        match self {
            ConfigScope::Global => paths::evo_dir(),
            ConfigScope::Project(folder) => folder.join(".evo"),
        }
    }
}

/// The four files the raw editors can open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConfigFile {
    /// `init.lisp` — models, providers and settings, re-evaluated on every boot.
    Init,
    /// `swarm.lisp` — the swarm's coordinator and lane configuration.
    Swarm,
    /// `memory.sexp` — curated memory, one form per entry.
    Memory,
    /// `lore.sexp` — durable lore, one form per entry.
    Lore,
}

impl ConfigFile {
    /// Every file, in the order a panel shows them.
    pub const ALL: [ConfigFile; 4] = [
        ConfigFile::Init,
        ConfigFile::Swarm,
        ConfigFile::Memory,
        ConfigFile::Lore,
    ];

    /// The name on disk, which is also what an editor is titled.
    pub fn file_name(self) -> &'static str {
        match self {
            ConfigFile::Init => "init.lisp",
            ConfigFile::Swarm => "swarm.lisp",
            ConfigFile::Memory => "memory.sexp",
            ConfigFile::Lore => "lore.sexp",
        }
    }
}

/// Where one of the four files is, in this scope.
pub fn path(scope: &ConfigScope, file: ConfigFile) -> PathBuf {
    scope.dir().join(file.file_name())
}

/// Why a config file could not be read.
#[derive(Debug)]
pub enum ConfigError {
    /// The path is a symbolic link. This app does not follow one on the way in
    /// and will not replace one on the way out.
    Symlink { path: PathBuf },
    /// The path exists and is not a regular file.
    NotRegular { path: PathBuf },
    /// The file is not UTF-8, so it cannot be edited as text.
    NotUtf8 { path: PathBuf },
    /// Any other I/O failure.
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Symlink { path } => write!(
                f,
                "{} is a symbolic link; this app does not edit or replace symlinks",
                path.display()
            ),
            ConfigError::NotRegular { path } => {
                write!(f, "{} is not a regular file", path.display())
            }
            ConfigError::NotUtf8 { path } => write!(f, "{} is not UTF-8 text", path.display()),
            ConfigError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Why a save did not happen. Any of these means the file is untouched.
#[derive(Debug)]
pub enum SaveError {
    /// The text was not checked, or was refused: nothing was written. The text
    /// has to be readable Lisp before it can replace a file a boot will read.
    Check(CheckError),
    /// The path is a symbolic link (see the module docs).
    Symlink { path: PathBuf },
    /// The path exists and is not a regular file.
    NotRegular { path: PathBuf },
    /// The file on disk is not the file that was read (`base`), so somebody else
    /// changed it — the text is not written.
    Conflict { path: PathBuf, reason: &'static str },
    /// Any other I/O failure.
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Check(error) => error.fmt(f),
            SaveError::Symlink { path } => write!(
                f,
                "{} is a symbolic link; this app does not edit or replace symlinks",
                path.display()
            ),
            SaveError::NotRegular { path } => {
                write!(f, "{} is not a regular file", path.display())
            }
            SaveError::Conflict { path, reason } => {
                write!(f, "{} {reason}; your edit was not written", path.display())
            }
            SaveError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SaveError {}

impl From<CheckError> for SaveError {
    fn from(error: CheckError) -> SaveError {
        SaveError::Check(error)
    }
}

impl From<ConfigError> for SaveError {
    fn from(error: ConfigError) -> SaveError {
        match error {
            ConfigError::Symlink { path } => SaveError::Symlink { path },
            ConfigError::NotRegular { path } => SaveError::NotRegular { path },
            ConfigError::NotUtf8 { path } => SaveError::Io {
                path,
                source: io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text"),
            },
            ConfigError::Io { path, source } => SaveError::Io { path, source },
        }
    }
}

/// Read one file: `Ok(Some(text))`, or `Ok(None)` when it is not there yet.
///
/// A missing file is not an error — a project that has never been configured
/// opens as an empty editor, and saving it creates it. A symlink, a directory and
/// a file that is not UTF-8 are errors: none of them can be edited as text.
///
/// The file is opened once, without following a symlink ([`open_regular`]), and
/// the text comes from that handle. Nothing looks the path up a second time, so a
/// symlink swapped in after the open cannot make this read somebody else's file.
pub fn read(path: &Path) -> Result<Option<String>, ConfigError> {
    let Some(mut file) = open_regular(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ConfigError::NotUtf8 {
            path: path.to_path_buf(),
        })
}

/// The regular file at `path`, opened **without following a symlink**, or `None`
/// when there is nothing there.
///
/// This is where the symlink refusal is enforced: on Unix the open carries
/// `O_NOFOLLOW`, so a symbolic link at the final component fails with `ELOOP`
/// rather than being followed, and there is no window between "the path is not a
/// symlink" and "the file is opened" for one to be swapped in. A directory, a
/// device and a fifo are refused too — the open carries `O_NONBLOCK` so a fifo
/// cannot make an open wait for a writer.
///
/// The *parent* directories are followed, as they are everywhere else in this
/// module (a `~/.evo` that is itself a symlink is a layout evo follows).
///
/// On a platform with no `O_NOFOLLOW` this falls back to looking first and then
/// opening, which is the honest best a kernel without it can do; the window
/// between the two looks is the price of that platform, and it is not claimed to
/// be closed.
fn open_regular(path: &Path) -> Result<Option<fs::File>, ConfigError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        match options.open(path) {
            Ok(file) => {
                let meta = file.metadata().map_err(|source| ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                if !meta.is_file() {
                    return Err(ConfigError::NotRegular {
                        path: path.to_path_buf(),
                    });
                }
                Ok(Some(file))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            // `O_NOFOLLOW` refuses the link with `ELOOP`. Anything else that can
            // raise it (a loop of directories above, say) is reported as the I/O
            // failure it is.
            Err(error) if is_symlink_refusal(path, &error) => Err(ConfigError::Symlink {
                path: path.to_path_buf(),
            }),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
    #[cfg(not(unix))]
    {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        if meta.file_type().is_symlink() {
            return Err(ConfigError::Symlink {
                path: path.to_path_buf(),
            });
        }
        if !meta.is_file() {
            return Err(ConfigError::NotRegular {
                path: path.to_path_buf(),
            });
        }
        fs::File::open(path)
            .map(Some)
            .map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

/// Whether an open failed because `O_NOFOLLOW` refused a symbolic link.
fn is_symlink_refusal(path: &Path, error: &io::Error) -> bool {
    if error.raw_os_error() != Some(loop_errno()) {
        return false;
    }
    // `ELOOP` also covers a loop made of the directories above, so the final
    // component itself is what decides which of the two this is.
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(unix)]
fn loop_errno() -> i32 {
    libc::ELOOP
}

#[cfg(not(unix))]
fn loop_errno() -> i32 {
    0
}

/// Is `text` readable Lisp? A real SBCL reads it and never evaluates it
/// ([`crate::lispcheck`] has the whole story, including what it refuses).
///
/// Blocking: it runs a process, so a caller runs it off the thread that draws. A
/// machine with no SBCL answers [`CheckError::NoChecker`] — the text is *not*
/// accepted on a weaker parser's say-so.
pub fn validate(text: &str) -> Result<(), CheckError> {
    lispcheck::Checker::discover()?.check(text)
}

/// Validate `text`, check it against `base`, and write it: what a Save does.
///
/// The order matters — the text is readable before anything else is touched, and
/// the file must still be the one that was read. Any failure means nothing was
/// written.
pub fn save(path: &Path, text: &str, base: Option<&str>) -> Result<(), SaveError> {
    validate(text)?;
    write(path, text, base)
}

/// The half of [`save`] after validation: check `text` against `base` and write
/// it atomically, keeping the file's permissions.
///
/// `base` is what [`read`] returned — `None` meaning the file was not there. The
/// write is refused unless the file on disk is still exactly that, so a change
/// made while the editor was open is never overwritten.
///
/// The replacement is staged first and the target is checked again immediately
/// before the rename, which is the last moment a client of a plain file can look.
/// That closes the long window between reading the target and replacing it; it is
/// not a promise against a writer that slips in *between* that check and the
/// rename, which nothing short of the other writer's cooperation could give.
pub fn write(path: &Path, text: &str, base: Option<&str>) -> Result<(), SaveError> {
    // A quick answer before anything is staged: a symlink, a directory, or a file
    // that is already not the one that was read needs no scratch file.
    let mode = check_target(path, base)?.unwrap_or(NEW_FILE_MODE);

    // The new bytes go to a private scratch file beside the target, already
    // flushed and already wearing `mode`. Nothing here touches the target.
    let staged =
        paths::Staged::stage(path, text.as_bytes(), mode).map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    // The check that counts, with the new bytes already on disk: the gap between
    // it and the rename is one syscall wide. A refusal here drops the stage, and
    // its scratch file goes with it.
    check_target(path, base)?;

    staged.commit().map_err(|source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Whether the target is still the file `base` was read from, and the permissions
/// to keep when it is: `Ok(None)` when there is no file there, `Ok(Some(mode))`
/// when there is.
///
/// The final path component is the whole subject here — a `~/.evo` that is
/// itself a symlink is a layout evo follows, so it is followed here too.
fn check_target(path: &Path, base: Option<&str>) -> Result<Option<u32>, SaveError> {
    // Opened the same way `read` opens it — never following a symlink — so the
    // bytes this compares are the bytes of the file it will replace, and not of
    // whatever a swapped-in link points at.
    let Some(mut file) = open_regular(path)? else {
        return match base {
            None => Ok(None),
            Some(_) => Err(SaveError::Conflict {
                path: path.to_path_buf(),
                reason: "was removed since it was opened",
            }),
        };
    };
    let Some(base) = base else {
        return Err(SaveError::Conflict {
            path: path.to_path_buf(),
            reason: "appeared since it was opened",
        });
    };

    let meta = file.metadata().map_err(|source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut current = Vec::new();
    file.read_to_end(&mut current)
        .map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if current != base.as_bytes() {
        return Err(SaveError::Conflict {
            path: path.to_path_buf(),
            reason: "changed on disk since it was opened",
        });
    }
    Ok(mode_of(&meta))
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode_of(_: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, removed on the way out.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir()
                .join(format!("store-config-file-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        /// This scratch as a project scope (so no test depends on `$HOME`).
        fn scope(&self) -> ConfigScope {
            ConfigScope::Project(self.0.clone())
        }

        /// The `init.lisp` of that scope, with its `.evo` directory made.
        fn init_lisp(&self) -> PathBuf {
            let path = path(&self.scope(), ConfigFile::Init);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mode_of_path(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata().unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_four_files_and_their_paths() {
        assert_eq!(ConfigFile::ALL.len(), 4);
        let project = ConfigScope::Project(PathBuf::from("/coding/foo"));
        assert_eq!(
            path(&project, ConfigFile::Init),
            PathBuf::from("/coding/foo/.evo/init.lisp")
        );
        assert_eq!(
            path(&project, ConfigFile::Swarm),
            PathBuf::from("/coding/foo/.evo/swarm.lisp")
        );
        assert_eq!(
            path(&project, ConfigFile::Memory),
            PathBuf::from("/coding/foo/.evo/memory.sexp")
        );
        assert_eq!(
            path(&project, ConfigFile::Lore),
            PathBuf::from("/coding/foo/.evo/lore.sexp")
        );
        assert_eq!(ConfigFile::Lore.file_name(), "lore.sexp");
        // The global scope is the evo home, whatever it resolves to.
        assert!(path(&ConfigScope::Global, ConfigFile::Lore).ends_with(".evo/lore.sexp"));
        assert_eq!(ConfigScope::Global.dir(), paths::evo_dir());
    }

    #[test]
    fn a_missing_file_reads_as_nothing_and_is_not_created() {
        let scratch = Scratch::new("missing");
        let missing = path(&scratch.scope(), ConfigFile::Init);
        assert_eq!(read(&missing).unwrap(), None);
        // Reading a file that is not there does not make it, or its directory.
        assert!(!scratch.0.join(".evo").exists());
    }

    #[test]
    fn an_existing_file_reads_its_text() {
        let scratch = Scratch::new("existing");
        let path = scratch.init_lisp();
        fs::write(&path, "(a)\n").unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("(a)\n"));
    }

    #[test]
    fn a_write_puts_the_file_there_and_leaves_no_scratch_behind() {
        let scratch = Scratch::new("write");
        let path = path(&scratch.scope(), ConfigFile::Swarm);
        write(&path, "(evo.swarm:set-setting :swarm-workers 4)\n", None).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "(evo.swarm:set-setting :swarm-workers 4)\n"
        );
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // And what was written is what the next read sees.
        assert_eq!(
            read(&path).unwrap().as_deref(),
            Some("(evo.swarm:set-setting :swarm-workers 4)\n")
        );
    }

    #[test]
    fn a_new_file_is_owner_only_and_an_existing_one_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("modes");
        let new = path(&scratch.scope(), ConfigFile::Lore);
        write(&new, "(a)\n", None).unwrap();
        assert_eq!(mode_of_path(&new), NEW_FILE_MODE);

        let existing = scratch.init_lisp();
        fs::write(&existing, "(a)\n").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();
        write(&existing, "(b)\n", Some("(a)\n")).unwrap();
        assert_eq!(mode_of_path(&existing), 0o640);
        assert_eq!(fs::read_to_string(&existing).unwrap(), "(b)\n");
    }

    #[test]
    fn a_file_changed_since_it_was_read_is_not_overwritten() {
        let scratch = Scratch::new("conflict");
        let path = path(&scratch.scope(), ConfigFile::Memory);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let base = "(:id \"mem-1\" :kind :fact :text \"first\")\n";
        fs::write(&path, base).unwrap();

        // A running session adds an entry while the file is open.
        let live = "(:id \"mem-1\" :kind :fact :text \"first\")\n(:id \"mem-2\")\n";
        fs::write(&path, live).unwrap();

        match write(
            &path,
            "(:id \"mem-1\" :kind :fact :text \"edited\")\n",
            Some(base),
        ) {
            Err(SaveError::Conflict { reason, .. }) => {
                assert_eq!(reason, "changed on disk since it was opened")
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        // The other writer's work is intact.
        assert_eq!(fs::read_to_string(&path).unwrap(), live);
    }

    #[test]
    fn a_file_that_appeared_or_vanished_is_a_conflict() {
        let scratch = Scratch::new("appeared");
        let appeared = path(&scratch.scope(), ConfigFile::Lore);
        fs::create_dir_all(appeared.parent().unwrap()).unwrap();
        fs::write(&appeared, "(a)\n").unwrap();
        assert!(matches!(
            write(&appeared, "", None),
            Err(SaveError::Conflict {
                reason: "appeared since it was opened",
                ..
            })
        ));

        let scratch = Scratch::new("vanished");
        let vanished = path(&scratch.scope(), ConfigFile::Lore);
        fs::create_dir_all(vanished.parent().unwrap()).unwrap();
        fs::write(&vanished, "(a)\n").unwrap();
        fs::remove_file(&vanished).unwrap();
        assert!(matches!(
            write(&vanished, "(b)\n", Some("(a)\n")),
            Err(SaveError::Conflict {
                reason: "was removed since it was opened",
                ..
            })
        ));
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_is_neither_read_nor_replaced() {
        let scratch = Scratch::new("symlink");
        let real = scratch.0.join("dotfiles-init.lisp");
        fs::write(&real, "(a)\n").unwrap();
        let link = scratch.0.join(".evo").join("init.lisp");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        match read(&link) {
            Err(ConfigError::Symlink { path }) => assert_eq!(path, link),
            other => panic!("expected a symlink error, got {other:?}"),
        }
        assert!(matches!(
            write(&link, "(b)\n", None),
            Err(SaveError::Symlink { .. })
        ));
        // The link is still a link, and the file it points at is untouched.
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "(a)\n");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_that_appears_after_reading_blocks_the_write() {
        let scratch = Scratch::new("late-symlink");
        let path = path(&scratch.scope(), ConfigFile::Swarm);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "(a)\n").unwrap();
        let base = read(&path).unwrap();

        // Swapped for a symlink while the editor had it open.
        let target = scratch.0.join("elsewhere.lisp");
        fs::write(&target, "(b)\n").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        assert!(matches!(
            write(&path, "(c)\n", base.as_deref()),
            Err(SaveError::Symlink { .. })
        ));
        assert_eq!(fs::read_to_string(&target).unwrap(), "(b)\n");
    }

    #[test]
    fn a_directory_or_a_non_utf8_file_is_not_editable() {
        let scratch = Scratch::new("not-text");
        let dir = path(&scratch.scope(), ConfigFile::Lore);
        fs::create_dir_all(&dir).unwrap();
        assert!(matches!(read(&dir), Err(ConfigError::NotRegular { .. })));

        let path = scratch.init_lisp();
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(read(&path), Err(ConfigError::NotUtf8 { .. })));
    }

    #[test]
    fn an_error_names_the_file_and_says_what_is_wrong() {
        let scratch = Scratch::new("display");
        let dir = path(&scratch.scope(), ConfigFile::Memory);
        fs::create_dir_all(&dir).unwrap();
        let message = read(&dir).unwrap_err().to_string();
        assert!(message.contains("memory.sexp"), "{message}");
        assert!(message.contains("not a regular file"), "{message}");

        let conflict = SaveError::Conflict {
            path: PathBuf::from("/x/.evo/lore.sexp"),
            reason: "changed on disk since it was opened",
        }
        .to_string();
        assert!(conflict.contains("lore.sexp"), "{conflict}");
        assert!(conflict.contains("not written"), "{conflict}");
    }

    /// The scratch file a write stages is created with `create_new`, so a symlink
    /// planted at its (guessable) name is not followed and not written through:
    /// whatever it points at is untouched, and the save still lands.
    #[test]
    #[cfg(unix)]
    fn a_planted_scratch_symlink_cannot_redirect_a_save() {
        let scratch = Scratch::new("planted-temp");
        let file = path(&scratch.scope(), ConfigFile::Init);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let victim = scratch.0.join("victim");
        fs::write(&victim, "do not touch").unwrap();
        std::os::unix::fs::symlink(&victim, paths::temp_candidate(&file, 0)).unwrap();

        write(&file, "(a)\n", None).expect("the save finds another scratch name");

        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch");
        assert!(paths::temp_candidate(&file, 0)
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&file).unwrap(), "(a)\n");
        assert!(!file.symlink_metadata().unwrap().file_type().is_symlink());
    }

    /// The check that runs just before the rename is the one that catches a writer
    /// that got in *after* the replacement was staged — the reason `write` looks
    /// twice instead of once.
    #[test]
    fn a_change_after_staging_is_caught_before_the_rename() {
        let scratch = Scratch::new("late-conflict");
        let file = path(&scratch.scope(), ConfigFile::Memory);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let base = "(:id \"mem-1\" :kind :fact :text \"first\")\n";
        fs::write(&file, base).unwrap();

        // What `write` does, in order — with the target changing in between.
        let staged = paths::Staged::stage(&file, b"(:id \"mem-1\" :text \"edited\")\n", 0o600)
            .expect("staged");
        let live = "(:id \"mem-1\" :kind :fact :text \"first\")\n(:id \"mem-2\")\n";
        fs::write(&file, live).unwrap();

        assert!(matches!(
            check_target(&file, Some(base)),
            Err(SaveError::Conflict {
                reason: "changed on disk since it was opened",
                ..
            })
        ));
        // What `write` does on that answer: drop the stage, rename nothing.
        drop(staged);
        assert_eq!(fs::read_to_string(&file).unwrap(), live);
        assert!(scratches(&file).is_empty(), "{:?}", scratches(&file));
    }

    /// A refused write leaves the file, its permissions and the directory exactly
    /// as they were — the scratch file it staged goes with it.
    #[test]
    fn a_refused_write_changes_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = Scratch::new("refused");
        let file = scratch.init_lisp();
        let base = "(a)\n";
        fs::write(&file, base).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(&file, "(a)\n(c)\n").unwrap();

        assert!(matches!(
            write(&file, "(b)\n", Some(base)),
            Err(SaveError::Conflict { .. })
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), "(a)\n(c)\n");
        assert_eq!(mode_of_path(&file), 0o640);
        assert!(scratches(&file).is_empty(), "{:?}", scratches(&file));
    }

    /// Every scratch file left beside `file` — which should be none, ever.
    fn scratches(file: &Path) -> Vec<String> {
        fs::read_dir(file.parent().unwrap())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.contains(".tmp-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The text comes from the handle that was opened, not from a second look at
    /// the path: swapping the file out from under an open one cannot change what
    /// is read, and the swap is refused the next time the path is opened.
    #[test]
    #[cfg(unix)]
    fn what_is_read_comes_from_the_opened_handle() {
        let scratch = Scratch::new("stable-handle");
        let file = scratch.init_lisp();
        fs::write(&file, "(the real one)\n").unwrap();
        let victim = scratch.0.join("victim");
        fs::write(&victim, "(somebody else's file)\n").unwrap();

        // Opened the way `read` opens it, and then the path is swapped for a
        // symlink to a file this process has no business reading.
        let mut opened = open_regular(&file).unwrap().expect("a file is there");
        fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&victim, &file).unwrap();

        let mut text = String::new();
        opened.read_to_string(&mut text).unwrap();
        assert_eq!(text, "(the real one)\n");

        // A fresh read sees the link, and refuses it rather than following it.
        assert!(matches!(read(&file), Err(ConfigError::Symlink { .. })));
    }

    /// A link to nothing is a link, not a missing file: it is refused, so a save
    /// never treats it as a file to create.
    #[test]
    #[cfg(unix)]
    fn a_symlink_to_nothing_is_refused_rather_than_missing() {
        let scratch = Scratch::new("dangling");
        let file = scratch.init_lisp();
        std::os::unix::fs::symlink(scratch.0.join("nowhere"), &file).unwrap();
        assert!(matches!(read(&file), Err(ConfigError::Symlink { .. })));
    }

    /// Opening a fifo for reading can wait for a writer. The read must refuse it
    /// instead — `O_NONBLOCK` is what makes that true, so this is the test that
    /// would notice if the open lost it: a blocked open never answers.
    #[test]
    #[cfg(unix)]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        use std::sync::mpsc;
        use std::time::Duration;

        let scratch = Scratch::new("fifo");
        let file = scratch.init_lisp();
        let name = CString::new(file.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path and a mode; the call only creates a fifo.
        let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo: {}", io::Error::last_os_error());

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read(&file).map(|_| ()));
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Err(ConfigError::NotRegular { .. })) => {}
            Ok(other) => panic!("expected NotRegular, got {other:?}"),
            Err(_) => panic!("reading a fifo blocked — a writer was waited for"),
        }
    }
}
