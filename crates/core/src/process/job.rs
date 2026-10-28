//! Windows Job Objects: a process tree that ends with its owner.
//!
//! A job created here has `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: when its last handle closes
//! (its owner exits for any reason, crash and Task Manager included), Windows kills every
//! process still in it. Children inherit their parent's job, so putting an agent in one
//! takes its whole tree along: shells, MCP servers, dev servers, builds.
//!
//! Elsewhere [`Job::new`] fails and callers go on without one; on Unix [`kill_group`] ends a
//! process group instead.

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        ResumeThread, THREAD_SUSPEND_RESUME,
    };

    /// An owned job handle (kept as an integer so the type is `Send`).
    pub struct Job(isize);

    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: plain Win32 calls; the handle is checked and owned by the result.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Self(handle as isize);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        fn handle(&self) -> HANDLE {
            self.0 as HANDLE
        }

        /// Puts this process in the job; everything it starts from now on is in it too.
        pub fn assign_current(&self) -> io::Result<()> {
            // SAFETY: the pseudo handle of the current process needs no closing.
            let ok = unsafe { AssignProcessToJobObject(self.handle(), GetCurrentProcess()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// How many processes are alive in the job right now
        /// (`JobObjectBasicAccountingInformation.ActiveProcesses`).
        ///
        /// The job is unnamed on purpose: it dies with its last handle, so it cannot be used to
        /// find a predecessor (`docs/tasks/LOCKS.md` §5, conclusion 3). This is only for
        /// waiting until the job is empty before releasing the locks.
        pub fn active_processes(&self) -> io::Result<u32> {
            // SAFETY: `info` is a live out-param of exactly the size the call is given.
            unsafe {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                let ok = QueryInformationJobObject(
                    self.handle(),
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

        /// Puts process `pid` in the job.
        pub fn assign(&self, pid: u32) -> io::Result<()> {
            // SAFETY: the process handle is checked and closed before returning.
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let ok = AssignProcessToJobObject(self.handle(), process);
                let err = io::Error::last_os_error();
                CloseHandle(process);
                if ok == 0 {
                    return Err(err);
                }
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; closing the last one kills the job's processes.
            unsafe {
                CloseHandle(self.handle());
            }
        }
    }

    /// Lets process `pid`, started with `CREATE_SUSPENDED`, run: resumes its threads (a
    /// suspended process has only its first one).
    pub fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: the snapshot and thread handles are checked and closed before returning;
        // `entry` is sized as Thread32First requires.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut resumed = 0;
            let mut more = Thread32First(snapshot, &mut entry);
            while more != 0 {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        if ResumeThread(thread) != u32::MAX {
                            resumed += 1;
                        }
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
            if resumed == 0 {
                return Err(io::Error::other(format!(
                    "no thread of process {pid} could be resumed"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(super) use imp::resume;

/// Kills process group `group` (SIGKILL); a group that is already gone is fine.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn kill_group(group: u32) {
    // SAFETY: killpg only sends a signal; it touches no memory of ours.
    unsafe {
        libc::killpg(group as libc::pid_t, libc::SIGKILL);
    }
}

/// Whether any process is still in group `group`: the Unix stand-in for the job's active
/// process count (`kill(-pgid, 0)`).
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn group_alive(group: u32) -> bool {
    // SAFETY: signal 0 sends nothing; it only asks whether the group exists. A group of
    // somebody else's processes answers `EPERM`, which still means it is there.
    unsafe {
        if libc::kill(-(group as libc::pid_t), 0) == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;

    pub struct Job(());

    impl Job {
        pub fn new() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "job objects are Windows-only",
            ))
        }

        pub fn assign_current(&self) -> io::Result<()> {
            Ok(())
        }

        pub fn assign(&self, _pid: u32) -> io::Result<()> {
            Ok(())
        }

        pub fn active_processes(&self) -> io::Result<u32> {
            Ok(0)
        }
    }
}

pub use imp::Job;

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn closing_the_job_kills_its_process() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let job = Job::new().unwrap();
        job.assign(child.id()).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "still running");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
