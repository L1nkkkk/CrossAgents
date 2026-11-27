//! Windows hidden ConPTY console host for pipe-based agent CLIs.
//!
//! When an agent process is spawned with `CREATE_NO_WINDOW`, it has no attached console; if the
//! agent itself (e.g. a native binary like `agy.exe`) spawns a child console program without
//! `CREATE_NO_WINDOW`, Windows allocates a brand-new visible `conhost.exe` window that flashes on
//! screen. Attaching the process to an invisible ConPTY (`CreatePseudoConsole` +
//! `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`) while keeping `stdin`/`stdout`/`stderr` redirected to
//! clean anonymous pipes (`STARTF_USESTDHANDLES`) gives the entire process tree a hidden console
//! without injecting ANSI escapes or line wrapping into the `stream-json` pipes.

#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossagents_core::process::Job;
use tokio::sync::watch;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::launch::{ExitInfo, Killer, LaunchSpec, Launched, ProcessHandle};
use crate::process::StderrTail;

struct OwnedConPty(HPCON);
// SAFETY: HPCON is an OS handle that can be closed from any thread once the process ends.
unsafe impl Send for OwnedConPty {}
unsafe impl Sync for OwnedConPty {}

impl Drop for OwnedConPty {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: handle was created by CreatePseudoConsole and is closed once on drop.
            unsafe { ClosePseudoConsole(self.0) };
        }
    }
}

struct SendHandle(HANDLE);
// SAFETY: Win32 process handles are thread-safe to wait on, terminate, and close.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

impl Drop for SendHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: owned process handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct ConPtyKiller {
    process: Arc<SendHandle>,
    killed: Arc<AtomicBool>,
    job: Arc<Option<Job>>,
}

impl Killer for ConPtyKiller {
    fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        if let Some(job) = self.job.as_ref() {
            let _ = job.terminate();
        }
        // SAFETY: `self.process.0` stays valid until `Arc<SendHandle>` is dropped after wait.
        unsafe {
            TerminateProcess(self.process.0, 1);
        }
    }
}

struct AttrList {
    _buf: Vec<u8>,
    /// Heap-boxed so its address stays stable across moves while referenced by the attribute list.
    _handles: Box<[HANDLE; 3]>,
    ptr: LPPROC_THREAD_ATTRIBUTE_LIST,
}

impl AttrList {
    fn new(hpc: HPCON, stdio: [HANDLE; 3]) -> io::Result<Self> {
        let mut size: usize = 0;
        // SAFETY: first call queries required buffer size for 2 attributes
        // (PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE + PROC_THREAD_ATTRIBUTE_HANDLE_LIST).
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; size];
        let ptr = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `buf` is sized as requested by InitializeProcThreadAttributeList.
        let ok = unsafe { InitializeProcThreadAttributeList(ptr, 2, 0, &mut size) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let ok = unsafe {
            UpdateProcThreadAttribute(
                ptr,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const _,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null_mut(),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(ptr) };
            return Err(err);
        }
        // Restrict handle inheritance to only this child's three stdio pipes so concurrent spawns
        // in the same process never inherit another child's stdin/stdout/stderr handles.
        let handles = Box::new(stdio);
        let ok = unsafe {
            UpdateProcThreadAttribute(
                ptr,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of::<[HANDLE; 3]>(),
                null_mut(),
                null_mut(),
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            unsafe { DeleteProcThreadAttributeList(ptr) };
            return Err(err);
        }
        Ok(Self {
            _buf: buf,
            _handles: handles,
            ptr,
        })
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was initialized by InitializeProcThreadAttributeList.
        unsafe { DeleteProcThreadAttributeList(self.ptr) };
    }
}

fn make_inheritable(handle: HANDLE) -> io::Result<()> {
    // SAFETY: valid pipe handle owned by this function.
    let ok = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn quote_win_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.bytes().any(|b| matches!(b, b' ' | b'\t' | b'"')) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

fn to_wide_null(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn build_env_block(spec: &LaunchSpec) -> Vec<u16> {
    // Case-insensitive keys on Windows: preserve the latest casing.
    let mut env: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        env.insert(k.to_string_lossy().to_ascii_uppercase(), (k, v));
    }
    for remove in &spec.env_remove {
        env.remove(&remove.to_ascii_uppercase());
    }
    for (k, v) in &spec.env {
        env.insert(
            k.to_ascii_uppercase(),
            (OsString::from(k), OsString::from(v)),
        );
    }
    let mut block = Vec::new();
    for (_, (k, v)) in env {
        block.extend(k.encode_wide());
        block.push(b'=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// A ConPTY-attached child whose stdio remain clean anonymous pipes.
pub(crate) struct PipedChild {
    pub stdin: tokio::fs::File,
    pub stdout: tokio::fs::File,
    pub stderr: tokio::fs::File,
    pub process: ProcessHandle,
}

/// Spawns `spec` attached to a hidden ConPTY while keeping `stdin`/`stdout`/`stderr` on clean
/// anonymous pipes.
pub(crate) fn spawn(spec: &LaunchSpec) -> io::Result<Launched> {
    let piped = spawn_piped(spec, true)?;
    let stderr = StderrTail::spawn(piped.stderr);
    Ok(Launched {
        stdin: Box::new(piped.stdin),
        stdout: Box::new(piped.stdout),
        stderr,
        process: piped.process,
    })
}

/// Spawns `spec` attached to a hidden ConPTY and returns raw async pipes for stdin/stdout/stderr.
///
/// Set `assign_job` to `false` when the caller already runs inside a kill-on-close job object
/// (such as `host::serve`).
pub(crate) fn spawn_piped(spec: &LaunchSpec, assign_job: bool) -> io::Result<PipedChild> {
    let (pty_in_read, pty_in_write) = io::pipe()?;
    let (mut pty_out_read, pty_out_write) = io::pipe()?;
    let (stdin_read, stdin_write) = io::pipe()?;
    let (stdout_read, stdout_write) = io::pipe()?;
    let (stderr_read, stderr_write) = io::pipe()?;

    make_inheritable(stdin_read.as_raw_handle() as HANDLE)?;
    make_inheritable(stdout_write.as_raw_handle() as HANDLE)?;
    make_inheritable(stderr_write.as_raw_handle() as HANDLE)?;

    let mut hpc: HPCON = 0;
    // SAFETY: `pty_in_read` and `pty_out_write` are valid pipe handles.
    let hr = unsafe {
        CreatePseudoConsole(
            COORD { X: 120, Y: 30 },
            pty_in_read.as_raw_handle() as HANDLE,
            pty_out_write.as_raw_handle() as HANDLE,
            0,
            &mut hpc,
        )
    };
    if hr < 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let conpty = OwnedConPty(hpc);
    // ConPTY duplicated its ends; drop ours so the PTY closes cleanly when `conpty` drops.
    drop(pty_in_read);
    drop(pty_out_write);

    let stdio = [
        stdin_read.as_raw_handle() as HANDLE,
        stdout_write.as_raw_handle() as HANDLE,
        stderr_write.as_raw_handle() as HANDLE,
    ];
    let attr_list = AttrList::new(conpty.0, stdio)?;
    const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    si.StartupInfo.wShowWindow = 0; // SW_HIDE
    si.StartupInfo.hStdInput = stdin_read.as_raw_handle() as HANDLE;
    si.StartupInfo.hStdOutput = stdout_write.as_raw_handle() as HANDLE;
    si.StartupInfo.hStdError = stderr_write.as_raw_handle() as HANDLE;
    si.lpAttributeList = attr_list.ptr;

    let mut cmdline = quote_win_arg(&spec.program.to_string_lossy());
    for arg in &spec.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_win_arg(arg));
    }
    let mut cmdline_wide = to_wide_null(OsStr::new(&cmdline));
    let cwd_wide = to_wide_null(spec.cwd.as_os_str());
    let env_block = build_env_block(spec);

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: all pointers remain valid across the synchronous CreateProcessW call.
    let ok = unsafe {
        CreateProcessW(
            null(),
            cmdline_wide.as_mut_ptr(),
            null(),
            null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env_block.as_ptr().cast(),
            cwd_wide.as_ptr(),
            &mut si.StartupInfo,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: we do not need the primary thread handle.
    unsafe { CloseHandle(pi.hThread) };
    drop(attr_list);
    // Drop child-side pipe ends in the parent so EOF propagates when the child exits.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);

    // Drain any grandchild output sent directly to CONOUT$ so it never blocks.
    std::thread::Builder::new()
        .name("conpty-drain".into())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pty_out_read.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        })
        .ok();

    let pid = Some(pi.dwProcessId);
    let job = Arc::new(if assign_job {
        pid.and_then(|pid| {
            let job = Job::new().ok()?;
            match job.assign(pid) {
                Ok(()) => Some(job),
                Err(err) => {
                    tracing::debug!(%err, "conpty agent process runs outside a job object");
                    None
                }
            }
        })
    } else {
        None
    });

    let process = Arc::new(SendHandle(pi.hProcess));
    let killed = Arc::new(AtomicBool::new(false));
    let (exit_tx, exit_rx) = watch::channel(None);

    let killer = Arc::new(ConPtyKiller {
        process: process.clone(),
        killed: killed.clone(),
        job: job.clone(),
    });

    tokio::task::spawn_blocking(move || {
        let _pty_in_write = pty_in_write;
        let _conpty = conpty;
        let _job = job;
        // SAFETY: `process.0` is an owned process handle kept alive by `process`.
        unsafe {
            WaitForSingleObject(process.0, INFINITE);
        }
        let mut raw_code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(process.0, &mut raw_code) };
        let code = (ok != 0).then_some(raw_code as i32);
        let _ = exit_tx.send(Some(ExitInfo::from_code(
            code,
            killed.load(Ordering::SeqCst),
        )));
    });

    // SAFETY: each pipe handle is uniquely owned and converted once into a File.
    let stdin_file = unsafe { File::from_raw_handle(OwnedHandle::from(stdin_write).into_raw_handle()) };
    let stdout_file = unsafe { File::from_raw_handle(OwnedHandle::from(stdout_read).into_raw_handle()) };
    let stderr_file = unsafe { File::from_raw_handle(OwnedHandle::from(stderr_read).into_raw_handle()) };

    let stdin = tokio::fs::File::from_std(stdin_file);
    let stdout = tokio::fs::File::from_std(stdout_file);
    let stderr = tokio::fs::File::from_std(stderr_file);

    Ok(PipedChild {
        stdin,
        stdout,
        stderr,
        process: ProcessHandle::new(pid, None, exit_rx, killer),
    })
}
