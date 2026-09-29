//! evo-desktop: the app shell (§7.1).
//!
//! Everything the window shows lives in the `workspace` crate; the process
//! around it — one instance, the window, the launch-time loading, the quit
//! sequence — is `evo_desktop`.

fn main() {
    evo_desktop::run();
}
