//! The managed block in a project's `.evo/swarm.lisp` (§9.6).
//!
//! evo-swarm has no lanes-model flag: a lane's default model is the
//! coordinator's unless the project's `swarm.lisp` overrides it. So when the
//! user picks a lanes model in the empty tab, we write it there — at the **top**
//! of the file, because `in-lanes` forms run in order and the last one wins, so
//! whatever the user wrote below keeps the last word.
//!
//! ```lisp
//! ;;; evo-desktop:begin — the lanes' default model (managed; edit outside the markers)
//! (evo.swarm:in-lanes ()
//!   (evo:set-setting :model "ark-deepseek-v4.1-flash")
//!   (evo:set-setting :model-provider :aiden))
//! ;;; evo-desktop:end
//! ```
//!
//! Both lines matter: a model id alone fails when that id is registered under
//! another provider (`find-model` refuses a model/provider mismatch), so the
//! provider is written too — both come from `/registry`.
//!
//! The file belongs to the folder, not the tab: two tabs in one folder share
//! it, and the newest write decides what the next lane initialization gets.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::paths;

/// First line of the block we own.
pub const BLOCK_BEGIN: &str = ";;; evo-desktop:begin";
/// Last line of the block we own.
pub const BLOCK_END: &str = ";;; evo-desktop:end";

/// The comment that opens the block, as §9.6 shows it.
const BLOCK_TITLE: &str =
    ";;; evo-desktop:begin — the lanes' default model (managed; edit outside the markers)";

/// `<folder>/.evo/swarm.lisp` — the file the coordinator reads for a project.
pub fn swarm_lisp_path(folder: &Path) -> PathBuf {
    folder.join(".evo").join("swarm.lisp")
}

/// The lanes model chosen for a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanesModel {
    pub model: String,
    /// Provider keyword, without the colon (`aiden`, `anthropic`).
    pub provider: String,
}

impl LanesModel {
    pub fn new(model: impl Into<String>, provider: impl Into<String>) -> LanesModel {
        LanesModel {
            model: model.into(),
            provider: provider.into(),
        }
    }
}

/// What [`set_lanes_model`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The block is now at the top of the file (created, or replaced in place).
    Written,
    /// The block was removed; the file survives with the rest of its content.
    Removed,
    /// The block was removed and the file deleted, because nothing else was left.
    FileDeleted,
    /// The file already says exactly this — nothing was written.
    Unchanged,
    /// Nothing to do: no file, and **Default** was chosen.
    Absent,
}

/// Write (or remove) the lanes-model block in `<folder>/.evo/swarm.lisp`.
///
/// `None` is the chooser's **Default**: remove our block, delete the file when
/// nothing else is left, and never touch the user's own lines either way.
/// The write is idempotent — the same call twice produces the same bytes, and
/// a second call writes nothing at all.
pub fn set_lanes_model(folder: &Path, model: Option<&LanesModel>) -> io::Result<WriteOutcome> {
    let path = swarm_lisp_path(folder);
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };

    match model {
        Some(model) => {
            let block = render_block(model)?;
            let current = existing.as_deref().unwrap_or("");
            let wanted = compose(current, &block);
            if existing.as_deref() == Some(wanted.as_str()) {
                return Ok(WriteOutcome::Unchanged);
            }
            paths::create_dir_private(path.parent().unwrap_or(folder))?;
            paths::write_atomic(&path, wanted.as_bytes(), paths::FILE_MODE)?;
            Ok(WriteOutcome::Written)
        }
        None => {
            let Some(current) = existing else {
                return Ok(WriteOutcome::Absent);
            };
            let (rest, found) = remove_blocks(&current);
            if !found {
                return Ok(WriteOutcome::Unchanged);
            }
            let rest = strip_leading_blank_lines(&rest);
            if rest.trim().is_empty() {
                fs::remove_file(&path)?;
                return Ok(WriteOutcome::FileDeleted);
            }
            paths::write_atomic(&path, rest.as_bytes(), paths::FILE_MODE)?;
            Ok(WriteOutcome::Removed)
        }
    }
}

/// Whether `<folder>/.evo/swarm.lisp` holds a block of ours.
pub fn has_lanes_model_block(folder: &Path) -> bool {
    match fs::read_to_string(swarm_lisp_path(folder)) {
        Ok(text) => remove_blocks(&text).1,
        Err(_) => false,
    }
}

/// The block, exactly as it will sit in the file, ending with a newline.
pub fn render_block(model: &LanesModel) -> io::Result<String> {
    let provider = provider_keyword(&model.provider).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{:?} is not usable as a provider keyword", model.provider),
        )
    })?;
    if model.model.trim().is_empty() || model.model.contains('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{:?} is not usable as a model id", model.model),
        ));
    }
    // The two settings are the *body* of the `in-lanes` form: closing the form on
    // its own line would make them top-level forms of the project's `swarm.lisp`
    // — evaluated in the coordinator, which is the one thing §9.6 must not do —
    // and leave a stray `)` behind, which stops the reader at whatever the user
    // wrote next.
    Ok(format!(
        "{BLOCK_TITLE}\n(evo.swarm:in-lanes ()\n  (evo:set-setting :model {})\n  (evo:set-setting :model-provider :{provider}))\n{BLOCK_END}\n",
        lisp_string(&model.model)
    ))
}

/// The file with our block at the top and everything else untouched.
fn compose(content: &str, block: &str) -> String {
    let (rest, _) = remove_blocks(content);
    let rest = strip_leading_blank_lines(&rest);
    if rest.trim().is_empty() {
        return block.to_string();
    }
    let mut out = String::with_capacity(block.len() + rest.len() + 1);
    out.push_str(block);
    out.push('\n');
    out.push_str(rest);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Drop every run of lines from a begin marker through its end marker.
/// Returns the remaining text and whether anything was removed. A begin marker
/// with no end marker is ours and is taken to the end of the file — a
/// half-written block must never survive to be doubled up later.
fn remove_blocks(content: &str) -> (String, bool) {
    let mut out = String::with_capacity(content.len());
    let mut found = false;
    let mut in_block = false;
    for line in content.split_inclusive('\n') {
        let marker = line.trim_start();
        if in_block {
            if marker.starts_with(BLOCK_END) {
                in_block = false;
            }
            continue;
        }
        if marker.starts_with(BLOCK_BEGIN) {
            in_block = true;
            found = true;
            continue;
        }
        out.push_str(line);
    }
    (out, found)
}

/// Cut the whitespace-only lines at the start of `text`, keeping the first
/// real line byte for byte.
fn strip_leading_blank_lines(text: &str) -> &str {
    let mut idx = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            idx += line.len();
        } else {
            break;
        }
    }
    &text[idx..]
}

/// A provider name usable as a Lisp keyword, lower-cased and without the colon.
fn provider_keyword(provider: &str) -> Option<String> {
    let name = provider.trim().trim_start_matches(':').to_ascii_lowercase();
    let mut chars = name.chars();
    let first = chars.next()?;
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.' | '_'));
    if (first.is_ascii_alphabetic() || first == '_') && rest_ok {
        Some(name)
    } else {
        None
    }
}

/// A Lisp string literal for `s`.
fn lisp_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("store-swarm-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(folder: &Path) -> String {
        fs::read_to_string(swarm_lisp_path(folder)).unwrap()
    }

    const BLOCK: &str =
        ";;; evo-desktop:begin — the lanes' default model (managed; edit outside the markers)\n\
                         (evo.swarm:in-lanes ()\n  \
                         (evo:set-setting :model \"ark-deepseek-v4.1-flash\")\n  \
                         (evo:set-setting :model-provider :aiden))\n\
                         ;;; evo-desktop:end\n";

    #[test]
    fn the_block_is_exactly_the_spec_form() {
        let rendered = render_block(&LanesModel::new("ark-deepseek-v4.1-flash", "aiden")).unwrap();
        assert_eq!(rendered, BLOCK);
        // The two settings evo actually reads (see swarm/init.lisp's baseline-forms).
        assert!(rendered.contains("(evo.swarm:in-lanes ()"));
        assert!(rendered.contains("(evo:set-setting :model \"ark-deepseek-v4.1-flash\")"));
        assert!(rendered.contains("(evo:set-setting :model-provider :aiden)"));
        // They have to be *inside* it: a form closed on the `in-lanes` line leaves
        // them as the coordinator's own top-level settings (§9.6 is about the lanes)
        // and a stray `)` that stops evo's reader at whatever the user wrote below.
        let body = rendered
            .split_once("(evo.swarm:in-lanes ()")
            .expect("the form")
            .1;
        assert_eq!(
            body.matches('(').count(),
            2,
            "the two settings are its body"
        );
        assert_eq!(
            body.matches(')').count(),
            3,
            "and the form closes after them"
        );
    }

    #[test]
    fn creates_the_directory_and_the_file() {
        let folder = temp_dir("create");
        assert!(!folder.join(".evo").exists());
        let outcome = set_lanes_model(
            &folder,
            Some(&LanesModel::new("ark-deepseek-v4.1-flash", "aiden")),
        )
        .unwrap();
        assert_eq!(outcome, WriteOutcome::Written);
        assert!(folder.join(".evo").exists());
        assert_eq!(read(&folder), BLOCK);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn is_idempotent_and_keeps_the_users_lines_verbatim() {
        let folder = temp_dir("idempotent");
        let user = ";; my project\n(evo:set-setting :model \"coordinator\")\n\n(evo.swarm:in-lanes () (evo:set-setting :model \"mine\"))\n";
        fs::create_dir_all(folder.join(".evo")).unwrap();
        fs::write(swarm_lisp_path(&folder), user).unwrap();

        assert_eq!(
            set_lanes_model(&folder, Some(&LanesModel::new("m1", "ark"))).unwrap(),
            WriteOutcome::Written
        );
        let once = read(&folder);
        assert_eq!(
            once,
            format!(
                "{}\n{user}",
                render_block(&LanesModel::new("m1", "ark")).unwrap()
            )
        );
        // Twice changes nothing at all — not even the mtime.
        let before = fs::metadata(swarm_lisp_path(&folder))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            set_lanes_model(&folder, Some(&LanesModel::new("m1", "ark"))).unwrap(),
            WriteOutcome::Unchanged
        );
        assert_eq!(read(&folder), once);
        assert_eq!(
            fs::metadata(swarm_lisp_path(&folder))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );

        // A different model replaces the block in place, still at the top.
        assert_eq!(
            set_lanes_model(&folder, Some(&LanesModel::new("m2", "aiden"))).unwrap(),
            WriteOutcome::Written
        );
        let twice = read(&folder);
        assert!(twice.starts_with(";;; evo-desktop:begin"));
        assert!(twice.contains(":model \"m2\""));
        assert!(!twice.contains("m1"));
        assert!(
            twice.ends_with(user),
            "the user's own lines survive:\n{twice}"
        );
        assert_eq!(
            twice.matches(BLOCK_BEGIN).count(),
            1,
            "never duplicate markers"
        );
        assert_eq!(twice.matches(BLOCK_END).count(), 1);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn default_removes_the_block_and_keeps_the_rest() {
        let folder = temp_dir("remove");
        let user = "(evo:set-setting :thinking :high)\n";
        fs::create_dir_all(folder.join(".evo")).unwrap();
        fs::write(swarm_lisp_path(&folder), format!("{BLOCK}\n{user}")).unwrap();

        assert_eq!(
            set_lanes_model(&folder, None).unwrap(),
            WriteOutcome::Removed
        );
        assert_eq!(read(&folder), user);
        // Again: there is no block left, and nothing is touched.
        let before = fs::metadata(swarm_lisp_path(&folder))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            set_lanes_model(&folder, None).unwrap(),
            WriteOutcome::Unchanged
        );
        assert_eq!(
            fs::metadata(swarm_lisp_path(&folder))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn default_deletes_a_file_that_was_only_our_block() {
        let folder = temp_dir("delete");
        set_lanes_model(&folder, Some(&LanesModel::new("m1", "aiden"))).unwrap();
        assert!(swarm_lisp_path(&folder).exists());
        assert_eq!(
            set_lanes_model(&folder, None).unwrap(),
            WriteOutcome::FileDeleted
        );
        assert!(!swarm_lisp_path(&folder).exists());
        // And with nothing there at all, there is nothing to do.
        assert_eq!(
            set_lanes_model(&folder, None).unwrap(),
            WriteOutcome::Absent
        );
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_deleted_file_reappears_when_a_model_is_chosen_again() {
        let folder = temp_dir("recreate");
        let model = LanesModel::new("ark-deepseek-v4.1-flash", "aiden");
        set_lanes_model(&folder, Some(&model)).unwrap();
        set_lanes_model(&folder, None).unwrap();
        assert_eq!(
            set_lanes_model(&folder, Some(&model)).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(read(&folder), BLOCK);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn hand_edited_and_half_written_blocks_are_cleaned_up() {
        let folder = temp_dir("handedited");
        fs::create_dir_all(folder.join(".evo")).unwrap();
        // Two blocks, one indented, one already partly stripped — plus user text.
        let messy = format!(
            "{BLOCK}\n(keep me)\n  {BLOCK}\n  ;;; evo-desktop:begin — mine\n  (stray)\n;;; evo-desktop:end\n(and me)\n"
        );
        fs::write(swarm_lisp_path(&folder), messy).unwrap();
        set_lanes_model(&folder, Some(&LanesModel::new("m9", "ark"))).unwrap();
        let written = read(&folder);
        assert_eq!(written.matches(BLOCK_BEGIN).count(), 1);
        assert_eq!(written.matches(BLOCK_END).count(), 1);
        assert!(written.contains("(keep me)"));
        assert!(written.contains("(and me)"));
        assert!(!written.contains("(stray)"));

        // A begin marker with no end marker is ours too: it goes to the end.
        fs::write(
            swarm_lisp_path(&folder),
            ";;; evo-desktop:begin\n(garbage\n",
        )
        .unwrap();
        assert_eq!(
            set_lanes_model(&folder, None).unwrap(),
            WriteOutcome::FileDeleted
        );
        assert!(!swarm_lisp_path(&folder).exists());
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_repeated_write_from_scratch_is_byte_stable() {
        let folder = temp_dir("stable");
        let model = LanesModel::new("ark-deepseek-v4.1-flash", "aiden");
        set_lanes_model(&folder, Some(&model)).unwrap();
        let once = read(&folder);
        for _ in 0..3 {
            set_lanes_model(&folder, Some(&model)).unwrap();
            assert_eq!(read(&folder), once);
        }
        assert_eq!(once, BLOCK);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_file_is_private_and_has_a_trailing_newline() {
        let folder = temp_dir("mode");
        set_lanes_model(&folder, Some(&LanesModel::new("m1", "aiden"))).unwrap();
        let text = read(&folder);
        assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(swarm_lisp_path(&folder))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn providers_are_normalized_and_bad_ones_refused() {
        assert_eq!(provider_keyword(":aiden"), Some("aiden".into()));
        assert_eq!(
            provider_keyword("  Anthropic-OAuth "),
            Some("anthropic-oauth".into())
        );
        assert_eq!(provider_keyword(""), None);
        assert_eq!(provider_keyword(":"), None);
        assert_eq!(provider_keyword("9lives"), None);
        assert_eq!(provider_keyword("has space"), None);
        assert_eq!(provider_keyword("paren)"), None);

        assert!(render_block(&LanesModel::new("m", "")).is_err());
        assert!(render_block(&LanesModel::new("m", "a b")).is_err());
        assert!(render_block(&LanesModel::new("", "aiden")).is_err());
        assert!(render_block(&LanesModel::new("two\nlines", "aiden")).is_err());

        // A normalized provider lands in the block without its colon.
        let block = render_block(&LanesModel::new("m", ":Aiden")).unwrap();
        assert!(block.contains(":model-provider :aiden)"), "{block}");
    }

    #[test]
    fn quotes_in_a_model_id_are_escaped() {
        let block = render_block(&LanesModel::new("we\"ird\\id", "aiden")).unwrap();
        assert!(
            block.contains(r#"(evo:set-setting :model "we\"ird\\id")"#),
            "{block}"
        );
    }

    #[test]
    fn paths_and_block_detection() {
        let folder = temp_dir("detect");
        assert_eq!(
            swarm_lisp_path(Path::new("/p")),
            PathBuf::from("/p/.evo/swarm.lisp")
        );
        assert!(!has_lanes_model_block(&folder));
        set_lanes_model(&folder, Some(&LanesModel::new("m1", "aiden"))).unwrap();
        assert!(has_lanes_model_block(&folder));
        set_lanes_model(&folder, None).unwrap();
        assert!(!has_lanes_model_block(&folder));
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn composition_edge_cases() {
        // No trailing newline on the user's content is repaired.
        assert_eq!(compose("(x)", BLOCK), format!("{BLOCK}\n(x)\n"));
        // Whitespace-only content is treated as empty.
        assert_eq!(compose("\n\n  \n", BLOCK), BLOCK);
        // Leading blank lines are dropped, indentation is kept.
        assert_eq!(
            compose("\n\n  (indented)\n", BLOCK),
            format!("{BLOCK}\n  (indented)\n")
        );
    }
}
