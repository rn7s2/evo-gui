//! `~/.evo/desktop/` — the app's own data directory (§6 of the spec).
//!
//! ```text
//! ~/.evo/desktop/
//!   app.json           window bounds, tab set, binary paths, schema version
//!   lock               single-instance lock (flock; pid inside)
//!   model-cache.json   last /registry snapshot, for the empty tab's choosers
//!   probe/             scratch cwd used only to learn the model catalog
//!   tabs/<id>/         token (0600, written by the server), swarm.log, tab.json
//! ```
//!
//! Nothing here is ever written outside `root` — the sole exception is the
//! managed block in a project's `.evo/swarm.lisp` (§9.6, see [`crate::swarm_config`]).

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::time;

/// Name of the per-tab bearer-token file, as passed to the server's `--token-file`.
pub const TOKEN_FILE: &str = "token";
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
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
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
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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

/// `~/.evo/desktop/`, honouring `$HOME`.
pub fn default_root_dir() -> PathBuf {
    evo_dir().join("desktop")
}

/// `~/.evo/` — the evo home the rest of the app's layout hangs off.
pub fn evo_dir() -> PathBuf {
    home_dir().join(".evo")
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

    pub fn probe_dir(&self) -> PathBuf {
        self.path.join("probe")
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

    /// Where the server writes its bearer token (`--token-file`). We hand the
    /// path to the server and never write, read or log the file itself
    /// (§2 rule 4).
    pub fn tab_token(&self, id: &TabId) -> PathBuf {
        self.tab_dir(id).join(TOKEN_FILE)
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

    /// Create the scratch cwd the model-catalog probe runs in (§9.4).
    pub fn ensure_probe_dir(&self) -> io::Result<PathBuf> {
        self.ensure()?;
        let dir = self.probe_dir();
        create_dir_private(&dir)?;
        Ok(dir)
    }

    /// Delete a closed tab's directory (token, log, tab.json). A no-op when it
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_private(parent)?;
    let tmp = temp_sibling(path);
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    // The name is unique per process *and* per call: the app writes from its own
    // threads (§2 rule 6), and two of them writing one file must not share a
    // scratch file.
    static TMP_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = TMP_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp-{}-{n}", std::process::id()))
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
        assert_eq!(root.model_cache(), PathBuf::from("/tmp/whatever/model-cache.json"));
        assert_eq!(root.probe_dir(), PathBuf::from("/tmp/whatever/probe"));
        let id = TabId::new();
        assert_eq!(root.tab_dir(&id), PathBuf::from(format!("/tmp/whatever/tabs/{id}")));
        assert_eq!(root.tab_token(&id), root.tab_dir(&id).join("token"));
        assert_eq!(root.tab_log(&id), root.tab_dir(&id).join("swarm.log"));
    }

    #[test]
    #[cfg(unix)]
    fn directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = Root::at(temp("modes"));
        let id = TabId::new();
        let dir = root.ensure_tab_dir(&id).unwrap();
        assert_eq!(root.path().metadata().unwrap().permissions().mode() & 0o777, 0o700);
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
        let payloads: Vec<String> = (0..8).map(|i| format!("{{\"who\":{i},\"pad\":\"{}\"}}", "x".repeat(4096))).collect();
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
}
