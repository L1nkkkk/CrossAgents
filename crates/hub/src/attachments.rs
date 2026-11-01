//! Files sent with messages (docs/tasks/DOGFOOD.md §3): checked, stored once per session by
//! content hash under `<home>/attachments/<agent id>/`, read back for clients.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossagents_api_types::{
    AttachmentContent, AttachmentUpload, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS,
    MAX_ATTACHMENTS_TOTAL, attachment_mime_ok,
};
use crossagents_core::{AgentId, Attachment, AttachmentKind, AttachmentSupport};
use sha2::{Digest, Sha256};

/// Why files were refused, for the client to show as it is.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub String);

fn dir(home: &Path, agent: AgentId) -> PathBuf {
    home.join("attachments").join(agent.to_string())
}

/// Checks the files against what the agent takes (docs/TODO.md §7). Types that messages
/// do not allow at all are left to [`store`], which says why.
pub fn check_support(
    uploads: &[AttachmentUpload],
    support: AttachmentSupport,
    agent: &str,
    driver: &str,
) -> Result<(), Refused> {
    for upload in uploads {
        let Some(kind) = AttachmentKind::of_mime(&upload.mime) else {
            continue;
        };
        if support.allows(kind) {
            continue;
        }
        let name = clean_name(&upload.name);
        let takes: Vec<&str> = support
            .kinds()
            .iter()
            .map(|kind| match kind {
                AttachmentKind::Image => "images",
                AttachmentKind::Pdf => "PDFs",
                AttachmentKind::Text => "text",
            })
            .collect();
        let takes = if takes.is_empty() {
            "no files".to_owned()
        } else {
            takes.join(", ")
        };
        let refused = match kind {
            AttachmentKind::Image => "images",
            AttachmentKind::Pdf => "PDFs",
            AttachmentKind::Text => "text files",
        };
        return Err(Refused(format!(
            "{name}: {agent} cannot take {refused} (its {driver} driver takes {takes})"
        )));
    }
    Ok(())
}

/// Checks `uploads` against the limits and stores them for `agent`.
pub fn store(
    home: &Path,
    agent: AgentId,
    uploads: Vec<AttachmentUpload>,
) -> Result<Vec<Attachment>, Refused> {
    if uploads.len() > MAX_ATTACHMENTS {
        return Err(Refused(format!(
            "at most {MAX_ATTACHMENTS} files per message (got {})",
            uploads.len()
        )));
    }
    let mut decoded = Vec::with_capacity(uploads.len());
    let mut total = 0;
    for upload in uploads {
        let name = clean_name(&upload.name);
        if !attachment_mime_ok(&upload.mime) {
            return Err(Refused(format!(
                "{name}: files of type {} cannot be sent (images, PDF, JSON and text can)",
                upload.mime
            )));
        }
        let bytes = STANDARD
            .decode(upload.data.as_bytes())
            .map_err(|e| Refused(format!("{name}: not valid base64 ({e})")))?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(Refused(format!(
                "{name} is {} MB; one file may be at most {} MB",
                bytes.len() / (1024 * 1024),
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        total += bytes.len();
        decoded.push((name, upload.mime, bytes));
    }
    if total > MAX_ATTACHMENTS_TOTAL {
        return Err(Refused(format!(
            "the files add up to {} MB; a message may carry at most {} MB",
            total / (1024 * 1024),
            MAX_ATTACHMENTS_TOTAL / (1024 * 1024)
        )));
    }
    let dir = dir(home, agent);
    std::fs::create_dir_all(&dir).map_err(|e| Refused(format!("cannot store files: {e}")))?;
    decoded
        .into_iter()
        .map(|(name, mime, bytes)| {
            let id = format!("{:x}", Sha256::digest(&bytes));
            let path = dir.join(&id);
            if !path.exists() {
                std::fs::write(&path, &bytes)
                    .map_err(|e| Refused(format!("cannot store {name}: {e}")))?;
            }
            Ok(Attachment {
                id,
                name,
                mime,
                size: bytes.len() as u64,
                path,
            })
        })
        .collect()
}

/// Drops everything stored for `agent` (its session is gone); nothing stored is fine.
pub fn remove(home: &Path, agent: AgentId) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir(home, agent)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        done => done,
    }
}

/// One stored file of `agent`'s; `None` for anything else (bad id, another session's file).
pub fn read(home: &Path, agent: AgentId, id: &str) -> Option<AttachmentContent> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let dir = dir(home, agent);
    let path = dir.join(id.to_ascii_lowercase());
    if !crossagents_core::attachment::is_within(&dir, &path) {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(AttachmentContent {
        id: id.to_ascii_lowercase(),
        mime: sniff(&bytes).to_owned(),
        size: bytes.len() as u64,
        data: STANDARD.encode(bytes),
    })
}

/// The type of stored bytes, from their first bytes (the store keeps bytes only).
pub(crate) fn sniff(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'%', b'P', b'D', b'F', ..] => "application/pdf",
        _ => "text/plain",
    }
}

/// The file's own name, without directories or characters that break a prompt line.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>'))
        .take(120)
        .collect();
    if cleaned.trim().is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(name: &str, mime: &str, bytes: &[u8]) -> AttachmentUpload {
        AttachmentUpload {
            name: name.into(),
            mime: mime.into(),
            data: STANDARD.encode(bytes),
        }
    }

    #[test]
    fn stores_once_by_hash_and_reads_back() {
        const PNG: &[u8] = &[0x89, b'P', b'N', b'G'];
        let home = tempfile::tempdir().unwrap();
        let agent = AgentId::new();
        let stored = store(
            home.path(),
            agent,
            vec![
                upload("../../shot.png", "image/png", PNG),
                upload("again.png", "image/png", PNG),
            ],
        )
        .unwrap();
        assert_eq!(stored[0].name, "shot.png");
        assert_eq!(stored[0].id, stored[1].id);
        assert_eq!(stored[0].id.len(), 64);
        assert!(stored[0].path.starts_with(home.path()));
        let back = read(home.path(), agent, &stored[0].id).unwrap();
        assert_eq!(STANDARD.decode(back.data).unwrap(), PNG);
        assert_eq!(back.mime, "image/png");
        // Another session cannot read it, nor a path in disguise.
        assert!(read(home.path(), AgentId::new(), &stored[0].id).is_none());
        assert!(read(home.path(), agent, "../token").is_none());
        // Removing the agent takes its files, and only its files.
        let other = AgentId::new();
        store(home.path(), other, vec![upload("b.png", "image/png", PNG)]).unwrap();
        remove(home.path(), agent).unwrap();
        assert!(read(home.path(), agent, &stored[0].id).is_none());
        assert!(!dir(home.path(), agent).exists());
        assert!(read(home.path(), other, &stored[0].id).is_some());
        remove(home.path(), agent).unwrap();
    }

    #[test]
    fn refuses_kinds_the_agent_does_not_take() {
        let codex = AttachmentSupport {
            image: true,
            pdf: false,
            text: true,
        };
        let refused = check_support(
            &[upload("notes.pdf", "application/pdf", b"%PDF")],
            codex,
            "alice",
            "codex-native",
        )
        .unwrap_err();
        assert!(refused.0.contains("notes.pdf"), "{}", refused.0);
        assert!(refused.0.contains("PDFs"), "{}", refused.0);
        assert!(refused.0.contains("codex-native"), "{}", refused.0);
        assert!(refused.0.contains("images, text"), "{}", refused.0);
        assert!(
            check_support(
                &[upload("shot.png", "image/png", b"png")],
                codex,
                "alice",
                "codex-native"
            )
            .is_ok()
        );
        assert!(
            check_support(
                &[upload("notes.txt", "text/plain", b"x")],
                AttachmentSupport::TEXT_ONLY,
                "alice",
                "acp-generic"
            )
            .is_ok()
        );
        // Types the hub does not take at all are `store`'s business, with its own message.
        assert!(
            check_support(
                &[upload("a.exe", "application/x-msdownload", b"MZ")],
                AttachmentSupport::TEXT_ONLY,
                "alice",
                "acp-generic"
            )
            .is_ok()
        );
        let none = AttachmentSupport {
            image: false,
            pdf: false,
            text: false,
        };
        assert!(
            check_support(
                &[upload("a.txt", "text/plain", b"x")],
                none,
                "alice",
                "acp-generic"
            )
            .unwrap_err()
            .0
            .contains("no files")
        );
    }

    #[test]
    fn refuses_what_breaks_the_limits() {
        let home = tempfile::tempdir().unwrap();
        let agent = AgentId::new();
        let exe = store(
            home.path(),
            agent,
            vec![upload("a.exe", "application/x-msdownload", b"MZ")],
        );
        assert!(exe.unwrap_err().0.contains("cannot be sent"));
        let many = (0..=MAX_ATTACHMENTS)
            .map(|i| upload(&format!("{i}.txt"), "text/plain", b"x"))
            .collect();
        assert!(
            store(home.path(), agent, many)
                .unwrap_err()
                .0
                .contains("at most")
        );
        let big = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        assert!(
            store(
                home.path(),
                agent,
                vec![upload("big.txt", "text/plain", &big)]
            )
            .is_err()
        );
        let bad = AttachmentUpload {
            name: "x.txt".into(),
            mime: "text/plain".into(),
            data: "%%".into(),
        };
        assert!(
            store(home.path(), agent, vec![bad])
                .unwrap_err()
                .0
                .contains("base64")
        );
    }
}
