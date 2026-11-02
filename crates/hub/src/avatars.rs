//! The avatar gallery: pictures a person uploads, by content hash under `<home>/avatars/`. A
//! session without an avatar of its own gets one, least used first, and keeps it: adding a
//! picture must not change anyone's face.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use crossagents_api_types::{AttachmentContent, MAX_AVATAR_BYTES, MAX_AVATAR_UPLOAD, MAX_AVATARS};
use crossagents_core::{Agent, AgentProfile, CoreError, EventBus, EventKind, Store};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::attachments::{Refused, sniff};

fn dir(home: &Path) -> PathBuf {
    home.join("avatars")
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The gallery's pictures, in a stable order. Nothing stored yet is an empty gallery.
pub fn list(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir(home)) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| valid_id(name))
        .collect();
    ids.sort();
    ids
}

/// Checks base64 pictures against the limits and adds them; one already there is not added twice.
pub fn store(home: &Path, images: &[String]) -> Result<Vec<String>, Refused> {
    if images.len() > MAX_AVATAR_UPLOAD {
        return Err(Refused(format!(
            "at most {MAX_AVATAR_UPLOAD} pictures at a time (got {})",
            images.len()
        )));
    }
    let mut decoded = Vec::with_capacity(images.len());
    for image in images {
        let bytes = STANDARD
            .decode(image.as_bytes())
            .map_err(|e| Refused(format!("not valid base64 ({e})")))?;
        if bytes.len() > MAX_AVATAR_BYTES {
            return Err(Refused(format!(
                "a picture may be at most {} KB",
                MAX_AVATAR_BYTES / 1024
            )));
        }
        // By its bytes, not what the client calls it: an SVG can carry a script.
        if !matches!(sniff(&bytes), "image/png" | "image/jpeg" | "image/webp") {
            return Err(Refused("avatars are PNG, JPEG or WebP".into()));
        }
        decoded.push(bytes);
    }
    let existing = list(home);
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| Refused(format!("cannot store pictures: {e}")))?;
    let mut count = existing.len();
    let mut added = Vec::new();
    for bytes in decoded {
        let id = format!("{:x}", Sha256::digest(&bytes));
        if existing.contains(&id) || added.contains(&id) {
            continue;
        }
        if count >= MAX_AVATARS {
            return Err(Refused(format!(
                "the gallery holds at most {MAX_AVATARS} pictures"
            )));
        }
        std::fs::write(dir.join(&id), &bytes)
            .map_err(|e| Refused(format!("cannot store a picture: {e}")))?;
        count += 1;
        added.push(id);
    }
    Ok(added)
}

/// One picture; `None` for anything else (a bad id, one that is gone).
pub fn read(home: &Path, id: &str) -> Option<AttachmentContent> {
    if !valid_id(id) {
        return None;
    }
    let bytes = std::fs::read(dir(home).join(id)).ok()?;
    Some(AttachmentContent {
        id: id.to_owned(),
        mime: sniff(&bytes).to_owned(),
        size: bytes.len() as u64,
        data: STANDARD.encode(bytes),
    })
}

/// Removes a picture; `false` when there is no such picture.
pub fn remove(home: &Path, id: &str) -> std::io::Result<bool> {
    if !valid_id(id) {
        return Ok(false);
    }
    match std::fs::remove_file(dir(home).join(id)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// A person chose text (an emoji or letters) or a picture: nothing is drawn over that.
pub fn wants_image(profile: &AgentProfile) -> bool {
    profile.avatar.is_none() && profile.avatar_image.is_none()
}

/// Draws from the least used pictures, so a team repeats a face only when it must.
fn pick(gallery: &[String], used: &HashMap<String, usize>, roll: u128) -> Option<String> {
    let count = |id: &String| used.get(id).copied().unwrap_or(0);
    let least = gallery.iter().map(count).min()?;
    let free: Vec<&String> = gallery.iter().filter(|id| count(id) == least).collect();
    free.get((roll % free.len() as u128) as usize)
        .map(|id| (*id).clone())
}

fn roll() -> u128 {
    Uuid::new_v4().as_u128()
}

fn usage(agents: &[Agent]) -> HashMap<String, usize> {
    let mut used = HashMap::new();
    for agent in agents {
        if let Some(id) = &agent.profile.avatar_image {
            *used.entry(id.clone()).or_insert(0) += 1;
        }
    }
    used
}

/// Fills in a new session's picture before it is stored; an empty gallery changes nothing.
pub async fn assign_new(
    home: &Path,
    store: &Arc<dyn Store>,
    profile: &mut AgentProfile,
) -> Result<(), CoreError> {
    if !wants_image(profile) {
        return Ok(());
    }
    let gallery = list(home);
    if gallery.is_empty() {
        return Ok(());
    }
    let used = usage(&store.list_agents().await?);
    profile.avatar_image = pick(&gallery, &used, roll());
    Ok(())
}

/// Gives every session that has no avatar of its own a picture, archived ones too (the project
/// overview still shows them). Returns how many got one.
pub async fn assign_missing(
    home: &Path,
    store: &Arc<dyn Store>,
    bus: &EventBus,
) -> Result<usize, CoreError> {
    let gallery = list(home);
    if gallery.is_empty() {
        return Ok(0);
    }
    let agents = store.list_agents().await?;
    let mut used = usage(&agents);
    let mut assigned = 0;
    for mut agent in agents {
        if !wants_image(&agent.profile) {
            continue;
        }
        let Some(id) = pick(&gallery, &used, roll()) else {
            break;
        };
        *used.entry(id.clone()).or_insert(0) += 1;
        agent.profile.avatar_image = Some(id);
        store.update_agent_profile(agent.id, &agent.profile).await?;
        bus.publish(EventKind::AgentUpdated { agent }).await?;
        assigned += 1;
    }
    Ok(assigned)
}

/// Takes a removed picture off the sessions showing it; they go back to their letter, and the
/// next upload may give them another.
pub async fn release(store: &Arc<dyn Store>, bus: &EventBus, id: &str) -> Result<(), CoreError> {
    for mut agent in store.list_agents().await? {
        if agent.profile.avatar_image.as_deref() != Some(id) {
            continue;
        }
        agent.profile.avatar_image = None;
        store.update_agent_profile(agent.id, &agent.profile).await?;
        bus.publish(EventKind::AgentUpdated { agent }).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 1];
    const WEBP: &[u8] = b"RIFF0000WEBPVP8 ";

    fn b64(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    #[test]
    fn stores_once_by_hash_reads_back_and_removes() {
        let home = tempfile::tempdir().unwrap();
        assert!(list(home.path()).is_empty());
        let added = store(home.path(), &[b64(PNG), b64(PNG), b64(WEBP)]).unwrap();
        assert_eq!(added.len(), 2);
        assert!(store(home.path(), &[b64(PNG)]).unwrap().is_empty());
        assert_eq!(list(home.path()).len(), 2);
        let back = read(home.path(), &added[0]).unwrap();
        assert_eq!(STANDARD.decode(back.data).unwrap(), PNG);
        assert_eq!(back.mime, "image/png");
        assert!(remove(home.path(), &added[0]).unwrap());
        assert!(!remove(home.path(), &added[0]).unwrap());
        assert!(read(home.path(), &added[0]).is_none());
    }

    #[test]
    fn refuses_other_types_big_files_and_paths() {
        let home = tempfile::tempdir().unwrap();
        let svg = b64(b"<svg onload=\"alert(1)\"/>");
        assert!(store(home.path(), &[svg]).unwrap_err().0.contains("PNG"));
        let mut big = PNG.to_vec();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert!(store(home.path(), &[b64(&big)]).is_err());
        assert!(store(home.path(), &["%%".to_owned()]).is_err());
        assert!(read(home.path(), "../token").is_none());
        assert!(!remove(home.path(), "../token").unwrap());
    }

    #[test]
    fn picks_among_the_least_used() {
        let gallery = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let used = HashMap::from([
            ("a".to_owned(), 2),
            ("b".to_owned(), 1),
            ("c".to_owned(), 1),
        ]);
        assert_eq!(pick(&gallery, &used, 0).as_deref(), Some("b"));
        assert_eq!(pick(&gallery, &used, 1).as_deref(), Some("c"));
        assert_eq!(pick(&[], &used, 0), None);
    }

    #[test]
    fn text_or_a_chosen_picture_is_kept() {
        assert!(wants_image(&AgentProfile::default()));
        assert!(!wants_image(&AgentProfile {
            avatar: Some("🎨".into()),
            ..Default::default()
        }));
        assert!(!wants_image(&AgentProfile {
            avatar_image: Some("x".into()),
            ..Default::default()
        }));
    }
}
