//! Host suspend/resume detection (ROADMAP 7.3, F6).
//!
//! "Su ... sospensione/ripresa host, le sessioni AI non proseguono
//! indefinitamente: revoca e stop secondo il percorso di emergenza ... al
//! resume si controllano scadenze prima di riaccettare input."
//!
//! The supervisor thread never waits more than
//! [`crate::supervisor::OBSERVE_INTERVAL`] (1 s) for a command. It marks the
//! instant before each wait and checks, as soon as the wait returns and
//! before serving anything, how much time passed by the wall clock and by
//! the monotonic clock. A gap much longer than the wait means the whole
//! process was frozen (host sleep or hibernation) or the wall clock jumped
//! forward: either way the agent sessions are terminated with
//! `host_resumed` and their VMs stopped on the emergency path, before any
//! queued agent request is served and before the expiry check (which would
//! otherwise report the same event as a lease expiry).
//!
//! Both clocks are read because they behave differently across a suspend
//! depending on the platform (the monotonic clock may or may not advance
//! while the host sleeps; the wall clock does). A forward jump of the wall
//! clock without a suspend (manual change, large NTP correction) is treated
//! the same way: the sessions end and the user authorizes again (fail-safe).
//!
//! Only the bounded wait is measured that way ([`ResumeDetector::check`]).
//! Serving a command and observing the VMs may legitimately take longer
//! than the threshold (name resolution of the endpoints, a cold reboot, a
//! report export, a Tools call). There both clocks advance by the work
//! time and, on Windows (`Instant` is `QueryPerformanceCounter`), the
//! monotonic clock also counts the time the host slept, so the length of a
//! work segment cannot tell work from a suspend. After work
//! ([`ResumeDetector::check_work`]) only a wall clock running ahead of the
//! monotonic one by more than the threshold counts (a suspend on a platform
//! whose monotonic clock stops, or a forward wall-clock jump). A suspend in
//! the middle of work on a host whose monotonic clock counts it is not seen
//! here: the leases of the sessions (at most 10 s) expire and the expiry
//! check ends them on the same emergency path.
//! No Windows power broadcast is used: it needs a window procedure and
//! `unsafe` code, which the service does not have.
//!
//! The time source is injectable ([`WallClock`]) so that tests can simulate
//! a jump without sleeping the host.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Default gap that counts as a resume: five times the longest wait of the
/// supervisor loop. Sleeps shorter than this keep the sessions if their
/// lease (at most 10 s) is still valid; longer ones always end them.
pub const DEFAULT_RESUME_GAP: Duration = Duration::from_secs(5);

/// Source of the wall-clock time (default: the system clock).
#[derive(Clone)]
pub struct WallClock(Arc<dyn Fn() -> SystemTime + Send + Sync>);

impl WallClock {
    /// Clock reading `f`.
    pub fn new(f: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Current wall-clock time.
    pub fn now(&self) -> SystemTime {
        (self.0)()
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new(SystemTime::now)
    }
}

impl fmt::Debug for WallClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WallClock")
    }
}

/// Detector of a host resume (or forward clock jump) between two points of
/// the supervisor loop.
#[derive(Debug)]
pub struct ResumeDetector {
    clock: WallClock,
    threshold: Duration,
    last_wall: SystemTime,
    last_mono: Instant,
}

impl ResumeDetector {
    /// Detector with the given clock and threshold, marked now.
    pub fn new(clock: WallClock, threshold: Duration) -> Self {
        let last_wall = clock.now();
        Self {
            clock,
            threshold,
            last_wall,
            last_mono: Instant::now(),
        }
    }

    /// Marks the thread alive now (before a bounded wait).
    pub fn mark(&mut self) {
        self.mark_at(self.clock.now(), Instant::now());
    }

    /// Gap since the last mark if it exceeds the threshold by either clock;
    /// the detector is marked again. Only after a bounded idle wait: after
    /// work of unbounded length see [`Self::check_work`].
    pub fn check(&mut self) -> Option<Duration> {
        self.check_at(self.clock.now(), Instant::now())
    }

    /// Check at the end of a work segment (serving a command, observing):
    /// only a wall clock that ran ahead of the monotonic clock by more than
    /// the threshold is a resume, never the length of the work itself; the
    /// detector is marked again.
    pub fn check_work(&mut self) -> Option<Duration> {
        self.check_work_at(self.clock.now(), Instant::now())
    }

    fn check_work_at(&mut self, wall: SystemTime, mono: Instant) -> Option<Duration> {
        let by_wall = wall.duration_since(self.last_wall).unwrap_or_default();
        let by_mono = mono.saturating_duration_since(self.last_mono);
        self.mark_at(wall, mono);
        (by_wall.saturating_sub(by_mono) > self.threshold).then_some(by_wall)
    }

    fn mark_at(&mut self, wall: SystemTime, mono: Instant) {
        self.last_wall = wall;
        self.last_mono = mono;
    }

    fn check_at(&mut self, wall: SystemTime, mono: Instant) -> Option<Duration> {
        // A backward wall-clock step is not a resume.
        let by_wall = wall.duration_since(self.last_wall).unwrap_or_default();
        let by_mono = mono.saturating_duration_since(self.last_mono);
        self.mark_at(wall, mono);
        let gap = by_wall.max(by_mono);
        (gap > self.threshold).then_some(gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_beyond_the_threshold_are_resumes() {
        let mut d = ResumeDetector::new(WallClock::default(), Duration::from_secs(5));
        let w0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let m0 = Instant::now();
        d.mark_at(w0, m0);
        // Ordinary wait of 1 s: nothing.
        assert_eq!(
            d.check_at(w0 + Duration::from_secs(1), m0 + Duration::from_secs(1)),
            None
        );
        // Suspend with a monotonic clock that stops while sleeping: the wall
        // clock shows the gap.
        let (w1, m1) = (w0 + Duration::from_secs(1), m0 + Duration::from_secs(1));
        d.mark_at(w1, m1);
        assert_eq!(
            d.check_at(
                w1 + Duration::from_secs(600),
                m1 + Duration::from_millis(900)
            ),
            Some(Duration::from_secs(600))
        );
        // Suspend with a monotonic clock that keeps counting.
        let (w2, m2) = (w1 + Duration::from_secs(600), m1 + Duration::from_secs(1));
        d.mark_at(w2, m2);
        assert_eq!(
            d.check_at(w2 + Duration::from_secs(90), m2 + Duration::from_secs(90)),
            Some(Duration::from_secs(90))
        );
        // Wall clock stepped back: not a resume.
        let (w3, m3) = (w2 + Duration::from_secs(90), m2 + Duration::from_secs(90));
        d.mark_at(w3, m3);
        assert_eq!(
            d.check_at(
                w3 - Duration::from_secs(3600),
                m3 + Duration::from_millis(500)
            ),
            None
        );
        // At the threshold exactly: not yet.
        let (w4, m4) = (w3, m3 + Duration::from_millis(500));
        d.mark_at(w4, m4);
        assert_eq!(
            d.check_at(w4 + Duration::from_secs(5), m4 + Duration::from_secs(5)),
            None
        );
    }

    /// A command or an observation longer than the threshold is not a
    /// resume: only the idle wait is measured by its length; after work,
    /// only a wall clock running ahead of the monotonic one counts.
    #[test]
    fn long_work_is_not_a_resume() {
        let mut d = ResumeDetector::new(WallClock::default(), Duration::from_secs(5));
        let w0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let m0 = Instant::now();
        // Idle wait of 1 s, then a command served for 40 s (both clocks).
        d.mark_at(w0, m0);
        let (w1, m1) = (w0 + Duration::from_secs(1), m0 + Duration::from_secs(1));
        assert_eq!(d.check_at(w1, m1), None);
        let (w2, m2) = (w1 + Duration::from_secs(40), m1 + Duration::from_secs(40));
        assert_eq!(d.check_work_at(w2, m2), None);
        // The next idle wait measures only itself.
        assert_eq!(
            d.check_at(w2 + Duration::from_secs(1), m2 + Duration::from_secs(1)),
            None
        );
        // Work during which the wall clock ran 10 min ahead of the monotonic
        // one (suspend with a stopped monotonic clock, or a forward jump).
        let (w3, m3) = (w2 + Duration::from_secs(1), m2 + Duration::from_secs(1));
        d.mark_at(w3, m3);
        assert_eq!(
            d.check_work_at(w3 + Duration::from_secs(602), m3 + Duration::from_secs(2)),
            Some(Duration::from_secs(602))
        );
        // Wall clock stepped back during work: not a resume.
        let (w4, m4) = (w3 + Duration::from_secs(602), m3 + Duration::from_secs(2));
        d.mark_at(w4, m4);
        assert_eq!(
            d.check_work_at(w4 - Duration::from_secs(3600), m4 + Duration::from_secs(30)),
            None
        );
    }
}
