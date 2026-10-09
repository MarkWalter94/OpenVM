//! Rate limit per connessione: token bucket (ROADMAP 9, "rate limit per
//! identità/sessione, code limitate").

use std::time::{Duration, Instant};

/// Parametri del token bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    /// Richieste ammesse in raffica.
    pub burst: u32,
    /// Richieste ricaricate al secondo.
    pub per_second: f64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            burst: 50,
            per_second: 100.0,
        }
    }
}

/// Stato del bucket di una connessione.
#[derive(Debug)]
pub struct TokenBucket {
    limit: RateLimit,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// Bucket pieno.
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            tokens: f64::from(limit.burst),
            last: Instant::now(),
        }
    }

    /// Consuma un gettone se disponibile.
    pub fn try_acquire(&mut self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    fn try_acquire_at(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.limit.per_second)
            .min(f64::from(self.limit.burst));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Tempo stimato per il prossimo gettone (solo diagnostico).
    pub fn retry_after(&self) -> Duration {
        if self.limit.per_second <= 0.0 {
            return Duration::MAX;
        }
        Duration::from_secs_f64(((1.0 - self.tokens).max(0.0)) / self.limit.per_second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refill() {
        let mut b = TokenBucket::new(RateLimit {
            burst: 3,
            per_second: 10.0,
        });
        let t0 = b.last;
        assert!(b.try_acquire_at(t0));
        assert!(b.try_acquire_at(t0));
        assert!(b.try_acquire_at(t0));
        assert!(!b.try_acquire_at(t0));
        assert!(b.try_acquire_at(t0 + Duration::from_millis(150)));
        assert!(!b.try_acquire_at(t0 + Duration::from_millis(150)));
    }

    #[test]
    fn never_exceeds_burst() {
        let mut b = TokenBucket::new(RateLimit {
            burst: 2,
            per_second: 1000.0,
        });
        let t0 = b.last;
        let later = t0 + Duration::from_secs(60);
        assert!(b.try_acquire_at(later));
        assert!(b.try_acquire_at(later));
        assert!(!b.try_acquire_at(later));
    }
}
