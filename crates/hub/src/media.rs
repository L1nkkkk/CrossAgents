//! `GET /v1/agents/{key}/files/media`: serves images, audio (mp3/wav/m4a/ogg/flac/aac/weba),
//! video (mp4/webm/mov/m4v/ogv/mkv), PDFs, text files, and common documents referenced by an
//! agent in its reply so desktop, remote desktop, and mobile PWA clients can play, preview, or
//! download them directly without local filesystem access.
//!
//! Security boundary: files must resolve inside the agent's working directory, the system temp
//! directory, or the agent's attachment store (after resolving `..` and symlinks), and be at
//! most [`MAX_MEDIA_BYTES`]. Supports `?offset=&limit=` chunked reads so large audio/video
//! files stream reliably over WebRTC DataChannels and iroh relays.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossagents_api_types::{MediaContent, MediaKind};
use crossagents_orchestrator::files::FileError;

/// Maximum media/document size served through this endpoint (50 MiB).
pub const MAX_MEDIA_BYTES: u64 = 50 * 1024 * 1024;
/// Default max bytes returned in one unchunked request before callers paginate with `offset`.
pub const DEFAULT_SINGLE_CHUNK_BYTES: u64 = 12 * 1024 * 1024;
/// Hard cap for one chunk response (12 MiB raw ≈ 16 MiB base64).
pub const MAX_CHUNK_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSpec {
    pub kind: MediaKind,
    pub mime: &'static str,
}

/// Normalizes a `file://` URI if an agent wrote `[audio](file:///C:/...)` or `file:///tmp/...`.
pub fn normalize_input_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(rest) = trimmed
        .strip_prefix("file://")
        .or_else(|| trimmed.strip_prefix("FILE://"))
    else {
        return trimmed.to_owned();
    };
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    // `file:///C:/foo.mp3` -> `C:/foo.mp3` on Windows.
    let bytes = decoded.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes[3] == b'/' || bytes[3] == b'\\')
    {
        decoded[1..].to_owned()
    } else {
        decoded
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Classifies a path by extension into the media/document types clients can preview or download.
pub fn spec_of_path(path: &Path) -> Option<MediaSpec> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let spec = match ext.as_str() {
        "png" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/png",
        },
        "jpg" | "jpeg" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/jpeg",
        },
        "gif" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/gif",
        },
        "webp" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/webp",
        },
        "bmp" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/bmp",
        },
        "svg" => MediaSpec {
            kind: MediaKind::Image,
            mime: "image/svg+xml",
        },
        "mp3" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/mpeg",
        },
        "wav" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/wav",
        },
        "m4a" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/mp4",
        },
        "ogg" | "oga" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/ogg",
        },
        "opus" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/opus",
        },
        "weba" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/webm",
        },
        "flac" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/flac",
        },
        "aac" => MediaSpec {
            kind: MediaKind::Audio,
            mime: "audio/aac",
        },
        "mp4" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/mp4",
        },
        "webm" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/webm",
        },
        "mov" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/quicktime",
        },
        "m4v" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/x-m4v",
        },
        "ogv" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/ogg",
        },
        "mkv" => MediaSpec {
            kind: MediaKind::Video,
            mime: "video/x-matroska",
        },
        "pdf" => MediaSpec {
            kind: MediaKind::Pdf,
            mime: "application/pdf",
        },
        "txt" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go"
        | "java" | "c" | "h" | "cpp" | "cc" | "hpp" | "toml" | "yaml" | "yml" | "sh" | "ps1"
        | "sql" | "ini" | "cfg" | "env" => MediaSpec {
            kind: MediaKind::Text,
            mime: "text/plain",
        },
        "md" | "markdown" => MediaSpec {
            kind: MediaKind::Text,
            mime: "text/markdown",
        },
        "json" => MediaSpec {
            kind: MediaKind::Text,
            mime: "application/json",
        },
        "csv" => MediaSpec {
            kind: MediaKind::Text,
            mime: "text/csv",
        },
        "xml" => MediaSpec {
            kind: MediaKind::Text,
            mime: "application/xml",
        },
        "html" | "htm" => MediaSpec {
            kind: MediaKind::Text,
            mime: "text/html",
        },
        "css" => MediaSpec {
            kind: MediaKind::Text,
            mime: "text/css",
        },
        "rtf" => MediaSpec {
            kind: MediaKind::Text,
            mime: "application/rtf",
        },
        "zip" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/zip",
        },
        "gz" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/gzip",
        },
        "tar" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/x-tar",
        },
        "doc" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/msword",
        },
        "docx" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        },
        "xls" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/vnd.ms-excel",
        },
        "xlsx" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        },
        "ppt" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/vnd.ms-powerpoint",
        },
        "pptx" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        },
        "epub" => MediaSpec {
            kind: MediaKind::File,
            mime: "application/epub+zip",
        },
        _ => return None,
    };
    Some(spec)
}

/// Locates `input` inside the agent's working directory, system temp directory, or the agent's
/// attachments directory.
pub fn locate(root: &Path, attachments_dir: Option<&Path>, input: &str) -> Result<PathBuf, FileError> {
    let normalized = normalize_input_path(input);
    let temp = std::env::temp_dir();
    let mut roots: Vec<&Path> = vec![root, &temp];
    if let Some(att) = attachments_dir
        && att.exists()
    {
        roots.push(att);
    }
    match crate::paths::confine_any(&roots, &normalized) {
        Ok(found) => Ok(found.abs),
        Err(FileError::Outside(what)) => Err(FileError::Outside(format!(
            "{what} (a media/file reference must be inside the agent's working directory or the system temp directory)"
        ))),
        Err(other) => Err(other),
    }
}

/// Validates leading magic bytes for structured binary media formats to prevent spoofed files.
fn header_matches(spec: MediaSpec, head: &[u8]) -> bool {
    if head.is_empty() {
        return matches!(spec.kind, MediaKind::Text);
    }
    match spec.mime {
        "image/png" => head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => head.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a"),
        "image/webp" => head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP",
        "image/bmp" => head.starts_with(b"BM"),
        "image/svg+xml" => {
            let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
            text.to_ascii_lowercase().contains("<svg")
        }
        "application/pdf" => head.starts_with(b"%PDF-"),
        "audio/wav" => head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WAVE",
        "audio/ogg" | "audio/opus" | "video/ogg" => head.starts_with(b"OggS"),
        "audio/flac" => head.starts_with(b"fLaC"),
        "audio/mpeg" => {
            head.starts_with(b"ID3")
                || (head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0)
                // Some encoders pad leading zero bytes before the first MPEG frame sync.
                || head.windows(2).take(1024).any(|w| w[0] == 0xFF && (w[1] & 0xE0) == 0xE0)
        }
        "audio/aac" => {
            (head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xF0) == 0xF0)
                || head.starts_with(b"ID3")
                || (head.len() >= 8 && &head[4..8] == b"ftyp")
        }
        "audio/mp4" | "video/mp4" | "video/quicktime" | "video/x-m4v" => {
            head.len() >= 8
                && (&head[4..8] == b"ftyp"
                    || &head[4..8] == b"moov"
                    || &head[4..8] == b"mdat"
                    || &head[4..8] == b"wide"
                    || &head[4..8] == b"free")
        }
        "audio/webm" | "video/webm" | "video/x-matroska" => {
            head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
        }
        _ => true,
    }
}

/// Reads one chunk (or the whole file when small) from `abs`.
pub fn load_chunk(
    abs: &Path,
    input: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<MediaContent, FileError> {
    let spec = spec_of_path(abs).ok_or_else(|| {
        FileError::BadRequest(format!(
            "{input} is not a supported previewable media or document type"
        ))
    })?;
    let meta = std::fs::metadata(abs)
        .map_err(|_| FileError::NotFound(format!("no file {input}")))?;
    if !meta.is_file() {
        return Err(FileError::BadRequest(format!(
            "{input} is not a regular file"
        )));
    }
    let total_size = meta.len();
    if total_size > MAX_MEDIA_BYTES {
        return Err(FileError::BadRequest(format!(
            "{input} is {:.1} MB; inline media/files may be at most {} MB",
            total_size as f64 / (1024.0 * 1024.0),
            MAX_MEDIA_BYTES / (1024 * 1024)
        )));
    }

    let mut file = std::fs::File::open(abs)
        .map_err(|e| FileError::Failed(format!("cannot read {input}: {e}")))?;

    let mut head = [0u8; 4096];
    let head_len = file
        .read(&mut head)
        .map_err(|e| FileError::Failed(format!("cannot read {input}: {e}")))?;
    if !header_matches(spec, &head[..head_len]) {
        return Err(FileError::BadRequest(format!(
            "{input} does not match its declared format ({})",
            spec.mime
        )));
    }

    let start = offset.unwrap_or(0).min(total_size);
    let chunk_cap = match (offset, limit) {
        (None, None) => DEFAULT_SINGLE_CHUNK_BYTES,
        (_, Some(l)) => l.clamp(1, MAX_CHUNK_BYTES),
        (Some(_), None) => 2 * 1024 * 1024,
    };

    file.seek(SeekFrom::Start(start))
        .map_err(|e| FileError::Failed(format!("cannot seek {input}: {e}")))?;
    let mut bytes = Vec::with_capacity(total_size.saturating_sub(start).min(chunk_cap) as usize);
    file.take(chunk_cap)
        .read_to_end(&mut bytes)
        .map_err(|e| FileError::Failed(format!("cannot read {input}: {e}")))?;
    let eof = start + bytes.len() as u64 >= total_size;
    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_owned();

    Ok(MediaContent {
        path: abs.display().to_string(),
        name,
        mime: spec.mime.to_owned(),
        kind: spec.kind,
        size: total_size,
        offset: start,
        data: STANDARD.encode(&bytes),
        eof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_mp3_wav_mp4_pdf_txt_and_streams_in_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mp3 = [b"ID3\x03\x00\x00\x00\x00\x00\x00".as_slice(), &[0xFF, 0xFB, 0x90, 0x00, 1, 2, 3, 4]].concat();
        std::fs::write(root.join("tts.mp3"), &mp3).unwrap();
        let loaded = load_chunk(&root.join("tts.mp3"), "tts.mp3", None, None).unwrap();
        assert_eq!(loaded.kind, MediaKind::Audio);
        assert_eq!(loaded.mime, "audio/mpeg");
        assert!(loaded.eof);
        assert_eq!(STANDARD.decode(&loaded.data).unwrap(), mp3);

        // Chunked read reassembles identically.
        let p1 = load_chunk(&root.join("tts.mp3"), "tts.mp3", Some(0), Some(8)).unwrap();
        assert!(!p1.eof);
        let p2 = load_chunk(&root.join("tts.mp3"), "tts.mp3", Some(8), Some(64)).unwrap();
        assert!(p2.eof);
        let combined = [
            STANDARD.decode(&p1.data).unwrap(),
            STANDARD.decode(&p2.data).unwrap(),
        ]
        .concat();
        assert_eq!(combined, mp3);

        // MP4 magic header check.
        let mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00";
        std::fs::write(root.join("demo.mp4"), mp4).unwrap();
        let v = load_chunk(&root.join("demo.mp4"), "demo.mp4", None, None).unwrap();
        assert_eq!(v.kind, MediaKind::Video);
        assert_eq!(v.mime, "video/mp4");

        // Spoofed mp4 rejected.
        std::fs::write(root.join("fake.mp4"), b"not a real video").unwrap();
        assert!(load_chunk(&root.join("fake.mp4"), "fake.mp4", None, None).is_err());

        // PDF & TXT work too.
        std::fs::write(root.join("doc.pdf"), b"%PDF-1.7\n").unwrap();
        assert_eq!(
            load_chunk(&root.join("doc.pdf"), "doc.pdf", None, None)
                .unwrap()
                .kind,
            MediaKind::Pdf
        );
        std::fs::write(root.join("notes.txt"), b"hello world").unwrap();
        assert_eq!(
            load_chunk(&root.join("notes.txt"), "notes.txt", None, None)
                .unwrap()
                .kind,
            MediaKind::Text
        );
    }

    #[test]
    fn refuses_traversal_outside_workspace_and_normalizes_file_uri() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(dir.path().join("secret.mp3"), b"ID3\x03").unwrap();
        assert!(matches!(
            locate(&ws, None, "../secret.mp3"),
            Err(FileError::Outside(_))
        ));

        let normalized = normalize_input_path("file:///C:/Users/me/tts%20out.mp3");
        assert_eq!(normalized, "C:/Users/me/tts out.mp3");
    }
}
