//! Windows primitives shared by `tests/lock_kill_race.rs` and its helper binary.
//!
//! Nothing here is product code: it is the smallest set of calls that reproduces what the
//! executor will do (an exclusive `LockFileEx` on a lock file, a named job with
//! `KILL_ON_JOB_CLOSE`, see `crates/core/src/process/job.rs`), plus the checks the test
//! makes from the outside (is that process still running, how many processes does that job
//! still hold).

#![allow(unsafe_code)]
// The module is compiled twice (helper binary and test), and each needs a different half.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::io;
use std::mem::zeroed;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, OPEN_ALWAYS, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
    QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

/// `ERROR_LOCK_VIOLATION`: the range is locked by somebody else.
pub const ERROR_LOCK_VIOLATION: i32 = 33;

/// `JOB_OBJECT_QUERY` for `OpenJobObjectW` (windows-sys does not export it).
const JOB_OBJECT_QUERY: u32 = 0x0004;

pub fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Opens (creating if needed) the lock file, like the executor does.
pub fn open_lock(path: &OsStr, inheritable: bool) -> io::Result<HANDLE> {
    // SAFETY: plain Win32 calls; `name` and `attributes` outlive the call, and the handle is
    // checked against INVALID_HANDLE_VALUE before it is returned.
    unsafe {
        let name = wide(path);
        let mut sa: SECURITY_ATTRIBUTES = zeroed();
        sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
        sa.bInheritHandle = i32::from(inheritable);
        let attributes = if inheritable {
            &raw const sa
        } else {
            std::ptr::null()
        };
        let handle = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            attributes,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }
}

/// Locks the whole file exclusively, waiting for whoever holds it.
pub fn lock(handle: HANDLE) -> io::Result<()> {
    // SAFETY: `overlapped` is a live zeroed struct for the duration of the call.
    unsafe {
        let mut overlapped: OVERLAPPED = zeroed();
        let ok = LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// One non-blocking attempt: `false` means somebody else holds the lock.
pub fn try_lock(handle: HANDLE) -> io::Result<bool> {
    // SAFETY: as in `lock`.
    unsafe {
        let mut overlapped: OVERLAPPED = zeroed();
        let ok = LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        );
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

pub fn unlock(handle: HANDLE) {
    // SAFETY: as in `lock`; the same zeroed range is unlocked.
    unsafe {
        let mut overlapped: OVERLAPPED = zeroed();
        UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &mut overlapped);
    }
}

pub fn close(handle: HANDLE) {
    // SAFETY: we own every handle we close.
    unsafe {
        CloseHandle(handle);
    }
}

/// Volume serial number and file index of an open handle — "the same file seen twice".
pub fn file_id(handle: HANDLE) -> Option<(u32, u64)> {
    // SAFETY: `info` is a plain out-param; a handle to something that is not a file fails.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = zeroed();
        if GetFileInformationByHandle(handle, &mut info) == 0 {
            return None;
        }
        Some((
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ))
    }
}

/// Whether process `pid` is still running: `WaitForSingleObject` with a zero timeout.
pub fn alive(pid: u32) -> bool {
    // SAFETY: the process handle is checked and closed; a zero timeout never blocks.
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let state = WaitForSingleObject(process, 0);
        CloseHandle(process);
        state == WAIT_TIMEOUT
    }
}

/// How long `pid` took to exit from now; `None` if it is still running after `timeout`.
/// A process that was already gone returns zero, which is why the caller also asks [`alive`].
pub fn wait_exit(pid: u32, timeout: std::time::Duration) -> Option<std::time::Duration> {
    // SAFETY: the process handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            return Some(std::time::Duration::ZERO);
        }
        let started = std::time::Instant::now();
        let millis = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        let state = WaitForSingleObject(process, millis);
        let elapsed = started.elapsed();
        CloseHandle(process);
        if state == WAIT_OBJECT_0 { Some(elapsed) } else { None }
    }
}

/// The process's creation time (a `FILETIME`, 100 ns units): a later check compares it so a
/// reused PID is not mistaken for the predecessor.
pub fn created(pid: u32) -> Option<u64> {
    // SAFETY: the process handle is checked and closed; the FILETIMEs are plain out-params.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut creation: FILETIME = zeroed();
        let mut exit: FILETIME = zeroed();
        let mut kernel: FILETIME = zeroed();
        let mut user: FILETIME = zeroed();
        let ok = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

/// `pid` is still the same process that was recorded (creation time included) and running.
pub fn alive_since(pid: u32, created_at: Option<u64>) -> bool {
    match created_at {
        Some(creation) => created(pid) == Some(creation) && alive(pid),
        None => alive(pid),
    }
}

/// Ends process `pid` (the test's stand-in for an outside `taskkill /f`).
pub fn kill(pid: u32) -> io::Result<()> {
    // SAFETY: the process handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ok = TerminateProcess(process, 1);
        let err = io::Error::last_os_error();
        CloseHandle(process);
        if ok == 0 { Err(err) } else { Ok(()) }
    }
}

/// A named job that kills what is in it when its last handle closes — the executor's job.
pub fn create_job(name: &OsStr) -> io::Result<HANDLE> {
    // SAFETY: `name` outlives the call; the handle is checked before it is returned.
    unsafe {
        let name = wide(name);
        let job = CreateJobObjectW(std::ptr::null(), name.as_ptr());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            let err = io::Error::last_os_error();
            CloseHandle(job);
            return Err(err);
        }
        Ok(job)
    }
}

pub fn assign(job: HANDLE, pid: u32) -> io::Result<()> {
    // SAFETY: the process handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ok = AssignProcessToJobObject(job, process);
        let err = io::Error::last_os_error();
        CloseHandle(process);
        if ok == 0 { Err(err) } else { Ok(()) }
    }
}

pub fn is_in_job(job: HANDLE, pid: u32) -> bool {
    // SAFETY: the process handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut inside = 0;
        let ok = IsProcessInJob(process, job, &mut inside);
        CloseHandle(process);
        ok != 0 && inside != 0
    }
}

/// `OpenJobObjectW(name)` — how the competitor finds its predecessor's tree.
pub fn open_job(name: &OsStr) -> io::Result<HANDLE> {
    // SAFETY: `name` outlives the call; the handle is checked before it is returned.
    unsafe {
        let name = wide(name);
        let job = OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

/// `JobObjectBasicAccountingInformation.ActiveProcesses`.
pub fn active_processes(job: HANDLE) -> io::Result<u32> {
    // SAFETY: `info` is a live struct sized exactly as the call is told.
    unsafe {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
        let ok = QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }
}
