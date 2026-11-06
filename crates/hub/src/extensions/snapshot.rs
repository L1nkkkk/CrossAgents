use super::manifest::Manifest;
use anyhow::{Result, ensure};
use crossagents_api_types::ExtensionVersion;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
pub type Metadata = BTreeMap<String, (u64, Option<SystemTime>)>;
fn walk(root: &Path, dir: &Path, out: &mut Metadata, size: &mut u64) -> Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "symbolic links are not allowed: {}",
        dir.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "reparse points are not allowed: {}",
            dir.display()
        );
    }
    if meta.is_dir() {
        for e in fs::read_dir(dir)? {
            walk(root, &e?.path(), out, size)?;
        }
    } else {
        ensure!(meta.is_file(), "not a regular file: {}", dir.display());
        *size += meta.len();
        ensure!(*size <= MAX_BYTES, "extension exceeds 50 MB");
        out.insert(
            dir.strip_prefix(root)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 path"))?
                .replace('\\', "/"),
            (meta.len(), meta.modified().ok()),
        );
    }
    Ok(())
}
pub fn metadata(root: &Path) -> Result<Metadata> {
    let mut out = Metadata::new();
    walk(root, root, &mut out, &mut 0)?;
    Ok(out)
}
pub fn inspect(root: &Path, name: &str) -> Result<(Manifest, ExtensionVersion)> {
    let files = metadata(root)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    for (path, (len, _)) in &files {
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update(len.to_le_bytes());
        let mut f = fs::File::open(root.join(path))?;
        let mut buf = [0; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            bytes += n as u64;
            ensure!(bytes <= MAX_BYTES, "extension exceeds 50 MB");
        }
    }
    let m = Manifest::read(root, name)?;
    let v = m.version(
        root,
        format!("{:x}", hash.finalize()),
        files.len() as u32,
        bytes,
    )?;
    Ok((m, v))
}
pub fn create(source: &Path, versions: &Path, name: &str) -> Result<ExtensionVersion> {
    let before = metadata(source)?;
    fs::create_dir_all(versions)?;
    let tmp = versions.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&tmp)?;
    let result = (|| {
        let mut total = 0;
        for path in before.keys() {
            let src = source.join(path);
            ensure!(
                !fs::symlink_metadata(&src)?.file_type().is_symlink(),
                "symbolic link during copy"
            );
            let dest = tmp.join(path);
            fs::create_dir_all(dest.parent().unwrap())?;
            // A bounded read also enforces the cap if the source grows during copying.
            let mut input = fs::File::open(&src)?.take(MAX_BYTES - total + 1);
            let copied = std::io::copy(&mut input, &mut fs::File::create(&dest)?)?;
            // Preserve executable bits for direct executable commands, then remove writes below.
            fs::set_permissions(&dest, input.get_ref().metadata()?.permissions())?;
            total += copied;
            ensure!(total <= MAX_BYTES, "extension exceeds 50 MB");
        }
        ensure!(
            before == metadata(source)?,
            "source changed while taking snapshot; retry"
        );
        let (_, version) = inspect(&tmp, name)?;
        seal(&tmp)?;
        let dest = versions.join(&version.fingerprint);
        if dest.exists() {
            ensure!(
                inspect(&dest, name)?.1.fingerprint == version.fingerprint,
                "existing snapshot is corrupt"
            );
            remove(&tmp)?;
        } else {
            fs::rename(&tmp, dest)?;
        }
        Ok(version)
    })();
    if tmp.exists() {
        let _ = remove(&tmp);
    }
    result
}
fn seal(path: &Path) -> Result<()> {
    if path.is_dir() {
        for e in fs::read_dir(path)? {
            seal(&e?.path())?;
        }
    }
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)?;
    Ok(())
}
/// Windows refuses deletion of read-only files; clear that attribute first.
// The readonly attribute is cleared only on Windows; Unix uses explicit permissions.
#[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))]
pub fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let link = meta.file_type().is_symlink();
    if link {
        if meta.is_dir() {
            fs::remove_dir(path)?;
        } else {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    #[cfg(windows)]
    {
        let mut perms = meta.permissions();
        perms.set_readonly(false);
        fs::set_permissions(path, perms)?;
    }
    #[cfg(unix)]
    if meta.is_dir() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    if meta.is_dir() {
        for e in fs::read_dir(path)? {
            remove(&e?.path())?;
        }
        fs::remove_dir(path)?;
    } else {
        #[cfg(windows)]
        {
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_readonly(false);
            fs::set_permissions(path, perms)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn path(versions: &Path, name: &str, fingerprint: &str) -> PathBuf {
    versions.join(name).join(fingerprint)
}
