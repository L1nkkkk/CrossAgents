//! `GET /v1/agents/{key}/files/image`: an image a tool call named by a path (a `ViewImage`
//! tool's `image_path`, say), for the GUI to show as a thumbnail.
//!
//! Only images, only inside the agent's working directory or the system temp directory
//! (that is where tools leave a screenshot or a render), only up to [`MAX_IMAGE_BYTES`],
//! and the file's first bytes must be the format its extension claims.

use std::path::{Path, PathBuf};

use crossagents_orchestrator::files::FileError;

/// The most an image may be; larger files are refused with what they are.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// Bytes looked at to tell an SVG from anything else named `.svg`.
const SNIFF: usize = 4096;

/// The image formats the API serves, as the extension names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Svg,
}

impl ImageType {
    /// The format `path`'s extension names; `None` for anything else.
    pub fn of_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::WebP,
            "bmp" => Self::Bmp,
            "svg" => Self::Svg,
            _ => return None,
        })
    }

    /// The MIME type the image goes out as (`image/png`, `image/svg+xml`, …).
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Svg => "image/svg+xml",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Png => "a png",
            Self::Jpeg => "a jpeg",
            Self::Gif => "a gif",
            Self::WebP => "a webp",
            Self::Bmp => "a bmp",
            Self::Svg => "an svg",
        }
    }

    /// Whether `bytes` start like this format (an SVG is text, so it is read as text).
    fn matches(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            Self::WebP => {
                bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
            }
            Self::Bmp => bytes.starts_with(b"BM"),
            Self::Svg => looks_like_svg(bytes),
        }
    }
}

/// Whether `bytes` read as an SVG document: after an optional byte-order mark, whitespace,
/// XML declaration, comments and doctype, the first element is `<svg`.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(SNIFF)];
    let text = String::from_utf8_lossy(head).into_owned();
    let mut rest = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("<?") {
            let Some(end) = after.find("?>") else {
                return false;
            };
            rest = after[end + 2..].trim_start();
        } else if let Some(after) = rest.strip_prefix("<!--") {
            let Some(end) = after.find("-->") else {
                return false;
            };
            rest = after[end + 3..].trim_start();
        } else if rest.len() >= 9 && rest.as_bytes()[..9].eq_ignore_ascii_case(b"<!doctype") {
            let Some(end) = rest.find('>') else {
                return false;
            };
            rest = rest[end + 1..].trim_start();
        } else {
            break;
        }
    }
    let bytes = rest.as_bytes();
    if bytes.len() < 4 || !bytes[..4].eq_ignore_ascii_case(b"<svg") {
        return false;
    }
    // `<svg>` or `<svg …`, not `<svgfoo`.
    matches!(bytes[4], b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

/// Where `input` points, if it is inside the agent's working directory `root` or the system
/// temp directory: the canonical path, so `..` and symlinks are resolved before the prefix
/// is judged. A relative path is taken from `root` first, then from the temp directory.
pub fn locate(root: &Path, input: &str) -> Result<PathBuf, FileError> {
    let temp = std::env::temp_dir();
    match crate::paths::confine_any(&[root, &temp], input) {
        Ok(found) => Ok(found.abs),
        Err(FileError::Outside(what)) => Err(FileError::Outside(format!(
            "{what} (an image must be in the agent's working directory or the system temp directory)"
        ))),
        Err(other) => Err(other),
    }
}

/// The format and bytes of the image at `abs`, which `input` named: refused unless the
/// extension is an image's, the file is at most [`MAX_IMAGE_BYTES`], and its first bytes
/// are the format's.
pub async fn load(abs: &Path, input: &str) -> Result<(ImageType, Vec<u8>), FileError> {
    let kind = ImageType::of_path(abs).ok_or_else(|| {
        FileError::BadRequest(format!(
            "{input} is not an image: images are png, jpg, jpeg, gif, webp, bmp and svg"
        ))
    })?;
    let meta = tokio::fs::metadata(abs)
        .await
        .map_err(|_| FileError::NotFound(format!("no file {input}")))?;
    if !meta.is_file() {
        return Err(FileError::BadRequest(format!(
            "{input} is not a regular file"
        )));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(too_big(input, meta.len()));
    }
    let bytes = tokio::fs::read(abs)
        .await
        .map_err(|e| FileError::Failed(format!("cannot read {input}: {e}")))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(too_big(input, bytes.len() as u64));
    }
    if !kind.matches(&bytes) {
        return Err(FileError::BadRequest(format!(
            "{input} does not start like {} (its name and its content disagree)",
            kind.name()
        )));
    }
    Ok((kind, bytes))
}

fn too_big(input: &str, len: u64) -> FileError {
    FileError::BadRequest(format!(
        "{input} is {:.1} MB; an image may be at most {} MB",
        len as f64 / (1024.0 * 1024.0),
        MAX_IMAGE_BYTES / (1024 * 1024)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_follow_their_extensions() {
        assert_eq!(ImageType::of_path(Path::new("a.PNG")), Some(ImageType::Png));
        assert_eq!(
            ImageType::of_path(Path::new("a.jpeg")),
            Some(ImageType::Jpeg)
        );
        assert_eq!(ImageType::of_path(Path::new("a.txt")), None);
        assert_eq!(ImageType::of_path(Path::new("png")), None);
    }

    #[test]
    fn first_bytes_must_be_the_format() {
        assert!(ImageType::Png.matches(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]));
        assert!(!ImageType::Png.matches(b"not a png"));
        assert!(ImageType::Jpeg.matches(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(ImageType::Gif.matches(b"GIF89a..."));
        assert!(ImageType::WebP.matches(b"RIFF\x10\x00\x00\x00WEBPVP8 "));
        assert!(!ImageType::WebP.matches(b"RIFF\x10\x00\x00\x00WAVE"));
        assert!(ImageType::Bmp.matches(b"BM6\x00"));
    }

    #[test]
    fn an_svg_may_start_with_a_declaration_a_comment_or_a_doctype() {
        assert!(ImageType::Svg.matches(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(ImageType::Svg.matches(
            b"\xef\xbb\xbf <?xml version=\"1.0\"?>\n<!-- made by hand -->\n<!DOCTYPE svg>\n<SVG width=\"1\">"
        ));
        assert!(!ImageType::Svg.matches(b"<svgfoo>"));
        assert!(!ImageType::Svg.matches(b"{\"not\": \"an svg\"}"));
    }

    #[test]
    fn a_traversal_is_outside_both_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(dir.path().join("secret.png"), b"x").unwrap();
        let err = locate(&root, "../secret.png").unwrap_err();
        assert!(matches!(err, FileError::Outside(_)), "{err}");
        assert!(err.to_string().contains("temp directory"), "{err}");
    }

    #[test]
    fn a_file_in_the_working_directory_or_the_temp_directory_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.png"), b"x").unwrap();
        let abs = locate(&root, "a.png").unwrap();
        assert!(abs.ends_with("a.png"));
        // An absolute path works too, as does one under the temp directory.
        assert!(locate(&root, &root.join("a.png").display().to_string()).is_ok());
        let in_temp = std::env::temp_dir().join("crossagents-image-test.png");
        std::fs::write(&in_temp, b"x").unwrap();
        let found = locate(&root, &in_temp.display().to_string()).unwrap();
        assert_eq!(found, dunce::canonicalize(&in_temp).unwrap());
        std::fs::remove_file(&in_temp).ok();
    }
}
