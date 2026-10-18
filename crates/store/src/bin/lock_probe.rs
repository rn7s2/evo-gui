//! Test helper: exercise the single-instance lock from a **separate process**.
//!
//! `tests/single_instance.rs` drives it; it is not part of the app.
//!
//! ```text
//! store-lock-probe <root> hold <seconds>   # take the lock, print, hold, exit
//! store-lock-probe <root> poke             # knock, or become the primary
//! ```
//!
//! Output is one line: `primary <pid>`, `secondary activated=true`, or
//! `secondary activated=false`. `hold` returns from `main` rather than calling
//! `exit`, so the lock and the socket are released the way the real app
//! releases them.

use std::process::ExitCode;

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use store::paths::Root;
#[cfg(unix)]
use store::single::SingleInstance;

/// Off Unix there is no lock to hold and no socket to knock on — the same reason
/// [`store::single`]'s coordination is Unix-only — so the helper has nothing to
/// exercise and says so rather than reporting a lock it never took.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("store-lock-probe: the single-instance lock is Unix only");
    ExitCode::from(2)
}

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(root), Some(mode)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: store-lock-probe <root> hold <seconds> | poke");
        return ExitCode::from(2);
    };
    let root = Root::at(root);

    match mode.as_str() {
        "hold" => {
            let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            match SingleInstance::acquire(&root) {
                Ok(SingleInstance::Primary(primary)) => {
                    println!("primary {}", primary.pid());
                    std::thread::sleep(Duration::from_secs(seconds));
                }
                Ok(SingleInstance::Secondary(s)) => println!("secondary activated={}", s.activated),
                Err(e) => {
                    eprintln!("store-lock-probe: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        "poke" => match SingleInstance::acquire(&root) {
            Ok(SingleInstance::Primary(primary)) => println!("primary {}", primary.pid()),
            Ok(SingleInstance::Secondary(s)) => println!("secondary activated={}", s.activated),
            Err(e) => {
                eprintln!("store-lock-probe: {e}");
                return ExitCode::FAILURE;
            }
        },
        other => {
            eprintln!("store-lock-probe: unknown mode {other:?}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}
