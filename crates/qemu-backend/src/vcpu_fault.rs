//! Guest stopped on a virtual CPU error: automatic reset, as hardware would
//! do, with a rate limit.
//!
//! Under KVM, or on real hardware, a guest triple fault resets the machine.
//! Under WHPX QEMU 11.1 reports it as `WHPX: Unexpected VP exit code 4`
//! (`WHvRunVpExitReasonUnrecoverableException`), stops the virtual CPUs and
//! leaves the guest stopped with the process alive (QMP `STOP` event):
//! nothing runs any more and every `input-send-event` is refused with
//! `VM not running`. The 11.1.0 source stops it in run state `paused`
//! (`vm_stop(RUN_STATE_PAUSED)` in `target/i386/whpx/whpx-all.c`); other
//! accelerators and builds use `internal-error`. LightVM never pauses a run
//! itself (no QMP `stop`) and disk errors use `io-error`, so a `paused` run
//! was stopped by the accelerator. Windows Vista Setup does this at its first
//! reboot (`docs/verification/R2c-01`).
//!
//! [`watch_vcpu_faults`] follows the QMP events of a run: on `STOP` (and on
//! `GUEST_PANICKED`) it reads `query-status` and, for `internal-error` or
//! `paused`, does what hardware would: `system_reset`, then `cont`
//! ([`reset_and_resume`]: after a reset from `internal-error` QEMU goes to
//! `prelaunch`, a paused guest stays paused; both need `cont`). At most [`VcpuFaultPolicy::max_resets`] automatic resets within
//! [`VcpuFaultPolicy::window`]: past that the guest is left stopped and the
//! halt is published ([`VcpuFaults::halt`]) so the user sees it and decides
//! (reset, or force stop and start again). `guest-panicked` is reset only if
//! the policy asks for it ([`VcpuFaultPolicy::reset_guest_panicked`]);
//! otherwise it is published as a halt straight away.
//!
//! Every decision is queued for the caller ([`VcpuFaults::take_events`]),
//! which logs and audits it.
//!
//! A run with `-action reboot=shutdown` (every WHPX run, [`ResetMode::ProcessExit`])
//! does not reset in place: `system_reset` ends QEMU with a reset cause
//! ([`crate::run::ExitCause::Reboot`]) and the owner starts the run again
//! from cold. There the exit *is* the reset: it counts as a successful
//! automatic reset and no halt is published. The fault count belongs to one
//! QEMU process, so across such cold reboots it starts again from zero; a
//! guest that keeps faulting is bounded by the owner's cap on cold reboots
//! (the service allows at most 10 in 300 s, then leaves the VM failed).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightvm_proto::ipc::{VcpuHalt, VcpuHaltReason};
use tokio::sync::{broadcast, watch};

use crate::qmp::{ConnectionState, EventKind, QmpClient, QmpError, RunState};
use crate::run::{ExitCause, ProcessState};

/// How a reset takes effect in a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResetMode {
    /// QEMU resets the machine in place: `system_reset`, then `cont` when
    /// the guest was stopped ([`reset_and_resume`]).
    #[default]
    InPlace,
    /// `-action reboot=shutdown`: `system_reset` ends the QEMU process with
    /// a reset cause and the owner starts the run again from cold. The reset
    /// succeeded when the process exited as [`ExitCause::Reboot`].
    ProcessExit,
}

/// Automatic reset policy for a guest whose virtual CPUs stopped on an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VcpuFaultPolicy {
    /// Automatic resets allowed within [`Self::window`]; the next fault in
    /// the window leaves the guest stopped. Zero disables the automatic reset.
    pub max_resets: u32,
    /// Sliding window of the limit.
    pub window: Duration,
    /// Also reset a guest that reported a panic (`guest-panicked`). Off by
    /// default: a panic is the guest's own verdict, left to the user.
    pub reset_guest_panicked: bool,
}

impl Default for VcpuFaultPolicy {
    fn default() -> Self {
        Self {
            max_resets: 3,
            window: Duration::from_secs(120),
            reset_guest_panicked: false,
        }
    }
}

/// Decision taken on a fault, queued for the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VcpuFaultEvent {
    /// `system_reset` and `cont` done: the guest runs again.
    AutoReset {
        /// Cause of the stop.
        reason: VcpuHaltReason,
        /// Automatic resets in the window, this one included.
        resets_in_window: u32,
    },
    /// The automatic reset failed (QMP refused or timed out): the guest is
    /// left stopped and the halt published.
    AutoResetFailed {
        /// Cause of the stop.
        reason: VcpuHaltReason,
        /// Error, without QEMU paths.
        error: String,
    },
    /// Limit reached (or reason not reset automatically): the guest is left
    /// stopped and the halt published.
    Halted(VcpuHalt),
}

/// Outcome of [`VcpuFaults::decide`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultDecision {
    /// Reset now; `n` resets in the window counting this one.
    Reset(u32),
    /// Leave the guest stopped.
    Halt(VcpuHalt),
    /// Already halted: nothing new to do.
    Ignore,
}

#[derive(Debug)]
struct Inner {
    policy: VcpuFaultPolicy,
    resets: VecDeque<Instant>,
    halt: Option<VcpuHalt>,
    events: Vec<VcpuFaultEvent>,
}

/// Fault state of a run, shared by the watcher, the run and its owner.
#[derive(Debug)]
pub struct VcpuFaults {
    inner: Mutex<Inner>,
}

/// Maximum queued events (the owner drains them often; a guest that keeps
/// faulting is halted after a few resets anyway).
const MAX_EVENTS: usize = 64;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl VcpuFaults {
    /// Empty state with `policy`.
    pub fn new(policy: VcpuFaultPolicy) -> Self {
        Self {
            inner: Mutex::new(Inner {
                policy,
                resets: VecDeque::new(),
                halt: None,
                events: Vec::new(),
            }),
        }
    }

    /// The guest is stopped on an error and was not reset automatically.
    pub fn halt(&self) -> Option<VcpuHalt> {
        lock(&self.inner).halt
    }

    /// Decisions taken since the last call.
    pub fn take_events(&self) -> Vec<VcpuFaultEvent> {
        std::mem::take(&mut lock(&self.inner).events)
    }

    fn push(inner: &mut Inner, ev: VcpuFaultEvent) {
        if inner.events.len() < MAX_EVENTS {
            inner.events.push(ev);
        }
    }

    /// Decides what to do with a fault observed at `now` and records the
    /// decision (a halt is published and queued; a reset is counted, its
    /// outcome is recorded by [`Self::reset_done`]).
    pub fn decide(&self, reason: VcpuHaltReason, now: Instant) -> FaultDecision {
        let mut g = lock(&self.inner);
        if g.halt.is_some() {
            return FaultDecision::Ignore;
        }
        let window = g.policy.window;
        while g
            .resets
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= window)
        {
            g.resets.pop_front();
        }
        let auto = match reason {
            VcpuHaltReason::InternalError => true,
            VcpuHaltReason::GuestPanicked => g.policy.reset_guest_panicked,
        };
        let done = u32::try_from(g.resets.len()).unwrap_or(u32::MAX);
        if auto && done < g.policy.max_resets {
            g.resets.push_back(now);
            return FaultDecision::Reset(done + 1);
        }
        let halt = VcpuHalt {
            reason,
            automatic_resets: if auto { done } else { 0 },
        };
        g.halt = Some(halt);
        Self::push(&mut g, VcpuFaultEvent::Halted(halt));
        FaultDecision::Halt(halt)
    }

    /// Records the outcome of an automatic reset decided by [`Self::decide`].
    pub fn reset_done(&self, reason: VcpuHaltReason, n: u32, r: Result<(), String>) {
        let mut g = lock(&self.inner);
        match r {
            Ok(()) => Self::push(
                &mut g,
                VcpuFaultEvent::AutoReset {
                    reason,
                    resets_in_window: n,
                },
            ),
            Err(error) => {
                g.halt = Some(VcpuHalt {
                    reason,
                    automatic_resets: n.saturating_sub(1),
                });
                Self::push(&mut g, VcpuFaultEvent::AutoResetFailed { reason, error });
            }
        }
    }

    /// The user reset the guest: the halt is cleared and the limit starts
    /// again.
    pub fn cleared(&self) {
        let mut g = lock(&self.inner);
        g.halt = None;
        g.resets.clear();
    }
}

/// Run states from which the guest only leaves through a reset.
fn needs_reset(s: &RunState) -> bool {
    matches!(
        s,
        RunState::InternalError | RunState::GuestPanicked | RunState::Shutdown
    )
}

/// Maximum wait for QEMU to carry out a requested reset (the request is
/// handled by its main loop after `system_reset` returns).
const RESET_SETTLE: Duration = Duration::from_secs(5);

/// `system_reset`, then `cont` if the virtual CPUs were stopped on an error
/// or before start (`internal-error`, `guest-panicked`, `shutdown`,
/// `paused`, `prelaunch`): QEMU moves such a guest to `prelaunch` (a paused
/// one stays `paused`) after the reset and refuses `cont` until the reset
/// happened, so the state is polled first. A
/// running guest is only reset. Returns `true` if `cont` was sent.
pub async fn reset_and_resume(qmp: &QmpClient) -> Result<bool, QmpError> {
    let before = qmp.query_status().await?.status;
    let resume = needs_reset(&before) || matches!(before, RunState::Prelaunch | RunState::Paused);
    qmp.system_reset().await?;
    if !resume {
        return Ok(false);
    }
    let deadline = Instant::now() + RESET_SETTLE;
    loop {
        let s = qmp.query_status().await?.status;
        match s {
            RunState::Running => return Ok(false),
            RunState::Prelaunch | RunState::Paused => break,
            s if needs_reset(&s) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => {
                return Err(QmpError::Timeout {
                    command: "system_reset",
                });
            }
        }
    }
    qmp.cont().await?;
    Ok(true)
}

/// Maximum wait for QEMU to end after `system_reset` under
/// [`ResetMode::ProcessExit`] (QEMU flushes the disks and stops its helpers
/// before exiting).
const RESET_EXIT_SETTLE: Duration = Duration::from_secs(30);

/// Resets the guest of a run according to `mode`. [`ResetMode::InPlace`] is
/// [`reset_and_resume`]. [`ResetMode::ProcessExit`] sends `system_reset` and
/// waits for the process to end with [`ExitCause::Reboot`] (observed on
/// `process`): that exit is the reset, done also when the reply to
/// `system_reset` was lost with the closing channel. Returns `true` if
/// `cont` was sent.
pub async fn reset_guest(
    qmp: &QmpClient,
    process: &watch::Receiver<ProcessState>,
    mode: ResetMode,
) -> Result<bool, QmpError> {
    match mode {
        ResetMode::InPlace => reset_and_resume(qmp).await,
        ResetMode::ProcessExit => {
            reset_by_exit(qmp, process.clone(), RESET_EXIT_SETTLE).await?;
            Ok(false)
        }
    }
}

async fn reset_by_exit(
    qmp: &QmpClient,
    mut process: watch::Receiver<ProcessState>,
    settle: Duration,
) -> Result<(), QmpError> {
    let sent = qmp.system_reset().await;
    let exited = tokio::time::timeout(
        settle,
        process.wait_for(|s| matches!(s, ProcessState::Exited { .. })),
    )
    .await;
    let rebooted = matches!(
        &exited,
        Ok(Ok(s)) if matches!(**s, ProcessState::Exited { cause: ExitCause::Reboot, .. })
    );
    drop(exited);
    if rebooted {
        return Ok(());
    }
    sent?;
    Err(QmpError::Timeout {
        command: "system_reset",
    })
}

/// Interval at which the watcher checks the process state while no QMP
/// event arrives.
const IDLE_CHECK: Duration = Duration::from_millis(500);

/// Follows the QMP events of a run and resets the guest when its virtual
/// CPUs stop on an error, according to the policy of `faults`. Ends when the
/// process exits or the QMP channel closes; never fails. Not active while a
/// stop is in progress (`Stopping`). Resets in place
/// ([`ResetMode::InPlace`]); see [`watch_vcpu_faults_with`].
pub async fn watch_vcpu_faults(
    qmp: Arc<QmpClient>,
    process: watch::Receiver<ProcessState>,
    faults: Arc<VcpuFaults>,
) {
    watch_vcpu_faults_with(qmp, process, faults, ResetMode::InPlace).await;
}

/// [`watch_vcpu_faults`] with the reset carried out as `mode` says.
pub async fn watch_vcpu_faults_with(
    qmp: Arc<QmpClient>,
    process: watch::Receiver<ProcessState>,
    faults: Arc<VcpuFaults>,
    mode: ResetMode,
) {
    let mut events = qmp.subscribe_events();
    // A fault before the subscription is seen by the first check.
    let mut check = true;
    loop {
        if matches!(*process.borrow(), ProcessState::Exited { .. })
            || matches!(qmp.state(), ConnectionState::Closed { .. })
        {
            return;
        }
        if check {
            check = false;
            if matches!(*process.borrow(), ProcessState::Running) {
                handle_fault(&qmp, &process, &faults, mode).await;
            }
        }
        match tokio::time::timeout(IDLE_CHECK, events.recv()).await {
            Ok(Ok(e)) => {
                check = matches!(e.kind(), EventKind::Stop | EventKind::GuestPanicked);
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => check = true,
            Ok(Err(broadcast::error::RecvError::Closed)) => return,
            Err(_) => {}
        }
    }
}

async fn handle_fault(
    qmp: &QmpClient,
    process: &watch::Receiver<ProcessState>,
    faults: &VcpuFaults,
    mode: ResetMode,
) {
    let Ok(status) = qmp.query_status().await else {
        return;
    };
    let reason = match status.status {
        RunState::InternalError | RunState::Paused => VcpuHaltReason::InternalError,
        RunState::GuestPanicked => VcpuHaltReason::GuestPanicked,
        _ => return,
    };
    if let FaultDecision::Reset(n) = faults.decide(reason, Instant::now()) {
        let r = reset_guest(qmp, process, mode)
            .await
            .map(drop)
            .map_err(|e| e.public_summary().to_string());
        faults.reset_done(reason, n, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max: u32) -> VcpuFaultPolicy {
        VcpuFaultPolicy {
            max_resets: max,
            window: Duration::from_secs(120),
            reset_guest_panicked: false,
        }
    }

    #[test]
    fn resets_up_to_the_limit_then_halts() {
        let f = VcpuFaults::new(policy(3));
        let t0 = Instant::now();
        for n in 1..=3 {
            assert_eq!(
                f.decide(
                    VcpuHaltReason::InternalError,
                    t0 + Duration::from_secs(n.into())
                ),
                FaultDecision::Reset(n)
            );
        }
        let halt = VcpuHalt {
            reason: VcpuHaltReason::InternalError,
            automatic_resets: 3,
        };
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(10)),
            FaultDecision::Halt(halt)
        );
        assert_eq!(f.halt(), Some(halt));
        assert_eq!(f.take_events(), vec![VcpuFaultEvent::Halted(halt)]);
        // Already halted: no new decision.
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(11)),
            FaultDecision::Ignore
        );
        // The user's reset starts again.
        f.cleared();
        assert_eq!(f.halt(), None);
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(12)),
            FaultDecision::Reset(1)
        );
    }

    #[test]
    fn the_window_slides() {
        let f = VcpuFaults::new(policy(2));
        let t0 = Instant::now();
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0),
            FaultDecision::Reset(1)
        );
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(60)),
            FaultDecision::Reset(2)
        );
        // The first reset left the window.
        assert_eq!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(120)),
            FaultDecision::Reset(2)
        );
        assert!(matches!(
            f.decide(VcpuHaltReason::InternalError, t0 + Duration::from_secs(121)),
            FaultDecision::Halt(_)
        ));
    }

    #[test]
    fn guest_panic_is_not_reset_unless_asked() {
        let f = VcpuFaults::new(policy(3));
        let t0 = Instant::now();
        let halt = VcpuHalt {
            reason: VcpuHaltReason::GuestPanicked,
            automatic_resets: 0,
        };
        assert_eq!(
            f.decide(VcpuHaltReason::GuestPanicked, t0),
            FaultDecision::Halt(halt)
        );
        let f = VcpuFaults::new(VcpuFaultPolicy {
            reset_guest_panicked: true,
            ..policy(3)
        });
        assert_eq!(
            f.decide(VcpuHaltReason::GuestPanicked, t0),
            FaultDecision::Reset(1)
        );
    }

    #[test]
    fn zero_disables_and_a_failed_reset_halts() {
        let f = VcpuFaults::new(policy(0));
        assert!(matches!(
            f.decide(VcpuHaltReason::InternalError, Instant::now()),
            FaultDecision::Halt(VcpuHalt {
                automatic_resets: 0,
                ..
            })
        ));
        let f = VcpuFaults::new(policy(3));
        let FaultDecision::Reset(n) = f.decide(VcpuHaltReason::InternalError, Instant::now())
        else {
            panic!()
        };
        f.reset_done(VcpuHaltReason::InternalError, n, Err("timeout".into()));
        assert_eq!(
            f.halt(),
            Some(VcpuHalt {
                reason: VcpuHaltReason::InternalError,
                automatic_resets: 0
            })
        );
        assert!(matches!(
            f.take_events()[..],
            [VcpuFaultEvent::AutoResetFailed { .. }]
        ));
    }
}
