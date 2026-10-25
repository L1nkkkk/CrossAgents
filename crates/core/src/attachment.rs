//! Files that come with a message (docs/tasks/DOGFOOD.md §3): stored by the hub, handed to
//! drivers as paths and described in events; never their bytes in the store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bytes of one text attachment a driver puts in the prompt itself; longer ones are named
/// by path instead.
pub const INLINE_TEXT_LIMIT: u64 = 256 * 1024;

/// A kind of file, as drivers treat them (docs/TODO.md §7): an image goes as an image to
/// drivers that take them, a PDF as a document, text goes into the prompt itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Pdf,
    /// Text and JSON: small ones are inlined, longer ones named by path.
    Text,
}

impl AttachmentKind {
    /// The kind of an allowed `mime` (`api_types::attachment_mime_ok`); `None` for others.
    pub fn of_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" => Some(Self::Image),
            "application/pdf" => Some(Self::Pdf),
            "application/json" => Some(Self::Text),
            _ if mime.starts_with("text/") => Some(Self::Text),
            _ => None,
        }
    }

    /// How a person names it in a message ("image", "PDF", "text").
    pub fn noun(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Pdf => "PDF",
            Self::Text => "text",
        }
    }
}

/// Which kinds of file an agent takes (docs/TODO.md §7). Other kinds are refused rather
/// than passed as a reference the agent cannot use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AttachmentSupport {
    pub image: bool,
    pub pdf: bool,
    pub text: bool,
}

impl AttachmentSupport {
    pub const ALL: Self = Self {
        image: true,
        pdf: true,
        text: true,
    };
    /// Every driver can put text in the prompt; not all take images or documents.
    pub const TEXT_ONLY: Self = Self {
        image: false,
        pdf: false,
        text: true,
    };

    pub fn allows(self, kind: AttachmentKind) -> bool {
        match kind {
            AttachmentKind::Image => self.image,
            AttachmentKind::Pdf => self.pdf,
            AttachmentKind::Text => self.text,
        }
    }

    /// The kinds it takes, in the order messages name them.
    pub fn kinds(self) -> Vec<AttachmentKind> {
        [
            (self.image, AttachmentKind::Image),
            (self.pdf, AttachmentKind::Pdf),
            (self.text, AttachmentKind::Text),
        ]
        .into_iter()
        .filter_map(|(takes, kind)| takes.then_some(kind))
        .collect()
    }
}

impl Default for AttachmentSupport {
    fn default() -> Self {
        Self::TEXT_ONLY
    }
}

/// A file sent with a message, as kept on disk by the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Attachment {
    /// The content's sha256, lowercase hex.
    pub id: String,
    pub name: String,
    pub mime: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size: u64,
    /// Where the hub keeps it.
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub path: PathBuf,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        matches!(
            self.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        )
    }

    pub fn is_pdf(&self) -> bool {
        self.mime == "application/pdf"
    }

    /// Text a driver can put in the prompt as it is.
    pub fn is_text(&self) -> bool {
        self.mime.starts_with("text/") || self.mime == "application/json"
    }

    /// The kind drivers think in (docs/TODO.md §7).
    pub fn kind(&self) -> Option<AttachmentKind> {
        AttachmentKind::of_mime(&self.mime)
    }

    /// The file's contents as text, when it is text and small enough to inline.
    pub fn inline_text(&self) -> Option<String> {
        if !self.is_text() || self.size > INLINE_TEXT_LIMIT {
            return None;
        }
        let bytes = std::fs::read(&self.path).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// One line naming the file for an agent that reads it itself.
    pub fn reference(&self) -> String {
        format!(
            "[attached file: {} ({}, {} bytes) at {}]",
            self.name,
            self.mime,
            self.size,
            self.path.display()
        )
    }

    /// The text wrapped as one file for the prompt.
    pub fn as_file_block(&self, text: &str) -> String {
        format!("<file name=\"{}\">\n{text}\n</file>", self.name)
    }
}

/// What one turn is asked: the message and its files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl Prompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    /// The message with every attachment written in: small text files inline, the others
    /// as references to their paths. For drivers that take text only.
    pub fn with_references(&self) -> String {
        let mut out = self.text.clone();
        for a in &self.attachments {
            out.push_str("\n\n");
            match a.inline_text() {
                Some(text) => out.push_str(&a.as_file_block(&text)),
                None => out.push_str(&a.reference()),
            }
        }
        out
    }
}

/// Whether `path` is a file under `dir` (after resolving `..` and links).
pub fn is_within(dir: &Path, path: &Path) -> bool {
    match (dir.canonicalize(), path.canonicalize()) {
        (Ok(dir), Ok(path)) => path.starts_with(dir),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, mime: &str, body: &[u8]) -> Attachment {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        Attachment {
            id: "x".into(),
            name: name.into(),
            mime: mime.into(),
            size: body.len() as u64,
            path,
        }
    }

    #[test]
    fn text_files_go_inline_and_others_by_path() {
        let dir = std::env::temp_dir().join(format!("ca-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = Prompt {
            text: "look".into(),
            attachments: vec![
                file(&dir, "notes.md", "text/markdown", b"# hi"),
                file(&dir, "shot.png", "image/png", b"\x89PNG"),
            ],
        };
        let text = prompt.with_references();
        assert!(
            text.starts_with("look\n\n<file name=\"notes.md\">\n# hi\n</file>"),
            "{text}"
        );
        assert!(
            text.contains("[attached file: shot.png (image/png, 4 bytes) at "),
            "{text}"
        );
        assert!(prompt.attachments[1].is_image());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn kinds_and_what_an_agent_takes() {
        assert_eq!(
            AttachmentKind::of_mime("image/webp"),
            Some(AttachmentKind::Image)
        );
        assert_eq!(
            AttachmentKind::of_mime("application/pdf"),
            Some(AttachmentKind::Pdf)
        );
        assert_eq!(
            AttachmentKind::of_mime("text/markdown"),
            Some(AttachmentKind::Text)
        );
        assert_eq!(
            AttachmentKind::of_mime("application/json"),
            Some(AttachmentKind::Text)
        );
        assert_eq!(AttachmentKind::of_mime("application/zip"), None);
        assert_eq!(AttachmentKind::of_mime("image/bmp"), None);

        assert!(AttachmentSupport::ALL.allows(AttachmentKind::Pdf));
        assert!(!AttachmentSupport::TEXT_ONLY.allows(AttachmentKind::Image));
        assert!(AttachmentSupport::TEXT_ONLY.allows(AttachmentKind::Text));
        assert_eq!(
            AttachmentSupport::TEXT_ONLY.kinds(),
            vec![AttachmentKind::Text]
        );
        assert_eq!(
            AttachmentSupport::ALL.kinds(),
            vec![
                AttachmentKind::Image,
                AttachmentKind::Pdf,
                AttachmentKind::Text
            ]
        );
    }
}
