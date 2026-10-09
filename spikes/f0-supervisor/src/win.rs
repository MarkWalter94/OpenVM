//! Involucri minimi sulle API Win32 usate dallo spike.
//!
//! Tutto il codice `unsafe` del crate vive in questo modulo: ogni blocco ha un
//! commento `SAFETY:` e le funzioni pubbliche espongono un'interfaccia sicura
//! (handle posseduti tramite `OwnedHandle`, errori come `io::Error`).

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

/// Converte un `HANDLE` grezzo in un handle posseduto, verificando gli errori.
fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` è un handle valido appena restituito da un'API Win32 e non è
    // posseduto da nessun altro oggetto; da qui in poi lo chiude `OwnedHandle`.
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

/// Limiti applicati al Job Object. `KILL_ON_JOB_CLOSE` è sempre attivo e
/// `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK` sono sempre disattivati.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobLimits {
    pub process_memory: Option<usize>,
    pub job_memory: Option<usize>,
}

/// Contabilità del job (tempi in unità da 100 ns).
#[derive(Debug, Clone, Copy)]
pub struct JobAccounting {
    pub user_100ns: u64,
    pub kernel_100ns: u64,
    pub total_processes: u32,
    pub active_processes: u32,
    pub terminated_processes: u32,
}

/// Limiti estesi letti dal job.
#[derive(Debug, Clone, Copy)]
pub struct JobExtended {
    pub limit_flags: u32,
    pub process_memory_limit: usize,
    pub job_memory_limit: usize,
    pub peak_process_memory: usize,
    pub peak_job_memory: usize,
}

/// Messaggio ricevuto dalla porta di completamento associata al job.
#[derive(Debug, Clone, Copy)]
pub struct JobMessage {
    pub code: u32,
    /// Per i messaggi di processo è il PID.
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
            _ => "ALTRO",
        }
    }
}

/// Job Object anonimo posseduto dal supervisor.
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
    port: Option<OwnedHandle>,
}

impl Job {
    /// Crea un job anonimo (nessun nome nel namespace degli oggetti, quindi
    /// nessun altro processo può aprirlo per nome) con `KILL_ON_JOB_CLOSE`.
    pub fn create(limits: JobLimits) -> io::Result<Job> {
        // SAFETY: attributi di sicurezza nulli (handle non ereditabile) e nome
        // nullo sono valori ammessi da CreateJobObjectW.
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
        // SAFETY: struttura POD di soli interi; lo zero è un valore valido.
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
        // SAFETY: `info` punta a una struttura valida di dimensione esatta
        // `size_of::<T>()` corrispondente alla classe richiesta.
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
        // SAFETY: strutture POD di soli interi/puntatori; lo zero è valido e il
        // buffer ha la dimensione esatta della struttura della classe.
        let mut info: T = unsafe { zeroed() };
        // SAFETY: vedi sopra; il puntatore resta valido per tutta la chiamata.
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

    /// Hard cap CPU in percentuale dei cicli dell'intero sistema (tutti i
    /// processori logici), come definito da `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`.
    /// `rate` è in centesimi di punto percentuale (2500 = 25%). `None` lo rimuove.
    pub fn set_cpu_hard_cap(&self, rate: Option<u32>) -> io::Result<()> {
        // SAFETY: struttura POD; lo zero è valido.
        let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        if let Some(r) = rate {
            info.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            info.Anonymous.CpuRate = r;
        }
        self.set_info(JobObjectCpuRateControlInformation, &info)
    }

    /// Associa una porta di completamento per ricevere le notifiche del job.
    pub fn attach_port(&mut self) -> io::Result<()> {
        // SAFETY: crea una nuova porta non associata a file (INVALID_HANDLE_VALUE).
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

    /// Legge i messaggi in coda sulla porta, attendendo al massimo `timeout_ms`
    /// per il primo e 0 ms per i successivi.
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
            // SAFETY: puntatori a variabili locali valide; la porta è viva.
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
        // SAFETY: handle di job valido posseduto da `self`.
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

    /// PID dei processi attualmente nel job.
    pub fn pids(&self) -> io::Result<Vec<u32>> {
        // Buffer per intestazione (2 x u32) più fino a 64 PID (usize).
        let mut buf = [0usize; 66];
        // SAFETY: il buffer è allineato a usize e lungo quanto dichiarato.
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

    /// Prova a impostare limiti che potrebbero richiedere privilegi.
    /// Restituisce l'esito di ciascun tentativo (nome, risultato).
    pub fn probe_privileged_limits(&self) -> Vec<(&'static str, Result<(), String>)> {
        let mut out = Vec::new();
        // Working set minimo/massimo.
        // SAFETY: struttura POD; lo zero è valido.
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
        // Classe di priorità realtime.
        // SAFETY: struttura POD; lo zero è valido.
        let mut pr: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        pr.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
        pr.BasicLimitInformation.PriorityClass = REALTIME_PRIORITY_CLASS;
        out.push((
            "JOB_OBJECT_LIMIT_PRIORITY_CLASS = REALTIME",
            self.set_info(JobObjectExtendedLimitInformation, &pr)
                .map_err(|e| e.to_string()),
        ));
        // Controllo del tasso di I/O (bandwidth 10 MB/s su tutti i volumi).
        let io = JOBOBJECT_IO_RATE_CONTROL_INFORMATION {
            MaxIops: 0,
            MaxBandwidth: 10_000_000,
            ReservationIops: 0,
            VolumeName: null(),
            BaseIoSize: 0,
            ControlFlags: 1, // JOB_OBJECT_IO_RATE_CONTROL_ENABLE
        };
        // SAFETY: puntatore a struttura locale valida; VolumeName nullo = tutti i volumi.
        let r = unsafe { SetIoRateControlInformationJobObject(raw(&self.handle), &io) };
        out.push((
            "SetIoRateControlInformationJobObject (10 MB/s)",
            if r == 0 {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(())
            },
        ));
        // Controllo del tasso di rete.
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
// Processi
// ---------------------------------------------------------------------------

/// Modalità console del processo figlio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleMode {
    /// Console senza finestra: Windows crea un `conhost.exe` per il figlio.
    NoWindow,
    /// Nessuna console: nessun `conhost.exe`.
    Detached,
}

/// Parametri di creazione di un processo senza shell.
#[derive(Debug)]
pub struct SpawnSpec<'a> {
    pub exe: &'a Path,
    pub args: &'a [OsString],
    /// Ambiente completo del figlio (nessuna eredità dal supervisor).
    pub env: &'a [(OsString, OsString)],
    pub stdout: &'a Path,
    pub stderr: &'a Path,
    pub job: Option<&'a Job>,
    pub console: ConsoleMode,
    pub cwd: Option<&'a Path>,
}

/// Processo creato: handle posseduto (identità) più PID informativo.
#[derive(Debug)]
pub struct Spawned {
    pub process: OwnedHandle,
    pub pid: u32,
}

/// Quoting di un argomento secondo le regole di `CommandLineToArgvW`/CRT MSVC.
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
    // SAFETY: duplica un handle valido nello stesso processo; `dup` riceve il
    // nuovo handle ereditabile, posseduto subito da `OwnedHandle`.
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

/// Crea un processo con:
/// - argomenti strutturati (nessuna shell), percorso eseguibile esplicito;
/// - ambiente completo fornito dal chiamante;
/// - eredità limitata ai soli tre handle standard (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`);
/// - assegnazione al job già alla creazione (`PROC_THREAD_ATTRIBUTE_JOB_LIST`),
///   senza finestra temporale in cui il processo è fuori dal job.
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
    // SAFETY: prima chiamata con lista nulla per ottenere la dimensione richiesta;
    // il fallimento con ERROR_INSUFFICIENT_BUFFER è atteso.
    unsafe { InitializeProcThreadAttributeList(null_mut(), n_attr, 0, &mut size) };
    let mut attr_buf = vec![0u64; size.div_ceil(8)];
    let attr: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: `attr` punta a un buffer allineato di almeno `size` byte.
    check(unsafe { InitializeProcThreadAttributeList(attr, n_attr, 0, &mut size) })?;
    let job_handle: [HANDLE; 1] = [spec.job.map(|j| j.raw_handle()).unwrap_or(null_mut())];
    let result = (|| {
        // SAFETY: `handles` vive fino alla fine di questa funzione, oltre CreateProcessW.
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
            // SAFETY: `job_handle` vive fino alla fine di questa funzione.
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
        // SAFETY: struttura POD; lo zero è valido.
        let mut si: STARTUPINFOEXW = unsafe { zeroed() };
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = handles[0];
        si.StartupInfo.hStdOutput = handles[1];
        si.StartupInfo.hStdError = handles[2];
        si.lpAttributeList = attr;
        // SAFETY: struttura POD; lo zero è valido.
        let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
        let console = match spec.console {
            ConsoleMode::NoWindow => CREATE_NO_WINDOW,
            ConsoleMode::Detached => DETACHED_PROCESS,
        };
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | console;
        // SAFETY: tutte le stringhe sono terminate da zero e vivono oltre la
        // chiamata; `cmdline` è un buffer modificabile come richiesto; il blocco
        // d'ambiente è UTF-16 terminato da doppio zero.
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
        // SAFETY: l'handle del thread primario non serve: lo chiudiamo subito.
        unsafe { CloseHandle(pi.hThread) };
        Ok(Spawned {
            process: owned(pi.hProcess)?,
            pid: pi.dwProcessId,
        })
    })();
    // SAFETY: lista inizializzata con successo sopra.
    unsafe { DeleteProcThreadAttributeList(attr) };
    drop((h_in, h_out, h_err));
    result
}

/// Attende la fine di un processo. `Some(codice)` se è terminato entro il timeout.
pub fn wait_process(h: &OwnedHandle, timeout_ms: u32) -> io::Result<Option<u32>> {
    // SAFETY: handle valido con diritto SYNCHRONIZE.
    let r = unsafe { WaitForSingleObject(raw(h), timeout_ms) };
    if r == WAIT_OBJECT_0 {
        let mut code = 0u32;
        // SAFETY: handle valido con diritto di query; `code` è locale.
        check(unsafe { GetExitCodeProcess(raw(h), &mut code) })?;
        Ok(Some(code))
    } else if r == WAIT_TIMEOUT {
        Ok(None)
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn terminate_process(h: &OwnedHandle, code: u32) -> io::Result<()> {
    // SAFETY: handle valido con diritto PROCESS_TERMINATE.
    check(unsafe { TerminateProcess(raw(h), code) })
}

/// Apre un processo per PID con diritti di attesa, query e (opzionale) terminazione.
pub fn open_process(pid: u32, terminate: bool) -> io::Result<OwnedHandle> {
    let mut access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    if terminate {
        access |= PROCESS_TERMINATE;
    }
    // SAFETY: OpenProcess non ha precondizioni oltre ai parametri.
    owned(unsafe { OpenProcess(access, 0, pid) })
}

/// Tempi del processo: (creazione FILETIME, kernel 100 ns, user 100 ns).
pub fn process_times(h: &OwnedHandle) -> io::Result<(u64, u64, u64)> {
    let z = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: puntatori a variabili locali; handle con diritto di query.
    check(unsafe { GetProcessTimes(raw(h), &mut c, &mut e, &mut k, &mut u) })?;
    Ok((filetime_u64(c), filetime_u64(k), filetime_u64(u)))
}

/// Memoria del processo: (working set, picco working set, private bytes).
pub fn process_memory(h: &OwnedHandle) -> io::Result<(usize, usize, usize)> {
    // PROCESS_MEMORY_COUNTERS_EX: PROCESS_MEMORY_COUNTERS seguito da PrivateUsage.
    #[repr(C)]
    struct Ex {
        base: PROCESS_MEMORY_COUNTERS,
        private_usage: usize,
    }
    // SAFETY: struttura POD; lo zero è valido.
    let mut m: Ex = unsafe { zeroed() };
    m.base.cb = size_of::<Ex>() as u32;
    // SAFETY: buffer locale della dimensione dichiarata.
    check(unsafe {
        K32GetProcessMemoryInfo(raw(h), (&mut m as *mut Ex).cast(), size_of::<Ex>() as u32)
    })?;
    Ok((
        m.base.WorkingSetSize,
        m.base.PeakWorkingSetSize,
        m.private_usage,
    ))
}

/// `true` se il processo appartiene al job (o a un job qualsiasi con `job = None`).
pub fn is_in_job(h: &OwnedHandle, job: Option<&Job>) -> io::Result<bool> {
    let mut r = 0;
    // SAFETY: handle di processo valido; job valido o nullo.
    check(unsafe {
        IsProcessInJob(
            raw(h),
            job.map(|j| j.raw_handle()).unwrap_or(null_mut()),
            &mut r,
        )
    })?;
    Ok(r != 0)
}

/// `true` se il processo corrente appartiene già a un job.
pub fn current_in_job() -> io::Result<bool> {
    let mut r = 0;
    // SAFETY: pseudo-handle del processo corrente; job nullo = qualunque job.
    check(unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut r) })?;
    Ok(r != 0)
}

/// Voce della lista processi.
#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
}

/// Elenco dei processi del sistema (Toolhelp, equivalente a `Get-Process`).
pub fn list_processes() -> io::Result<Vec<ProcEntry>> {
    // SAFETY: nessuna precondizione.
    let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    let mut out = Vec::new();
    // SAFETY: struttura POD; lo zero è valido; `dwSize` impostato come richiesto.
    let mut e: PROCESSENTRY32W = unsafe { zeroed() };
    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: snapshot valido e struttura inizializzata.
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
        // SAFETY: come sopra.
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
