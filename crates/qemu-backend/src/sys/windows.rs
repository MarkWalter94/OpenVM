//! Involucri sicuri sulle API Win32 usate dal backend.
//!
//! Derivato da `spikes/f0-supervisor/src/win.rs` (F0-06), ridotto alle
//! primitive necessarie: Job Object con `KILL_ON_JOB_CLOSE`, creazione del
//! processo già dentro il job con eredità limitata ai tre handle standard,
//! attesa, tempo di creazione e terminazione del job.
//!
//! Tutto il codice `unsafe` del crate vive qui: ogni blocco ha un commento
//! `SAFETY:` e le funzioni esposte al resto del crate sono sicure (handle
//! posseduti tramite `OwnedHandle`, errori come `io::Error`).
#![allow(unsafe_code)]

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

/// `FILE_SHARE_READ`, per l'apertura esclusiva in scrittura (F0-05, 7.2).
pub(crate) const FILE_SHARE_READ: u32 = windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
/// `FILE_SHARE_WRITE`.
pub(crate) const FILE_SHARE_WRITE: u32 = windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
/// `ERROR_SHARING_VIOLATION` (32).
pub(crate) const ERROR_SHARING_VIOLATION: i32 =
    windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32;

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

/// Job Object anonimo con `KILL_ON_JOB_CLOSE`, senza `BREAKAWAY_OK` né
/// `SILENT_BREAKAWAY_OK`. L'handle non è ereditabile e il job non ha nome.
#[derive(Debug)]
pub(crate) struct Job {
    handle: OwnedHandle,
}

/// Contabilità essenziale del job.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JobAccounting {
    pub(crate) active_processes: u32,
    pub(crate) total_processes: u32,
}

/// Limiti letti dal job: tetto CPU (centesimi di punto percentuale di tutti
/// i processori logici), memoria massima del job e picco osservato, in byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JobLimitInfo {
    pub(crate) cpu_rate: Option<u32>,
    pub(crate) job_memory: Option<u64>,
    pub(crate) peak_job_memory: u64,
}

impl Job {
    /// Job con `KILL_ON_JOB_CLOSE` e, se indicati, memoria massima del job e
    /// tetto CPU rigido, applicati prima che qualunque processo vi entri.
    pub(crate) fn create(cpu_rate: Option<u32>, job_memory: Option<u64>) -> io::Result<Job> {
        // SAFETY: attributi di sicurezza nulli (handle non ereditabile) e nome
        // nullo sono valori ammessi da CreateJobObjectW.
        let h = unsafe { CreateJobObjectW(null(), null()) };
        let job = Job { handle: owned(h)? };
        // SAFETY: struttura POD di soli interi; lo zero è un valore valido.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(m) = job_memory {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = usize::try_from(m).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "limite di memoria fuori intervallo",
                )
            })?;
        }
        // SAFETY: `info` è una struttura valida della dimensione esatta della
        // classe JobObjectExtendedLimitInformation; l'handle è vivo.
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

    /// Imposta il tetto CPU rigido del job (`rate` in centesimi di punto
    /// percentuale dell'intero sistema, 1..=10000).
    fn set_cpu_hard_cap(&self, rate: u32) -> io::Result<()> {
        // SAFETY: struttura POD di soli interi; lo zero è un valore valido.
        let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        info.ControlFlags =
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
        info.Anonymous.CpuRate = rate;
        // SAFETY: `info` è una struttura valida della dimensione esatta della
        // classe JobObjectCpuRateControlInformation; l'handle è vivo.
        check(unsafe {
            SetInformationJobObject(
                raw(&self.handle),
                JobObjectCpuRateControlInformation,
                (&info as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
        })
    }

    /// Limiti effettivamente applicati al job e picco di memoria osservato.
    pub(crate) fn limit_info(&self) -> io::Result<JobLimitInfo> {
        // SAFETY: struttura POD; lo zero è valido.
        let mut e: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer della dimensione esatta della classe, handle vivo.
        check(unsafe {
            QueryInformationJobObject(
                raw(&self.handle),
                JobObjectExtendedLimitInformation,
                (&mut e as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        // SAFETY: struttura POD; lo zero è valido.
        let mut c: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer della dimensione esatta della classe, handle vivo.
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
            // SAFETY: l'unione contiene soli `u32`: qualunque valore letto è
            // valido; con HARD_CAP attivo il campo significativo è CpuRate.
            cpu_rate: (c.ControlFlags & hard == hard).then_some(unsafe { c.Anonymous.CpuRate }),
            job_memory: (e.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0)
                .then_some(e.JobMemoryLimit as u64),
            peak_job_memory: e.PeakJobMemoryUsed as u64,
        })
    }

    fn raw_handle(&self) -> HANDLE {
        raw(&self.handle)
    }

    /// Termina tutti i processi del job con `exit_code`.
    pub(crate) fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: handle di job valido posseduto da `self`.
        check(unsafe { TerminateJobObject(raw(&self.handle), exit_code) })
    }

    pub(crate) fn accounting(&self) -> io::Result<JobAccounting> {
        // SAFETY: struttura POD; lo zero è valido.
        let mut a: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer della dimensione esatta della classe richiesta,
        // valido per tutta la chiamata; handle vivo.
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

    /// Flag dei limiti attivi (per la verifica di `KILL_ON_JOB_CLOSE`).
    pub(crate) fn limit_flags(&self) -> io::Result<u32> {
        // SAFETY: struttura POD; lo zero è valido.
        let mut e: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        // SAFETY: buffer della dimensione esatta della classe, handle vivo.
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

    /// `true` se il processo appartiene a questo job.
    pub(crate) fn contains(&self, process: &OwnedHandle) -> io::Result<bool> {
        let mut r = 0;
        // SAFETY: handle di processo e di job validi; `r` è locale.
        check(unsafe { IsProcessInJob(raw(process), self.raw_handle(), &mut r) })?;
        Ok(r != 0)
    }
}

/// Costante di `KILL_ON_JOB_CLOSE`, esposta per le verifiche.
pub(crate) const KILL_ON_JOB_CLOSE: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

/// Modalità console del processo figlio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsoleMode {
    /// Console senza finestra: Windows crea un `conhost.exe` figlio (nel job).
    #[allow(
        dead_code,
        reason = "alternativa verificata in F0-06, non usata per default"
    )]
    NoWindow,
    /// Nessuna console: nessun `conhost.exe` (raccomandazione F0-06).
    Detached,
}

/// Parametri di creazione di un processo senza shell.
pub(crate) struct SpawnSpec<'a> {
    pub(crate) exe: &'a Path,
    pub(crate) args: &'a [OsString],
    /// Ambiente completo del figlio (nessuna eredità dal supervisor).
    pub(crate) env: &'a [(OsString, OsString)],
    pub(crate) stdout: &'a Path,
    pub(crate) stderr: &'a Path,
    pub(crate) job: &'a Job,
    pub(crate) console: ConsoleMode,
    pub(crate) cwd: Option<&'a Path>,
}

/// Processo creato: handle posseduto (identità) più PID informativo.
#[derive(Debug)]
pub(crate) struct Spawned {
    pub(crate) process: OwnedHandle,
    pub(crate) pid: u32,
}

/// Quoting di un argomento secondo le regole di `CommandLineToArgvW`/CRT MSVC.
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

/// Crea un processo con argomenti strutturati (nessuna shell), ambiente
/// esplicito, eredità limitata a stdin (`NUL`), stdout e stderr (file) e
/// appartenenza al job dalla creazione.
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
    // SAFETY: prima chiamata con lista nulla per ottenere la dimensione
    // richiesta; il fallimento con ERROR_INSUFFICIENT_BUFFER è atteso.
    unsafe { InitializeProcThreadAttributeList(null_mut(), n_attr, 0, &mut size) };
    let mut attr_buf = vec![0u64; size.div_ceil(8)];
    let attr: LPPROC_THREAD_ATTRIBUTE_LIST = attr_buf.as_mut_ptr().cast();
    // SAFETY: `attr` punta a un buffer allineato di almeno `size` byte.
    check(unsafe { InitializeProcThreadAttributeList(attr, n_attr, 0, &mut size) })?;
    let job_handle: [HANDLE; 1] = [spec.job.raw_handle()];
    let result = (|| {
        // SAFETY: `handles` vive fino alla fine di questa funzione, oltre
        // CreateProcessW; dimensione esatta dell'array.
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
        // chiamata; `cmdline` è un buffer modificabile come richiesto; il
        // blocco d'ambiente è UTF-16 terminato da doppio zero; `si` e la lista
        // di attributi sono inizializzati.
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
        // SAFETY: l'handle del thread primario non serve: lo chiudiamo subito;
        // è valido perché CreateProcessW è riuscita.
        unsafe { CloseHandle(pi.hThread) };
        Ok(Spawned {
            process: owned(pi.hProcess)?,
            pid: pi.dwProcessId,
        })
    })();
    // SAFETY: lista inizializzata con successo sopra e non più usata dopo.
    unsafe { DeleteProcThreadAttributeList(attr) };
    drop((h_in, h_out, h_err));
    result
}

/// Attende la fine di un processo. `Some(codice)` se è terminato entro il
/// timeout (`None` = attesa infinita).
pub(crate) fn wait_process(h: &OwnedHandle, timeout_ms: Option<u32>) -> io::Result<Option<u32>> {
    // SAFETY: handle valido con diritto SYNCHRONIZE (handle di CreateProcessW).
    let r = unsafe { WaitForSingleObject(raw(h), timeout_ms.unwrap_or(INFINITE)) };
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

/// Tempo di creazione del processo (FILETIME, unità da 100 ns dal 1601).
pub(crate) fn creation_time(h: &OwnedHandle) -> io::Result<u64> {
    let z = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: puntatori a variabili locali; handle con diritto di query.
    check(unsafe { GetProcessTimes(raw(h), &mut c, &mut e, &mut k, &mut u) })?;
    Ok(filetime_u64(c))
}

/// Memoria fisica dell'host: `(totale, disponibile)` in byte.
pub(crate) fn physical_memory() -> io::Result<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: struttura POD di soli interi; lo zero è un valore valido.
    let mut m: MEMORYSTATUSEX = unsafe { zeroed() };
    m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `m` è una struttura locale valida con `dwLength` impostato come
    // richiesto dall'API.
    check(unsafe { GlobalMemoryStatusEx(&mut m) })?;
    Ok((m.ullTotalPhys, m.ullAvailPhys))
}

/// Spazio libero disponibile all'utente corrente sul volume di `dir`, in byte.
pub(crate) fn free_disk_bytes(dir: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = wide(dir.as_os_str());
    let mut avail = 0u64;
    // SAFETY: stringa UTF-16 terminata da zero valida per la durata della
    // chiamata; puntatori a variabili locali o nulli (parametri facoltativi).
    check(unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, null_mut(), null_mut()) })?;
    Ok(avail)
}

/// Esito della ricerca di un processo per PID e tempo di creazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidStatus {
    /// Nessun processo con quel PID.
    NotFound,
    /// Il PID appartiene a un altro processo (tempo di creazione diverso, o
    /// processo non apribile dall'utente corrente: un figlio creato da noi lo
    /// sarebbe).
    Reused,
    /// Stesso processo, terminato (qualcuno tiene ancora un handle).
    Exited,
    /// Stesso processo, ancora in esecuzione.
    Alive,
}

/// Cerca il processo `pid` e lo confronta con il tempo di creazione atteso
/// (identità del processo, F0-06 §3). Non termina né modifica nulla.
pub(crate) fn pid_status(pid: u32, expected_creation_time: u64) -> io::Result<PidStatus> {
    // SAFETY: OpenProcess accetta qualunque PID; in caso di errore restituisce
    // un handle nullo, gestito subito sotto. Nessun puntatore è passato.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identita_del_processo_corrente() {
        let pid = std::process::id();
        // SAFETY: pseudo-handle del processo corrente, non va chiuso; lo si
        // duplica in un handle reale posseduto.
        let cur = unsafe { GetCurrentProcess() };
        let mut dup: HANDLE = null_mut();
        // SAFETY: duplicazione dello pseudo-handle corrente in un handle reale.
        check(unsafe { DuplicateHandle(cur, cur, cur, &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) })
            .unwrap();
        let h = owned(dup).unwrap();
        let ct = creation_time(&h).unwrap();
        assert_eq!(pid_status(pid, ct).unwrap(), PidStatus::Alive);
        assert_eq!(pid_status(pid, ct + 1).unwrap(), PidStatus::Reused);
        // PID 0 (System Idle): OpenProcess fallisce con ERROR_INVALID_PARAMETER.
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
    fn job_con_limiti_cpu_e_memoria() {
        let job = Job::create(Some(2500), Some(512 << 20)).unwrap();
        let i = job.limit_info().unwrap();
        assert_eq!(i.cpu_rate, Some(2500));
        assert_eq!(i.job_memory, Some(512 << 20));
        assert_eq!(i.peak_job_memory, 0);
        assert_eq!(
            job.limit_flags().unwrap() & KILL_ON_JOB_CLOSE,
            KILL_ON_JOB_CLOSE
        );
        let senza = Job::create(None, None).unwrap().limit_info().unwrap();
        assert_eq!(senza.cpu_rate, None);
        assert_eq!(senza.job_memory, None);
    }

    #[test]
    fn job_con_kill_on_close() {
        let job = Job::create(None, None).unwrap();
        assert_eq!(
            job.limit_flags().unwrap() & KILL_ON_JOB_CLOSE,
            KILL_ON_JOB_CLOSE
        );
        assert_eq!(job.accounting().unwrap().active_processes, 0);
    }
}
