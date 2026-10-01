//! What a message carries besides its words, as `input.send` takes it (CONTRACT §5.5).
//!
//! The composer's own `Attachment` does not cross into here — a client's widget types
//! are the client's — so this is the small neutral shape between them, and the two
//! answers evo's protocol wants from it:
//!
//! * an **image** rides with the turn itself, in the op's `images` array: a file on
//!   disk as `{path}` (evo reads it, sniffs its type and journals it, which is what
//!   puts it back in the transcript on resume), bytes with no file behind them — a
//!   clipboard paste — as `{name, media_type, data}` with the data base64;
//! * a **file** is not embedded anywhere (`docs/serve.md` has no shape for one):
//!   its absolute path is named in the message's own text, and the agent reads it
//!   with its own tools if it wants to.
//!
//! Every path that leaves here is absolute: the agent's tools run in the session's
//! own working directory, and evo's reader is given a path it can open as it stands.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

/// One thing attached to a message, as the wire takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attached {
    /// An image file on disk: sent as `{path}`.
    ImageFile(PathBuf),
    /// Image bytes with no file behind them: `{name, media_type, data}`.
    ImageBytes {
        name: String,
        media_type: String,
        bytes: Vec<u8>,
    },
    /// Any other file: its absolute path goes into the message's text.
    File(PathBuf),
}

/// The heading the names of a message's files go under.
///
/// It is the *only* thing the agent is told about a file — the reader's words, then
/// this list, in the same text — so it is a sentence's worth of context, and no more:
/// what the file is, where it is, and that reading it is the agent's own decision.
pub const FILES_HEADING: &str = "Attached files:";

/// One `input.send`'s payload: the words, and the images it carries.
///
/// The words are the reader's own, with the files named under them ([`FILES_HEADING`])
/// — the reader's text first, because that is what they wrote and what the turn is
/// about. A message with no words is the file block alone, with no blank line above
/// it; a message with no files is exactly the words it was given.
pub fn turn(text: &str, attachments: &[Attached]) -> (String, Vec<Value>) {
    (text_with_files(text, attachments), images(attachments))
}

/// The message's text: the reader's words, then the block naming every file.
pub fn text_with_files(text: &str, attachments: &[Attached]) -> String {
    let paths: Vec<PathBuf> = attachments
        .iter()
        .filter_map(|attachment| match attachment {
            Attached::File(path) => Some(absolute(path)),
            _ => None,
        })
        .collect();
    if paths.is_empty() {
        return text.to_string();
    }
    let files = paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let block = format!("{FILES_HEADING}\n{files}");
    // A draft of nothing but spaces is a draft of nothing: the block is the message.
    match text.trim_end() {
        "" => block,
        words => format!("{words}\n\n{block}"),
    }
}

/// The op's `images` array: one entry per image, in the order they were attached.
///
/// evo sniffs each one's media type itself, so ours is a statement of what the client
/// thinks it is — for bytes it is the only thing that could know, and for a path it is
/// the file evo will read.
pub fn images(attachments: &[Attached]) -> Vec<Value> {
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            Attached::ImageFile(path) => Some(json!({ "path": absolute(path) })),
            Attached::ImageBytes {
                name,
                media_type,
                bytes,
            } => Some(json!({
                "name": name,
                "media_type": media_type,
                "data": BASE64.encode(bytes),
            })),
            Attached::File(_) => None,
        })
        .collect()
}

/// The path as the agent's own tools will see it: resolved, so a relative draft path
/// is not left to whoever's working directory reads it.
///
/// A path that cannot be resolved is still the path the reader named — a file removed
/// between the drop and the send, a folder this process may not open — and saying so
/// is evo's job, not this one's: evo refuses an image it cannot read, and the agent
/// reports a file it cannot open. What must not happen is a *relative* path reaching
/// a process that runs somewhere else.
fn absolute(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|dir| dir.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real file to name: the tests are about what the wire carries, and evo's own
    /// reader opens these paths for real.
    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("evo-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("a scratch file");
        std::fs::canonicalize(&path).expect("the scratch file's own path")
    }

    /// The one byte of a PNG's signature: enough for a path that is only ever named.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn an_image_file_travels_as_its_own_absolute_path() {
        let path = scratch("shot.png", PNG);
        // The reader may well have attached a relative path: what leaves here is the
        // same file, by the absolute name the agent's tools understand.
        let relative = PathBuf::from("src/attachments.rs");
        let (text, images) = turn(
            "what is this?",
            &[
                Attached::ImageFile(path.clone()),
                Attached::ImageFile(relative.clone()),
            ],
        );
        assert_eq!(text, "what is this?", "an image is not named in the text");
        assert_eq!(
            images,
            vec![
                json!({"path": path}),
                json!({"path": std::fs::canonicalize(&relative).expect("this very file")}),
            ],
            "each image is a path, resolved"
        );
        for image in &images {
            let named = image["path"].as_str().expect("a path");
            assert!(Path::new(named).is_absolute(), "{named} is absolute");
        }
    }

    #[test]
    fn pasted_bytes_travel_as_base64_under_their_own_name_and_type() {
        // `✓` is the case that catches an encoder that works byte-wise on a string:
        // three bytes, one character, and a base64 body nobody can guess.
        let bytes = "✓✓".as_bytes().to_vec();
        let (_, images) = turn(
            "",
            &[Attached::ImageBytes {
                name: "pasted image.png".to_string(),
                media_type: "image/png".to_string(),
                bytes: bytes.clone(),
            }],
        );
        assert_eq!(
            images,
            vec![json!({
                "name": "pasted image.png",
                "media_type": "image/png",
                "data": BASE64.encode(&bytes),
            })]
        );
        assert_eq!(images[0]["data"].as_str(), Some("4pyT4pyT"));
    }

    #[test]
    fn a_files_absolute_path_is_named_under_the_readers_own_words() {
        let csv = scratch("notes.csv", b"a,b\n1,2\n");
        let pdf = scratch("brief.pdf", b"%PDF-1.4\n");
        let (text, images) = turn(
            "summarise these",
            &[Attached::File(csv.clone()), Attached::File(pdf.clone())],
        );
        assert_eq!(
            text,
            format!(
                "summarise these\n\n{FILES_HEADING}\n- {}\n- {}",
                csv.display(),
                pdf.display()
            ),
            "the reader's words first, then one line per file"
        );
        assert!(images.is_empty(), "a file is not an image: {images:?}");
    }

    #[test]
    fn a_message_with_no_words_is_the_files_alone() {
        let csv = scratch("only.csv", b"a,b\n");
        let (text, _) = turn("", &[Attached::File(csv.clone())]);
        assert_eq!(text, format!("{FILES_HEADING}\n- {}", csv.display()));
        assert!(
            !text.starts_with('\n'),
            "no blank line where the words would have been"
        );
        // Whitespace is not words either: a draft of a space sends the block.
        let (spaced, _) = turn("   \n", &[Attached::File(csv.clone())]);
        assert_eq!(spaced, text);
    }

    #[test]
    fn words_alone_are_the_message_and_nothing_is_invented() {
        let (text, images) = turn("just this", &[]);
        assert_eq!(text, "just this");
        assert!(images.is_empty());
    }

    #[test]
    fn the_heading_names_a_file_that_is_gone() {
        // A path that cannot be resolved is still named — an agent that cannot open
        // it says so, which is better than the attachment disappearing unmentioned.
        let (text, _) = turn("", &[Attached::File(PathBuf::from("/nowhere/gone.pdf"))]);
        assert_eq!(text, format!("{FILES_HEADING}\n- /nowhere/gone.pdf"));
    }
}
