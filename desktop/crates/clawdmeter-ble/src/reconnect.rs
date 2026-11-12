//! When to try connecting again: exponential backoff, cut short by wakeups.
//!
//! A pure state machine over unix seconds, so the daemon loop stays a thin
//! driver and the policy is unit-tested. The policy:
//!
//! * a failed attempt waits 1, 2, 4, 8 … s, capped at [`BACKOFF_CAP`], and at
//!   [`LONG_CAP`] after [`LONG_AFTER`] failures in a row (a board that's
//!   gone for good, or a lost pairing, isn't hammered every 15 s);
//! * a success resets the delay;
//! * a wakeup (the OS reports the device connected, or advertising) makes the
//!   next attempt due at once and resets the delay. A wakeup that lands while
//!   an attempt is in flight is latched: if that attempt fails (it raced the
//!   device coming back), the retry follows after [`WOKEN_RETRY`] instead of
//!   the backoff.

/// Longest wait between attempts (s). Low enough that a board coming back
/// unnoticed by the wakeup watcher (no BlueZ auto-connect, no scan running)
/// is still picked up quickly; an attempt against an absent device is just a
/// D-Bus listing plus a bounded connect.
pub const BACKOFF_CAP: u64 = 15;

/// Failures in a row (about 5 min at the normal cap) before the long cap.
pub const LONG_AFTER: u32 = 20;
/// Longest wait once [`LONG_AFTER`] attempts have failed (s). A wakeup still
/// makes the next attempt due at once.
pub const LONG_CAP: u64 = 60;

/// Delay after a failed attempt that a wakeup raced (s): long enough that a
/// device which advertises but won't take a connection can't spin the loop.
pub const WOKEN_RETRY: u64 = 1;

/// Reconnect scheduling state.
#[derive(Debug, Clone)]
pub struct Reconnect {
    delay: u64,
    next_at: i64,
    woken: bool,
    /// Failures since the last success, wakeup or fresh start.
    streak: u32,
}

impl Default for Reconnect {
    fn default() -> Self {
        Reconnect { delay: 1, next_at: 0, woken: false, streak: 0 }
    }
}

impl Reconnect {
    /// An attempt may start now.
    pub fn due(&self, now: i64) -> bool {
        now >= self.next_at
    }

    /// Unix second the next attempt is due.
    pub fn next_at(&self) -> i64 {
        self.next_at
    }

    /// An attempt failed. Returns the wait (s) before the next one.
    pub fn failed(&mut self, now: i64) -> u64 {
        self.streak = self.streak.saturating_add(1);
        let cap = if self.streak > LONG_AFTER { LONG_CAP } else { BACKOFF_CAP };
        let wait = if std::mem::take(&mut self.woken) {
            self.delay = 1;
            WOKEN_RETRY
        } else {
            let d = self.delay.min(cap);
            self.delay = (self.delay * 2).min(cap);
            d
        };
        self.next_at = now + wait as i64;
        wait
    }

    /// An attempt succeeded.
    pub fn connected(&mut self) {
        self.delay = 1;
        self.woken = false;
        self.streak = 0;
    }

    /// The link dropped (or the user asked to reconnect): try again after
    /// `after` seconds, with a fresh backoff.
    pub fn retry_in(&mut self, now: i64, after: u64) {
        self.delay = 1;
        self.woken = false;
        self.streak = 0;
        self.next_at = now + after as i64;
    }

    /// The device showed up. `in_flight`: an attempt is running right now.
    pub fn wake(&mut self, now: i64, in_flight: bool) {
        self.delay = 1;
        self.streak = 0;
        self.next_at = self.next_at.min(now);
        if in_flight {
            self.woken = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap() {
        let mut r = Reconnect::default();
        assert!(r.due(0));
        let waits: Vec<u64> = (0..7).map(|_| r.failed(100)).collect();
        assert_eq!(waits, [1, 2, 4, 8, 15, 15, 15]);
        assert!(!r.due(114));
        assert!(r.due(115));
    }

    #[test]
    fn a_long_losing_streak_backs_off_further() {
        let mut r = Reconnect::default();
        let waits: Vec<u64> = (0..LONG_AFTER + 3).map(|_| r.failed(0)).collect();
        assert_eq!(waits[LONG_AFTER as usize - 1], BACKOFF_CAP);
        assert_eq!(&waits[LONG_AFTER as usize..], [15, 30, 60], "then doubles on to the long cap");
        r.wake(0, false);
        assert_eq!(r.failed(0), 1, "a wakeup starts over");
    }

    #[test]
    fn success_resets() {
        let mut r = Reconnect::default();
        for _ in 0..5 {
            r.failed(0);
        }
        r.connected();
        assert_eq!(r.failed(10), 1);
    }

    #[test]
    fn wake_while_idle_makes_the_attempt_due_now() {
        let mut r = Reconnect::default();
        for _ in 0..6 {
            r.failed(100);
        }
        assert!(!r.due(101));
        r.wake(101, false);
        assert!(r.due(101));
        // The attempt that wake triggered fails: backoff restarts at 1 s.
        assert_eq!(r.failed(102), 1);
        assert_eq!(r.failed(103), 2);
    }

    #[test]
    fn wake_during_an_attempt_is_latched() {
        let mut r = Reconnect::default();
        for _ in 0..6 {
            r.failed(0);
        }
        // An attempt is running (started when due); the device comes back.
        r.wake(200, true);
        // That attempt fails anyway: retry almost at once, not after 15 s.
        assert_eq!(r.failed(201), WOKEN_RETRY);
        assert!(r.due(201 + WOKEN_RETRY as i64));
        // The latch is consumed: the next failure backs off normally.
        assert_eq!(r.failed(203), 1);
        assert_eq!(r.failed(204), 2);
    }

    #[test]
    fn success_clears_a_latched_wake() {
        let mut r = Reconnect::default();
        r.wake(0, true);
        r.connected();
        assert_eq!(r.failed(5), 1);
        assert_eq!(r.failed(6), 2);
    }

    #[test]
    fn wake_never_postpones() {
        let mut r = Reconnect::default();
        r.retry_in(50, 2);
        r.wake(10, false);
        assert_eq!(r.next_at(), 10);
    }

    #[test]
    fn retry_in_after_a_drop() {
        let mut r = Reconnect::default();
        for _ in 0..6 {
            r.failed(0);
        }
        r.retry_in(100, 2);
        assert!(!r.due(101));
        assert!(r.due(102));
        assert_eq!(r.failed(102), 1);
    }
}
