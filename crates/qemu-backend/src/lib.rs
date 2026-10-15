//! `qemu-backend`: detection of host capabilities, construction of the QEMU
//! command from controlled options, asynchronous QMP client over a Unix socket and
//! management of the QEMU processes (ROADMAP sections 5, 6.1, 6.3, 6.4, 7.2, 7.3, 7.4; F1).
//!
//! # API
//!
//! - [`probe`]: [`probe::probe_host`] runs the approved QEMU binary
//!   (`--version`, `-accel help`, `-machine help`, `-device help`, `-cpu help`,
//!   `qemu-img --version`/`--help`) with a timeout and produces [`HostCapabilities`];
//!   [`probe::probe_accelerator`] actually starts QEMU paused with the machine
//!   family and firmware kind of a hardware profile and queries
//!   `query-accelerators` via QMP ([`AcceleratorStatus`], cached per
//!   [`ProbeTarget`] in [`HostCapabilities`]).
//! - [`cmdline`]: typed [`VmLaunchSpec`] with a closed [`DeviceProfile`]
//!   (ADR-0027) → `Vec<OsString>` arguments;
//!   paths come only from [`ManagedPaths`] as already verified
//!   [`ManagedPath`]s, text values are tokens of a restricted alphabet.
//! - [`qmp`]: asynchronous (tokio) [`QmpClient`] over a Unix socket, typed
//!   commands with a correlated `id`, events on a separate channel, per-command
//!   timeout. No public API runs arbitrary QMP commands.
//! - [`process`]: [`ProcessContainer`] (Job Object on Windows; signature ready
//!   for cgroup/`PR_SET_PDEATHSIG` on Linux), process identity with handle,
//!   creation time and `run_id`, classification of exit codes.
//! - [`run`]: [`QemuRun`] ties together process, QMP, post-launch checks
//!   (accelerator, absence of NICs), state machine, graceful shutdown with
//!   escalation and an [`EmergencyStop`] independent of the QMP queue.
//! - [`storage_guard`]: file exclusion through share mode (F0-05).
//! - [`screenshot`]: `screendump` PPM → PNG with limits and cleanup.
//! - [`boot_prompt`]: automatic key at the "Press any key to boot from CD or
//!   DVD" prompt of the Windows UEFI media, for a disk without an installed
//!   system only.
//! - [`vcpu_fault`]: a guest whose virtual CPUs stop on an error
//!   (`internal-error`, for example a triple fault under WHPX) is reset and
//!   resumed as hardware would, within a rate limit, then left stopped and
//!   reported.
//!
//! # `unsafe` code
//!
//! The workspace sets `unsafe_code = "deny"`. The only exception is the
//! `sys::windows` module, annotated with `#[allow(unsafe_code)]`: Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `CreateProcessW` with
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` (process in the job from creation, without the
//! window of a later assignment) and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
//! (inheritance limited to the three standard handles), `TerminateJobObject`,
//! `GetProcessTimes`, `OpenProcess` (verification of the PID + creation time
//! identity). The standard library does not expose these primitives in stable
//! form and the `win32job` crate allows neither assignment at creation nor
//! the handle list: the choice follows the results verified in
//! `docs/verification/F0-06-process-supervisor.md`. Every `unsafe` block has a
//! `SAFETY:` comment and the module exposes only safe functions.

//!
//! # Shared types
//!
//! Identifiers ([`lightvm_proto::RunId`], [`lightvm_proto::MediaId`],
//! [`lightvm_proto::VmId`]), typed input ([`lightvm_proto::InputEvent`]),
//! geometry, public errors ([`lightvm_proto::PublicError`]) and observations
//! ([`lightvm_proto::BackendObservation`]) come from `lightvm-proto`. Only the
//! backend-specific types remain local: qcode allowlist
//! ([`input`]), screenshot with the PNG bytes ([`CapturedScreenshot`]), process
//! states and reports ([`run`]).

pub mod boot_prompt;
pub mod cmdline;
pub mod error;
pub mod image;
pub mod input;
pub mod paths;
pub mod prepare;
pub mod probe;
pub mod process;
pub mod qmp;
pub mod run;
pub mod screenshot;
pub mod storage_guard;
pub mod storage_helper;
mod sys;
pub mod validate;
pub mod vcpu_fault;

pub use cmdline::{
    Accelerator, CdromBus, CpuModel, CpuPolicy, DeviceProfile, DiskBus, DiskFormat, DiskSpec,
    DisplayDevice, FirmwareKind, MachineFamily, MachineType, PointerDevice, ProbeFirmware,
    ProbeTarget, QemuCommand, TpmCommand, VmLaunchSpec,
};
pub use error::BackendError;
pub use paths::{ManagedPath, ManagedPaths};
pub use prepare::{HostResources, RunDirs};
pub use probe::{
    Aarch64FirmwareInfo, AcceleratorStatus, BiosInfo, HostCapabilities, ProbeConfig, SwtpmInfo,
};
pub use process::{
    ContainmentMechanism, ExitCodeClass, IdentityStatus, JobLimitReport, JobLimits,
    ProcessContainer, ProcessIdentity,
};
pub use qmp::{QmpClient, QmpConfig, QmpEvent};
pub use run::{
    CloseReport, EmergencyStop, ExitCause, ExitReport, ProcessState, QemuRun, Reconciliation,
    RunConfig, ShutdownPolicy, StopPhase,
};
pub use screenshot::CapturedScreenshot;
pub use vcpu_fault::{VcpuFaultEvent, VcpuFaultPolicy, VcpuFaults};
