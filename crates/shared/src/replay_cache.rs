//! In-memory replay cache for signed-request nonces. Bounds the window
//! during which a captured signature can be re-presented to the server
//! with the same nonce+timestamp. The window is short by design — long
//! enough to absorb modest clock skew between client and server, short
//! enough to keep memory bounded and the replay surface tight.
//!
//! Used by `ClientSignatureVerifier` (the external listener's auth
//! path) and tested directly here for the time-window + duplicate
//! semantics.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Default freshness window for external client signed requests. Locked
/// at 5 minutes; tighter than that
/// risks rejecting legitimate requests on clients with mild clock skew,
/// looser than that widens the replay surface for captured signatures.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(300);

/// Bounded record of recently-seen nonces. A nonce is "seen" until its
/// associated timestamp falls outside the freshness window relative to
/// now. The map self-evicts on insert (no background thread).
pub struct ReplayCache {
    window: Duration,
    seen: Mutex<HashMap<String, i64>>,
    now: fn() -> i64,
}

impl ReplayCache {
    /// Build a cache with the given freshness window and the system
    /// clock as the time source.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            seen: Mutex::new(HashMap::new()),
            now: system_now_secs,
        }
    }

    /// Build a cache with a caller-supplied time source. Tests use
    /// this to drive the clock forward without sleeping.
    pub fn with_clock(window: Duration, now: fn() -> i64) -> Self {
        Self {
            window,
            seen: Mutex::new(HashMap::new()),
            now,
        }
    }

    /// Record a nonce as seen at the supplied client-asserted
    /// timestamp. Returns `true` when the nonce is fresh and not a
    /// duplicate (caller proceeds); `false` when the timestamp is
    /// outside the window or the nonce was already recorded for the
    /// current window (caller rejects).
    pub fn insert_if_fresh(&self, nonce: &str, ts: i64) -> bool {
        let now = (self.now)();
        if !within_window(now, ts, self.window) {
            return false;
        }
        let mut seen = self.seen.lock().expect("replay cache mutex poisoned");
        // Opportunistic eviction: walk the map and drop any nonce whose
        // recorded timestamp is now outside the window. Bounded-work
        // approach acceptable because the map only holds entries from
        // the last `window` seconds — at typical signed-request rates
        // (handfuls per minute per client) cardinality stays small.
        let cutoff = now - self.window.as_secs() as i64;
        seen.retain(|_, recorded_ts| *recorded_ts >= cutoff);
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(nonce.to_string(), ts);
        true
    }

    /// Number of nonces currently retained. Useful for tests + telemetry.
    pub fn len(&self) -> usize {
        self.seen.lock().expect("replay cache mutex poisoned").len()
    }

    /// True when no nonces are currently retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Pure window-check: `ts` is within `window` of `now` in either
/// direction. Extracted so the windowing rule can be exercised
/// independently from the cache mechanics.
pub fn within_window(now: i64, ts: i64, window: Duration) -> bool {
    let w = window.as_secs() as i64;
    let delta = (now - ts).abs();
    delta <= w
}

fn system_now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after Unix epoch")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // Thread-local clock so each parallel test gets an isolated value. A single
    // shared static races under cargo's default parallel test execution (one
    // test's set_clock corrupts another's view). Still a plain `fn() -> i64`,
    // matching the with_clock signature; eviction runs on the calling thread.
    thread_local! {
        static CLOCK_NOW: Cell<i64> = const { Cell::new(1_000_000) };
    }
    fn test_clock() -> i64 {
        CLOCK_NOW.with(|c| c.get())
    }
    fn set_clock(t: i64) {
        CLOCK_NOW.with(|c| c.set(t));
    }

    #[test]
    fn within_window_accepts_exact_now() {
        assert!(within_window(1_000_000, 1_000_000, Duration::from_secs(60)));
    }

    #[test]
    fn within_window_accepts_edge_inside() {
        assert!(within_window(1_000_000, 999_940, Duration::from_secs(60)));
        assert!(within_window(1_000_000, 1_000_060, Duration::from_secs(60)));
    }

    #[test]
    fn within_window_rejects_just_outside() {
        assert!(!within_window(1_000_000, 999_939, Duration::from_secs(60)));
        assert!(!within_window(
            1_000_000,
            1_000_061,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn insert_if_fresh_accepts_first_use() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        assert!(cache.insert_if_fresh("nonce-1", 1_000_000));
    }

    #[test]
    fn insert_if_fresh_rejects_duplicate_within_window() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        assert!(cache.insert_if_fresh("nonce-1", 1_000_000));
        assert!(!cache.insert_if_fresh("nonce-1", 1_000_000));
    }

    #[test]
    fn insert_if_fresh_rejects_stale_timestamp() {
        set_clock(2_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        // ts is 10 minutes in the past relative to clock; 5-minute window.
        assert!(!cache.insert_if_fresh("nonce-1", 2_000_000 - 600));
    }

    #[test]
    fn insert_if_fresh_rejects_future_timestamp_beyond_window() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        // ts is 10 minutes in the future; 5-minute window.
        assert!(!cache.insert_if_fresh("nonce-1", 1_000_000 + 600));
    }

    #[test]
    fn insert_if_fresh_accepts_after_window_elapses_for_same_nonce() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        assert!(cache.insert_if_fresh("nonce-1", 1_000_000));
        // Advance clock past the window. The nonce's recorded timestamp
        // is now < cutoff; opportunistic eviction on the next insert
        // drops it, so the same nonce is acceptable with a fresh
        // timestamp.
        set_clock(1_000_000 + 301);
        assert!(cache.insert_if_fresh("nonce-1", 1_000_000 + 301));
    }

    #[test]
    fn eviction_purges_old_entries_on_insert() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(60), test_clock);
        for i in 0..5 {
            assert!(cache.insert_if_fresh(&format!("n-{i}"), 1_000_000 + i));
        }
        assert_eq!(cache.len(), 5);
        // Jump past the window. Any insert triggers eviction.
        set_clock(1_000_000 + 200);
        assert!(cache.insert_if_fresh("n-new", 1_000_000 + 200));
        // All original entries are stale and evicted; only the new one
        // remains.
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn distinct_nonces_coexist_within_window() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        assert!(cache.insert_if_fresh("a", 1_000_000));
        assert!(cache.insert_if_fresh("b", 1_000_001));
        assert!(cache.insert_if_fresh("c", 1_000_002));
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn default_window_is_five_minutes() {
        assert_eq!(DEFAULT_WINDOW.as_secs(), 300);
    }

    #[test]
    fn is_empty_reflects_length() {
        set_clock(1_000_000);
        let cache = ReplayCache::with_clock(Duration::from_secs(300), test_clock);
        assert!(cache.is_empty());
        assert!(cache.insert_if_fresh("nonce-1", 1_000_000));
        assert!(!cache.is_empty());
    }
}
