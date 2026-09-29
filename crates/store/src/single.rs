//! One instance only (§2 rule 1).
//!
//! Two halves, both needed:
//!
//! * an `flock(LOCK_EX|LOCK_NB)` on `~/.evo/desktop/lock`, with our pid written
//!   inside. The kernel drops the lock when the process dies — however it dies —
//!   so a later launch is never blocked by a stale lock, and there is nothing to
//!   clean up.
//! * an activation socket (`~/.evo/desktop/activate.sock`). A second launch
//!   cannot take the lock, so it knocks: it connects, writes `activate <pid>`,
//!   and returns so its `main` can exit 0 while the running instance raises its
//!   window.
//!
//! ```no_run
//! use store::single::SingleInstance;
//! use store::paths::Root;
//! match SingleInstance::acquire(&Root::default()).unwrap() {
//!     SingleInstance::Primary(p) => { /* run the app; poll p.try_activation() */ }
//!     SingleInstance::Secondary(_) => { /* exit 0 */ }
//! }
//! ```

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::paths::{self, Root};

/// The verb a secondary sends to the primary.
pub const ACTIVATE: &str = "activate";

/// How long a secondary keeps trying to reach the primary's socket. The primary
/// creates it moments after taking the lock, so this only covers that gap.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the accept loop waits between non-blocking `accept` calls.
///
/// The loop cannot block in `accept`: shutdown must not depend on a wake-up
/// connection, because the socket file can be gone (a killed predecessor, a
/// removed directory, a test that cleans up first) and `accept` on a dead path
/// would then never return. One syscall every 25 ms is the price of a `Drop`
/// that always terminates.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// How long the primary waits for a knocked connection to say something.
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Outcome of [`SingleInstance::acquire`].
#[derive(Debug)]
pub enum SingleInstance {
    /// We hold the lock: this process is the app.
    Primary(Primary),
    /// Another process holds the lock; we asked it to raise its window.
    Secondary(Secondary),
}

/// The process that lost the lock race.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Secondary {
    /// True when the running instance acknowledged the knock. Either way the
    /// caller exits 0: the app is up, which is what the user asked for.
    pub activated: bool,
}

/// A message from a secondary instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    /// The verb (`activate`).
    pub command: String,
    /// The secondary's pid, when it sent one.
    pub pid: Option<u32>,
    /// The line as received.
    pub raw: String,
}

impl Activation {
    fn parse(line: &str) -> Activation {
        let raw = line.trim().to_string();
        let mut parts = raw.split_whitespace();
        let command = parts.next().unwrap_or_default().to_string();
        let pid = parts.next().and_then(|p| p.parse::<u32>().ok());
        Activation { command, pid, raw }
    }
}

impl SingleInstance {
    /// Take the lock, or knock on the instance that holds it.
    ///
    /// Creates the root if it does not exist. A lock file left behind by a dead
    /// process is taken over: the lock itself died with that process.
    pub fn acquire(root: &Root) -> io::Result<SingleInstance> {
        root.ensure()?;
        let file = open_lock_file(&root.lock())?;
        match file.try_lock() {
            Ok(()) => Ok(SingleInstance::Primary(Primary::claim(root, file)?)),
            Err(std::fs::TryLockError::WouldBlock) => {
                Ok(SingleInstance::Secondary(notify_blocked(root)))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Convenience over [`SingleInstance::acquire`] with `~/.evo/desktop/`.
    pub fn acquire_default() -> io::Result<SingleInstance> {
        SingleInstance::acquire(&Root::default())
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        paths::create_dir_private(parent)?;
    }
    OpenOptions::new().read(true).write(true).create(true).mode(paths::FILE_MODE).open(path)
}

/// Knocked but not let in: announce ourselves to whoever holds the lock.
fn notify_blocked(root: &Root) -> Secondary {
    let sock = root.activate_sock();
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Ok(mut stream) = UnixStream::connect(&sock) {
            let message = format!("{ACTIVATE} {}\n", std::process::id());
            if stream.write_all(message.as_bytes()).is_ok() && stream.flush().is_ok() {
                // The primary may take a moment to read it; it is queued in the
                // socket either way, so there is nothing to wait for here.
                return Secondary { activated: true };
            }
        }
        if Instant::now() >= deadline {
            return Secondary { activated: false };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The running instance: holds the lock and listens for knocks.
pub struct Primary {
    /// Holding this open is what holds the lock. Never read: it is an RAII
    /// handle, and dropping it releases the lock.
    #[allow(dead_code)]
    lock: File,
    lock_path: PathBuf,
    /// Held for the lifetime of the primary: it is what keeps the socket
    /// bound. The accept thread works on a clone; dropping this with the
    /// primary is what finally takes the socket away.
    #[allow(dead_code)]
    listener: UnixListener,
    rx: Receiver<Activation>,
    sock_path: PathBuf,
    running: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Primary {
    fn claim(root: &Root, lock: File) -> io::Result<Primary> {
        // The pid is inside for a human reading the file; the lock, not the
        // file, is what says "running".
        lock.set_len(0)?;
        {
            let mut f = &lock;
            f.write_all(format!("{}\n", std::process::id()).as_bytes())?;
            f.flush()?;
        }

        let sock_path = root.activate_sock();
        let listener = bind_activation_socket(&sock_path)?;
        // The accept loop polls rather than blocking, so that dropping the
        // primary always terminates (see `ACCEPT_POLL`).
        listener.set_nonblocking(true)?;
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let accept = {
            let running = Arc::clone(&running);
            let thread_listener = listener.try_clone()?;
            std::thread::Builder::new()
                .name("store-activation".into())
                .spawn(move || accept_loop(thread_listener, tx, running))?
        };

        Ok(Primary { lock, lock_path: root.lock(), listener, rx, sock_path, running, accept: Some(accept) })
    }

    /// This process's pid, as recorded in the lock file.
    pub fn pid(&self) -> u32 {
        std::process::id()
    }

    /// Path of the lock file we hold.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Path of the activation socket.
    pub fn socket_path(&self) -> &Path {
        &self.sock_path
    }

    /// The channel a secondary's knock arrives on.
    pub fn activation_rx(&self) -> &Receiver<Activation> {
        &self.rx
    }

    /// A knock, if one is waiting. Call this from the UI's event loop.
    pub fn try_activation(&self) -> Option<Activation> {
        // `Empty` (no knock yet) and `Disconnected` (the accept thread is gone)
        // both mean the same thing to the caller.
        self.rx.try_recv().ok()
    }

    /// Wait up to `timeout` for a knock.
    pub fn recv_activation(&self, timeout: Duration) -> Option<Activation> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Drop for Primary {
    fn drop(&mut self) {
        // The accept loop polls, so this flag alone stops it; the connection is
        // just a nudge so it stops on the next instant rather than the next
        // tick. It may fail — the socket file is not ours to rely on.
        self.running.store(false, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.sock_path);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

impl std::fmt::Debug for Primary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Primary").field("pid", &self.pid()).field("socket", &self.sock_path).finish()
    }
}

/// Bind the activation socket, clearing a file left by a process that died
/// without unlinking it (a stale socket, or anything else sitting there).
fn bind_activation_socket(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        paths::create_dir_private(parent)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match UnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // Something recreated it between the unlink and the bind.
            std::fs::remove_file(path)?;
            UnixListener::bind(path)
        }
        Err(e) => Err(e),
    }
}

fn accept_loop(listener: UnixListener, tx: mpsc::Sender<Activation>, running: Arc<AtomicBool>) {
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                handle_connection(stream, &tx);
            }
            // Nothing waiting: sleep a tick and look again.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // A closed listener is how the loop ends if the socket ever dies
            // under it.
            Err(_) => break,
        }
    }
}

/// One short line in, one ack out. A peer that connects and says nothing is
/// dropped after the read timeout, so the loop always comes back.
fn handle_connection(stream: UnixStream, tx: &mpsc::Sender<Activation>) {
    // An accepted socket may inherit the listener's non-blocking flag; put it
    // back, so a read means "wait for the line", not "try once".
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = String::new();
    if let Ok(reader) = stream.try_clone() {
        let mut reader = BufReader::new(reader).take(4096);
        let _ = reader.read_line(&mut line);
    }
    let activation = Activation::parse(&line);
    if activation.command == ACTIVATE {
        let _ = tx.send(activation);
    }
    let mut out = &stream;
    let _ = out.write_all(b"ok\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-si-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    #[test]
    fn first_acquires_second_knocks() {
        let root = temp_root("once");
        let first = SingleInstance::acquire(&root).unwrap();
        let primary = match first {
            SingleInstance::Primary(p) => p,
            SingleInstance::Secondary(_) => panic!("first acquire must win the lock"),
        };
        assert!(root.lock().exists());
        assert_eq!(fs::read_to_string(root.lock()).unwrap().trim(), std::process::id().to_string());
        assert!(root.activate_sock().exists());

        let second = SingleInstance::acquire(&root).unwrap();
        match second {
            SingleInstance::Secondary(s) => assert!(s.activated),
            SingleInstance::Primary(_) => panic!("second acquire must not win the lock"),
        }
        let activation = primary.recv_activation(Duration::from_secs(5)).expect("knock");
        assert_eq!(activation.command, ACTIVATE);
        assert_eq!(activation.pid, Some(std::process::id()));

        drop(primary);
        assert!(!root.activate_sock().exists(), "the socket goes with the primary");
        assert!(matches!(SingleInstance::acquire(&root).unwrap(), SingleInstance::Primary(_)));
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn a_secondary_without_a_primary_is_not_activated() {
        let root = temp_root("noprimary");
        root.ensure().unwrap();
        // Hold the lock on a file handle but never publish a socket: the knock
        // cannot land, and the caller is told so rather than hanging.
        let file = open_lock_file(&root.lock()).unwrap();
        file.try_lock().unwrap();
        let second = SingleInstance::acquire(&root).unwrap();
        match second {
            SingleInstance::Secondary(s) => assert!(!s.activated),
            SingleInstance::Primary(_) => panic!("the lock is held"),
        }
        drop(file);
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn lock_dies_with_the_holder() {
        let root = temp_root("deadlock");
        {
            let first = SingleInstance::acquire(&root).unwrap();
            assert!(matches!(first, SingleInstance::Primary(_)));
            assert!(matches!(SingleInstance::acquire(&root).unwrap(), SingleInstance::Secondary(_)));
        } // both the lock and the socket are gone here
        assert!(matches!(SingleInstance::acquire(&root).unwrap(), SingleInstance::Primary(_)));
        // The lock file itself stays behind — that is fine, and is not stale.
        assert!(root.lock().exists());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn a_stale_socket_file_is_replaced() {
        let root = temp_root("stale");
        root.ensure().unwrap();
        fs::write(root.activate_sock(), b"not a socket").unwrap();
        let primary = match SingleInstance::acquire(&root).unwrap() {
            SingleInstance::Primary(p) => p,
            SingleInstance::Secondary(_) => panic!("no lock is held"),
        };
        assert!(root.activate_sock().exists());
        match SingleInstance::acquire(&root).unwrap() {
            SingleInstance::Secondary(s) => assert!(s.activated),
            SingleInstance::Primary(_) => panic!("primary holds the lock"),
        }
        assert!(primary.recv_activation(Duration::from_secs(5)).is_some());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn activation_parsing() {
        assert_eq!(
            Activation::parse("activate 4711\n"),
            Activation { command: "activate".into(), pid: Some(4711), raw: "activate 4711".into() }
        );
        assert_eq!(Activation::parse("activate").command, "activate");
        assert_eq!(Activation::parse("bogus").command, "bogus");
        assert_eq!(Activation::parse("  ").command, "");
    }
}
