//! Keyed logarithmic warning rate limiter (`#1087`).
//!
//! Ensures repetitive client-triggered warnings (`"unknown RPC"`,
//! `"unknown Quasar notice type"`, `NOT_IN_CATALOG`, marshal oddities) log on
//! occurrences `1, 2, 3, 4, 8, 16, ...` and track suppressed counts for the
//! `#1077` ops endpoint and pre-flight summary.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Default maximum distinct keys tracked per [`WarnLimiter`] instance.
pub const DEFAULT_MAX_WARN_KEYS: usize = 4096;

/// Returns `true` when occurrence `count` (`1`-based in [`WarnLimiter::check`])
/// should emit a warning: `count <= 3 || count.is_power_of_two()`.
#[inline]
pub const fn should_log_nth(count: u64) -> bool {
    count <= 3 || count.is_power_of_two()
}

/// Canonical warning categories covered by the global logarithmic rate limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarnCategory {
    UnknownRpc,
    UnknownQuasarNoticeType,
    NotInCatalog,
    MarshalOddity,
}

impl WarnCategory {
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownRpc => "unknown RPC",
            Self::UnknownQuasarNoticeType => "unknown Quasar notice type",
            Self::NotInCatalog => "NOT_IN_CATALOG",
            Self::MarshalOddity => "marshal-oddity",
        }
    }
}

impl std::fmt::Display for WarnCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Keyed warning identity for [`global_warn_limiter`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WarnKey {
    pub category: WarnCategory,
    pub detail: String,
}

impl WarnKey {
    pub fn new(category: WarnCategory, detail: impl Into<String>) -> Self {
        Self {
            category,
            detail: detail.into(),
        }
    }
}

/// Per-key statistics retained inside [`WarnLimiter`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarnEntryStats {
    pub total: u64,
    pub logged: u64,
    pub suppressed: u64,
    #[serde(skip)]
    last_seq: u64,
}

/// Decision returned by [`WarnLimiter::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarnDecision {
    pub should_log: bool,
    pub count: u64,
    pub suppressed_for_key: u64,
    pub total_suppressed: u64,
}

#[derive(Debug)]
struct LimiterState<K> {
    seq: u64,
    total_logged: u64,
    total_suppressed: u64,
    evicted_by_category: BTreeMap<WarnCategory, (u64, u64)>,
    entries: HashMap<K, WarnEntryStats>,
}

impl<K> Default for LimiterState<K> {
    fn default() -> Self {
        Self {
            seq: 0,
            total_logged: 0,
            total_suppressed: 0,
            evicted_by_category: BTreeMap::new(),
            entries: HashMap::new(),
        }
    }
}

/// Thread-safe, bounded, keyed logarithmic warning rate limiter.
pub struct WarnLimiter<K>
where
    K: Eq + Hash + Clone,
{
    max_keys: usize,
    category_of: Option<fn(&K) -> WarnCategory>,
    total_logged: AtomicU64,
    total_suppressed: AtomicU64,
    state: Mutex<LimiterState<K>>,
}

impl<K> std::fmt::Debug for WarnLimiter<K>
where
    K: Eq + Hash + Clone + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarnLimiter")
            .field("max_keys", &self.max_keys)
            .field("total_logged", &self.total_logged.load(Ordering::Relaxed))
            .field("total_suppressed", &self.total_suppressed.load(Ordering::Relaxed))
            .finish()
    }
}

impl<K> Default for WarnLimiter<K>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K> WarnLimiter<K>
where
    K: Eq + Hash + Clone,
{
    pub fn new() -> Self {
        Self::with_max_keys(DEFAULT_MAX_WARN_KEYS)
    }

    pub fn with_max_keys(max_keys: usize) -> Self {
        Self::with_max_keys_and_category(max_keys, None)
    }

    fn with_max_keys_and_category(
        max_keys: usize,
        category_of: Option<fn(&K) -> WarnCategory>,
    ) -> Self {
        Self {
            max_keys: max_keys.max(1),
            category_of,
            total_logged: AtomicU64::new(0),
            total_suppressed: AtomicU64::new(0),
            state: Mutex::new(LimiterState::default()),
        }
    }

    /// Records one occurrence of `key` and returns whether this occurrence should be logged.
    pub fn check(&self, key: K) -> WarnDecision {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.seq = guard.seq.saturating_add(1);
        let seq = guard.seq;

        if !guard.entries.contains_key(&key) && guard.entries.len() >= self.max_keys {
            if let Some(evict_key) = guard
                .entries
                .iter()
                .min_by_key(|(_, st)| st.last_seq)
                .map(|(k, _)| k.clone())
            {
                if let Some(evicted) = guard.entries.remove(&evict_key) {
                    if let Some(cat_fn) = self.category_of {
                        let cat = cat_fn(&evict_key);
                        let bucket = guard.evicted_by_category.entry(cat).or_default();
                        bucket.0 = bucket.0.saturating_add(evicted.logged);
                        bucket.1 = bucket.1.saturating_add(evicted.suppressed);
                    }
                }
            }
        }

        let entry = guard.entries.entry(key).or_default();
        entry.total = entry.total.saturating_add(1);
        entry.last_seq = seq;
        let count = entry.total;
        let should_log = should_log_nth(count);
        if should_log {
            entry.logged = entry.logged.saturating_add(1);
        } else {
            entry.suppressed = entry.suppressed.saturating_add(1);
        }
        let suppressed_for_key = entry.suppressed;
        if should_log {
            guard.total_logged = guard.total_logged.saturating_add(1);
            self.total_logged.store(guard.total_logged, Ordering::Release);
        } else {
            guard.total_suppressed = guard.total_suppressed.saturating_add(1);
            self.total_suppressed
                .store(guard.total_suppressed, Ordering::Release);
        }
        let total_suppressed = guard.total_suppressed;

        WarnDecision {
            should_log,
            count,
            suppressed_for_key,
            total_suppressed,
        }
    }

    pub fn suppressed_count(&self, key: &K) -> u64 {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.get(key).map(|s| s.suppressed).unwrap_or(0)
    }

    pub fn total_count(&self, key: &K) -> u64 {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.get(key).map(|s| s.total).unwrap_or(0)
    }

    pub fn total_logged(&self) -> u64 {
        self.total_logged.load(Ordering::Acquire)
    }

    pub fn total_suppressed(&self) -> u64 {
        self.total_suppressed.load(Ordering::Acquire)
    }

    pub fn snapshot_entries(&self) -> Vec<(K, WarnEntryStats)> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    pub fn clear(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.clear();
        guard.evicted_by_category.clear();
        guard.seq = 0;
        guard.total_logged = 0;
        guard.total_suppressed = 0;
        self.total_logged.store(0, Ordering::Release);
        self.total_suppressed.store(0, Ordering::Release);
    }
}

impl WarnLimiter<WarnKey> {
    /// Creates a `WarnKey`-keyed limiter that preserves evicted category totals
    /// in [`Self::summary`].
    pub fn with_max_keys_categorized(max_keys: usize) -> Self {
        Self::with_max_keys_and_category(max_keys, Some(|k: &WarnKey| k.category))
    }

    /// Captures a single-lock atomic summary across active and evicted keys.
    pub fn summary(&self) -> WarnSuppressedSummary {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_category_suppressed: BTreeMap<String, u64> = BTreeMap::new();
        let mut by_category_logged: BTreeMap<String, u64> = BTreeMap::new();

        for (cat, &(ev_logged, ev_suppressed)) in &guard.evicted_by_category {
            let cat_name = cat.as_str().to_string();
            *by_category_logged.entry(cat_name.clone()).or_insert(0) += ev_logged;
            *by_category_suppressed.entry(cat_name).or_insert(0) += ev_suppressed;
        }

        let mut keys: Vec<WarnKeySummary> = Vec::with_capacity(guard.entries.len());
        for (key, st) in &guard.entries {
            let cat = key.category.as_str().to_string();
            *by_category_suppressed.entry(cat.clone()).or_insert(0) += st.suppressed;
            *by_category_logged.entry(cat).or_insert(0) += st.logged;
            if st.suppressed > 0 {
                keys.push(WarnKeySummary {
                    category: key.category,
                    detail: key.detail.clone(),
                    total: st.total,
                    logged: st.logged,
                    suppressed: st.suppressed,
                });
            }
        }
        keys.sort_by(|a, b| {
            b.suppressed
                .cmp(&a.suppressed)
                .then_with(|| a.category.cmp(&b.category))
                .then_with(|| a.detail.cmp(&b.detail))
        });
        keys.truncate(32);

        WarnSuppressedSummary {
            total_logged: guard.total_logged,
            total_suppressed: guard.total_suppressed,
            by_category_suppressed,
            by_category_logged,
            top_suppressed_keys: keys,
        }
    }
}

/// Summary of warning suppression exposed on ops/telemetry snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarnSuppressedSummary {
    pub total_logged: u64,
    pub total_suppressed: u64,
    pub by_category_suppressed: BTreeMap<String, u64>,
    pub by_category_logged: BTreeMap<String, u64>,
    pub top_suppressed_keys: Vec<WarnKeySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarnKeySummary {
    pub category: WarnCategory,
    pub detail: String,
    pub total: u64,
    pub logged: u64,
    pub suppressed: u64,
}

static GLOBAL_WARN_LIMITER: OnceLock<WarnLimiter<WarnKey>> = OnceLock::new();

/// Process-wide [`WarnLimiter`] shared across RPC dispatch, Quasar gateway, and marshal framing.
pub fn global_warn_limiter() -> &'static WarnLimiter<WarnKey> {
    GLOBAL_WARN_LIMITER
        .get_or_init(|| WarnLimiter::with_max_keys_categorized(DEFAULT_MAX_WARN_KEYS))
}

/// Checks whether a global warning for `(category, detail)` should be emitted on this occurrence.
pub fn check_global_warn(category: WarnCategory, detail: impl Into<String>) -> WarnDecision {
    global_warn_limiter().check(WarnKey::new(category, detail))
}

/// Returns the current global suppressed warning summary for the `#1077` ops endpoint.
pub fn global_warn_suppressed_summary() -> WarnSuppressedSummary {
    global_warn_limiter().summary()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_log_nth_follows_logarithmic_schedule() {
        let expected_logged = [1_u64, 2, 3, 4, 8, 16, 32, 64];
        for n in 1_u64..=64 {
            assert_eq!(
                should_log_nth(n),
                expected_logged.contains(&n),
                "unexpected schedule for count={n}"
            );
        }
        assert!(should_log_nth(1) && !should_log_nth(5));
    }

    #[test]
    fn warn_limiter_tracks_suppressed_counts_and_evicts_oldest_when_capped() {
        let limiter: WarnLimiter<WarnKey> = WarnLimiter::with_max_keys(3);
        let k_rpc = WarnKey::new(WarnCategory::UnknownRpc, "svc.Missing");
        let k_notice = WarnKey::new(WarnCategory::UnknownQuasarNoticeType, "foo.Notice");

        let mut logged_counts = Vec::new();
        for i in 1_u64..=16 {
            let dec = limiter.check(k_rpc.clone());
            assert_eq!(dec.count, i);
            if dec.should_log {
                logged_counts.push(i);
            }
        }
        assert_eq!(logged_counts, vec![1, 2, 3, 4, 8, 16]);
        assert_eq!(limiter.total_count(&k_rpc), 16);
        assert_eq!(limiter.suppressed_count(&k_rpc), 10);

        // Distinct key starts fresh at 1..=4 (powers of two) and suppresses 5.
        assert!(limiter.check(k_notice.clone()).should_log);
        assert!(limiter.check(k_notice.clone()).should_log);
        assert!(limiter.check(k_notice.clone()).should_log);
        assert!(limiter.check(k_notice.clone()).should_log);
        assert!(!limiter.check(k_notice.clone()).should_log);
        assert_eq!(limiter.suppressed_count(&k_notice), 1);
        assert_eq!(limiter.total_suppressed(), 11);
        assert_eq!(limiter.total_logged(), 10);

        // Exceed max_keys(3): oldest key gets evicted.
        let k3 = WarnKey::new(WarnCategory::NotInCatalog, "svc.Unlisted");
        let k4 = WarnKey::new(WarnCategory::MarshalOddity, "frame.truncated");
        assert!(limiter.check(k3).should_log);
        assert!(limiter.check(k4).should_log);
        assert_eq!(limiter.snapshot_entries().len(), 3);
    }

    #[test]
    fn warn_limiter_preserves_evicted_category_totals_and_handles_edge_bounds() {
        assert!(should_log_nth(0));
        assert!(!should_log_nth(u64::MAX));
        assert!(should_log_nth(1_u64 << 63));

        let clamp_zero: WarnLimiter<WarnKey> = WarnLimiter::with_max_keys_categorized(0);
        for _ in 0..5 {
            let _ = clamp_zero.check(WarnKey::new(WarnCategory::UnknownRpc, "a.B"));
        }
        assert!(clamp_zero.check(WarnKey::new(WarnCategory::NotInCatalog, "c.D")).should_log);
        assert_eq!(clamp_zero.snapshot_entries().len(), 1);
        let summary = clamp_zero.summary();
        assert_eq!(summary.total_logged, 5);
        assert_eq!(summary.total_suppressed, 1);
        assert_eq!(summary.by_category_logged.values().sum::<u64>(), summary.total_logged);
        assert_eq!(
            summary.by_category_suppressed.values().sum::<u64>(),
            summary.total_suppressed
        );
    }

    #[test]
    fn warn_limiter_is_consistent_under_concurrent_updates_and_evictions() {
        use std::sync::Arc;

        let limiter = Arc::new(WarnLimiter::with_max_keys_categorized(4));
        let mut handles = Vec::new();
        for tid in 0..4_u64 {
            let l = Arc::clone(&limiter);
            handles.push(std::thread::spawn(move || {
                for i in 0..50_u64 {
                    let key = WarnKey::new(
                        WarnCategory::UnknownQuasarNoticeType,
                        format!("notice.{}", (tid + i) % 8),
                    );
                    let _ = l.check(key);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker panicked");
        }

        let s = limiter.summary();
        assert_eq!(s.total_logged + s.total_suppressed, 200);
        assert_eq!(s.by_category_logged.values().sum::<u64>(), s.total_logged);
        assert_eq!(
            s.by_category_suppressed.values().sum::<u64>(),
            s.total_suppressed
        );
        assert_eq!(limiter.snapshot_entries().len(), 4);
    }
}
