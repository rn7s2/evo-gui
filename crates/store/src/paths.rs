//! `~/.evo/desktop/` — the app's own data directory (§6 of the spec).
//!
//! ```text
//! ~/.evo/desktop/
//!   app.json           window bounds, tab set, binary paths, schema version
//!   lock               single-instance lock (flock; pid inside)
//!   model-cache.json   last `catalog --json` body, for the empty tab's choosers
//!   tabs/<id>/         ready.json (0600, written by the server), swarm.log, tab.json
//! ```
//!
//! Nothing here is ever written outside `root`. The app writes no project file:
//! `swarm.lisp` belongs to the folder's author (§9, F3).

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::time;

/// Name of the per-tab ready file, as passed to the server's `--ready-file`: the
/// port, url and bearer token it writes once it is listening (§1).
pub const READY_FILE: &str = "ready.json";
/// Name of a tab's captured stdout+stderr.
pub const LOG_FILE: &str = "swarm.log";
/// Name of a tab's own descriptor.
pub const TAB_FILE: &str = "tab.json";
/// Name of the activation socket a secondary instance knocks on (§2 rule 1).
pub const ACTIVATE_SOCK: &str = "activate.sock";

/// Permissions for every file this crate creates: owner-only.
pub const FILE_MODE: u32 = 0o600;
/// Permissions for every directory this crate creates: owner-only.
pub const DIR_MODE: u32 = 0o700;

/// A tab's identity. Opaque, filesystem-safe, stable for the life of the tab.
///
/// Generated as a v4-shaped UUID; parsed values are checked before they are
/// ever joined onto a path (a hand-edited `app.json` must not escape `tabs/`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TabId(String);

impl TabId {
    /// A fresh random id, formatted as a UUID v4.
    pub fn new() -> TabId {
        let mut b = [0u8; 16];
        fill_random(&mut b);
        b[6] = (b[6] & 0x0f) | 0x40; // version 4
        b[8] = (b[8] & 0x3f) | 0x80; // variant 1
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        TabId(format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        ))
    }

    /// Accept a stored id only if it is safe to use as a single path segment.
    pub fn parse(s: &str) -> Option<TabId> {
        let ok = !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if ok {
            Some(TabId(s.to_owned()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TabId {
    fn default() -> Self {
        TabId::new()
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn fill_random(buf: &mut [u8]) {
    // /dev/urandom on every target we ship; the fallback never runs in practice
    // and only has to be good enough to keep two tabs' directories apart.
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    let mut seed = time::now_epoch() ^ (std::process::id() as u64) << 32;
    for b in buf.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (seed >> 33) as u8;
    }
}

/// The app's data root. Construct with [`Root::default`] (`~/.evo/desktop/`)
/// or [`Root::at`] for a test directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    path: PathBuf,
}

impl Default for Root {
    fn default() -> Root {
        Root::at(default_root_dir())
    }
}

/// `~/.evo/desktop/`, honouring `$EVO_HOME` and `$HOME`.
pub fn default_root_dir() -> PathBuf {
    evo_dir().join("desktop")
}

/// The evo home the rest of the app's layout hangs off: `$EVO_HOME` when it is
/// set and non-empty, else `~/.evo/`.
///
/// `EVO_HOME` is evo's own override (`evo-home`, `src/util/util.lisp`): a child
/// the app spawns reads its `init.lisp`, `swarm.lisp` and memory from there, so
/// the files the app shows a person have to resolve the same way. Both are empty
/// or unset on a normal machine, and the test home sets both to the same
/// directory, so this changes nothing unless somebody moved evo.
pub fn evo_dir() -> PathBuf {
    match std::env::var_os("EVO_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => home_dir().join(".evo"),
    }
}

/// `$HOME`, or the current directory when the environment has none.
pub fn home_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => PathBuf::from("."),
    }
}

impl Root {
    pub fn at(path: impl Into<PathBuf>) -> Root {
        Root { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn app_json(&self) -> PathBuf {
        self.path.join("app.json")
    }

    pub fn lock(&self) -> PathBuf {
        self.path.join("lock")
    }

    pub fn model_cache(&self) -> PathBuf {
        self.path.join("model-cache.json")
    }

    pub fn activate_sock(&self) -> PathBuf {
        self.path.join(ACTIVATE_SOCK)
    }

    pub fn tabs_dir(&self) -> PathBuf {
        self.path.join("tabs")
    }

    pub fn tab_dir(&self, id: &TabId) -> PathBuf {
        self.path.join("tabs").join(id.as_str())
    }

    pub fn tab_json(&self, id: &TabId) -> PathBuf {
        self.tab_dir(id).join(TAB_FILE)
    }

    /// Where the server writes its ready file (`--ready-file`). We hand the
    /// path to the server and never write the file ourselves; what it holds —
    /// the port, the url and the bearer token — is read from there and never
    /// logged (§2 rule 4).
    pub fn tab_ready(&self, id: &TabId) -> PathBuf {
        self.tab_dir(id).join(READY_FILE)
    }

    /// Where a tab's `evo-swarm serve` stdout+stderr go (§3).
    pub fn tab_log(&self, id: &TabId) -> PathBuf {
        self.tab_dir(id).join(LOG_FILE)
    }

    /// Create the whole layout, owner-only.
    pub fn ensure(&self) -> io::Result<()> {
        create_dir_private(&self.path)
    }

    /// Create the root plus `tabs/<id>/`, ready for a server to be spawned in.
    pub fn ensure_tab_dir(&self, id: &TabId) -> io::Result<PathBuf> {
        self.ensure()?;
        create_dir_private(&self.tabs_dir())?;
        let dir = self.tab_dir(id);
        create_dir_private(&dir)?;
        Ok(dir)
    }

    /// Delete a closed tab's directory (ready file, log, tab.json). A no-op when it
    /// is already gone; refuses anything that is not a plain directory inside
    /// `tabs/`.
    pub fn remove_tab_dir(&self, id: &TabId) -> io::Result<()> {
        let dir = self.tab_dir(id);
        if !dir.is_dir() {
            return Ok(());
        }
        let inside = dir.parent() == Some(self.tabs_dir().as_path());
        if !inside {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to remove {}", dir.display()),
            ));
        }
        fs::remove_dir_all(&dir)
    }
}

/// `mkdir -p` with mode 0700 (never widened for an existing directory).
pub fn create_dir_private(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// Write bytes atomically: a sibling temp file, fsync, rename over the target.
///
/// A reader never sees a half-written file, and a crash mid-write leaves the
/// previous contents intact.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    Staged::stage(path, bytes, mode)?.commit()
}

/// How many scratch names one write may try before giving up. Every try is a new
/// name, so this is only a guard against a directory that will not hold a file at
/// all; a handful of concurrent writers needs a handful of names.
const TEMP_ATTEMPTS: u32 = 1024;

/// A replacement for a file, staged beside it and not yet in place.
///
/// The bytes are already on disk — written, flushed, and wearing their final
/// permissions — so the only thing between a caller and the new file is
/// [`commit`](Staged::commit), one rename. A caller that has to check something
/// still holds does it *after* staging: the gap between "the check said yes" and
/// the file being replaced is then the rename itself.
///
/// Nothing is left behind: a `Staged` dropped without committing removes its
/// scratch file, and so does one whose commit failed.
pub(crate) struct Staged {
    target: PathBuf,
    /// The scratch file, while it is still this write's to remove.
    ///
    /// A committed stage has none: its file has *become* the target, and the name it
    /// was staged at is free again for whoever asks for it next. Removing that name
    /// then would take the file out from under another writer — see [`Staged::commit`].
    temp: Option<PathBuf>,
}

impl Staged {
    /// Put `bytes` in a fresh, private scratch file beside `target`.
    ///
    /// The scratch file is opened with `create_new` (`O_CREAT | O_EXCL`), so an
    /// entry already at the name — a symlink somebody planted there included — is
    /// never followed and never written through: that attempt is abandoned
    /// untouched and the next name is tried. A missing parent directory is
    /// created owner-only, as everywhere else in this crate.
    pub(crate) fn stage(target: &Path, bytes: &[u8], mode: u32) -> io::Result<Staged> {
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        create_dir_private(parent)?;

        let mut last = None;
        for attempt in 0..TEMP_ATTEMPTS {
            let temp = temp_candidate(target, attempt);
            match stage_at(target, &temp, bytes, mode) {
                Ok(staged) => return Ok(staged),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "no free scratch name")
        }))
    }

    /// Put it in place: one rename, which replaces the target atomically.
    ///
    /// The rename is the commit, so the scratch name is free the instant it lands —
    /// and another writer, racing for the same target, may already be staging at it.
    /// This clears its own hold on the name *before* the drop that would remove it:
    /// a drop at that point would delete that other writer's scratch file, and its
    /// commit would fail with `NotFound` — one whole write lost, not merely delayed.
    /// (A drop that still holds the name removes it, which is the cleanup for a stage
    /// that failed or was never committed.)
    pub(crate) fn commit(mut self) -> io::Result<()> {
        let temp = self
            .temp
            .as_ref()
            .expect("a staged file has a scratch path");
        // A rename that fails leaves the scratch file staged, and `self` drops with it.
        fs::rename(temp, &self.target)?;
        self.temp = None;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        // The cleanup for a stage that failed, or was never committed. A missing file
        // is not an error.
        if let Some(temp) = self.temp.take() {
            let _ = fs::remove_file(temp);
        }
    }
}

/// The scratch name for one attempt: `.{name}.tmp-{pid}-{attempt}`.
///
/// It is guessable on purpose — it does not have to be unguessable, because
/// `create_new` is what makes the write safe: whoever loses the race for a name
/// simply moves to the next number, and whatever they left at that name is never
/// touched.
pub(crate) fn temp_candidate(path: &Path, attempt: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp-{}-{attempt}", std::process::id()))
}

/// Create the scratch file — never replacing anything — fill it, flush it, and
/// give it its final permissions, all before anybody can see it.
fn stage_at(target: &Path, temp: &Path, bytes: &[u8], mode: u32) -> io::Result<Staged> {
    let mut opts = fs::OpenOptions::new();
    // `create_new` is `O_CREAT | O_EXCL`: it fails on anything already at the
    // name, and it does not follow a symlink. This is the whole defence against a
    // planted scratch file — everything below this line only ever touches a file
    // this call created.
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(mode);
    }
    let mut file = opts.open(temp)?;

    let sealed = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        // Exact permissions, on the scratch file, *before* the rename: nothing
        // after the commit may fail, and the file that appears already has them.
        .and_then(|()| set_mode(temp, mode));
    drop(file);
    match sealed {
        Ok(()) => Ok(Staged {
            target: target.to_path_buf(),
            temp: Some(temp.to_path_buf()),
        }),
        Err(error) => {
            let _ = fs::remove_file(temp);
            Err(error)
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

/// Serialize `value` as pretty JSON and write it atomically (mode 0600).
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes, FILE_MODE)
}

/// Read and parse a JSON file.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Copy `path` to `<path>.bak`, replacing an older backup. A missing source is
/// not an error — there is simply nothing to keep.
pub fn backup(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bak = backup_path(path);
    match fs::copy(path, &bak) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Move a file we could not parse aside as `<path>.bak`, so the user's data is
/// not silently destroyed. An existing backup is never clobbered.
pub fn quarantine(path: &Path) -> io::Result<()> {
    let bak = backup_path(path);
    if bak.exists() || !path.exists() {
        return Ok(());
    }
    fs::rename(path, &bak)
}

/// `<path>` with a `.bak` extension appended (`app.json` → `app.json.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".bak");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("store-paths-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn tab_ids_are_unique_and_path_safe() {
        let a = TabId::new();
        let b = TabId::new();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 36);
        assert_eq!(a.as_str().matches('-').count(), 4);
        assert!(TabId::parse(a.as_str()).is_some());
        assert!(TabId::parse("../../etc").is_none());
        assert!(TabId::parse("a/b").is_none());
        assert!(TabId::parse("").is_none());
        assert!(TabId::parse(&"x".repeat(65)).is_none());
    }

    #[test]
    fn layout_matches_the_spec() {
        let root = Root::at("/tmp/whatever");
        assert_eq!(root.app_json(), PathBuf::from("/tmp/whatever/app.json"));
        assert_eq!(root.lock(), PathBuf::from("/tmp/whatever/lock"));
        assert_eq!(
            root.model_cache(),
            PathBuf::from("/tmp/whatever/model-cache.json")
        );
        let id = TabId::new();
        assert_eq!(
            root.tab_dir(&id),
            PathBuf::from(format!("/tmp/whatever/tabs/{id}"))
        );
        assert_eq!(root.tab_ready(&id), root.tab_dir(&id).join("ready.json"));
        assert_eq!(root.tab_log(&id), root.tab_dir(&id).join("swarm.log"));
    }

    #[test]
    #[cfg(unix)]
    fn directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = Root::at(temp("modes"));
        let id = TabId::new();
        let dir = root.ensure_tab_dir(&id).unwrap();
        assert_eq!(
            root.path().metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        root.remove_tab_dir(&id).unwrap();
        assert!(!dir.exists());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_temp() {
        let dir = temp("atomic");
        let path = dir.join("app.json");
        write_atomic(&path, b"{\"a\":1}", FILE_MODE).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        write_atomic(&path, b"{\"a\":2}", FILE_MODE).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn concurrent_writes_never_mix_contents() {
        // The app writes from its own threads; two of them writing one file must
        // not share a scratch file, and the file must always be one whole write.
        let dir = temp("concurrent");
        let path = dir.join("app.json");
        let payloads: Vec<String> = (0..8)
            .map(|i| format!("{{\"who\":{i},\"pad\":\"{}\"}}", "x".repeat(4096)))
            .collect();
        std::thread::scope(|scope| {
            for payload in &payloads {
                let path = path.clone();
                scope.spawn(move || {
                    for _ in 0..25 {
                        write_atomic(&path, payload.as_bytes(), FILE_MODE).unwrap();
                    }
                });
            }
        });
        let text = fs::read_to_string(&path).unwrap();
        assert!(payloads.contains(&text), "a torn write: {text:.80}");
        let leftovers = fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1, "scratch files left behind");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quarantine_keeps_the_unparsable_file() {
        let dir = temp("quarantine");
        let path = dir.join("app.json");
        write_atomic(&path, b"not json", FILE_MODE).unwrap();
        quarantine(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), "not json");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The scratch name is guessable, so somebody can plant something at it. The
    /// write must not follow it: the plant is left alone, whatever it points at
    /// is never opened, and the bytes go to the next free name.
    #[test]
    #[cfg(unix)]
    fn a_planted_scratch_symlink_is_not_followed() {
        let dir = temp("planted");
        fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        fs::write(&victim, "do not touch").unwrap();
        let path = dir.join("app.json");

        let planted = temp_candidate(&path, 0);
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        write_atomic(&path, b"{\"a\":1}", FILE_MODE).unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch");
        assert!(planted.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        assert!(!path.symlink_metadata().unwrap().file_type().is_symlink());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A staged file is finished before it is in place: the bytes are there, the
    /// permissions are the final ones, and the target has not been touched. One
    /// that is dropped instead of committed leaves nothing at all.
    #[test]
    #[cfg(unix)]
    fn a_stage_carries_its_final_permissions_and_a_drop_cleans_up() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp("stage");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.json");

        let staged = Staged::stage(&path, b"{\"a\":1}", 0o640).unwrap();
        let scratch = staged
            .temp
            .clone()
            .expect("a staged file has a scratch path to clean up");
        assert!(scratch.exists());
        assert_eq!(fs::read(&scratch).unwrap(), b"{\"a\":1}");
        assert_eq!(
            scratch.metadata().unwrap().permissions().mode() & 0o777,
            0o640
        );
        // Nothing has been replaced yet.
        assert!(!path.exists());

        drop(staged);
        assert!(!scratch.exists());
        assert!(!path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A committed stage holds nothing afterwards: the rename took its scratch file,
    /// and the cleanup a failed or abandoned stage runs is not run at all.
    ///
    /// That is what makes the *name* safe to free. Another writer racing for the same
    /// target stages at the same names — the name carries the pid and the attempt, so
    /// it reaches the same candidates — and a drop that removed "its" scratch file
    /// after the rename would remove theirs instead: their commit then fails with
    /// `NotFound` and a whole write is lost. The interleaving is a window of
    /// microseconds, which is what `concurrent_writes_never_mix_contents` below is
    /// there to hit; this pins the state it depends on.
    #[test]
    #[cfg(unix)]
    fn a_committed_stage_holds_nothing_to_clean_up() {
        let dir = temp("committed-stage");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.json");

        let staged = Staged::stage(&path, b"{\"a\":1}", 0o600).unwrap();
        let scratch = staged.temp.clone().expect("a scratch path");
        staged.commit().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        assert!(
            !scratch.exists(),
            "the rename took the scratch file, and the commit left nothing behind it"
        );
        // Nothing else of this write's is left to run, so the name is the next
        // writer's: whatever stages there now owns it.
        fs::write(&scratch, b"{\"b\":2}").unwrap();
        assert_eq!(fs::read(&scratch).unwrap(), b"{\"b\":2}");

        fs::remove_file(&scratch).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The scratch name is a new one per attempt, so a name that is taken is not
    /// an error — the write moves on.
    #[test]
    fn a_taken_scratch_name_moves_to_the_next_attempt() {
        let dir = temp("taken");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.json");
        // Something is already at the first name.
        fs::write(temp_candidate(&path, 0), "in the way").unwrap();

        write_atomic(&path, b"{\"a\":1}", FILE_MODE).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        assert_eq!(
            fs::read_to_string(temp_candidate(&path, 0)).unwrap(),
            "in the way"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
