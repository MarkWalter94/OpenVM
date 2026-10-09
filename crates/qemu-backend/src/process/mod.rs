//! Contenimento dei processi QEMU e identità del processo (ROADMAP 6.1, 7.2
//! punto 4, 7.3; ADR-0002, ADR-0009, ADR-0015; F0-06, F0-07).
//!
//! Un [`ProcessContainer`] per esecuzione:
//!
//! - Windows: Job Object anonimo con `KILL_ON_JOB_CLOSE`, senza breakaway;
//!   il processo vi entra **alla creazione** (`PROC_THREAD_ATTRIBUTE_JOB_LIST`).
//!   La morte del supervisor chiude l'handle del job e termina QEMU (F0-06,
//!   scenario a). L'Emergency Stop è `TerminateJobObject`.
//! - Linux: firma pronta, implementazione rinviata. F0-07 propone
//!   `PR_SET_PDEATHSIG(SIGKILL)` impostato da un thread di spawn dedicato più un
//!   cgroup v2 per VM con `cgroup.kill` dentro uno scope systemd delegato;
//!   [`new_container`] restituisce oggi un errore `unsupported`.

use std::ffi::OsString;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lightvm_proto::RunId;

use crate::error::Result;
use crate::paths::ManagedPath;

#[cfg(windows)]
mod windows;

/// Meccanismo di contenimento effettivo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentMechanism {
    /// Job Object Windows con `KILL_ON_JOB_CLOSE`, processo nel job dalla creazione.
    WindowsJobObject,
    /// Linux: cgroup v2 delegato con `cgroup.kill` più `PR_SET_PDEATHSIG` (rinviato).
    LinuxCgroupPdeathsig,
}

/// Identità serializzabile del processo: PID più tempo di creazione. Il PID da
/// solo può essere riassegnato (F0-06, sezione 3); l'identità primaria resta
/// l'handle posseduto da [`ChildProcess`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessIdentity {
    /// Esecuzione a cui appartiene il processo.
    pub run_id: RunId,
    /// PID (informativo).
    pub pid: u32,
    /// Tempo di creazione (Windows: FILETIME in unità da 100 ns).
    pub creation_time: u64,
}

/// Stato di un processo cercato per identità (PID + tempo di creazione).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStatus {
    /// Nessun processo con quel PID.
    NotFound,
    /// Il PID è stato riassegnato a un altro processo.
    Reused,
    /// Stesso processo, terminato ma ancora referenziato da un handle.
    Exited,
    /// Stesso processo, in esecuzione.
    Alive,
}

impl IdentityStatus {
    /// `true` se il processo con questa identità è ancora in esecuzione.
    pub fn is_alive(self) -> bool {
        matches!(self, Self::Alive)
    }
}

impl ProcessIdentity {
    /// Cerca il processo con questa identità senza toccarlo (verifica di
    /// assenza di orfani, riconciliazione dopo un riavvio del supervisor).
    pub fn status(&self) -> Result<IdentityStatus> {
        #[cfg(windows)]
        {
            use crate::sys::windows::{PidStatus, pid_status};
            Ok(match pid_status(self.pid, self.creation_time)? {
                PidStatus::NotFound => IdentityStatus::NotFound,
                PidStatus::Reused => IdentityStatus::Reused,
                PidStatus::Exited => IdentityStatus::Exited,
                PidStatus::Alive => IdentityStatus::Alive,
            })
        }
        #[cfg(not(windows))]
        {
            Err(crate::error::BackendError::Unsupported {
                what: "verifica dell'identità del processo su questo sistema".into(),
                hint: "implementazione Linux (pidfd) non ancora presente".into(),
            })
        }
    }
}

/// Codice d'uscita usato dall'Emergency Stop con `TerminateJobObject`.
pub const EMERGENCY_EXIT_CODE: u32 = 0xE5E5;

/// Classe del codice d'uscita di QEMU su Windows (F0-06 §2b, F0-08).
///
/// Il codice da solo non distingue sempre la causa (`taskkill /F` produce 1,
/// come un errore di QEMU): la causa definitiva combina codice e azioni del
/// backend (vedi [`crate::run::ExitCause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCodeClass {
    /// 0: uscita ordinata (`quit`, spegnimento del guest, chiusura del job).
    Clean,
    /// 1: errore di QEMU (riga di comando, acceleratore) o `taskkill /F`.
    QemuErrorOrForcedKill,
    /// [`EMERGENCY_EXIT_CODE`]: terminazione del job richiesta dal backend.
    EmergencyStop,
    /// `0xFFFFFFFF`: `TerminateProcess(-1)` esterno (per esempio `Stop-Process`).
    ExternalTermination,
    /// NTSTATUS di errore (`0xC0000000` e oltre): eccezione o crash, compreso
    /// il superamento del limite di memoria del job (F0-08).
    Exception(u32),
    /// Altro codice.
    Other(u32),
    /// Codice non disponibile.
    Unknown,
}

/// Classifica un codice d'uscita.
pub fn classify_exit_code(code: Option<u32>) -> ExitCodeClass {
    match code {
        None => ExitCodeClass::Unknown,
        Some(0) => ExitCodeClass::Clean,
        Some(1) => ExitCodeClass::QemuErrorOrForcedKill,
        Some(EMERGENCY_EXIT_CODE) => ExitCodeClass::EmergencyStop,
        Some(0xFFFF_FFFF) => ExitCodeClass::ExternalTermination,
        Some(c) if c >= 0xC000_0000 => ExitCodeClass::Exception(c),
        Some(c) => ExitCodeClass::Other(c),
    }
}

/// Richiesta di avvio di un processo nel contenitore.
#[derive(Clone, Debug)]
pub struct SpawnRequest {
    /// Esecuzione a cui apparterrà il processo.
    pub run_id: RunId,
    /// Eseguibile approvato.
    pub program: ManagedPath,
    /// Argomenti strutturati.
    pub args: Vec<OsString>,
    /// Ambiente completo del figlio (nessuna eredità dal supervisor).
    pub env: Vec<(OsString, OsString)>,
    /// File che riceve stdout.
    pub stdout: ManagedPath,
    /// File che riceve stderr.
    pub stderr: ManagedPath,
}

/// Ambiente minimo per QEMU su questo host.
///
/// F0-06 (1b): con ambiente vuoto Winsock non si inizializza e QEMU non crea il
/// socket QMP; con il solo `SystemRoot` QMP e WHPX funzionano. Non servono
/// `PATH`, `TEMP`, `USERPROFILE`, `APPDATA` con firmware a percorsi assoluti e
/// `-no-user-config`. Nessuna credenziale (provider, client MCP) è trasmessa.
pub fn minimal_environment() -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        env.push((OsString::from("SystemRoot"), root));
    }
    env
}

/// Contenitore dei processi di un'esecuzione.
pub trait ProcessContainer: Send + Sync + fmt::Debug {
    /// Avvia il processo già dentro il contenitore (nessuna finestra temporale
    /// in cui il processo è fuori dal contenitore).
    fn spawn(&self, req: &SpawnRequest) -> Result<ChildProcess>;

    /// Termina con primitive OS tutti i processi del contenitore, senza QMP
    /// (Emergency Stop, ADR-0009). Non attende: vedi [`Self::active_count`].
    fn kill_all(&self, exit_code: u32) -> Result<()>;

    /// Processi ancora attivi nel contenitore.
    fn active_count(&self) -> Result<u32>;

    /// Meccanismo effettivo.
    fn mechanism(&self) -> ContainmentMechanism;

    /// Verifica che il processo appartenga al contenitore e che il
    /// contenitore termini i processi alla propria chiusura.
    fn verify_membership(&self, child: &ChildProcess) -> Result<()>;
}

/// Crea il contenitore per una nuova esecuzione.
pub fn new_container() -> Result<Arc<dyn ProcessContainer>> {
    #[cfg(windows)]
    {
        Ok(Arc::new(windows::JobContainer::new()?))
    }
    #[cfg(not(windows))]
    {
        // TODO(F1/R2a): cgroup v2 delegato + `cgroup.kill` e
        // `PR_SET_PDEATHSIG(SIGKILL)` da thread di spawn dedicato (F0-07).
        Err(crate::error::BackendError::Unsupported {
            what: "contenimento dei processi QEMU su questo sistema operativo".into(),
            hint: "implementazione Linux (cgroup v2 + PR_SET_PDEATHSIG, F0-07) non ancora presente"
                .into(),
        })
    }
}

/// Processo figlio posseduto: l'handle è l'identità primaria.
#[derive(Debug)]
pub struct ChildProcess {
    identity: ProcessIdentity,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
}

impl ChildProcess {
    /// Identità serializzabile.
    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Attende l'uscita fino a `timeout` (`None` = senza limite). Restituisce
    /// il codice d'uscita se il processo è terminato.
    pub fn wait(&self, timeout: Option<Duration>) -> Result<Option<u32>> {
        #[cfg(windows)]
        {
            let ms = timeout.map(|t| u32::try_from(t.as_millis()).unwrap_or(u32::MAX - 1));
            Ok(crate::sys::windows::wait_process(&self.handle, ms)?)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(crate::error::BackendError::StateConflict(
                "processo non gestito su questo sistema".into(),
            ))
        }
    }

    /// Codice d'uscita se il processo è già terminato.
    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        self.wait(Some(Duration::ZERO))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classi_dei_codici_di_uscita() {
        assert_eq!(classify_exit_code(None), ExitCodeClass::Unknown);
        assert_eq!(classify_exit_code(Some(0)), ExitCodeClass::Clean);
        assert_eq!(
            classify_exit_code(Some(1)),
            ExitCodeClass::QemuErrorOrForcedKill
        );
        assert_eq!(
            classify_exit_code(Some(0xE5E5)),
            ExitCodeClass::EmergencyStop
        );
        assert_eq!(
            classify_exit_code(Some(0xFFFF_FFFF)),
            ExitCodeClass::ExternalTermination
        );
        assert_eq!(
            classify_exit_code(Some(0xC000_0409)),
            ExitCodeClass::Exception(0xC000_0409)
        );
        assert_eq!(classify_exit_code(Some(3)), ExitCodeClass::Other(3));
    }
}
