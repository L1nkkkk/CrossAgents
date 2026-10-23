//! Windows Job Objects: a process tree that ends with its owner.
//!
//! A job created here has `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: when its last handle closes
//! (its owner exits for any reason, crash and Task Manager included), Windows kills every
//! process still in it. Children inherit their parent's job, so putting an agent in one
//! takes its whole tree along: shells, MCP servers, dev servers, builds.
//!
//! Elsewhere [`Job::new`] fails and callers go on without one.

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
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
