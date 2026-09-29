//! The About dialog (§7.1 polish): what the app is, which build, the binaries it
//! spawns, and where its state and log live.
//!
//! The picture in it is the app's own icon, embedded in the binary so the dialog
//! does not depend on where the bundle was put — the same file the bundle's
//! `AppIcon.icns` is built from (`assets/icon/make_icon.py`).
//!
//! The two binary versions are read with `--version` **once**, on a thread of its
//! own at startup: a probe is process work, and this dialog is opened from a menu
//! (an action handler, on the UI thread). What it found is kept on the [`Shell`],
//! so opening the dialog is a read.

use std::path::Path;
use std::sync::Arc;

use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, px, App, Image, ImageFormat, ImageSource, Window};

use crate::Shell;

/// The app's icon, at the size the bundle's `AppIcon.icns` was built from.
const APP_ICON: &[u8] = include_bytes!("../../../assets/icon/icon-1024.png");

/// How wide the picture is drawn: a dialog's header, not a splash screen.
const ICON_SIZE: f32 = 88.0;

/// What `--version` said about the two binaries. `None` is "could not be read"
/// (not there, not executable, or no `--version` to ask).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Versions {
    pub swarm: Option<String>,
    pub agent: Option<String>,
}

/// Run `bin --version` and take the first line it prints.
///
/// The whole line is kept: both binaries introduce themselves (`evo-swarm 0.1.0`),
/// which is more use in the dialog than a bare number, and it is what a person can
/// compare against what they built.
pub fn probe(bin: &Path) -> Option<String> {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.to_owned())
}

/// One binary's version, as the dialog shows it.
///
/// Both binaries introduce themselves (`evo-swarm 0.1.0`), and the row already
/// names them, so the name is dropped when the binary leads with it.
pub fn version_text(name: &str, probed: Option<&str>) -> String {
    let Some(line) = probed else {
        return "no --version".to_owned();
    };
    let line = line.trim();
    match line.strip_prefix(name).map(str::trim) {
        Some(rest) if !rest.is_empty() => rest.to_owned(),
        _ => line.to_owned(),
    }
}

/// Both binaries as one log line: what each answered, or that it did not.
pub fn summary(versions: &Versions) -> String {
    let line = |name: &str, probed: Option<&str>| match probed {
        Some(line) => line.to_owned(),
        None => format!("{name} (unknown)"),
    };
    format!(
        "{}, {}",
        line("evo-swarm", versions.swarm.as_deref()),
        line("evo-agent", versions.agent.as_deref())
    )
}

/// A path as a person reads it: `$HOME` becomes `~`.
pub fn tilde(path: &Path, home: Option<&str>) -> String {
    let text = path.display().to_string();
    match home {
        Some(home) if !home.is_empty() => match text.strip_prefix(home) {
            Some("") => "~".to_owned(),
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => text,
        },
        _ => text,
    }
}

/// Read both binaries' versions once, on a thread of their own, and keep them on
/// the [`Shell`] for the dialog (and the log).
pub fn start(cx: &mut App) {
    let (swarm, agent, log) = {
        let shell = cx.global::<Shell>();
        (
            shell.binaries.evo_swarm.clone(),
            shell.binaries.evo_agent.clone(),
            shell.log.clone(),
        )
    };
    let (tx, rx) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("evo-desktop-versions".to_owned())
        .spawn(move || {
            let versions = Versions {
                swarm: probe(&swarm),
                agent: probe(&agent),
            };
            let _ = tx.send_blocking(versions);
        });
    if let Err(error) = spawned {
        log.warn(format!("could not start the version probe: {error}"));
        return;
    }
    cx.spawn(async move |cx| {
        let Ok(versions) = rx.recv().await else {
            return;
        };
        cx.update(|cx| {
            let log = cx.global::<Shell>().log.clone();
            log.info(format!("versions: {}", summary(&versions)));
            cx.global_mut::<Shell>().versions = versions;
        });
    })
    .detach();
}

/// Open the dialog. Called from the About menu item, so the window is the app's
/// own and the call has to wait for it (see `menus::in_window_later`).
pub fn open(window: &mut Window, cx: &mut App) {
    let (root, log, versions, home) = {
        let shell = cx.global::<Shell>();
        (
            shell.root.clone(),
            shell.log.clone(),
            shell.versions.clone(),
            crate::launcher::home_string(),
        )
    };
    let app_version = env!("CARGO_PKG_VERSION");
    let state_dir = tilde(root.path(), home.as_deref());
    let log_path = tilde(log.path(), home.as_deref());

    window.open_alert_dialog(cx, move |alert, _window, cx| {
        alert
            .icon(icon())
            .title("Evo Desktop")
            .description(format!("Version {app_version}"))
            .child(
                v_flex()
                    .w_full()
                    .mt_2()
                    .gap_1()
                    .child(row(
                        cx,
                        "evo-swarm",
                        &version_text("evo-swarm", versions.swarm.as_deref()),
                    ))
                    .child(row(
                        cx,
                        "evo-agent",
                        &version_text("evo-agent", versions.agent.as_deref()),
                    )),
            )
            .child(
                v_flex()
                    .w_full()
                    .mt_2()
                    .gap_1()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .p_3()
                    .child(row(cx, "State", &state_dir))
                    .child(row(cx, "Log", &log_path)),
            )
            .width(px(430.))
            .show_cancel(false)
            .ok_text("Close")
    });
}

/// The app's own icon, embedded, drawn at a header's size.
fn icon() -> impl IntoElement {
    let image = Image::from_bytes(ImageFormat::Png, APP_ICON.to_vec());
    img(ImageSource::Image(Arc::new(image)))
        .w(px(ICON_SIZE))
        .h(px(ICON_SIZE))
        .rounded(px(18.))
}

/// One label-and-value line: the label is the calm part, the value carries the
/// text — a monospace face for anything a person might copy.
fn row(cx: &App, label: &'static str, value: &str) -> impl IntoElement {
    h_flex()
        .w_full()
        .gap_3()
        .items_start()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .whitespace_nowrap()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().foreground)
                .child(value.to_owned()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_version_is_what_the_binary_said_without_repeating_its_name() {
        assert_eq!(version_text("evo-swarm", Some("evo-swarm 0.1.0")), "0.1.0");
        assert_eq!(
            version_text("evo-agent", Some("  evo-agent   2.0.0  ")),
            "2.0.0"
        );
        // A binary that answers with something else keeps its whole line.
        assert_eq!(
            version_text("evo-swarm", Some("build 7f3c9d2")),
            "build 7f3c9d2"
        );
        assert_eq!(version_text("evo-swarm", Some("evo-swarm")), "evo-swarm");
        assert_eq!(version_text("evo-agent", None), "no --version");
    }

    #[test]
    fn the_log_line_names_both_binaries() {
        let versions = Versions {
            swarm: Some("evo-swarm 0.1.0".to_owned()),
            agent: None,
        };
        assert_eq!(summary(&versions), "evo-swarm 0.1.0, evo-agent (unknown)");
    }

    #[test]
    fn a_path_under_home_is_shortened() {
        let home = Some("/Users/x");
        assert_eq!(
            tilde(Path::new("/Users/x/.evo/desktop"), home),
            "~/.evo/desktop"
        );
        assert_eq!(tilde(Path::new("/Users/x"), home), "~");
        assert_eq!(tilde(Path::new("/tmp/elsewhere"), home), "/tmp/elsewhere");
        // `/Users/xx` is not inside `/Users/x`: the separator is the border.
        assert_eq!(tilde(Path::new("/Users/xx/y"), home), "/Users/xx/y");
        // No home to shorten around: the path is the path.
        assert_eq!(tilde(Path::new("/Users/x/.evo"), None), "/Users/x/.evo");
        assert_eq!(tilde(Path::new("/Users/x/.evo"), Some("")), "/Users/x/.evo");
    }

    #[test]
    fn the_icon_is_embedded_and_is_a_png() {
        assert!(APP_ICON.len() > 1024, "the icon travels with the binary");
        assert_eq!(&APP_ICON[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_binary_that_is_not_there_reads_as_no_version() {
        assert_eq!(probe(Path::new("/nonexistent/evo-swarm")), None);
    }

    #[test]
    fn a_binary_that_answers_is_read() {
        // `/bin/echo --version` prints its argument and succeeds: any program's
        // `--version` is read the same way.
        let line = probe(Path::new("/bin/echo")).expect("echo answers");
        assert_eq!(line, "--version");
    }

    #[test]
    fn a_binary_that_refuses_reads_as_no_version() {
        // `/usr/bin/false` succeeds at nothing.
        assert_eq!(probe(Path::new("/usr/bin/false")), None);
    }

    #[test]
    fn a_real_binary_of_this_project_can_be_probed() {
        // Not an assertion about the version: only that the shape the dialog
        // shows is what a real `--version` gives (skipped when not installed).
        let bin = PathBuf::from("/usr/local/bin/evo-swarm");
        if !bin.exists() {
            return;
        }
        let line = probe(&bin).expect("the installed evo-swarm answers --version");
        assert!(line.starts_with("evo-swarm"), "{line:?}");
    }
}
