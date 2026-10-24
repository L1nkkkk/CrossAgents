//! Files that come with a message (docs/tasks/DOGFOOD.md §3): stored by the hub, handed to
//! drivers as paths and described in events; never their bytes in the store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bytes of one text attachment a driver puts in the prompt itself; longer ones are named
/// by path instead.
pub const INLINE_TEXT_LIMIT: u64 = 256 * 1024;

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
}
