//! Minimal wrappers over the Win32 APIs used by the spike.
//!
//! All of the crate's `unsafe` code lives in this module: every block has a
//! `SAFETY:` comment and the public functions expose a safe interface
//! (handles owned through `OwnedHandle`, errors as `io::Error`).

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PRIORITY_CLASS,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_WORKINGSET,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOBOBJECT_IO_RATE_CONTROL_INFORMATION, JOBOBJECT_NET_RATE_CONTROL_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, JobObjectNetRateControlInformation,
    QueryInformationJobObject, SetInformationJobObject, SetIoRateControlInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// Converts a raw `HANDLE` into an owned handle, checking for errors.
fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a valid handle just returned by a Win32 API and is not
    // owned by any other object; from here on `OwnedHandle` closes it.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle() as HANDLE
}

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn filetime_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

// ---------------------------------------------------------------------------
// Job Object
// ---------------------------------------------------------------------------

/// Limits applied to the Job Object. `KILL_ON_JOB_CLOSE` is always on and
/// `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK` are always off.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobLimits {
    pub process_memory: Option<usize>,
    pub job_memory: Option<usize>,
}

/// Job accounting (times in 100 ns units).
#[derive(Debug, Clone, Copy)]
pub struct JobAccounting {
    pub user_100ns: u64,
    pub kernel_100ns: u64,
    pub total_processes: u32,
    pub active_processes: u32,
    pub terminated_processes: u32,
}

/// Extended limits read from the job.
#[derive(Debug, Clone, Copy)]
pub struct JobExtended {
    pub limit_flags: u32,
    pub process_memory_limit: usize,
    pub job_memory_limit: usize,
    pub peak_process_memory: usize,
    pub peak_job_memory: usize,
}

/// Message received from the completion port associated with the job.
#[derive(Debug, Clone, Copy)]
pub struct JobMessage {
    pub code: u32,
    /// For process messages it is the PID.
    pub value: usize,
}

impl JobMessage {
    pub fn name(&self) -> &'static str {
        match self.code {
            1 => "END_OF_JOB_TIME",
            2 => "END_OF_PROCESS_TIME",
            3 => "ACTIVE_PROCESS_LIMIT",
            4 => "ACTIVE_PROCESS_ZERO",
            6 => "NEW_PROCESS",
            7 => "EXIT_PROCESS",
            8 => "ABNORMAL_EXIT_PROCESS",
            9 => "PROCESS_MEMORY_LIMIT",
            10 => "JOB_MEMORY_LIMIT",
            11 => "NOTIFICATION_LIMIT",
            12 => "JOB_CYCLE_TIME_LIMIT",
            _ => "OTHER",
        }
    }
}

/// Anonymous Job Object owned by the supervisor.
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
    port: Option<OwnedHandle>,
}

impl Job {
    /// Creates an anonymous job (no name in the object namespace, so
    /// no other process can open it by name) with `KILL_ON_JOB_CLOSE`.
    pub fn create(limits: JobLimits) -> io::Result<Job> {
        // SAFETY: null security attributes (non-inheritable handle) and a null
        // name are values accepted by CreateJobObjectW.
        let h = unsafe { CreateJobObjectW(null(), null()) };
        let job = Job {
            handle: owned(h)?,
            port: None,
        };
        job.set_limits(limits)?;
        Ok(job)
    }

    pub fn raw_handle(&self) -> HANDLE {
        raw(&self.handle)
    }

    pub fn set_limits(&self, limits: JobLimits) -> io::Result<()> {
        // SAFETY: POD structure of integers only; zero is a valid value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(m) = limits.process_memory {
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            info.ProcessMemoryLimit = m;
        }
        if let Some(m) = limits.job_memory {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = m;
        }
        info.BasicLimitInformation.LimitFlags = flags;
        self.set_info(JobObjectExtendedLimitInformation, &info)
    }

    fn set_info<T>(&self, class: i32, info: &T) -> io::Result<()> {
        // SAFETY: `info` points to a valid structure of exact size
        // `size_of::<T>()` matching the requested class.
        check(unsafe {
            SetInformationJobObject(
                raw(&self.handle),
                class,
                (info as *const T).cast(),
                size_of::<T>() as u32,
            )
        })
    }

    fn query_info<T>(&self, class: i32) -> io::Result<T> {
        // SAFETY: POD structures of integers/pointers only; zero is valid and the
        // buffer has the exact size of the class's structure.
        let mut info: T = unsafe { zeroed() };
        // SAFETY: see above; the pointer stays valid for the whole call.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                class,
                (&mut info as *mut T).cast(),
                size_of::<T>() as u32,
                null_mut(),
            )
        })?;
        Ok(info)
    }

    /// CPU hard cap as a percentage of the cycles of the whole system (all
    /// logical processors), as defined by `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`.
    /// `rate` is in hundredths of a percentage point (2500 = 25%). `None` removes it.
    pub fn set_cpu_hard_cap(&self, rate: Option<u32>) -> io::Result<()> {
        // SAFETY: POD structure; zero is valid.
        let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        if let Some(r) = rate {
            info.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            info.Anonymous.CpuRate = r;
        }
        self.set_info(JobObjectCpuRateControlInformation, &info)
    }

    /// Associates a completion port to receive the job's notifications.
    pub fn attach_port(&mut self) -> io::Result<()> {
        // SAFETY: creates a new port not associated with a file (INVALID_HANDLE_VALUE).
        let port =
            owned(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) })?;
        let info = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: null_mut(),
            CompletionPort: raw(&port),
        };
        self.set_info(JobObjectAssociateCompletionPortInformation, &info)?;
        self.port = Some(port);
        Ok(())
    }

    /// Reads the messages queued on the port, waiting at most `timeout_ms`
    /// for the first one and 0 ms for the following ones.
    pub fn drain_messages(&self, timeout_ms: u32) -> Vec<JobMessage> {
        let mut out = Vec::new();
        let Some(port) = &self.port else {
            return out;
        };
        let mut wait = timeout_ms;
        loop {
            let mut code = 0u32;
            let mut key = 0usize;
            let mut ov: *mut OVERLAPPED = null_mut();
            // SAFETY: pointers to valid local variables; the port is alive.
            let ok =
                unsafe { GetQueuedCompletionStatus(raw(port), &mut code, &mut key, &mut ov, wait) };
            if ok == 0 {
                break;
            }
            out.push(JobMessage {
                code,
                value: ov as usize,
            });
            wait = 0;
        }
        out
    }

    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: valid job handle owned by `self`.
        check(unsafe { TerminateJobObject(raw(&self.handle), exit_code) })
    }

    pub fn accounting(&self) -> io::Result<JobAccounting> {
        let a: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
            self.query_info(JobObjectBasicAccountingInformation)?;
        Ok(JobAccounting {
            user_100ns: a.TotalUserTime as u64,
            kernel_100ns: a.TotalKernelTime as u64,
            total_processes: a.TotalProcesses,
            active_processes: a.ActiveProcesses,
            terminated_processes: a.TotalTerminatedProcesses,
        })
    }

    pub fn extended(&self) -> io::Result<JobExtended> {
        let e: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
            self.query_info(JobObjectExtendedLimitInformation)?;
        Ok(JobExtended {
            limit_flags: e.BasicLimitInformation.LimitFlags,
            process_memory_limit: e.ProcessMemoryLimit,
            job_memory_limit: e.JobMemoryLimit,
            peak_process_memory: e.PeakProcessMemoryUsed,
            peak_job_memory: e.PeakJobMemoryUsed,
        })
    }

    /// PIDs of the processes currently in the job.
    pub fn pids(&self) -> io::Result<Vec<u32>> {
        // Buffer for the header (2 x u32) plus up to 64 PIDs (usize).
        let mut buf = [0usize; 66];
        // SAFETY: the buffer is usize-aligned and as long as declared.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr().cast(),
                size_of::<[usize; 66]>() as u32,
                null_mut(),
            )
        })?;
        let in_list = buf[0] >> 32;
        Ok(buf[1..1 + in_list.min(64)]
            .iter()
            .map(|&p| p as u32)
            .collect())
    }

    /// Tries to set limits that might require privileges.
    /// Returns the outcome of each attempt (name, result).
    pub fn probe_privileged_limits(&self) -> Vec<(&'static str, Result<(), String>)> {
        let mut out = Vec::new();
        // Minimum/maximum working set.
        // SAFETY: POD structure; zero is valid.
        let mut ws: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        ws.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_WORKINGSET;
        ws.BasicLimitInformation.MinimumWorkingSetSize = 64 << 20;
        ws.BasicLimitInformation.MaximumWorkingSetSize = 512 << 20;
        out.push((
            "JOB_OBJECT_LIMIT_WORKINGSET (64-512 MiB)",
            self.set_info(JobObjectExtendedLimitInformation, &ws)
                .map_err(|e| e.to_string()),
        ));
        // Realtime priority class.
        // SAFETY: POD structure; zero is valid.
        let mut pr: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        pr.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
        pr.BasicLimitInformation.PriorityClass = REALTIME_PRIORITY_CLASS;
        out.push((
            "JOB_OBJECT_LIMIT_PRIORITY_CLASS = REALTIME",
            self.set_info(JobObjectExtendedLimitInformation, &pr)
                .map_err(|e| e.to_string()),
        ));
        // I/O rate control (bandwidth 10 MB/s on all volumes).
        let io = JOBOBJECT_IO_RATE_CONTROL_INFORMATION {
            MaxIops: 0,
            MaxBandwidth: 10_000_000,
            ReservationIops: 0,
            VolumeName: null(),
            BaseIoSize: 0,
            ControlFlags: 1, // JOB_OBJECT_IO_RATE_CONTROL_ENABLE
        };
        // SAFETY: pointer to a valid local structure; null VolumeName = all volumes.
        let r = unsafe { SetIoRateControlInformationJobObject(raw(&self.handle), &io) };
        out.push((
            "SetIoRateControlInformationJobObject (10 MB/s)",
            if r == 0 {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(())
            },
        ));
        // Network rate control.
        let net = JOBOBJECT_NET_RATE_CONTROL_INFORMATION {
            MaxBandwidth: 1_000_000,
            ControlFlags: 1 | 2, // ENABLE | MAX_BANDWIDTH
            DscpTag: 0,
        };
        out.push((
            "JobObjectNetRateControlInformation (1 MB/s)",
            self.set_info(JobObjectNetRateControlInformation, &net)
                .map_err(|e| e.to_string()),
        ));
        out
    }
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// Console mode of the child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleMode {
    /// Console without a window: Windows creates a `conhost.exe` for the child.
    NoWindow,
    /// No console: no `conhost.exe`.
    Detached,
}

/// Parameters for creating a process without a shell.
#[derive(Debug)]
pub struct SpawnSpec<'a> {
    pub exe: &'a Path,
    pub args: &'a [OsString],
    /// Complete environment of the child (no inheritance from the supervisor).
    pub env: &'a [(OsString, OsString)],
    pub stdout: &'a Path,
    pub stderr: &'a Path,
    pub job: Option<&'a Job>,
    pub console: ConsoleMode,
    pub cwd: Option<&'a Path>,
}

/// Created process: owned handle (identity) plus informational PID.
#[derive(Debug)]
pub struct Spawned {
    pub process: OwnedHandle,
    pub pid: u32,
}

/// Quoting of an argument according to the `CommandLineToArgvW`/MSVC CRT rules.
pub fn quote_arg(arg: &OsStr, out: &mut Vec<u16>) {
    let a: Vec<u16> = arg.encode_wide().collect();
    let needs = a.is_empty()
        || a.iter()
            .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
    if !needs {
        out.extend_from_slice(&a);
        return;
    }
    out.push(u16::from(b'"'));
    let mut backslashes = 0usize;
    for &c in &a {
        if c == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if c == u16::from(b'"') {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
    out.push(u16::from(b'"'));
}

fn inheritable_dup(h: &OwnedHandle) -> io::Result<OwnedHandle> {
    let mut dup: HANDLE = null_mut();
    // SAFETY: duplicates a valid handle within the same process; `dup` receives the
    // new inheritable handle, immediately owned by `OwnedHandle`.
    check(unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            raw(h),
            GetCurrentProcess(),
            &mut dup,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    })?;
    owned(dup)
}

/// Creates a process with:
/// - structured arguments (no shell), explicit executable path;
/// - complete environment supplied by the caller;
/// - inheritance limited to the three standard handles only (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`);
/// - assignment to the job already at creation (`PROC_THREAD_ATTRIBUTE_JOB_LIST`),
///   with no time window in which the process is outside the job.
pub fn spawn(spec: &SpawnSpec<'_>) -> io::Result<Spawned> {
    let stdin = std::fs::File::open("NUL")?;
    let stdout = std::fs::File::create(spec.stdout)?;
    let stderr = std::fs::File::create(spec.stderr)?;
    let h_in = inheritable_dup(&OwnedHandle::from(stdin))?;
    let h_out = inheritable_dup(&OwnedHandle::from(stdout))?;
    let h_err = inheritable_dup(&OwnedHandle::from(stderr))?;
    let handles: [HANDLE; 3] = [raw(&h_in), raw(&h_out), raw(&h_err)];

    let mut cmdline: Vec<u16> = Vec::new();
    quote_arg(spec.exe.as_os_str(), &mut cmdline);
    for a in spec.args {
        cmdline.push(u16::from(b' '));
        quote_arg(a, &mut cmdline);
    }
    cmdline.push(0);
    let app = wide(spec.exe.as_os_str());

    let mut env_sorted: Vec<&(OsString, OsString)> = spec.env.iter().collect();
    env_sorted.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
    let mut envblock: Vec<u16> = Vec::new();
    for (k, v) in env_sorted {
        envblock.extend(k.encode_wide());
        envblock.push(u16::from(b'='));
        envblock.extend(v.encode_wide());
        envblock.push(0);
    }
    if envblock.is_empty() {
        envblock.push(0);
    }
    envblock.push(0);
    let cwd = spec.cwd.map(|p| wide(p.as_os_str()));

    let n_attr = if spec.job.is_some() { 2 } else { 1 };
    let mut size = 0usize;
    // SAFETY: first call with a null list to get the required size;
    // failure with ERROR_INSUFFICIENT_BUFFER is expected.
    unsafe { InitializeProcThreadAttributeList(null_mut(), n_attr, 0, &mut size) };
    let mut attr_buf = vec![0u64; size.div_ceil(8)];
    let attr: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: `attr` points to an aligned buffer of at least `size` bytes.
    check(unsafe { InitializeProcThreadAttributeList(attr, n_attr, 0, &mut size) })?;
    let job_handle: [HANDLE; 1] = [spec.job.map(|j| j.raw_handle()).unwrap_or(null_mut())];
    let result = (|| {
        // SAFETY: `handles` lives until the end of this function, beyond CreateProcessW.
        check(unsafe {
            UpdateProcThreadAttribute(
                attr,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                size_of::<[HANDLE; 3]>(),
                null_mut(),
                null(),
            )
        })?;
        if spec.job.is_some() {
            // SAFETY: `job_handle` lives until the end of this function.
            check(unsafe {
                UpdateProcThreadAttribute(
                    attr,
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    job_handle.as_ptr().cast(),
                    size_of::<[HANDLE; 1]>(),
                    null_mut(),
                    null(),
                )
            })?;
        }
        // SAFETY: POD structure; zero is valid.
        let mut si: STARTUPINFOEXW = unsafe { zeroed() };
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[1];
        si.StartupInfo.hStdError = handles[2];
        si.lpAttributeList = attr;
        // SAFETY: POD structure; zero is valid.
        let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
        let console = match spec.console {
            ConsoleMode::NoWindow => CREATE_NO_WINDOW,
            ConsoleMode::Detached => DETACHED_PROCESS,
        };
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | console;
        // SAFETY: all strings are zero-terminated and outlive the
        // call; `cmdline` is a mutable buffer as required; the environment
        // block is UTF-16 terminated by a double zero.
        check(unsafe {
            CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                null(),
                null(),
                1,
                flags,
                envblock.as_ptr().cast(),
                cwd.as_ref().map(|c| c.as_ptr()).unwrap_or(null()),
                &si.StartupInfo,
                &mut pi,
            )
        })?;
        // SAFETY: the primary thread handle is not needed: we close it right away.
        unsafe { CloseHandle(pi.hThread) };
        Ok(Spawned {
            process: owned(pi.hProcess)?,
            pid: pi.dwProcessId,
        })
    })();
    // SAFETY: list successfully initialised above.
    unsafe { DeleteProcThreadAttributeList(attr) };
    drop((h_in, h_out, h_err));
    result
}

/// Waits for a process to end. `Some(code)` if it terminated within the timeout.
pub fn wait_process(h: &OwnedHandle, timeout_ms: u32) -> io::Result<Option<u32>> {
    // SAFETY: valid handle with the SYNCHRONIZE right.
    let r = unsafe { WaitForSingleObject(raw(h), timeout_ms) };
    if r == WAIT_OBJECT_0 {
        let mut code = 0u32;
        // SAFETY: valid handle with the query right; `code` is local.
        check(unsafe { GetExitCodeProcess(raw(h), &mut code) })?;
        Ok(Some(code))
    } else if r == WAIT_TIMEOUT {
        Ok(None)
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn terminate_process(h: &OwnedHandle, code: u32) -> io::Result<()> {
    // SAFETY: valid handle with the PROCESS_TERMINATE right.
    check(unsafe { TerminateProcess(raw(h), code) })
}

/// Opens a process by PID with wait, query and (optional) terminate rights.
pub fn open_process(pid: u32, terminate: bool) -> io::Result<OwnedHandle> {
    let mut access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    if terminate {
        access |= PROCESS_TERMINATE;
    }
    // SAFETY: OpenProcess has no preconditions beyond its parameters.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

/// Process times: (creation FILETIME, kernel 100 ns, user 100 ns).
pub fn process_times(h: &OwnedHandle) -> io::Result<(u64, u64, u64)> {
    let z = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: pointers to local variables; handle with the query right.
    check(unsafe { GetProcessTimes(raw(h), &mut c, &mut e, &mut k, &mut u) })?;
    Ok((filetime_u64(c), filetime_u64(k), filetime_u64(u)))
}

/// Process memory: (working set, peak working set, private bytes).
pub fn process_memory(h: &OwnedHandle) -> io::Result<(usize, usize, usize)> {
    // PROCESS_MEMORY_COUNTERS_EX: PROCESS_MEMORY_COUNTERS followed by PrivateUsage.
    #[repr(C)]
    struct Ex {
        base: PROCESS_MEMORY_COUNTERS,
        private_usage: usize,
    }
    // SAFETY: POD structure; zero is valid.
    let mut m: Ex = unsafe { zeroed() };
    m.base.cb = size_of::<Ex>() as u32;
    // SAFETY: local buffer of the declared size.
    check(unsafe {
        K32GetProcessMemoryInfo(raw(h), (&mut m as *mut Ex).cast(), size_of::<Ex>() as u32)
    })?;
    Ok((
        m.base.WorkingSetSize,
        m.base.PeakWorkingSetSize,
        m.private_usage,
    ))
}

/// `true` if the process belongs to the job (or to any job with `job = None`).
pub fn is_in_job(h: &OwnedHandle, job: Option<&Job>) -> io::Result<bool> {
    let mut r = 0;
    // SAFETY: valid process handle; valid or null job.
    check(unsafe {
        IsProcessInJob(
            raw(h),
            job.map(|j| j.raw_handle()).unwrap_or(null_mut()),
            &mut r,
        )
    })?;
    Ok(r != 0)
}

/// `true` if the current process already belongs to a job.
pub fn current_in_job() -> io::Result<bool> {
    let mut r = 0;
    // SAFETY: pseudo-handle of the current process; null job = any job.
    check(unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut r) })?;
    Ok(r != 0)
}

/// Entry of the process list.
#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

/// List of the system's processes (Toolhelp, equivalent to `Get-Process`).
pub fn list_processes() -> io::Result<Vec<ProcEntry>> {
    // SAFETY: no preconditions.
    let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut out = Vec::new();
    // SAFETY: POD structure; zero is valid; `dwSize` set as required.
    let mut e: PROCESSENTRY32W = unsafe { zeroed() };
    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: valid snapshot and initialised structure.
    let mut ok = unsafe { Process32FirstW(raw(&snap), &mut e) };
    while ok != 0 {
        let len = e
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(e.szExeFile.len());
        out.push(ProcEntry {
            pid: e.th32ProcessID,
            ppid: e.th32ParentProcessID,
            name: String::from_utf16_lossy(&e.szExeFile[..len]),
        });
        // SAFETY: as above.
        ok = unsafe { Process32NextW(raw(&snap), &mut e) };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        let mut v = Vec::new();
        quote_arg(OsStr::new(s), &mut v);
        String::from_utf16(&v).unwrap()
    }

    #[test]
    fn quoting_msvc() {
        assert_eq!(q("abc"), "abc");
        assert_eq!(q(""), "\"\"");
        assert_eq!(q("a b"), "\"a b\"");
        assert_eq!(q("a\"b"), "\"a\\\"b\"");
        assert_eq!(q("C:\\dir with space\\"), "\"C:\\dir with space\\\\\"");
        assert_eq!(q("C:\\no\\space"), "C:\\no\\space");
    }
}
