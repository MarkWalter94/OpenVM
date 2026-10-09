//! `qemu-backend`: rilevamento delle capacità dell'host, costruzione del comando
//! QEMU da opzioni controllate, client QMP asincrono su socket Unix e gestione
//! dei processi QEMU (ROADMAP sezioni 5, 6.1, 6.3, 6.4, 7.2, 7.3, 7.4; F1).
//!
//! # API
//!
//! - [`probe`]: [`probe::probe_host`] esegue il binario QEMU approvato
//!   (`--version`, `-accel help`, `-machine help`, `-device help`, `-cpu help`,
//!   `qemu-img --version`/`--help`) con timeout e produce [`HostCapabilities`];
//!   [`probe::probe_accelerator`] avvia davvero QEMU in pausa e interroga
//!   `query-accelerators` via QMP ([`AcceleratorStatus`]).
//! - [`cmdline`]: [`VmLaunchSpec`] tipizzato → argomenti `Vec<OsString>`;
//!   i percorsi arrivano soltanto da [`ManagedPaths`] come [`ManagedPath`]
//!   già verificati, i valori testuali sono token di un alfabeto ristretto.
//! - [`qmp`]: [`QmpClient`] asincrono (tokio) su socket Unix, comandi
//!   tipizzati con `id` correlato, eventi su canale separato, timeout per
//!   comando. Nessuna API pubblica esegue comandi QMP arbitrari.
//! - [`process`]: [`ProcessContainer`] (Job Object su Windows; firma pronta
//!   per cgroup/`PR_SET_PDEATHSIG` su Linux), identità del processo con handle e
//!   tempo di creazione e `run_id`, classificazione dei codici d'uscita.
//! - [`run`]: [`QemuRun`] collega processo, QMP, verifiche post-avvio
//!   (acceleratore, assenza di NIC), macchina a stati, arresto gentile con
//!   scalata ed [`EmergencyStop`] indipendente dalla coda QMP.
//! - [`storage_guard`]: esclusione dei file tramite share mode (F0-05).
//! - [`screenshot`]: `screendump` PPM → PNG con limiti e pulizia.
//!
//! # Codice `unsafe`
//!
//! Il workspace imposta `unsafe_code = "deny"`. L'unica eccezione è il modulo
//! `sys::windows`, annotato con `#[allow(unsafe_code)]`: Job Object con
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `CreateProcessW` con
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` (processo nel job dalla creazione, senza la
//! finestra di un'assegnazione successiva) e `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
//! (eredità limitata ai tre handle standard), `TerminateJobObject`,
//! `GetProcessTimes`, `OpenProcess` (verifica dell'identità PID + tempo di
//! creazione). La libreria standard non espone queste primitive in forma
//! stabile e il crate `win32job` non permette l'assegnazione alla creazione né
//! la lista di handle: la scelta segue i risultati verificati in
//! `docs/verification/F0-06-supervisore-processi.md`. Ogni blocco `unsafe` ha un
//! commento `SAFETY:` e il modulo espone soltanto funzioni sicure.

//!
//! # Tipi condivisi
//!
//! Identificativi ([`lightvm_proto::RunId`], [`lightvm_proto::MediaId`],
//! [`lightvm_proto::VmId`]), input tipizzato ([`lightvm_proto::InputEvent`]),
//! geometria, errori pubblici ([`lightvm_proto::PublicError`]) e osservazioni
//! ([`lightvm_proto::BackendObservation`]) vengono da `lightvm-proto`. Restano
//! locali soltanto i tipi specifici del backend: allowlist dei qcode
//! ([`input`]), screenshot con i byte del PNG ([`CapturedScreenshot`]), stati e
//! rapporti del processo ([`run`]).

pub mod cmdline;
pub mod error;
pub mod input;
pub mod paths;
pub mod prepare;
pub mod probe;
pub mod process;
pub mod qmp;
pub mod run;
pub mod screenshot;
pub mod storage_guard;
mod sys;
pub mod validate;

pub use cmdline::{
    Accelerator, CpuModel, DiskFormat, DiskSpec, MachineType, QemuCommand, VmLaunchSpec,
};
pub use error::BackendError;
pub use paths::{ManagedPath, ManagedPaths};
pub use prepare::{HostResources, RunDirs};
pub use probe::{AcceleratorStatus, HostCapabilities, ProbeConfig};
pub use process::{
    ContainmentMechanism, ExitCodeClass, IdentityStatus, ProcessContainer, ProcessIdentity,
};
pub use qmp::{QmpClient, QmpConfig, QmpEvent};
pub use run::{
    CloseReport, EmergencyStop, ExitCause, ExitReport, ProcessState, QemuRun, Reconciliation,
    RunConfig, ShutdownPolicy, StopPhase,
};
pub use screenshot::CapturedScreenshot;
