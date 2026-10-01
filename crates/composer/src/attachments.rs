//! What a message carries besides its words: the attachments a reader adds with
//! the composer's `+`, drops on it, or pastes from the clipboard.
//!
//! Two kinds, because evo treats them differently. An **image** is sent with the
//! turn itself (`input.send`'s `images`, CONTRACT §5.5) and evo journals it, so it
//! is in the transcript — and in the transcript a resumed session rebuilds. A
//! **file** is not embedded anywhere: the agent is handed its absolute path, in
//! the message, and reads it with its own tools if it wants to.

use std::path::PathBuf;
use std::sync::Arc;

/// One thing attached to the draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Stable within one composer, so a tile can be removed by id while others are
    /// added around it.
    pub id: u64,
    /// What the tile says under its thumbnail: a file's own name, or the name a
    /// pasted image is given (`pasted image.png`).
    pub name: String,
    pub kind: AttachmentKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentKind {
    /// An image file on disk, sent as `{path}` (evo reads, sniffs and journals it).
    ImageFile(PathBuf),
    /// Image bytes with no file behind them — a clipboard paste — sent as
    /// `{name, media_type, data}` with `data` base64.
    ImageBytes {
        media_type: String,
        bytes: Arc<Vec<u8>>,
    },
    /// Any other file: its absolute path goes into the message's text.
    File(PathBuf),
}

impl Attachment {
    /// Whether this attachment travels as an image of the turn.
    pub fn is_image(&self) -> bool {
        matches!(
            self.kind,
            AttachmentKind::ImageFile(_) | AttachmentKind::ImageBytes { .. }
        )
    }
}

/// A message on its way out: the words, and what is attached to them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outgoing {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl From<String> for Outgoing {
    fn from(text: String) -> Outgoing {
        Outgoing {
            text,
            attachments: Vec::new(),
        }
    }
}

impl From<&str> for Outgoing {
    fn from(text: &str) -> Outgoing {
        Outgoing::from(text.to_string())
    }
}
