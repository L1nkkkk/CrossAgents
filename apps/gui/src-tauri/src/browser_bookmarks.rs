use std::{
    fs,
    path::{Path, PathBuf},
};

use reqwest::Url;
use serde::Serialize;
use serde_json::Value;

const MAX_BAR_ITEMS: usize = 40;
const MAX_FOLDER_ITEMS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BookmarkLink {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BookmarkEntry {
    Url(BookmarkLink),
    Folder {
        kind: &'static str,
        id: String,
        name: String,
        children: Vec<BookmarkLink>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BookmarksResult {
    pub source: Option<String>,
    pub entries: Vec<BookmarkEntry>,
}

/// Only http(s) links belong on the bookmark bar; javascript: and file: are ignored.
pub fn bookmark_url_allowed(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    Url::parse(trimmed).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"))
}

fn to_link(node: &Value, fallback_id: String) -> Option<BookmarkLink> {
    if node.get("type").and_then(Value::as_str) != Some("url") {
        return None;
    }
    let url = node.get("url").and_then(Value::as_str)?;
    if !bookmark_url_allowed(url) {
        return None;
    }
    let raw_name = node
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let name = if raw_name.is_empty() {
        Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| url.to_owned())
    } else {
        raw_name.to_owned()
    };
    let id = node
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or(fallback_id);
    Some(BookmarkLink {
        kind: "url",
        id,
        name,
        url: url.to_owned(),
    })
}

fn collect_links(nodes: &[Value], prefix: &str, out: &mut Vec<BookmarkLink>) {
    for (index, node) in nodes.iter().enumerate() {
        if out.len() >= MAX_FOLDER_ITEMS {
            break;
        }
        match node.get("type").and_then(Value::as_str) {
            Some("url") => {
                if let Some(link) = to_link(node, format!("{prefix}-{index}")) {
                    out.push(link);
                }
            }
            Some("folder") => {
                if let Some(children) = node.get("children").and_then(Value::as_array) {
                    collect_links(children, &format!("{prefix}-{index}"), out);
                }
            }
            _ => {}
        }
    }
}

/// Parses a Chromium `Bookmarks` or `AccountBookmarks` JSON string into top-level bookmark-bar
/// links and folders. When both `bookmark_bar` and `other` have bookmarks, `other` is appended as
/// an「其他书签」folder at the end; when `bookmark_bar` is empty, `other` is used directly.
pub fn parse_chrome_bookmarks(raw_json: &str) -> Vec<BookmarkEntry> {
    let Ok(parsed) = serde_json::from_str::<Value>(raw_json) else {
        return Vec::new();
    };
    let Some(roots) = parsed.get("roots").and_then(Value::as_object) else {
        return Vec::new();
    };
    let bar_children = roots
        .get("bookmark_bar")
        .and_then(|v| v.get("children"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let other_children = roots
        .get("other")
        .and_then(|v| v.get("children"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let source = if !bar_children.is_empty() {
        bar_children
    } else {
        other_children
    };

    let mut entries = Vec::new();
    for (index, node) in source.iter().enumerate() {
        if entries.len() >= MAX_BAR_ITEMS {
            break;
        }
        match node.get("type").and_then(Value::as_str) {
            Some("url") => {
                if let Some(link) = to_link(node, format!("b-{index}")) {
                    entries.push(BookmarkEntry::Url(link));
                }
            }
            Some("folder") => {
                let Some(raw_children) = node.get("children").and_then(Value::as_array) else {
                    continue;
                };
                let mut children = Vec::new();
                collect_links(raw_children, &format!("f-{index}"), &mut children);
                if children.is_empty() {
                    continue;
                }
                let name = node
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("文件夹")
                    .to_owned();
                let id = node
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("f-{index}"));
                entries.push(BookmarkEntry::Folder {
                    kind: "folder",
                    id,
                    name,
                    children,
                });
            }
            _ => {}
        }
    }

    if !bar_children.is_empty() && !other_children.is_empty() {
        let mut other_links = Vec::new();
        collect_links(other_children, "other", &mut other_links);
        if !other_links.is_empty() {
            entries.push(BookmarkEntry::Folder {
                kind: "folder",
                id: "other-bookmarks".into(),
                name: "其他书签".into(),
                children: other_links,
            });
        }
    }

    entries
}

fn expand_profiles(root: &Path) -> Vec<PathBuf> {
    let profiles = ["Default", "Profile 1", "Profile 2", "Profile 3"];
    let files = ["AccountBookmarks", "Bookmarks"];
    let mut out = Vec::with_capacity(profiles.len() * files.len());
    for profile in profiles {
        for file in files {
            out.push(root.join(profile).join(file));
        }
    }
    out
}

fn candidate_bookmark_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Local"));
        let mut out = expand_profiles(&local.join("Google").join("Chrome").join("User Data"));
        out.extend(expand_profiles(
            &local.join("Microsoft").join("Edge").join("User Data"),
        ));
        out
    }
    #[cfg(target_os = "macos")]
    {
        let support = home.join("Library").join("Application Support");
        let mut out = expand_profiles(&support.join("Google").join("Chrome"));
        out.extend(expand_profiles(&support.join("Microsoft Edge")));
        out
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let mut out = expand_profiles(&cfg.join("google-chrome"));
        out.extend(expand_profiles(&cfg.join("chromium")));
        out
    }
}

#[tauri::command]
pub fn browser_bookmarks() -> BookmarksResult {
    for path in candidate_bookmark_paths() {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let entries = parse_chrome_bookmarks(&raw);
        if !entries.is_empty() {
            return BookmarksResult {
                source: Some(path.to_string_lossy().into_owned()),
                entries,
            };
        }
    }
    BookmarksResult {
        source: None,
        entries: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_disallowed_schemes_and_parses_folders() {
        let raw = r#"{
          "roots": {
            "bookmark_bar": {
              "children": [
                { "id": "1", "type": "url", "name": "GitHub", "url": "https://github.com/" },
                { "id": "2", "type": "url", "name": "Bad", "url": "javascript:alert(1)" },
                {
                  "id": "3",
                  "type": "folder",
                  "name": "工作",
                  "children": [
                    { "id": "4", "type": "url", "name": "", "url": "https://example.com/docs" },
                    { "id": "5", "type": "url", "name": "File", "url": "file:///etc/passwd" }
                  ]
                }
              ]
            },
            "other": {
              "children": [
                { "id": "6", "type": "url", "name": "Extra", "url": "http://localhost:3000" }
              ]
            }
          }
        }"#;
        let entries = parse_chrome_bookmarks(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            BookmarkEntry::Url(BookmarkLink {
                kind: "url",
                id: "1".into(),
                name: "GitHub".into(),
                url: "https://github.com/".into(),
            })
        );
        assert_eq!(
            entries[1],
            BookmarkEntry::Folder {
                kind: "folder",
                id: "3".into(),
                name: "工作".into(),
                children: vec![BookmarkLink {
                    kind: "url",
                    id: "4".into(),
                    name: "example.com".into(),
                    url: "https://example.com/docs".into(),
                }],
            }
        );
        assert_eq!(
            entries[2],
            BookmarkEntry::Folder {
                kind: "folder",
                id: "other-bookmarks".into(),
                name: "其他书签".into(),
                children: vec![BookmarkLink {
                    kind: "url",
                    id: "6".into(),
                    name: "Extra".into(),
                    url: "http://localhost:3000".into(),
                }],
            }
        );
    }

    #[test]
    fn rejects_invalid_json_and_non_http_urls() {
        assert!(parse_chrome_bookmarks("not json").is_empty());
        assert!(!bookmark_url_allowed("javascript:void(0)"));
        assert!(!bookmark_url_allowed("file:///C:/test.html"));
        assert!(bookmark_url_allowed("https://example.com"));
    }
}
