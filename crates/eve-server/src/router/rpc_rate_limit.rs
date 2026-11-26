//! Per-`ServerContext` RPC rate-budget policy and token-bucket rate limiter (`#1186`, `#1540`).

use eve_db::lock_rank::{RankedMutex, RANK_LEAF};
use std::collections::HashMap;

/// Burst token capacity for standard per-session RPC throttling.
pub const RPC_RATE_BURST_TOKENS: f64 = 300.0;
/// Sustained per-session RPC refill rate (tokens per second).
pub const RPC_RATE_REFILL_PER_SEC: f64 = 120.0;
/// Per-session bootstrap burst token allowance during login and undock transitions.
pub const RPC_BOOTSTRAP_BURST_ALLOWANCE: u32 = 512;
/// Minimum bootstrap burst tokens restored on cooldown-gated major session transitions
/// (initial auth binding, character switch, or undocking into space).
pub const RPC_BOOTSTRAP_TRANSITION_ALLOWANCE: u32 = 256;
/// Idle age after which stale per-session rate buckets are evicted during pruning
/// regardless of remaining `bootstrap_tokens`.
pub const RPC_RATE_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// Minimum cooldown between transition-triggered bootstrap token grants for one session.
pub const RPC_BOOTSTRAP_GRANT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);
/// Minimum bucket map size before idle bucket pruning runs on new-session insertion.
pub const RPC_RATE_PRUNE_THRESHOLD: usize = 4_096;

/// Immutable per-`ServerContext` RPC rate-budget policy (`#1540`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcRateLimitPolicy {
    /// Default production token bucket (`RPC_RATE_BURST_TOKENS` / `RPC_RATE_REFILL_PER_SEC` / bootstrap allowance).
    Default,
    /// Unbounded per-session RPC throughput with zero bucket-map lock overhead.
    Unlimited,
}

/// Per-session RPC rate budget combining a token bucket with a bounded login/undock bootstrap allowance.
#[derive(Debug, Clone)]
pub(crate) struct SessionRpcRateBucket {
    tokens: f64,
    bootstrap_tokens: u32,
    last_refill: std::time::Instant,
    last_seen: std::time::Instant,
    last_bootstrap_grant: Option<std::time::Instant>,
}

impl SessionRpcRateBucket {
    pub(crate) fn new(now: std::time::Instant) -> Self {
        Self {
            tokens: RPC_RATE_BURST_TOKENS,
            bootstrap_tokens: RPC_BOOTSTRAP_BURST_ALLOWANCE,
            last_refill: now,
            last_seen: now,
            last_bootstrap_grant: None,
        }
    }

    fn refill(&mut self, now: std::time::Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens =
                (self.tokens + elapsed * RPC_RATE_REFILL_PER_SEC).min(RPC_RATE_BURST_TOKENS);
            self.last_refill = now;
        }
    }

    pub(crate) fn try_consume(&mut self, now: std::time::Instant, is_bootstrap: bool) -> bool {
        self.refill(now);
        self.last_seen = now;
        if is_bootstrap && self.bootstrap_tokens > 0 {
            self.bootstrap_tokens -= 1;
            return true;
        }
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub(crate) fn grant_transition_bootstrap(&mut self, now: std::time::Instant) -> bool {
        if let Some(prev) = self.last_bootstrap_grant {
            if now.saturating_duration_since(prev) < RPC_BOOTSTRAP_GRANT_COOLDOWN {
                return false;
            }
        }
        self.last_bootstrap_grant = Some(now);
        self.bootstrap_tokens = self
            .bootstrap_tokens
            .max(RPC_BOOTSTRAP_TRANSITION_ALLOWANCE)
            .min(RPC_BOOTSTRAP_BURST_ALLOWANCE);
        true
    }
}

/// Per-`ServerContext` RPC rate limiter constructed with an immutable [`RpcRateLimitPolicy`].
pub(crate) struct RpcRateLimiter {
    policy: RpcRateLimitPolicy,
    buckets: RankedMutex<HashMap<u64, SessionRpcRateBucket>, RANK_LEAF>,
}

impl RpcRateLimiter {
    pub(crate) fn new(policy: RpcRateLimitPolicy) -> Self {
        Self {
            policy,
            buckets: RankedMutex::new(HashMap::new()),
        }
    }

    #[inline]
    pub(crate) fn policy(&self) -> RpcRateLimitPolicy {
        self.policy
    }

    pub(crate) fn try_consume_at(
        &self,
        session_id: u64,
        now: std::time::Instant,
        is_bootstrap: bool,
    ) -> bool {
        // Unlimited policy skips token accounting before touching the bucket mutex so
        // unbounded benchmarks and multi-actor test fixtures incur zero lock overhead.
        if matches!(self.policy, RpcRateLimitPolicy::Unlimited) {
            return true;
        }
        let mut guard = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() > RPC_RATE_PRUNE_THRESHOLD && !guard.contains_key(&session_id) {
            guard.retain(|_, bucket| {
                // Evict stale entries past idle TTL BEFORE `refill(now)` updates timestamps,
                // regardless of remaining `bootstrap_tokens`, so one bootstrap call never pins
                // an inactive bucket indefinitely (`BF-1186B-03`).
                if now.saturating_duration_since(bucket.last_seen) >= RPC_RATE_IDLE_TTL {
                    return false;
                }
                bucket.refill(now);
                bucket.tokens < RPC_RATE_BURST_TOKENS - 1e-6
                    || bucket.bootstrap_tokens < RPC_BOOTSTRAP_BURST_ALLOWANCE
            });
        }
        guard
            .entry(session_id)
            .or_insert_with(|| SessionRpcRateBucket::new(now))
            .try_consume(now, is_bootstrap)
    }

    pub(crate) fn grant_bootstrap_at(
        &self,
        session_id: u64,
        now: std::time::Instant,
    ) -> bool {
        if matches!(self.policy, RpcRateLimitPolicy::Unlimited) {
            return false;
        }
        let mut guard = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(bucket) = guard.get_mut(&session_id) {
            bucket.grant_transition_bootstrap(now)
        } else {
            false
        }
    }

    pub(crate) fn release_session(&self, session_id: u64) {
        if matches!(self.policy, RpcRateLimitPolicy::Unlimited) {
            return;
        }
        self.buckets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    }

    #[cfg(test)]
    pub(crate) fn bucket_count(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    #[cfg(test)]
    pub(crate) fn bootstrap_tokens_remaining(&self, session_id: u64) -> Option<u32> {
        self.buckets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session_id)
            .map(|b| b.bootstrap_tokens)
    }
}
