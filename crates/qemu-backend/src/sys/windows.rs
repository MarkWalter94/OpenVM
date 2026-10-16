//! Safe wrappers over the Win32 APIs used by the backend.
//!
//! Derived from `spikes/f0-supervisor/src/win.rs` (F0-06), reduced to the
//! necessary primitives: Job Object with `KILL_ON_JOB_CLOSE`, creation of the
//! process already inside the job with inheritance limited to the three standard handles,
//! wait, creation time and job termination.
//!
//! All the `unsafe` code of the crate lives here: every block has a
//! `SAFETY:` comment and the functions exposed to the rest of the crate are safe (handles
//! owned through `OwnedHandle`, errors as `io::Error`).
#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

/// An IPv4 TCP row with Windows' owning PID, used only to verify TPM peers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TcpRow {
    pub local: std::net::SocketAddrV4,
    pub remote: std::net::SocketAddrV4,
    pub pid: u32,
    pub listening: bool,
    pub established: bool,
}

pub(crate) fn tcp_rows() -> io::Result<Vec<TcpRow>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    let mut size = 0u32;
    // SAFETY: documented sizing query, valid output pointer and no input buffer.
    let first = unsafe {
        GetExtendedTcpTable(
            null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if first != 0 && first != 122 {
        return Err(io::Error::from_raw_os_error(first as i32));
    }
    for _ in 0..3 {
        if !(4..=4 * 1024 * 1024).contains(&size) {
            return Err(io::Error::other("invalid TCP table size"));
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let capacity = buffer.len() * 8;
        // SAFETY: the aligned allocation has at least `size` writable bytes;
        // Windows writes only that capacity and updates size on a race.
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if status == 122 {
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let count = buffer[0] as u32 as usize;
        let stride = size_of::<MIB_TCPROW_OWNER_PID>();
        if count > capacity.saturating_sub(4) / stride {
            return Err(io::Error::other("truncated TCP table"));
        }
        let mut rows = Vec::with_capacity(count);
        for index in 0..count {
            // SAFETY: count/stride validated against initialized allocation;
            // the table header is 4 bytes, so unaligned reads are intentional.
            let row = unsafe {
                (buffer
                    .as_ptr()
                    .cast::<u8>()
                    .add(4 + index * stride)
                    .cast::<MIB_TCPROW_OWNER_PID>())
                .read_unaligned()
            };
            let address = |ip: u32, port: u32| {
                std::net::SocketAddrV4::new(
                    std::net::Ipv4Addr::from(ip.to_ne_bytes()),
                    u16::from_be(port as u16),
                )
            };
            rows.push(TcpRow {
                local: address(row.dwLocalAddr, row.dwLocalPort),
                remote: address(row.dwRemoteAddr, row.dwRemotePort),
                pid: row.dwOwningPid,
                listening: row.dwState == 2,
                established: row.dwState == 5,
            });
        }
        return Ok(rows);
    }
    Err(io::Error::other("TCP table changed repeatedly"))
}

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessTimes, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// `FILE_SHARE_READ`, for opening with write exclusion (F0-05, 7.2).
pub(crate) const FILE_SHARE_READ: u32 = windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
/// `FILE_SHARE_WRITE`.
pub(crate) const FILE_SHARE_WRITE: u32 = windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
/// `ERROR_SHARING_VIOLATION` (32).
pub(crate) const ERROR_SHARING_VIOLATION: i32 =
    windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32;

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

/// Anonymous Job Object with `KILL_ON_JOB_CLOSE`, without `BREAKAWAY_OK` or
/// `SILENT_BREAKAWAY_OK`. The handle is not inheritable and the job has no name.
#[derive(Debug)]
pub(crate) struct Job {
    handle: OwnedHandle,
}

/// Essential job accounting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JobAccounting {
    pub(crate) active_processes: u32,
    pub(crate) total_processes: u32,
}

/// Limits read from the job: CPU cap (hundredths of a percentage point of all
/// logical processors), maximum job memory and observed peak, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JobLimitInfo {
    pub(crate) cpu_rate: Option<u32>,
    pub(crate) job_memory: Option<u64>,
    pub(crate) peak_job_memory: u64,
}

impl Job {
    /// Job with `KILL_ON_JOB_CLOSE` and, if given, maximum job memory and
    /// hard CPU cap, applied before any process enters it.
    pub(crate) fn create(cpu_rate: Option<u32>, job_memory: Option<u64>) -> io::Result<Job> {
        // SAFETY: null security attributes (non-inheritable handle) and a null
        // name are values accepted by CreateJobObjectW.
        let h = unsafe { CreateJobObjectW(null(), null()) };
        let job = Job { handle: owned(h)? };
        // SAFETY: POD structure of integers only; zero is a valid value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(m) = job_memory {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = usize::try_from(m).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "memory limit out of range")
            })?;
        }
        // SAFETY: `info` is a valid structure of the exact size of the
        // JobObjectExtendedLimitInformation class; the handle is alive.
        check(unsafe {
            SetInformationJobObject(
                raw(&job.handle),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        if let Some(rate) = cpu_rate {
            job.set_cpu_hard_cap(rate)?;
        }
        Ok(job)
    }

    /// Sets the job's hard CPU cap (`rate` in hundredths of a percentage
    /// point of the whole system, 1..=10000).
    fn set_cpu_hard_cap(&self, rate: u32) -> io::Result<()> {
        // SAFETY: POD structure of integers only; zero is a valid value.
        let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        info.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        info.Anonymous.CpuRate = rate;
        // SAFETY: `info` is a valid structure of the exact size of the
        // JobObjectCpuRateControlInformation class; the handle is alive.
        check(unsafe {
            SetInformationJobObject(
                raw(&self.handle),
                JobObjectCpuRateControlInformation,
                (&info as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
        })
    }

    /// Limits actually applied to the job and observed memory peak.
    pub(crate) fn limit_info(&self) -> io::Result<JobLimitInfo> {
        // SAFETY: POD structure; zero is valid.
        let mut e: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer of the exact size of the class, handle alive.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectExtendedLimitInformation,
                (&mut e as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        // SAFETY: POD structure; zero is valid.
        let mut c: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer of the exact size of the class, handle alive.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectCpuRateControlInformation,
                (&mut c as *mut JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        let hard = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        Ok(JobLimitInfo {
            // SAFETY: the union contains only `u32`s: any value read is
            // valid; with HARD_CAP active the meaningful field is CpuRate.
            cpu_rate: (c.ControlFlags & hard == hard).then_some(unsafe { c.Anonymous.CpuRate }),
            job_memory: (e.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0)
                .then_some(e.JobMemoryLimit as u64),
            peak_job_memory: e.PeakJobMemoryUsed as u64,
        })
    }

    fn raw_handle(&self) -> HANDLE {
        raw(&self.handle)
    }

    /// Terminates all processes of the job with `exit_code`.
    pub(crate) fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: valid job handle owned by `self`.
        check(unsafe { TerminateJobObject(raw(&self.handle), exit_code) })
    }

    pub(crate) fn accounting(&self) -> io::Result<JobAccounting> {
        // SAFETY: POD structure; zero is valid.
        let mut a: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer of the exact size of the requested class,
        // valid for the whole call; handle alive.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectBasicAccountingInformation,
                (&mut a as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        Ok(JobAccounting {
            active_processes: a.ActiveProcesses,
            total_processes: a.TotalProcesses,
        })
    }

    /// Flags of the active limits (to verify `KILL_ON_JOB_CLOSE`).
    pub(crate) fn limit_flags(&self) -> io::Result<u32> {
        // SAFETY: POD structure; zero is valid.
        let mut e: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer of the exact size of the class, handle alive.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectExtendedLimitInformation,
                (&mut e as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        Ok(e.BasicLimitInformation.LimitFlags)
    }

    /// `true` if the process belongs to this job.
    pub(crate) fn contains(&self, process: &OwnedHandle) -> io::Result<bool> {
        let mut r = 0;
        // SAFETY: valid process and job handles; `r` is local.
        check(unsafe { IsProcessInJob(raw(process), self.raw_handle(), &mut r) })?;
        Ok(r != 0)
    }
}

/// `KILL_ON_JOB_CLOSE` constant, exposed for checks.
pub(crate) const KILL_ON_JOB_CLOSE: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

/// Console mode of the child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsoleMode {
    /// Console without a window: Windows creates a child `conhost.exe` (in the job).
    #[allow(
        dead_code,
        reason = "alternative verified in F0-06, not used by default"
    )]
    NoWindow,
    /// No console: no `conhost.exe` (F0-06 recommendation).
    Detached,
}

/// Parameters for creating a process without a shell.
pub(crate) struct SpawnSpec<'a> {
    pub(crate) exe: &'a Path,
    pub(crate) args: &'a [OsString],
    /// Complete environment of the child (nothing inherited from the supervisor).
    pub(crate) env: &'a [(OsString, OsString)],
    pub(crate) stdout: &'a Path,
    pub(crate) stderr: &'a Path,
    pub(crate) job: &'a Job,
    pub(crate) console: ConsoleMode,
    pub(crate) cwd: Option<&'a Path>,
}

/// Created process: owned handle (identity) plus informational PID.
#[derive(Debug)]
pub(crate) struct Spawned {
    pub(crate) process: OwnedHandle,
    pub(crate) pid: u32,
}

/// Quoting of an argument according to the `CommandLineToArgvW`/MSVC CRT rules.
pub(crate) fn quote_arg(arg: &OsStr, out: &mut Vec<u16>) {
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

/// Creates a process with structured arguments (no shell), explicit
/// environment, inheritance limited to stdin (`NUL`), stdout and stderr (files) and
/// membership of the job from creation.
pub(crate) fn spawn(spec: &SpawnSpec<'_>) -> io::Result<Spawned> {
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

    let n_attr = 2;
    let mut size = 0usize;
    // SAFETY: first call with a null list to obtain the required
    // size; failure with ERROR_INSUFFICIENT_BUFFER is expected.
    unsafe { InitializeProcThreadAttributeList(null_mut(), n_attr, 0, &mut size) };
    let mut attr_buf = vec![0u64; size.div_ceil(8)];
    let attr: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: `attr` points to an aligned buffer of at least `size` bytes.
    check(unsafe { InitializeProcThreadAttributeList(attr, n_attr, 0, &mut size) })?;
    let job_handle: [HANDLE; 1] = [spec.job.raw_handle()];
    let result = (|| {
        // SAFETY: `handles` lives until the end of this function, beyond
        // CreateProcessW; exact size of the array.
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
        // SAFETY: all strings are zero-terminated and live beyond the
        // call; `cmdline` is a mutable buffer as required; the
        // environment block is UTF-16 terminated by a double zero; `si` and the
        // attribute list are initialised.
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
        // SAFETY: the primary thread handle is not needed: we close it right away;
        // it is valid because CreateProcessW succeeded.
        unsafe { CloseHandle(pi.hThread) };
        Ok(Spawned {
            process: owned(pi.hProcess)?,
            pid: pi.dwProcessId,
        })
    })();
    // SAFETY: list successfully initialised above and no longer used afterwards.
    unsafe { DeleteProcThreadAttributeList(attr) };
    drop((h_in, h_out, h_err));
    result
}

/// Waits for a process to end. `Some(code)` if it exited within the
/// timeout (`None` = infinite wait).
pub(crate) fn wait_process(h: &OwnedHandle, timeout_ms: Option<u32>) -> io::Result<Option<u32>> {
    // SAFETY: valid handle with the SYNCHRONIZE right (handle from CreateProcessW).
    let r = unsafe { WaitForSingleObject(raw(h), timeout_ms.unwrap_or(INFINITE)) };
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

/// Process creation time (FILETIME, 100 ns units since 1601).
pub(crate) fn creation_time(h: &OwnedHandle) -> io::Result<u64> {
    let z = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: pointers to local variables; handle with the query right.
    check(unsafe { GetProcessTimes(raw(h), &mut c, &mut e, &mut k, &mut u) })?;
    Ok(filetime_u64(c))
}

/// Host physical memory: `(total, available)` in bytes.
pub(crate) fn physical_memory() -> io::Result<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: POD structure of integers only; zero is a valid value.
    let mut m: MEMORYSTATUSEX = unsafe { zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `m` is a valid local structure with `dwLength` set as
    // required by the API.
    check(unsafe { GlobalMemoryStatusEx(&mut m) })?;
    Ok((m.ullTotalPhys, m.ullAvailPhys))
}

/// Free space available to the current user on the volume of `dir`, in bytes.
pub(crate) fn free_disk_bytes(dir: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = wide(dir.as_os_str());
    let mut avail = 0u64;
    // SAFETY: zero-terminated UTF-16 string valid for the duration of the
    // call; pointers to local variables or null (optional parameters).
    check(unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, null_mut(), null_mut()) })?;
    Ok(avail)
}

/// Outcome of looking up a process by PID and creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidStatus {
    /// No process with that PID.
    NotFound,
    /// The PID belongs to another process (different creation time, or
    /// process the current user cannot open: a child we created
    /// could be opened).
    Reused,
    /// Same process, exited (someone still holds a handle).
    Exited,
    /// Same process, still running.
    Alive,
}

/// Looks up process `pid` and compares it with the expected creation time
/// (process identity, F0-06 §3). Neither terminates nor modifies anything.
pub(crate) fn pid_status(pid: u32, expected_creation_time: u64) -> io::Result<PidStatus> {
    // SAFETY: OpenProcess accepts any PID; on error it returns
    // a null handle, handled right below. No pointer is passed.
    let h = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if h.is_null() {
        let e = io::Error::last_os_error();
        let code = e.raw_os_error().map(|c| c as u32);
        return match code {
            Some(windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER) => {
                Ok(PidStatus::NotFound)
            }
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED) => Ok(PidStatus::Reused),
            _ => Err(e),
        };
    }
    let h = owned(h)?;
    if creation_time(&h)? != expected_creation_time {
        return Ok(PidStatus::Reused);
    }
    Ok(match wait_process(&h, Some(0))? {
        Some(_) => PidStatus::Exited,
        None => PidStatus::Alive,
    })
}

/// `false` only if no running process has this PID (`OpenProcess` fails
/// with `ERROR_INVALID_PARAMETER`, or the process already exited); any other
/// outcome (access denied, error) counts as running.
pub(crate) fn pid_may_be_running(pid: u32) -> bool {
    // SAFETY: OpenProcess accepts any PID; on error it returns a null handle,
    // handled right below. No pointer is passed.
    let h = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if h.is_null() {
        let code = io::Error::last_os_error().raw_os_error().map(|c| c as u32);
        return code != Some(windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER);
    }
    match owned(h) {
        Ok(h) => !matches!(wait_process(&h, Some(0)), Ok(Some(_))),
        Err(_) => true,
    }
}

/// Path of an open file as resolved by the system
/// (`GetFinalPathNameByHandleW`, normalized, DOS volume name): every link,
/// junction, mount point, `subst` or mapped drive on the way is resolved;
/// a file on a network share has the `\\?\UNC\` form.
pub(crate) fn final_path_by_handle(file: &std::fs::File) -> io::Result<OsString> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };
    let h = file.as_raw_handle() as HANDLE;
    let mut buf = vec![0u16; 512];
    loop {
        let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: `h` is the valid handle of `file`, borrowed for the call;
        // `buf` is a writable buffer of `cap` UTF-16 units.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                h,
                buf.as_mut_ptr(),
                cap,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            return Ok(OsString::from_wide(&buf));
        }
        // Too small: `n` is the required size including the terminator.
        if n > 32 * 1024 {
            return Err(io::Error::other("final path too long"));
        }
        buf = vec![0u16; n + 1];
    }
}

/// Mount root (with the final separator) of the volume containing `path`
/// (`GetVolumePathNameW`): `C:\`, or the folder where a volume is mounted.
pub(crate) fn volume_path_name(path: &Path) -> io::Result<OsString> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    let w = wide(path.as_os_str());
    let mut buf = vec![0u16; w.len().max(4) + 2];
    let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: zero-terminated UTF-16 input valid for the call; `buf` is a
    // writable buffer of `cap` units, at least as long as the input path
    // (the mount root is a prefix of it).
    check(unsafe { GetVolumePathNameW(w.as_ptr(), buf.as_mut_ptr(), cap) })?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    buf.truncate(end);
    Ok(OsString::from_wide(&buf))
}

/// `GetDriveTypeW` of a volume root (`DRIVE_*` constants).
pub(crate) fn drive_type(root: &Path) -> u32 {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let w = wide(root.as_os_str());
    // SAFETY: zero-terminated UTF-16 string valid for the duration of the call.
    unsafe { GetDriveTypeW(w.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_identity() {
        let pid = std::process::id();
        // SAFETY: pseudo-handle of the current process, must not be closed; it is
        // duplicated into a real owned handle.
        let cur = unsafe { GetCurrentProcess() };
        let mut dup: HANDLE = null_mut();
        // SAFETY: duplication of the current pseudo-handle into a real handle.
        check(unsafe { DuplicateHandle(cur, cur, cur, &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) })
            .unwrap();
        let h = owned(dup).unwrap();
        let ct = creation_time(&h).unwrap();
        assert_eq!(pid_status(pid, ct).unwrap(), PidStatus::Alive);
        assert_eq!(pid_status(pid, ct + 1).unwrap(), PidStatus::Reused);
        // PID 0 (System Idle): OpenProcess fails with ERROR_INVALID_PARAMETER.
        assert_eq!(pid_status(0, 0).unwrap(), PidStatus::NotFound);
    }

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

    #[test]
    fn job_with_cpu_and_memory_limits() {
        let job = Job::create(Some(2500), Some(512 << 20)).unwrap();
        let i = job.limit_info().unwrap();
        assert_eq!(i.cpu_rate, Some(2500));
        assert_eq!(i.job_memory, Some(512 << 20));
        assert_eq!(i.peak_job_memory, 0);
        assert_eq!(
            job.limit_flags().unwrap() & KILL_ON_JOB_CLOSE,
            KILL_ON_JOB_CLOSE
        );
        let without = Job::create(None, None).unwrap().limit_info().unwrap();
        assert_eq!(without.cpu_rate, None);
        assert_eq!(without.job_memory, None);
    }

    #[test]
    fn job_with_kill_on_close() {
        let job = Job::create(None, None).unwrap();
        assert_eq!(
            job.limit_flags().unwrap() & KILL_ON_JOB_CLOSE,
            KILL_ON_JOB_CLOSE
        );
        assert_eq!(job.accounting().unwrap().active_processes, 0);
    }
}
