//! The OS lock on one lock file: `LockFileEx` on Windows, `flock` on Unix.
//!
//! The file itself stays empty; all it carries is the lock, so a lock file can be created,
//! locked, unlocked and deleted without ever reading or writing a byte.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::SystemTime;

use super::Access;

/// The raw OS object behind a held lock (a `HANDLE` or a file descriptor), for `exec-run`
/// to hand on to the process tree (`docs/tasks/LOCKS.md` §4.5).
#[cfg(windows)]
pub type RawLock = isize;
#[cfg(unix)]
pub type RawLock = std::os::unix::io::RawFd;
#[cfg(not(any(windows, unix)))]
pub type RawLock = usize;

/// Which file an open handle or a path refers to: "the same file seen twice".
///
/// A name is not enough. A lock file can be deleted and created again under the same name, and
/// then an old handle locks a file nobody can see while a newcomer locks the new one. Comparing
/// this value across the two is how that is noticed — here, and in the nesting check of
/// `docs/tasks/LOCKS.md` §4.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileId {
    /// Volume serial number (Windows) or device number (Unix).
    pub device: u64,
    /// File index (Windows) or inode number (Unix).
    pub file: u64,
}

/// The identity of the open file behind `file`.
pub fn file_id(file: &File) -> io::Result<FileId> {
    imp::file_id(file)
}

/// The identity of whatever is at `path` right now.
///
/// Fails when nothing is there any more, which is itself an answer: the file a handle holds is
/// then not the one at the path.
pub fn path_id(path: &Path) -> io::Result<FileId> {
    let file = OpenOptions::new().read(true).open(path)?;
    file_id(&file)
}

/// One open lock file. Dropping it unlocks and closes it.
pub struct LockFile {
    file: File,
}

impl LockFile {
    /// Opens the lock file, creating it if it does not exist yet.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(Self { file })
    }

    /// One non-blocking attempt. `Ok(false)` means somebody else holds it in a way that
    /// conflicts with `access`; the caller backs off and tries again.
    pub fn try_lock(&self, access: Access) -> io::Result<bool> {
        imp::try_lock(&self.file, access)
    }

    /// Releases the lock. Closing the file does the same, so this is only needed to drop the
    /// lock while keeping the file open.
    pub fn unlock(&self) {
        imp::unlock(&self.file);
    }

    /// Whether this handle still holds the file that is at `path` — i.e. whether the lock just
    /// taken is a lock on the resource everybody else will find there.
    ///
    /// `false` after the lock file was deleted and created again, or deleted and not yet
    /// created again: the lock is real but on a file nobody can reach, and taking it must not
    /// count as holding the resource.
    pub fn still_at(&self, path: &Path) -> bool {
        match (file_id(&self.file), path_id(path)) {
            (Ok(open), Ok(at_path)) => open == at_path,
            _ => false,
        }
    }

    /// Marks the lock file as just used, so [`super::cleanup`] leaves it alone.
    pub fn touch(&self) {
        let _ = self.file.set_modified(SystemTime::now());
    }

    /// The handle or descriptor, for `exec-run` to make inheritable.
    pub fn raw(&self) -> RawLock {
        imp::raw(&self.file)
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        imp::unlock(&self.file);
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK,
        LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    use super::{FileId, RawLock};
    use crate::exec::Access;

    /// Half of the address space: the byte range a lock covers (`u32::MAX, u32::MAX`).
    const WHOLE_FILE: u32 = u32::MAX;

    /// `ERROR_LOCK_VIOLATION`: the range is held by somebody else (`LockFileEx` reports it as
    /// the last OS error, `windows-sys` keeps it out of the typed constants).
    const ERROR_LOCK_VIOLATION: i32 = 33;

    pub(super) fn try_lock(file: &File, access: Access) -> io::Result<bool> {
        let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
        if access == Access::Exclusive {
            flags |= LOCKFILE_EXCLUSIVE_LOCK;
        }
        // SAFETY: the handle belongs to `file` and stays open for the call; `overlapped` is a
        // live zeroed struct. Nothing else is touched.
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            let handle = file.as_raw_handle() as HANDLE;
            let ok = LockFileEx(handle, flags, 0, WHOLE_FILE, WHOLE_FILE, &mut overlapped);
            if ok != 0 {
                return Ok(true);
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
                Ok(false)
            } else {
                Err(err)
            }
        }
    }

    pub(super) fn unlock(file: &File) {
        // SAFETY: as in `try_lock`; unlocking a range nobody holds is a no-op that just fails.
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            UnlockFileEx(
                file.as_raw_handle() as HANDLE,
                0,
                WHOLE_FILE,
                WHOLE_FILE,
                &mut overlapped,
            );
        }
    }

    pub(super) fn raw(file: &File) -> RawLock {
        file.as_raw_handle() as RawLock
    }

    /// Volume serial number and file index of the open file.
    pub(super) fn file_id(file: &File) -> io::Result<FileId> {
        // SAFETY: `info` is a plain out-param; the handle belongs to `file` and stays open.
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = zeroed();
            if GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(FileId {
                device: u64::from(info.dwVolumeSerialNumber),
                file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            })
        }
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::unix::io::AsRawFd;

    use super::RawLock;
    use crate::exec::Access;

    pub(super) fn try_lock(file: &File, access: Access) -> io::Result<bool> {
        let operation = match access {
            Access::Shared => libc::LOCK_SH,
            Access::Exclusive => libc::LOCK_EX,
        } | libc::LOCK_NB;
        // SAFETY: `flock` only reads the descriptor; it touches no memory of ours.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        // EWOULDBLOCK and EAGAIN are the same number on every platform we run on.
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(false)
        } else {
            Err(err)
        }
    }

    pub(super) fn unlock(file: &File) {
        // SAFETY: as in `try_lock`, with the descriptor that took the lock.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    pub(super) fn raw(file: &File) -> RawLock {
        file.as_raw_fd()
    }

    /// Device and inode of the open file — what `stat` would report for it.
    pub(super) fn file_id(file: &File) -> io::Result<FileId> {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata()?;
        Ok(FileId {
            device: metadata.dev(),
            file: metadata.ino(),
        })
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use std::fs::File;
    use std::io;

    use super::{FileId, RawLock};
    use crate::exec::Access;

    pub(super) fn try_lock(_file: &File, _access: Access) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no file locks on this platform",
        ))
    }

    pub(super) fn unlock(_file: &File) {}

    pub(super) fn raw(_file: &File) -> RawLock {
        0
    }

    pub(super) fn file_id(_file: &File) -> io::Result<FileId> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no file identities on this platform",
        ))
    }
}
